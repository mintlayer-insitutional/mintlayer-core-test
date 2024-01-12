// Copyright (c) 2023 RBB S.r.l
// opensource@mintlayer.org
// SPDX-License-Identifier: MIT
// Licensed under the MIT License;
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// https://github.com/mintlayer/mintlayer-core/blob/master/LICENSE
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::blockchain_state::BlockchainState;

use mempool::FeeRate;
use serialization::Encode;

use super::*;

use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
    time::Duration,
};

use api_server_common::storage::{
    impls::in_memory::transactional::TransactionalApiServerInMemoryStorage,
    storage_api::{
        ApiServerStorageRead, ApiServerStorageWrite, ApiServerTransactionRw, Transactional,
    },
};

use chainstate::{BlockSource, ChainInfo};
use chainstate_test_framework::{TestFramework, TransactionBuilder};
use common::{
    address::Address,
    chain::{
        output_value::OutputValue,
        signature::{
            inputsig::{
                authorize_pubkey_spend::sign_pubkey_spending,
                standard_signature::StandardInputSignature, InputWitness,
            },
            sighash::{sighashtype::SigHashType, signature_hash},
        },
        stakelock::StakePoolData,
        CoinUnit, Destination, OutPointSourceId, PoolId, SignedTransaction, TxInput, TxOutput,
        UtxoOutPoint,
    },
    primitives::{per_thousand::PerThousand, Amount, CoinOrTokenId, Idable, H256},
};
use crypto::{
    key::{KeyKind, PrivateKey},
    random::{seq::IteratorRandom, CryptoRng, Rng},
    vrf::{VRFKeyKind, VRFPrivateKey},
};
use logging::log;
use pos_accounting::{make_delegation_id, make_pool_id};
use rstest::rstest;
use test_utils::random::{make_seedable_rng, Seed};
use tokio::sync::mpsc;

struct MockLocalState {
    genesis_id: Id<GenBlock>,
    blocks: Vec<Id<Block>>,
    new_tip_tx: mpsc::UnboundedSender<Id<Block>>,
}

impl MockLocalState {
    fn new(chain_config: &ChainConfig, new_tip_tx: mpsc::UnboundedSender<Id<Block>>) -> Self {
        Self {
            genesis_id: chain_config.genesis_block_id(),
            blocks: Vec::new(),
            new_tip_tx,
        }
    }

    fn get_best_block_id(&self) -> Id<GenBlock> {
        self.blocks.last().cloned().map_or(self.genesis_id, Into::into)
    }

    fn get_block_height(&self) -> BlockHeight {
        BlockHeight::from(self.blocks.len() as u64)
    }
}

#[async_trait::async_trait]
impl LocalBlockchainState for MockLocalState {
    type Error = Infallible;

    async fn best_block(&self) -> Result<(BlockHeight, Id<GenBlock>), Self::Error> {
        Ok((self.get_block_height(), self.get_best_block_id()))
    }

    async fn scan_blocks(
        &mut self,
        common_block_height: BlockHeight,
        blocks: Vec<Block>,
    ) -> Result<(), Self::Error> {
        assert!(!blocks.is_empty());
        assert!(
            common_block_height <= self.get_block_height(),
            "Invalid common block height: {common_block_height}, max: {}",
            self.get_block_height()
        );

        self.blocks.truncate(common_block_height.into_int() as usize);
        for block in blocks {
            assert_eq!(*block.header().prev_block_id(), self.get_best_block_id());
            self.blocks.push(block.header().block_id());
            let _ = self.new_tip_tx.send(block.header().block_id());
        }

        log::debug!(
            "new block added to local state: {}, block height: {}",
            self.get_best_block_id(),
            self.get_block_height()
        );

        Ok(())
    }
}

#[derive(Clone)]
struct MockRemoteNode {
    tf: Arc<Mutex<TestFramework>>,
}

impl MockRemoteNode {
    fn new(rng: &mut (impl Rng + CryptoRng)) -> Self {
        let tf = Arc::new(Mutex::new(TestFramework::builder(rng).build()));
        Self { tf }
    }
}

#[async_trait::async_trait]
impl RemoteNode for MockRemoteNode {
    type Error = Infallible;

