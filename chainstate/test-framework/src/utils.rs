// Copyright (c) 2022 RBB S.r.l
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

use crate::{framework::BlockOutputs, TestFramework};
use chainstate_storage::{BlockchainStorageRead, TipStorageTag};
use chainstate_types::pos_randomness::PoSRandomness;
use common::{
    chain::{
        block::{consensus_data::PoSData, timestamp::BlockTimestamp, BlockRewardTransactable},
        config::{create_unit_test_config, Builder as ConfigBuilder, ChainType, EpochIndex},
        output_value::OutputValue,
        signature::{
            inputsig::{standard_signature::StandardInputSignature, InputWitness},
            sighash::sighashtype::SigHashType,
        },
        stakelock::StakePoolData,
        Block, CoinUnit, ConsensusUpgrade, Destination, GenBlock, Genesis, NetUpgrades,
        OutPointSourceId, PoSChainConfig, PoSChainConfigBuilder, PoolId, TxInput, TxOutput,
        UtxoOutPoint,
    },
    primitives::{per_thousand::PerThousand, Amount, BlockHeight, Compact, Id, Idable, H256},
    Uint256,
};
use crypto::{
    key::{PrivateKey, PublicKey},
    random::Rng,
    vrf::{VRFPrivateKey, VRFPublicKey},
};
use pos_accounting::{PoSAccountingDB, PoSAccountingView};

pub fn empty_witness(rng: &mut impl Rng) -> InputWitness {
    use crypto::random::SliceRandom;
    let mut msg: Vec<u8> = (1..100).collect();
    msg.shuffle(rng);
    InputWitness::NoSignature(Some(msg))
}

pub fn anyonecanspend_address() -> Destination {
    Destination::AnyoneCanSpend
}

pub fn get_output_value(output: &TxOutput) -> Option<OutputValue> {
    match output {
        TxOutput::Transfer(v, _) | TxOutput::LockThenTransfer(v, _, _) | TxOutput::Burn(v) => {
            Some(v.clone())
        }
        TxOutput::CreateStakePool(_, _)
        | TxOutput::ProduceBlockFromStake(_, _)
        | TxOutput::CreateDelegationId(_, _)
        | TxOutput::DelegateStaking(_, _)
        | TxOutput::IssueFungibleToken(_)
        | TxOutput::DataDeposit(_) => None,
        TxOutput::IssueNft(token_id, _, _) => {
            Some(OutputValue::TokenV1(*token_id, Amount::from_atoms(1)))
        }
    }
}

pub fn create_new_outputs(
    srcid: OutPointSourceId,
    outs: &[TxOutput],
    rng: &mut impl Rng,
) -> Vec<(InputWitness, TxInput, TxOutput)> {
    outs.iter()
        .enumerate()
        .filter_map(move |(index, output)| create_utxo_data(srcid.clone(), index, output, rng))
        .collect()
}

pub fn create_utxo_data(
    outsrc: OutPointSourceId,
    index: usize,
    output: &TxOutput,
    rng: &mut impl Rng,
) -> Option<(InputWitness, TxInput, TxOutput)> {
    match output {
        TxOutput::Transfer(v, _) | TxOutput::LockThenTransfer(v, _, _) => {
            let new_output = match v {
                OutputValue::Coin(output_value) => {
                    let spent_value =
                        Amount::from_atoms(rng.gen_range(0..output_value.into_atoms()));
                    let new_value = (*output_value - spent_value).unwrap();
                    utils::ensure!(new_value >= Amount::from_atoms(1));
                    TxOutput::Transfer(OutputValue::Coin(new_value), anyonecanspend_address())
                }
                OutputValue::TokenV0(_) => return None, // ignore
                OutputValue::TokenV1(token_id, output_value) => {
                    let spent_value =
                        Amount::from_atoms(rng.gen_range(0..output_value.into_atoms()));
                    let new_value = (*output_value - spent_value).unwrap();
                    utils::ensure!(new_value >= Amount::from_atoms(1));
                    TxOutput::Transfer(
                        OutputValue::TokenV1(*token_id, new_value),
                        anyonecanspend_address(),
                    )
                }
            };

            Some((
                empty_witness(rng),
                TxInput::from_utxo(outsrc, index as u32),
                new_output,
            ))
        }
        TxOutput::Burn(_)
        | TxOutput::CreateStakePool(_, _)
        | TxOutput::ProduceBlockFromStake(_, _)
        | TxOutput::CreateDelegationId(_, _)
        | TxOutput::DelegateStaking(_, _)
        | TxOutput::IssueFungibleToken(_)
        | TxOutput::IssueNft(_, _, _)
        | TxOutput::DataDeposit(_) => None,
    }
}

pub fn outputs_from_genesis(genesis: &Genesis) -> BlockOutputs {
    [(
        OutPointSourceId::BlockReward(genesis.get_id().into()),
        genesis.utxos().to_vec(),
    )]
    .into_iter()
    .collect()
}

pub fn outputs_from_block(blk: &Block) -> BlockOutputs {
    std::iter::once((
        OutPointSourceId::BlockReward(blk.get_id().into()),
        blk.block_reward().outputs().to_vec(),
    ))
    .chain(blk.transactions().iter().map(|tx| {
        (
            OutPointSourceId::Transaction(tx.transaction().get_id()),
            tx.transaction().outputs().to_owned(),
        )
    }))
    .collect()
}

