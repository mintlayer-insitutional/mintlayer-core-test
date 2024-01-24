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

//! Common code for wallet UI applications

pub mod mnemonic;
pub mod read;
mod sync;
pub mod synced_controller;
pub mod types;

const NORMAL_DELAY: Duration = Duration::from_secs(1);
const ERROR_DELAY: Duration = Duration::from_secs(10);

use futures::never::Never;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use read::ReadOnlyController;
use sync::InSync;
use synced_controller::SyncedController;

use common::{
    address::AddressError,
    chain::{
        tokens::{RPCTokenInfo, TokenId},
        Block, ChainConfig, GenBlock, PoolId, SignedTransaction, Transaction, TxOutput,
    },
    primitives::{
        time::{get_time, Time},
        Amount, BlockHeight, Id, Idable,
    },
};
use consensus::GenerateBlockInputData;
use crypto::{
    ephemeral_e2e::EndToEndPrivateKey,
    key::hdkd::u31::U31,
    random::{make_pseudo_rng, make_true_rng, Rng},
};
use logging::log;
use mempool::tx_accumulator::PackingStrategy;
pub use node_comm::node_traits::{ConnectedPeer, NodeInterface, PeerId};
pub use node_comm::{
    handles_client::WalletHandlesClient, make_cold_wallet_rpc_client, make_rpc_client,
    rpc_client::NodeRpcClient,
};
use wallet::{
    wallet::WalletPoolsFilter, wallet_events::WalletEvents, DefaultWallet, WalletError,
    WalletResult,
};
pub use wallet_types::{
    account_info::DEFAULT_ACCOUNT_INDEX,
    utxo_types::{UtxoState, UtxoStates, UtxoType, UtxoTypes},
};
use wallet_types::{seed_phrase::StoreSeedPhrase, with_locked::WithLocked};

#[derive(thiserror::Error, Debug)]
pub enum ControllerError<T: NodeInterface> {
    #[error("Node call error: {0}")]
    NodeCallError(T::Error),
    #[error("Wallet sync error: {0}")]
    SyncError(String),
    #[error("Synchronization is paused until the node has {0} blocks ({1} blocks currently)")]
    NotEnoughBlockHeight(BlockHeight, BlockHeight),
    #[error("Wallet file {0} error: {1}")]
    WalletFileError(PathBuf, String),
    #[error("Wallet error: {0}")]
    WalletError(wallet::wallet::WalletError),
    #[error("Encoding error: {0}")]
    AddressEncodingError(#[from] AddressError),
    #[error("No staking pool found")]
    NoStakingPool,
    #[error("Token with Id {0} is frozen")]
    FrozenToken(TokenId),
    #[error("Wallet is locked")]
    WalletIsLocked,
    #[error("Cannot lock wallet because staking is running")]
    StakingRunning,
    #[error("End-to-end encryption error: {0}")]
    EndToEndEncryptionError(#[from] crypto::ephemeral_e2e::error::Error),
    #[error("The node is not in sync yet")]
    NodeNotInSyncYet,
    #[error("Lookahead size cannot be 0")]
    InvalidLookaheadSize,
}

#[derive(Clone, Copy)]
pub struct ControllerConfig {
    /// In which top N MB should we aim for our transactions to be in the mempool
    /// e.g. for 5, we aim to be in the top 5 MB of transactions based on paid fees
    /// This is to avoid getting trimmed off the lower end if the mempool runs out of memory
    pub in_top_x_mb: usize,
}

pub struct Controller<T, W> {
    chain_config: Arc<ChainConfig>,

    rpc_client: T,

    wallet: DefaultWallet,

    staking_started: BTreeSet<U31>,

    wallet_events: W,
}

impl<T, WalletEvents> std::fmt::Debug for Controller<T, WalletEvents> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Controller").finish()
    }
}

pub type RpcController<N, WalletEvents> = Controller<N, WalletEvents>;
pub type HandlesController<WalletEvents> = Controller<WalletHandlesClient, WalletEvents>;

