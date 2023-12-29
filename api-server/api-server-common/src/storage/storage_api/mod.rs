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

use std::collections::{BTreeMap, BTreeSet};

use common::{
    chain::{
        tokens::{
            IsTokenFreezable, IsTokenFrozen, IsTokenUnfreezable, NftIssuance, TokenId,
            TokenTotalSupply,
        },
        AccountNonce, Block, ChainConfig, DelegationId, Destination, GenBlock, PoolId,
        SignedTransaction, Transaction, TxOutput, UtxoOutPoint,
    },
    primitives::{Amount, BlockHeight, CoinOrTokenId, Id},
};
use pos_accounting::PoolData;
use serialization::{Decode, Encode};

use self::block_aux_data::BlockAuxData;

pub mod block_aux_data;

#[allow(dead_code)]
#[derive(Debug, thiserror::Error)]
pub enum ApiServerStorageError {
    #[error("Low level storage error: {0}")]
    LowLevelStorageError(String),
    #[error("Deserialization error: {0}")]
    DeserializationError(String),
    #[error("Storage initialization failed: {0}")]
    InitializationError(String),
    #[error("Invalid initialized state")]
    InvalidInitializedState(String),
    #[error("Acquiring connection from the pool/transaction failed with error: {0}")]
    AcquiringConnectionFailed(String),
    #[error("Read-only tx begin failed: {0}")]
    RoTxBeginFailed(String),
    #[error("Read/write tx begin failed: {0}")]
    RwTxBeginFailed(String),
    #[error("Transaction commit failed: {0}")]
    TxCommitFailed(String),
    #[error("Transaction rw rollback failed: {0}")]
    TxRwRollbackFailed(String),
    #[error("Invalid block received: {0}")]
    InvalidBlock(String),
}

#[derive(Debug, Eq, PartialEq, Clone, Encode, Decode)]
pub struct Delegation {
    spend_destination: Destination,
    pool_id: PoolId,
    balance: Amount,
    next_nonce: AccountNonce,
}

impl Delegation {
    pub fn new(
        spend_destination: Destination,
        pool_id: PoolId,
        balance: Amount,
        next_nonce: AccountNonce,
    ) -> Self {
        Self {
            spend_destination,
            pool_id,
            balance,
            next_nonce,
        }
    }

    pub fn spend_destination(&self) -> &Destination {
        &self.spend_destination
    }

    pub fn pool_id(&self) -> PoolId {
        self.pool_id
    }

    pub fn balance(&self) -> &Amount {
        &self.balance
    }

    pub fn next_nonce(&self) -> &AccountNonce {
        &self.next_nonce
    }

    pub fn stake(&self, rewards: Amount) -> Self {
        Self {
            spend_destination: self.spend_destination.clone(),
            pool_id: self.pool_id,
            balance: (self.balance + rewards).expect("no overflow"),
            next_nonce: self.next_nonce,
        }
    }

    pub fn spend_share(&self, amount: Amount, nonce: AccountNonce) -> Self {
        Self {
            spend_destination: self.spend_destination.clone(),
            pool_id: self.pool_id,
            balance: (self.balance - amount).expect("not underflow"),
            next_nonce: nonce.increment().expect("no overflow"),
        }
    }
}

#[derive(Debug, Clone, Encode, Decode)]
pub struct Utxo {
    output: TxOutput,
    spent: bool,
}

impl Utxo {
    pub fn new(output: TxOutput, spent: bool) -> Self {
        Self { output, spent }
    }

    pub fn output(&self) -> &TxOutput {
        &self.output
    }

    pub fn into_output(self) -> TxOutput {
        self.output
    }