pub fn create_chain_config_with_default_staking_pool(
    rng: &mut impl Rng,
    staking_pk: PublicKey,
    vrf_pk: VRFPublicKey,
) -> (ConfigBuilder, PoolId) {
    let stake_amount = create_unit_test_config().min_stake_pool_pledge();
    let mint_amount = Amount::from_atoms(100_000_000 * common::chain::CoinUnit::ATOMS_PER_COIN);

    let genesis_pool_id = PoolId::new(H256::random_using(rng));
    let genesis_stake_pool_data = StakePoolData::new(
        stake_amount,
        Destination::PublicKey(staking_pk.clone()),
        vrf_pk,
        Destination::PublicKey(staking_pk),
        PerThousand::new(1000).unwrap(),
        Amount::ZERO,
    );

    let chain_config = create_chain_config_with_staking_pool(
        mint_amount,
        genesis_pool_id,
        genesis_stake_pool_data,
    );

    (chain_config, genesis_pool_id)
}

pub fn create_chain_config_with_staking_pool(
    mint_amount: Amount,
    pool_id: PoolId,
    pool_data: StakePoolData,
) -> ConfigBuilder {
    let upgrades = vec![
        (BlockHeight::new(0), ConsensusUpgrade::IgnoreConsensus),
        (
            BlockHeight::new(1),
            ConsensusUpgrade::PoS {
                initial_difficulty: Some(Uint256::MAX.into()),
                config: PoSChainConfigBuilder::new_for_unit_test().build(),
            },
        ),
    ];

    let mint_output =
        TxOutput::Transfer(OutputValue::Coin(mint_amount), Destination::AnyoneCanSpend);

    let pool = TxOutput::CreateStakePool(pool_id, Box::new(pool_data));

    let genesis_time = common::time_getter::TimeGetter::default().get_time();
    let genesis = Genesis::new(
        String::new(),
        BlockTimestamp::from_time(genesis_time),
        vec![mint_output, pool],
    );

    let net_upgrades = NetUpgrades::initialize(upgrades).unwrap();
    ConfigBuilder::new(ChainType::Regtest)
        .consensus_upgrades(net_upgrades)
        .genesis_custom(genesis)
}

pub fn produce_kernel_signature(
    tf: &TestFramework,
    staking_sk: &PrivateKey,
    reward_outputs: &[TxOutput],
    staking_destination: Destination,
    kernel_utxo_block_id: Id<GenBlock>,
    kernel_outpoint: UtxoOutPoint,
) -> StandardInputSignature {
    let block_outputs = tf.outputs_from_genblock(kernel_utxo_block_id);
    let utxo = &block_outputs.get(&kernel_outpoint.source_id()).unwrap()
        [kernel_outpoint.output_index() as usize];

    let kernel_inputs = vec![kernel_outpoint.into()];

    let block_reward_tx =
        BlockRewardTransactable::new(Some(kernel_inputs.as_slice()), Some(reward_outputs), None);
    StandardInputSignature::produce_uniparty_signature_for_input(
        staking_sk,
        SigHashType::default(),
        staking_destination,
        &block_reward_tx,
        std::iter::once(Some(utxo)).collect::<Vec<_>>().as_slice(),
        0,
    )
    .unwrap()
}

// TODO: consider replacing this function with consensus::pos::stake
#[allow(clippy::too_many_arguments)]
pub fn pos_mine(
    storage: &impl BlockchainStorageRead,
    pos_config: &PoSChainConfig,
    initial_timestamp: BlockTimestamp,
    kernel_outpoint: UtxoOutPoint,
    kernel_witness: InputWitness,
    vrf_sk: &VRFPrivateKey,
    sealed_epoch_randomness: PoSRandomness,
    pool_id: PoolId,
    final_supply: CoinUnit,
    epoch_index: EpochIndex,
    target: Compact,
) -> Option<(PoSData, BlockTimestamp)> {
    let pos_db = PoSAccountingDB::<_, TipStorageTag>::new(&storage);

    let pool_balance = pos_db.get_pool_balance(pool_id).unwrap().unwrap();
    let pledge_amount = pos_db.get_pool_data(pool_id).unwrap().unwrap().owner_balance().unwrap();

    let mut timestamp = initial_timestamp;
    for _ in 0..1000 {
        let transcript = chainstate_types::vrf_tools::construct_transcript(
            epoch_index,
            &sealed_epoch_randomness.value(),
            timestamp,
        );
        let vrf_data = vrf_sk.produce_vrf_data(transcript.into());

        let pos_data = PoSData::new(
            vec![kernel_outpoint.clone().into()],
            vec![kernel_witness.clone()],
            pool_id,
            vrf_data,
            target,
        );

        let vrf_pk = VRFPublicKey::from_private_key(vrf_sk);
        if consensus::check_pos_hash(
            pos_config.consensus_version(),
            epoch_index,
            &sealed_epoch_randomness,
            &pos_data,
            &vrf_pk,
            timestamp,
            pledge_amount,
            pool_balance,
            final_supply.to_amount_atoms(),
        )
        .is_ok()
        {
            return Some((pos_data, timestamp));
        }

        timestamp = timestamp.add_int_seconds(1).unwrap();
    }
    None
}