impl<T: NodeInterface + Clone + Send + Sync + 'static, W: WalletEvents> Controller<T, W> {
    pub async fn new(
        chain_config: Arc<ChainConfig>,
        rpc_client: T,
        wallet: DefaultWallet,
        wallet_events: W,
    ) -> Result<Self, ControllerError<T>> {
        let mut controller = Self {
            chain_config,
            rpc_client,
            wallet,
            staking_started: BTreeSet::new(),
            wallet_events,
        };

        log::info!("Syncing the wallet...");
        controller.try_sync_once().await?;

        Ok(controller)
    }

    pub fn new_unsynced(
        chain_config: Arc<ChainConfig>,
        rpc_client: T,
        wallet: DefaultWallet,
        wallet_events: W,
    ) -> Self {
        Self {
            chain_config,
            rpc_client,
            wallet,
            staking_started: BTreeSet::new(),
            wallet_events,
        }
    }

    pub fn create_wallet(
        chain_config: Arc<ChainConfig>,
        file_path: impl AsRef<Path>,
        mnemonic: mnemonic::Mnemonic,
        passphrase: Option<&str>,
        whether_to_store_seed_phrase: StoreSeedPhrase,
        best_block_height: BlockHeight,
        best_block_id: Id<GenBlock>,
    ) -> Result<DefaultWallet, ControllerError<T>> {
        utils::ensure!(
            !file_path.as_ref().exists(),
            ControllerError::WalletFileError(
                file_path.as_ref().to_owned(),
                "File already exists".to_owned()
            )
        );

        let db = wallet::wallet::open_or_create_wallet_file(file_path)
            .map_err(ControllerError::WalletError)?;
        let wallet = wallet::Wallet::create_new_wallet(
            Arc::clone(&chain_config),
            db,
            &mnemonic.to_string(),
            passphrase,
            whether_to_store_seed_phrase,
            best_block_height,
            best_block_id,
        )
        .map_err(ControllerError::WalletError)?;

        Ok(wallet)
    }

    pub fn recover_wallet(
        chain_config: Arc<ChainConfig>,
        file_path: impl AsRef<Path>,
        mnemonic: mnemonic::Mnemonic,
        passphrase: Option<&str>,
        whether_to_store_seed_phrase: StoreSeedPhrase,
    ) -> Result<DefaultWallet, ControllerError<T>> {
        utils::ensure!(
            !file_path.as_ref().exists(),
            ControllerError::WalletFileError(
                file_path.as_ref().to_owned(),
                "File already exists".to_owned()
            )
        );

        let db = wallet::wallet::open_or_create_wallet_file(file_path)
            .map_err(ControllerError::WalletError)?;
        let wallet = wallet::Wallet::recover_wallet(
            Arc::clone(&chain_config),
            db,
            &mnemonic.to_string(),
            passphrase,
            whether_to_store_seed_phrase,
        )
        .map_err(ControllerError::WalletError)?;

        Ok(wallet)
    }

    fn make_backup_wallet_file(file_path: impl AsRef<Path>, version: u32) -> WalletResult<()> {
        let backup_name = file_path
            .as_ref()
            .file_name()
            .map(|file_name| {
                let mut file_name = file_name.to_os_string();
                file_name.push(format!("_backup_v{version}"));
                file_name
            })
            .ok_or(WalletError::WalletFileError(
                file_path.as_ref().to_owned(),
                "File path is not a file".to_owned(),
            ))?;
        let backup_file_path = file_path.as_ref().with_file_name(backup_name);
        logging::log::info!(
            "The wallet DB requires a migration, creating a backup file: {}",
            backup_file_path.to_string_lossy()
        );
        fs::copy(&file_path, backup_file_path).map_err(|_| {
            WalletError::WalletFileError(
                file_path.as_ref().to_owned(),
                "Could not make a backup of the file before migrating it".to_owned(),
            )
        })?;
        Ok(())
    }

    pub fn open_wallet(
        chain_config: Arc<ChainConfig>,
        file_path: impl AsRef<Path>,
        password: Option<String>,
    ) -> Result<DefaultWallet, ControllerError<T>> {
        utils::ensure!(
            file_path.as_ref().exists(),
            ControllerError::WalletFileError(
                file_path.as_ref().to_owned(),
                "File does not exist".to_owned()
            )
        );

        let db = wallet::wallet::open_or_create_wallet_file(&file_path)
            .map_err(ControllerError::WalletError)?;

        let wallet =
            wallet::Wallet::load_wallet(Arc::clone(&chain_config), db, password, |version| {
                Self::make_backup_wallet_file(file_path.as_ref(), version)
            })
            .map_err(ControllerError::WalletError)?;

        Ok(wallet)
    }

    fn serializable_seed_phrase_to_vec(
        serializable_seed_phrase: wallet_types::seed_phrase::SerializableSeedPhrase,
    ) -> Vec<String> {
        match serializable_seed_phrase {
            wallet_types::seed_phrase::SerializableSeedPhrase::V0(_, words) => {
                words.mnemonic().to_vec()
            }
        }
    }

    pub fn seed_phrase(&self) -> Result<Option<Vec<String>>, ControllerError<T>> {
        self.wallet
            .seed_phrase()
            .map(|opt| opt.map(Self::serializable_seed_phrase_to_vec))
            .map_err(ControllerError::WalletError)
    }

    /// Delete the seed phrase if stored in the database
    pub fn delete_seed_phrase(&self) -> Result<Option<Vec<String>>, ControllerError<T>> {
        self.wallet
            .delete_seed_phrase()
            .map(|opt| opt.map(Self::serializable_seed_phrase_to_vec))
            .map_err(ControllerError::WalletError)
    }

    /// Rescan the blockchain
    /// Resets the wallet to the genesis block
    pub fn reset_wallet_to_genesis(&mut self) -> Result<(), ControllerError<T>> {
        self.wallet.reset_wallet_to_genesis().map_err(ControllerError::WalletError)
    }

    /// Encrypts the wallet using the specified `password`, or removes the existing encryption if `password` is `None`.
    ///
    /// # Arguments
    ///
    /// * `password` - An optional `String` representing the new password for encrypting the wallet.
    ///
    /// # Returns
    ///
    /// This method returns an error if the wallet is locked
    pub fn encrypt_wallet(&mut self, password: &Option<String>) -> Result<(), ControllerError<T>> {
        self.wallet.encrypt_wallet(password).map_err(ControllerError::WalletError)
    }

    /// Unlocks the wallet using the specified password.
    ///
    /// # Arguments
    ///
    /// * `password` - A `String` representing the password that was used to encrypt the wallet.
    ///
    /// # Returns
    ///
    /// This method returns an error if the password is incorrect
    pub fn unlock_wallet(&mut self, password: &String) -> Result<(), ControllerError<T>> {
        self.wallet.unlock_wallet(password).map_err(ControllerError::WalletError)
    }

    /// Locks the wallet by making the encrypted private keys inaccessible.
    ///
    /// # Returns
    ///
    /// This method returns an error if the wallet is not encrypted.
    pub fn lock_wallet(&mut self) -> Result<(), ControllerError<T>> {
        utils::ensure!(
            self.staking_started.is_empty(),
            ControllerError::StakingRunning
        );
        self.wallet.lock_wallet().map_err(ControllerError::WalletError)
    }

    /// Sets the lookahead size for key generation
    ///
    /// # Returns
    ///
    /// This method returns an error if you try to set lookahead size to 0
    pub fn set_lookahead_size(
        &mut self,
        lookahead_size: u32,
        force_reduce: bool,
    ) -> Result<(), ControllerError<T>> {
        utils::ensure!(lookahead_size > 0, ControllerError::InvalidLookaheadSize);

        self.wallet
            .set_lookahead_size(lookahead_size, force_reduce)
            .map_err(ControllerError::WalletError)
    }

    pub fn account_names(&self) -> impl Iterator<Item = &Option<String>> {
        self.wallet.account_names()
    }

    pub async fn get_token_number_of_decimals(
        &self,
        token_id: TokenId,
    ) -> Result<u8, ControllerError<T>> {
        Ok(self.get_token_info(token_id).await?.token_number_of_decimals())
    }

    pub async fn get_token_info(
        &self,
        token_id: TokenId,
    ) -> Result<RPCTokenInfo, ControllerError<T>> {
        fetch_token_info(&self.rpc_client, token_id).await
    }

    pub async fn generate_block_by_pool(
        &self,
        account_index: U31,
        pool_id: PoolId,
        transactions: Vec<SignedTransaction>,
        transaction_ids: Vec<Id<Transaction>>,
        packing_strategy: PackingStrategy,
    ) -> Result<Block, ControllerError<T>> {
        let pos_data = self
            .wallet
            .get_pos_gen_block_data(account_index, pool_id)
            .map_err(ControllerError::WalletError)?;

        let public_key = self
            .rpc_client
            .generate_block_e2e_public_key()
            .await
            .map_err(ControllerError::NodeCallError)?;

        let input_data = GenerateBlockInputData::PoS(pos_data.into());

        let mut rng = make_true_rng();
        let ephemeral_private_key = EndToEndPrivateKey::new_from_rng(&mut rng);
        let ephemeral_public_key = ephemeral_private_key.public_key();
        let shared_secret = ephemeral_private_key.shared_secret(&public_key);
        let encrypted_input_data = shared_secret.encode_then_encrypt(&input_data, &mut rng)?;

        self.rpc_client
            .generate_block_e2e(
                encrypted_input_data,
                ephemeral_public_key,
                transactions,
                transaction_ids,
                packing_strategy,
            )
            .await
            .map_err(ControllerError::NodeCallError)
    }

    /// Attempt to generate a new block by trying all pools. If all pools fail,
    /// the last pool block generation error is returned (or `ControllerError::NoStakingPool` if there are no staking pools).
    pub async fn generate_block(
        &self,
        account_index: U31,
        transactions: Vec<SignedTransaction>,
        transaction_ids: Vec<Id<Transaction>>,
        packing_strategy: PackingStrategy,
    ) -> Result<Block, ControllerError<T>> {
        let pools = self
            .wallet
            .get_pool_ids(account_index, WalletPoolsFilter::Stake)
            .map_err(ControllerError::WalletError)?;

        let mut last_error = ControllerError::NoStakingPool;
        for (pool_id, _) in pools {
            let block_res = self
                .generate_block_by_pool(
                    account_index,
                    pool_id,
                    transactions.clone(),
                    transaction_ids.clone(),
                    packing_strategy,
                )
                .await;
            match block_res {
                Ok(block) => return Ok(block),
                Err(err) => last_error = err,
            }
        }
        Err(last_error)
    }

    /// Try to generate the `block_count` number of blocks.
    /// The function may return an error early if some attempt fails.
    pub async fn generate_blocks(
        &mut self,
        account_index: U31,
        block_count: u32,
    ) -> Result<(), ControllerError<T>> {
        for _ in 0..block_count {
            self.sync_once().await?;
            let block = self
                .generate_block(
                    account_index,
                    vec![],
                    vec![],
                    PackingStrategy::FillSpaceFromMempool,
                )
                .await?;

            self.rpc_client
                .submit_block(block)
                .await
                .map_err(ControllerError::NodeCallError)?;
        }

        self.sync_once().await
    }

    pub fn create_account(
        &mut self,
        name: Option<String>,
    ) -> Result<(U31, Option<String>), ControllerError<T>> {
        self.wallet.create_next_account(name).map_err(ControllerError::WalletError)
    }

    pub fn stop_staking(&mut self, account_index: U31) -> Result<(), ControllerError<T>> {
        log::info!("Stop staking, account_index: {}", account_index);
        self.staking_started.remove(&account_index);
        Ok(())
    }

    pub fn best_block(&self) -> (Id<GenBlock>, BlockHeight) {
        *self
            .wallet
            .get_best_block()
            .values()
            .min_by_key(|(_block_id, block_height)| block_height)
            .expect("there must be at least one account")
    }

    pub async fn get_stake_pool_balances(
        &self,
        account_index: U31,
    ) -> Result<BTreeMap<PoolId, Amount>, ControllerError<T>> {
        let stake_pool_utxos = self
            .wallet
            .get_utxos(
                account_index,
                UtxoType::CreateStakePool | UtxoType::ProduceBlockFromStake,
                UtxoState::Confirmed.into(),
                WithLocked::Unlocked,
            )
            .map_err(ControllerError::WalletError)?;
        let pool_ids = stake_pool_utxos.into_iter().filter_map(|(_, utxo)| match utxo {
            TxOutput::ProduceBlockFromStake(_, pool_id) | TxOutput::CreateStakePool(pool_id, _) => {
                Some(pool_id)
            }
            TxOutput::Transfer(_, _)
            | TxOutput::LockThenTransfer(_, _, _)
            | TxOutput::Burn(_)
            | TxOutput::CreateDelegationId(_, _)
            | TxOutput::DelegateStaking(_, _)
            | TxOutput::IssueFungibleToken(_)
            | TxOutput::IssueNft(_, _, _)
            | TxOutput::DataDeposit(_) => None,
        });
        let mut balances = BTreeMap::new();
        for pool_id in pool_ids {
            let balance_opt = self
                .rpc_client
                .get_stake_pool_balance(pool_id)
                .await
                .map_err(ControllerError::NodeCallError)?;
            if let Some(balance) = balance_opt {
                balances.insert(pool_id, balance);
            }
        }
        Ok(balances)
    }

    /// Synchronize the wallet to the current node tip height and return
    pub async fn sync_once(&mut self) -> Result<(), ControllerError<T>> {
        let res = sync::sync_once(
            &self.chain_config,
            &self.rpc_client,
            &mut self.wallet,
            &self.wallet_events,
        )
        .await?;

        match res {
            InSync::Synced => Ok(()),
            InSync::NodeOutOfSync => Err(ControllerError::NodeNotInSyncYet),
        }
    }

    pub async fn try_sync_once(&mut self) -> Result<(), ControllerError<T>> {
        sync::sync_once(
            &self.chain_config,
            &self.rpc_client,
            &mut self.wallet,
            &self.wallet_events,
        )
        .await?;

        Ok(())
    }

    pub async fn synced_controller(
        &mut self,
        account_index: U31,
        config: ControllerConfig,
    ) -> Result<SyncedController<T, W>, ControllerError<T>> {
        self.sync_once().await?;
        Ok(SyncedController::new(
            &mut self.wallet,
            self.rpc_client.clone(),
            self.chain_config.as_ref(),
            &self.wallet_events,
            &mut self.staking_started,
            account_index,
            config,
        ))
    }

    pub fn readonly_controller(&self, account_index: U31) -> ReadOnlyController<T> {
        ReadOnlyController::new(
            &self.wallet,
            self.rpc_client.clone(),
            self.chain_config.as_ref(),
            account_index,
        )
    }

    /// Synchronize the wallet in the background from the node's blockchain.
    /// Try staking new blocks if staking was started.
    pub async fn run(&mut self) -> Result<Never, ControllerError<T>> {
        let mut rebroadcast_txs_timer = get_time();
        let staking_started = self.staking_started.clone();

        'outer: loop {
            let sync_res = self.sync_once().await;

            if let Err(e) = sync_res {
                log::error!("Wallet sync error: {e}");
                tokio::time::sleep(ERROR_DELAY).await;
                continue;
            }

            for account_index in staking_started.iter() {
                let generate_res = self
                    .generate_block(
                        *account_index,
                        vec![],
                        vec![],
                        PackingStrategy::FillSpaceFromMempool,
                    )
                    .await;

                if let Ok(block) = generate_res {
                    log::info!(
                        "New block generated successfully, block id: {}",
                        block.get_id()
                    );

                    let submit_res = self.rpc_client.submit_block(block).await;
                    if let Err(e) = submit_res {
                        log::error!("Block submit failed: {e}");
                        tokio::time::sleep(ERROR_DELAY).await;
                    }

                    continue 'outer;
                }
            }

            tokio::time::sleep(NORMAL_DELAY).await;

            self.rebroadcast_txs(&mut rebroadcast_txs_timer).await;
        }
    }

    /// Rebroadcast not confirmed transactions
    async fn rebroadcast_txs(&mut self, rebroadcast_txs_again_at: &mut Time) {
        if get_time() >= *rebroadcast_txs_again_at {
            let txs = self.wallet.get_transactions_to_be_broadcast();
            match txs {
                Err(error) => {
                    log::error!("Fetching transactions for rebroadcasting failed: {error}");
                }
                Ok(txs) => {
                    for tx in txs {
                        let tx_id = tx.transaction().get_id();
                        let res = self.rpc_client.submit_transaction(tx, Default::default()).await;
                        if let Err(e) = res {
                            log::warn!("Rebroadcasting for tx {tx_id} failed: {e}");
                        }
                    }
                }
            }

            // Reset the timer with a new random interval between 2 and 5 minutes
            let sleep_interval_sec = make_pseudo_rng().gen_range(120..=300);
            *rebroadcast_txs_again_at = (get_time() + Duration::from_secs(sleep_interval_sec))
                .expect("Sleep intervals cannot be this large");
        }
    }
}

pub async fn fetch_token_info<T: NodeInterface>(
    rpc_client: &T,
    token_id: TokenId,
) -> Result<RPCTokenInfo, ControllerError<T>> {
    rpc_client
        .get_token_info(token_id)
        .await
        .map_err(ControllerError::NodeCallError)?
        .ok_or(ControllerError::WalletError(WalletError::UnknownTokenId(
            token_id,
        )))
}