    async fn chainstate(&self) -> Result<ChainInfo, Self::Error> {
        Ok(self.tf.lock().unwrap().chainstate.info().unwrap())
    }
    async fn last_common_ancestor(
        &self,
        first_block: Id<GenBlock>,
        second_block: Id<GenBlock>,
    ) -> Result<Option<(Id<GenBlock>, BlockHeight)>, Self::Error> {
        Ok(self
            .tf
            .lock()
            .unwrap()
            .chainstate
            .last_common_ancestor_by_id(&first_block, &second_block)
            .unwrap())
    }

    async fn mainchain_blocks(
        &self,
        from: BlockHeight,
        max_count: usize,
    ) -> Result<Vec<Block>, Self::Error> {
        Ok(self
            .tf
            .lock()
            .unwrap()
            .chainstate
            .get_mainchain_blocks(from, max_count)
            .unwrap())
    }

    async fn mempool_feerate_points(&self) -> Result<Vec<(usize, FeeRate)>, Self::Error> {
        Ok(vec![(
            1,
            FeeRate::from_amount_per_kb(Amount::from_atoms(1)),
        )])
    }
}

fn create_chain(
    node: &MockRemoteNode,
    rng: &mut (impl Rng + CryptoRng),
    parent: u64,
    count: usize,
) {
    let mut tf = node.tf.lock().unwrap();
    let parent_id = tf.chainstate.get_block_id_from_height(&parent.into()).unwrap().unwrap();
    tf.create_chain(&parent_id, count, rng).unwrap();
}

async fn wait_new_tip(node: &MockRemoteNode, new_tip_tx: &mut mpsc::UnboundedReceiver<Id<Block>>) {
    let expected_block_id = node.tf.lock().unwrap().best_block_id();
    let wait_fut = async move { while new_tip_tx.recv().await.unwrap() != expected_block_id {} };
    tokio::time::timeout(Duration::from_secs(60), wait_fut).await.unwrap();
}