    pub fn spent(&self) -> bool {
        self.spent
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct FungibleTokenData {
    pub token_ticker: Vec<u8>,
    pub number_of_decimals: u8,
    pub metadata_uri: Vec<u8>,
    pub circulating_supply: Amount,
    pub total_supply: TokenTotalSupply,
    pub is_locked: bool,
    pub frozen: IsTokenFrozen,
    pub authority: Destination,
}

impl FungibleTokenData {
    pub fn mint_tokens(mut self, amount: Amount) -> Self {
        self.circulating_supply = (self.circulating_supply + amount).expect("no overflow");
        self
    }

    pub fn unmint_tokens(mut self, amount: Amount) -> Self {
        self.circulating_supply = (self.circulating_supply - amount).expect("no underflow");
        self
    }

    pub fn freeze(mut self, is_token_unfreezable: IsTokenUnfreezable) -> Self {
        self.frozen = IsTokenFrozen::Yes(is_token_unfreezable);
        self
    }

    pub fn unfreeze(mut self) -> Self {
        self.frozen = IsTokenFrozen::No(IsTokenFreezable::Yes);
        self
    }

    pub fn lock(mut self) -> Self {
        self.is_locked = true;
        self
    }

    pub fn change_authority(mut self, authority: Destination) -> Self {
        self.authority = authority;
        self
    }
}

#[async_trait::async_trait]
pub trait ApiServerStorageRead: Sync {
    async fn is_initialized(&self) -> Result<bool, ApiServerStorageError>;

    async fn get_storage_version(&self) -> Result<Option<u32>, ApiServerStorageError>;

    async fn get_address_balance(
        &self,
        address: &str,
        coin_or_token_id: CoinOrTokenId,
    ) -> Result<Option<Amount>, ApiServerStorageError>;

    async fn get_address_transactions(
        &self,
        address: &str,
    ) -> Result<Vec<Id<Transaction>>, ApiServerStorageError>;

    async fn get_best_block(&self) -> Result<(BlockHeight, Id<GenBlock>), ApiServerStorageError>;

    async fn get_block(&self, block_id: Id<Block>) -> Result<Option<Block>, ApiServerStorageError>;

    async fn get_block_aux_data(
        &self,
        block_id: Id<Block>,
    ) -> Result<Option<BlockAuxData>, ApiServerStorageError>;

    async fn get_delegation(
        &self,
        delegation_id: DelegationId,
    ) -> Result<Option<Delegation>, ApiServerStorageError>;

    async fn get_pool_delegations(
        &self,
        pool_id: PoolId,
    ) -> Result<BTreeMap<DelegationId, Delegation>, ApiServerStorageError>;

    async fn get_main_chain_block_id(
        &self,
        block_height: BlockHeight,
    ) -> Result<Option<Id<Block>>, ApiServerStorageError>;

    async fn get_pool_data(
        &self,
        pool_id: PoolId,
    ) -> Result<Option<PoolData>, ApiServerStorageError>;

    async fn get_latest_pool_data(
        &self,
        len: u32,
        offset: u32,
    ) -> Result<Vec<(PoolId, PoolData)>, ApiServerStorageError>;

    async fn get_pool_data_with_largest_pledge(
        &self,
        len: u32,
        offset: u32,
    ) -> Result<Vec<(PoolId, PoolData)>, ApiServerStorageError>;

    #[allow(clippy::type_complexity)]
    async fn get_transaction_with_block(
        &self,
        transaction_id: Id<Transaction>,
    ) -> Result<Option<(Option<BlockAuxData>, SignedTransaction)>, ApiServerStorageError>;

    #[allow(clippy::type_complexity)]
    async fn get_transaction(
        &self,
        transaction_id: Id<Transaction>,
    ) -> Result<Option<(Option<Id<Block>>, SignedTransaction)>, ApiServerStorageError>;

    async fn get_utxo(&self, outpoint: UtxoOutPoint)
        -> Result<Option<Utxo>, ApiServerStorageError>;

    async fn get_address_available_utxos(
        &self,
        address: &str,
    ) -> Result<Vec<(UtxoOutPoint, TxOutput)>, ApiServerStorageError>;

    async fn get_delegations_from_address(
        &self,
        address: &Destination,
    ) -> Result<Vec<(DelegationId, Delegation)>, ApiServerStorageError>;

    async fn get_fungible_token_issuance(
        &self,
        token_id: TokenId,
    ) -> Result<Option<FungibleTokenData>, ApiServerStorageError>;

    async fn get_nft_token_issuance(
        &self,
        token_id: TokenId,
    ) -> Result<Option<NftIssuance>, ApiServerStorageError>;
}

#[async_trait::async_trait]
pub trait ApiServerStorageWrite: ApiServerStorageRead {
    async fn initialize_storage(
        &mut self,
        chain_config: &ChainConfig,
    ) -> Result<(), ApiServerStorageError>;

    async fn del_address_balance_above_height(
        &mut self,
        block_height: BlockHeight,
    ) -> Result<(), ApiServerStorageError>;

    async fn del_address_transactions_above_height(
        &mut self,
        block_height: BlockHeight,
    ) -> Result<(), ApiServerStorageError>;

    async fn set_address_balance_at_height(
        &mut self,
        address: &str,
        amount: Amount,
        coin_or_token_id: CoinOrTokenId,
        block_height: BlockHeight,
    ) -> Result<(), ApiServerStorageError>;

    async fn set_address_transactions_at_height(
        &mut self,
        address: &str,
        transaction_ids: BTreeSet<Id<Transaction>>,
        block_height: BlockHeight,
    ) -> Result<(), ApiServerStorageError>;

    async fn set_mainchain_block(
        &mut self,
        block_id: Id<Block>,
        block_height: BlockHeight,
        block: &Block,
    ) -> Result<(), ApiServerStorageError>;

    async fn set_delegation_at_height(
        &mut self,
        delegation_id: DelegationId,
        delegation: &Delegation,
        block_height: BlockHeight,
    ) -> Result<(), ApiServerStorageError>;

    async fn set_transaction(
        &mut self,
        transaction_id: Id<Transaction>,
        owning_block: Option<Id<Block>>,
        transaction: &SignedTransaction,
    ) -> Result<(), ApiServerStorageError>;

    async fn set_block_aux_data(
        &mut self,
        block_id: Id<Block>,
        block_aux_data: &BlockAuxData,
    ) -> Result<(), ApiServerStorageError>;

    async fn del_main_chain_blocks_above_height(
        &mut self,
        block_height: BlockHeight,
    ) -> Result<(), ApiServerStorageError>;

    async fn set_pool_data_at_height(
        &mut self,
        pool_id: PoolId,
        pool_data: &PoolData,
        block_height: BlockHeight,
    ) -> Result<(), ApiServerStorageError>;

    async fn del_delegations_above_height(
        &mut self,
        block_height: BlockHeight,
    ) -> Result<(), ApiServerStorageError>;

    async fn del_pools_above_height(
        &mut self,
        block_height: BlockHeight,
    ) -> Result<(), ApiServerStorageError>;

    async fn set_utxo_at_height(
        &mut self,
        outpoint: UtxoOutPoint,
        utxo: Utxo,
        address: &str,
        block_height: BlockHeight,
    ) -> Result<(), ApiServerStorageError>;

    async fn del_utxo_above_height(
        &mut self,
        block_height: BlockHeight,
    ) -> Result<(), ApiServerStorageError>;

    async fn set_fungible_token_issuance(
        &mut self,
        token_id: TokenId,
        block_height: BlockHeight,
        issuance: FungibleTokenData,
    ) -> Result<(), ApiServerStorageError>;

    async fn set_nft_token_issuance(
        &mut self,
        token_id: TokenId,
        block_height: BlockHeight,
        issuance: NftIssuance,
    ) -> Result<(), ApiServerStorageError>;

    async fn del_token_issuance_above_height(
        &mut self,
        block_height: BlockHeight,
    ) -> Result<(), ApiServerStorageError>;

    async fn del_nft_issuance_above_height(
        &mut self,
        block_height: BlockHeight,
    ) -> Result<(), ApiServerStorageError>;
}

#[async_trait::async_trait]
pub trait ApiServerTransactionRw: ApiServerStorageWrite + ApiServerStorageRead {
    async fn commit(self) -> Result<(), ApiServerStorageError>;
    async fn rollback(self) -> Result<(), ApiServerStorageError>;
}

#[async_trait::async_trait]
pub trait ApiServerTransactionRo: ApiServerStorageRead {
    async fn close(self) -> Result<(), ApiServerStorageError>;
}

#[async_trait::async_trait]
pub trait Transactional<'tx> {
    /// Associated read-only transaction type.
    type TransactionRo: ApiServerTransactionRo + Send + 'tx;

    /// Associated read-write transaction type.
    type TransactionRw: ApiServerTransactionRw + Send + 'tx;

    /// Start a read-only transaction.
    async fn transaction_ro<'db: 'tx>(
        &'db self,
    ) -> Result<Self::TransactionRo, ApiServerStorageError>;

    /// Start a read-write transaction.
    async fn transaction_rw<'db: 'tx>(
        &'db mut self,
    ) -> Result<Self::TransactionRw, ApiServerStorageError>;
}

pub trait ApiServerStorage: for<'tx> Transactional<'tx> + Send + Sync {}