fn run_sync(chain_config: Arc<ChainConfig>, node: MockRemoteNode, mut local_state: MockLocalState) {
    tokio::spawn(async move {
        loop {
            let _ = sync_once(&chain_config, &node, &mut local_state).await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
}

#[rstest]
#[trace]
#[case(test_utils::random::Seed::from_entropy())]
#[tokio::test]
async fn basic_sync(#[case] seed: Seed) {
    let mut rng = make_seedable_rng(seed);
    let node = MockRemoteNode::new(&mut rng);
    let chain_config = Arc::clone(node.tf.lock().unwrap().chainstate.get_chain_config());
    let (new_tip_tx, mut new_tip_rx) = mpsc::unbounded_channel();
    let local_state = MockLocalState::new(&chain_config, new_tip_tx);

    run_sync(Arc::clone(&chain_config), node.clone(), local_state);

    // Build blocks
    for height in 1..10 {
        create_chain(&node, &mut rng, height - 1, 1);
        wait_new_tip(&node, &mut new_tip_rx).await;
    }

    // Reorgs
    for height in 10..20 {
        create_chain(&node, &mut rng, height - 5, 5);
        wait_new_tip(&node, &mut new_tip_rx).await;
    }

    // More blocks
    for height in 20..30 {
        create_chain(&node, &mut rng, height - 1, 1);
        wait_new_tip(&node, &mut new_tip_rx).await;
    }

    // More reorgs
    for height in 30..40 {
        create_chain(&node, &mut rng, height - 5, 5);
        wait_new_tip(&node, &mut new_tip_rx).await;
    }
}

#[rstest]
#[trace]
#[case(test_utils::random::Seed::from_entropy())]
#[tokio::test]
async fn restart_from_genesis(#[case] seed: Seed) {
    let mut rng = make_seedable_rng(seed);
    let node = MockRemoteNode::new(&mut rng);
    let chain_config = Arc::clone(node.tf.lock().unwrap().chainstate.get_chain_config());
    let (new_tip_tx, mut new_tip_rx) = mpsc::unbounded_channel();
    let local_state = MockLocalState::new(&chain_config, new_tip_tx);

    run_sync(Arc::clone(&chain_config), node.clone(), local_state);

    create_chain(&node, &mut rng, 0, 10);
    wait_new_tip(&node, &mut new_tip_rx).await;

    *node.tf.lock().unwrap() = TestFramework::builder(&mut rng).build();

    create_chain(&node, &mut rng, 0, 10);
    wait_new_tip(&node, &mut new_tip_rx).await;
}

#[rstest]
#[trace]
#[case(test_utils::random::Seed::from_entropy())]
#[tokio::test]
async fn randomized(#[case] seed: Seed) {
    let mut rng = make_seedable_rng(seed);
    let node = MockRemoteNode::new(&mut rng);
    let chain_config = Arc::clone(node.tf.lock().unwrap().chainstate.get_chain_config());
    let (new_tip_tx, mut new_tip_rx) = mpsc::unbounded_channel();
    let local_state = MockLocalState::new(&chain_config, new_tip_tx);

    run_sync(Arc::clone(&chain_config), node.clone(), local_state);

    create_chain(&node, &mut rng, 0, 1);
    wait_new_tip(&node, &mut new_tip_rx).await;

    for _ in 0..100 {
        let new_tip = {
            let mut tf = node.tf.lock().unwrap();
            let old_best_block = tf.best_block_id();
            // Select a random block from the 5 latest to build a new chain
            let parent =
                *tf.block_indexes.iter().rev().take(5).choose(&mut rng).unwrap().block_id();
            tf.create_chain(&parent.into(), 1, &mut rng).unwrap();
            old_best_block != tf.best_block_id()
        };

        if new_tip {
            wait_new_tip(&node, &mut new_tip_rx).await;
        }
    }
}

#[rstest]
#[trace]
#[case(test_utils::random::Seed::from_entropy())]
#[tokio::test]
async fn basic_sync_real_state(#[case] seed: Seed) {
    let mut rng = make_seedable_rng(seed);

    let initial_pledge = 40_000 * CoinUnit::ATOMS_PER_COIN + rng.gen_range(10000..100000);
    let (staking_sk, pk) = PrivateKey::new_from_rng(&mut rng, KeyKind::Secp256k1Schnorr);
    let (vrf_sk, vrf_pk) = VRFPrivateKey::new_from_rng(&mut rng, VRFKeyKind::Schnorrkel);
    let staking_key = Destination::PublicKey(pk.clone());
    let pool_data = StakePoolData::new(
        Amount::from_atoms(initial_pledge),
        staking_key.clone(),
        vrf_pk,
        staking_key.clone(),
        PerThousand::new_from_rng(&mut rng),
        Amount::from_atoms(rng.gen_range(0..100)),
    );
    let pool_id = PoolId::new(H256::random_using(&mut rng));

    let chain_config = chainstate_test_framework::create_chain_config_with_staking_pool(
        Amount::from_atoms(initial_pledge * 2),
        pool_id,
        pool_data,
    )
    .build();
    let mut tf = TestFramework::builder(&mut rng).with_chain_config(chain_config).build();

    let chain_config = Arc::clone(tf.chainstate.get_chain_config());
    let storage = {
        let mut storage = TransactionalApiServerInMemoryStorage::new(&chain_config);

        let mut db_tx = storage.transaction_rw().await.unwrap();
        db_tx.initialize_storage(&chain_config).await.unwrap();
        db_tx.commit().await.unwrap();

        storage
    };
    let mut local_state = BlockchainState::new(chain_config.clone(), storage);
    local_state.scan_genesis(chain_config.genesis_block().as_ref()).await.unwrap();

    let remaining_coins = initial_pledge;
    eprintln!("coins: {remaining_coins}");
    let transaction = TransactionBuilder::new()
        .add_input(
            TxInput::from_utxo(
                OutPointSourceId::BlockReward(chain_config.genesis_block_id()),
                0,
            ),
            InputWitness::NoSignature(None),
        )
        .add_output(TxOutput::Transfer(
            OutputValue::Coin(Amount::from_atoms(remaining_coins)),
            Destination::AnyoneCanSpend,
        ))
        .build();

    let prev_block_hash = chain_config.genesis_block_id();
    let prev_tx_id = transaction.transaction().get_id();
    let target_block_time = chain_config.target_block_spacing();
    let block = create_block(
        &mut tf,
        target_block_time,
        &mut rng,
        prev_block_hash,
        staking_sk.clone(),
        vrf_sk.clone(),
        pool_id,
        vec![transaction],
    );

    let prev_block_hash = block.get_id();
    sync_and_compare(&mut tf, block, &mut local_state, pool_id).await;

    let remaining_coins = remaining_coins - rng.gen_range(0..10);
    eprintln!("coins: {remaining_coins}");
    let transaction = TransactionBuilder::new()
        .add_input(
            TxInput::from_utxo(OutPointSourceId::Transaction(prev_tx_id), 0),
            InputWitness::NoSignature(None),
        )
        .add_output(TxOutput::Transfer(
            OutputValue::Coin(Amount::from_atoms(remaining_coins)),
            Destination::AnyoneCanSpend,
        ))
        .build();

    let prev_tx_id = transaction.transaction().get_id();
    tf.progress_time_seconds_since_epoch(target_block_time.as_secs());
    let block = create_block(
        &mut tf,
        target_block_time,
        &mut rng,
        prev_block_hash.into(),
        staking_sk.clone(),
        vrf_sk.clone(),
        pool_id,
        vec![transaction],
    );

    let prev_block_hash = block.get_id();
    sync_and_compare(&mut tf, block, &mut local_state, pool_id).await;

    let remaining_coins = remaining_coins - rng.gen_range(0..10);
    eprintln!("coins: {remaining_coins}");
    let transaction = TransactionBuilder::new()
        .add_input(
            TxInput::from_utxo(OutPointSourceId::Transaction(prev_tx_id), 0),
            InputWitness::NoSignature(None),
        )
        .add_output(TxOutput::Transfer(
            OutputValue::Coin(Amount::from_atoms(remaining_coins)),
            Destination::AnyoneCanSpend,
        ))
        .add_output(TxOutput::CreateDelegationId(
            Destination::AnyoneCanSpend,
            pool_id,
        ))
        .build();
    let delegation_id = make_delegation_id(&UtxoOutPoint::new(
        OutPointSourceId::Transaction(prev_tx_id),
        0,
    ));
    let prev_tx_id = transaction.transaction().get_id();

    let amount_to_stake = rng.gen_range(100..1000);
    let remaining_coins = remaining_coins - amount_to_stake - rng.gen_range(0..10);
    eprintln!("coins: {remaining_coins} {amount_to_stake}");
    let stake_transaction = TransactionBuilder::new()
        .add_input(
            TxInput::from_utxo(OutPointSourceId::Transaction(prev_tx_id), 0),
            InputWitness::NoSignature(None),
        )
        .add_output(TxOutput::Transfer(
            OutputValue::Coin(Amount::from_atoms(remaining_coins)),
            Destination::AnyoneCanSpend,
        ))
        .add_output(TxOutput::DelegateStaking(
            Amount::from_atoms(amount_to_stake),
            delegation_id,
        ))
        .build();
    let prev_tx_id = stake_transaction.transaction().get_id();

    tf.progress_time_seconds_since_epoch(target_block_time.as_secs());
    let block = create_block(
        &mut tf,
        target_block_time,
        &mut rng,
        prev_block_hash.into(),
        staking_sk.clone(),
        vrf_sk.clone(),
        pool_id,
        vec![transaction, stake_transaction],
    );

    let prev_block_hash = block.get_id();
    sync_and_compare(&mut tf, block, &mut local_state, pool_id).await;

    let remaining_coins = remaining_coins - rng.gen_range(0..10);
    eprintln!("coins: {remaining_coins}");
    let transaction = TransactionBuilder::new()
        .add_input(
            TxInput::from_utxo(OutPointSourceId::Transaction(prev_tx_id), 0),
            InputWitness::NoSignature(None),
        )
        .add_output(TxOutput::Transfer(
            OutputValue::Coin(Amount::from_atoms(remaining_coins)),
            Destination::AnyoneCanSpend,
        ))
        .build();
    let prev_tx_id = transaction.transaction().get_id();

    tf.progress_time_seconds_since_epoch(target_block_time.as_secs());
    let block = create_block(
        &mut tf,
        target_block_time,
        &mut rng,
        prev_block_hash.into(),
        staking_sk.clone(),
        vrf_sk.clone(),
        pool_id,
        vec![transaction],
    );

    let prev_block_hash = block.get_id();
    sync_and_compare(&mut tf, block, &mut local_state, pool_id).await;

    let initial_pledge = 40_000 * CoinUnit::ATOMS_PER_COIN
        + rng.gen_range(
            0..remaining_coins - chain_config.min_stake_pool_pledge().into_atoms() - 100,
        );
    let (new_staking_sk, new_pk) = PrivateKey::new_from_rng(&mut rng, KeyKind::Secp256k1Schnorr);
    let (new_vrf_sk, vrf_pk) = VRFPrivateKey::new_from_rng(&mut rng, VRFKeyKind::Schnorrkel);
    let staking_key = Destination::PublicKey(new_pk);
    let pool_data = StakePoolData::new(
        Amount::from_atoms(initial_pledge),
        staking_key.clone(),
        vrf_pk,
        staking_key.clone(),
        PerThousand::new_from_rng(&mut rng),
        Amount::from_atoms(rng.gen_range(0..100)),
    );
    let new_pool_id = make_pool_id(&UtxoOutPoint::new(
        OutPointSourceId::Transaction(prev_tx_id),
        0,
    ));

    eprintln!("coins {remaining_coins}, {initial_pledge}");
    let remaining_coins = remaining_coins - initial_pledge - rng.gen_range(0..10);
    let transaction = TransactionBuilder::new()
        .add_input(
            TxInput::from_utxo(OutPointSourceId::Transaction(prev_tx_id), 0),
            InputWitness::NoSignature(None),
        )
        .add_output(TxOutput::Transfer(
            OutputValue::Coin(Amount::from_atoms(remaining_coins)),
            Destination::AnyoneCanSpend,
        ))
        .add_output(TxOutput::CreateStakePool(new_pool_id, Box::new(pool_data)))
        .build();
    let prev_tx_id = transaction.transaction().get_id();
    let new_pool_tx_id = prev_tx_id;
    let coin_tx_out = transaction.transaction().outputs()[0].clone();

    tf.progress_time_seconds_since_epoch(target_block_time.as_secs());
    let block = create_block(
        &mut tf,
        target_block_time,
        &mut rng,
        prev_block_hash.into(),
        staking_sk.clone(),
        vrf_sk.clone(),
        pool_id,
        vec![transaction],
    );
    let from_block_output = block.block_reward().outputs()[0].clone();

    let prev_block_hash = block.get_id();
    sync_and_compare(&mut tf, block, &mut local_state, pool_id).await;

    let remaining_coins = remaining_coins - rng.gen_range(0..10);
    let transaction = TransactionBuilder::new()
        .add_input(
            TxInput::from_utxo(OutPointSourceId::Transaction(prev_tx_id), 0),
            InputWitness::NoSignature(None),
        )
        .add_input(
            TxInput::from_utxo(OutPointSourceId::BlockReward(prev_block_hash.into()), 0),
            InputWitness::NoSignature(None),
        )
        .add_output(TxOutput::Transfer(
            OutputValue::Coin(Amount::from_atoms(remaining_coins)),
            Destination::AnyoneCanSpend,
        ))
        .build();

    let sighash = signature_hash(
        SigHashType::default(),
        transaction.transaction(),
        &[Some(&coin_tx_out), Some(&from_block_output)],
        1,
    )
    .unwrap();

    let signature = sign_pubkey_spending(&staking_sk, &pk, &sighash).unwrap();

    let input_witness = InputWitness::Standard(StandardInputSignature::new(
        SigHashType::default(),
        signature.encode(),
    ));

    let transaction = SignedTransaction::new(
        transaction.transaction().clone(),
        vec![InputWitness::NoSignature(None), input_witness],
    )
    .unwrap();

    let staker_balance = local_state
        .storage()
        .transaction_ro()
        .await
        .unwrap()
        .get_pool_data(pool_id)
        .await
        .unwrap()
        .unwrap()
        .staker_balance()
        .unwrap();

    tf.progress_time_seconds_since_epoch(target_block_time.as_secs());
    let block = tf
        .make_pos_block_builder(&mut rng)
        .with_parent(prev_block_hash.into())
        .with_block_signing_key(new_staking_sk.clone())
        .with_stake_spending_key(new_staking_sk)
        .with_vrf_key(new_vrf_sk.clone())
        .with_stake_pool(new_pool_id)
        .with_kernel_input(UtxoOutPoint::new(
            OutPointSourceId::Transaction(new_pool_tx_id),
            1,
        ))
        .with_transactions(vec![transaction])
        .build();

    sync_and_compare(&mut tf, block, &mut local_state, new_pool_id).await;
    let decommissioned_pool = local_state
        .storage()
        .transaction_ro()
        .await
        .unwrap()
        .get_pool_data(pool_id)
        .await
        .unwrap()
        .unwrap();

    // after decommission the staker balance is 0
    assert_eq!(decommissioned_pool.staker_balance().unwrap(), Amount::ZERO);
    let address = Address::<Destination>::new(
        tf.chain_config(),
        decommissioned_pool.decommission_destination(),
    )
    .expect("Unable to encode destination");

    //  but the address still has the same balance
    let balance = local_state
        .storage()
        .transaction_ro()
        .await
        .unwrap()
        .get_address_balance(address.get(), CoinOrTokenId::Coin)
        .await
        .unwrap()
        .unwrap_or(Amount::ZERO);

    assert_eq!(balance, staker_balance);
}

#[allow(clippy::too_many_arguments)]
fn create_block(
    tf: &mut TestFramework,
    target_block_time: Duration,
    rng: &mut (impl Rng + CryptoRng),
    prev_block_hash: Id<GenBlock>,
    staking_sk: PrivateKey,
    vrf_sk: VRFPrivateKey,
    pool_id: PoolId,
    transactions: Vec<SignedTransaction>,
) -> Block {
    tf.progress_time_seconds_since_epoch(target_block_time.as_secs());
    let block = tf
        .make_pos_block_builder(rng)
        .with_parent(prev_block_hash)
        .with_block_signing_key(staking_sk.clone())
        .with_stake_spending_key(staking_sk)
        .with_vrf_key(vrf_sk.clone())
        .with_stake_pool(pool_id)
        .with_transactions(transactions)
        .build();
    block
}

async fn sync_and_compare(
    tf: &mut TestFramework,
    block: Block,
    local_state: &mut BlockchainState<TransactionalApiServerInMemoryStorage>,
    pool_id: PoolId,
) {
    tf.process_block(block.clone(), BlockSource::Local).unwrap();
    let block_height = local_state
        .storage()
        .transaction_ro()
        .await
        .unwrap()
        .get_best_block()
        .await
        .unwrap()
        .0;
    local_state.scan_blocks(block_height, vec![block]).await.unwrap();

    let node_data = tf.chainstate.get_stake_pool_data(pool_id).unwrap().unwrap();

    let tx = local_state.storage().transaction_ro().await.unwrap();
    let scanner_data = tx.get_pool_data(pool_id).await.unwrap().unwrap();

    assert_eq!(node_data.staker_balance(), scanner_data.staker_balance());

    let address =
        Address::<Destination>::new(tf.chain_config(), scanner_data.decommission_destination())
            .expect("Unable to encode destination");

    let balance = tx
        .get_address_balance(address.get(), CoinOrTokenId::Coin)
        .await
        .unwrap()
        .unwrap_or(Amount::ZERO);

    assert_eq!(balance, scanner_data.staker_balance().unwrap());

    let node_delegations = tf
        .chainstate
        .get_stake_pool_delegations_shares(pool_id)
        .unwrap()
        .unwrap_or_default();

    let scanner_delegations = tx.get_pool_delegations(pool_id).await.unwrap();

    assert_eq!(node_delegations.len(), scanner_delegations.len());

    for (id, share) in node_delegations {
        let scanner_delegation = scanner_delegations.get(&id).unwrap();
        assert_eq!(&share, scanner_delegation.balance());

        let address =
            Address::<Destination>::new(tf.chain_config(), scanner_delegation.spend_destination())
                .expect("Unable to encode destination");

        let balance = tx
            .get_address_balance(address.get(), CoinOrTokenId::Coin)
            .await
            .unwrap()
            .unwrap_or(Amount::ZERO);

        assert_eq!(balance, share);
    }
}
