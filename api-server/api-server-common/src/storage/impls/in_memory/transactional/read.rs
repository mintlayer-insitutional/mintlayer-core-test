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

use std::collections::BTreeMap;

use common::{
    chain::{
        tokens::{NftIssuance, TokenId},
        Block, DelegationId, Destination, GenBlock, PoolId, SignedTransaction, Transaction,
        TxOutput, UtxoOutPoint,
    },
    primitives::{Amount, BlockHeight, CoinOrTokenId, Id},
};
use pos_accounting::PoolData;

use crate::storage::storage_api::{
    block_aux_data::BlockAuxData, ApiServerStorageError, ApiServerStorageRead, Delegation,
    FungibleTokenData, Utxo,
};

use super::ApiServerInMemoryStorageTransactionalRo;

#[async_trait::async_trait]
impl<'t> ApiServerStorageRead for ApiServerInMemoryStorageTransactionalRo<'t> {
    async fn is_initialized(&self) -> Result<bool, ApiServerStorageError> {
        self.transaction.is_initialized()
    }

    async fn get_address_balance(
        &self,
        address: &str,
        coin_or_token_id: CoinOrTokenId,
    ) -> Result<Option<Amount>, ApiServerStorageError> {
        self.transaction.get_address_balance(address, coin_or_token_id)
    }

    async fn get_address_transactions(
        &self,
        address: &str,
    ) -> Result<Vec<Id<Transaction>>, ApiServerStorageError> {
        self.transaction.get_address_transactions(address)
    }

    async fn get_block(&self, block_id: Id<Block>) -> Result<Option<Block>, ApiServerStorageError> {
        self.transaction.get_block(block_id)
    }

    async fn get_transaction_with_block(
        &self,
        transaction_id: Id<Transaction>,
    ) -> Result<Option<(Option<BlockAuxData>, SignedTransaction)>, ApiServerStorageError> {
        self.transaction.get_transaction_with_block(transaction_id)
    }

    async fn get_delegation(
        &self,
        delegation_id: DelegationId,
    ) -> Result<Option<Delegation>, ApiServerStorageError> {
        self.transaction.get_delegation(delegation_id)
    }

    async fn get_pool_delegations(
        &self,
        pool_id: PoolId,
    ) -> Result<BTreeMap<DelegationId, Delegation>, ApiServerStorageError> {
        self.transaction.get_pool_delegations(pool_id)
    }

    async fn get_latest_pool_data(
        &self,
        len: u32,
        offset: u32,
    ) -> Result<Vec<(PoolId, PoolData)>, ApiServerStorageError> {
        self.transaction.get_latest_pool_ids(len, offset)
    }

    async fn get_pool_data_with_largest_pledge(
        &self,
        len: u32,
        offset: u32,
    ) -> Result<Vec<(PoolId, PoolData)>, ApiServerStorageError> {
        self.transaction.get_pool_data_with_largest_pledge(len, offset)
    }

    async fn get_transaction(
        &self,
        transaction_id: Id<Transaction>,
    ) -> Result<Option<(Option<Id<Block>>, SignedTransaction)>, ApiServerStorageError> {
        self.transaction.get_transaction(transaction_id)
    }

    async fn get_storage_version(&self) -> Result<Option<u32>, ApiServerStorageError> {
        Ok(Some(self.transaction.get_storage_version()?))
    }

    async fn get_best_block(&self) -> Result<(BlockHeight, Id<GenBlock>), ApiServerStorageError> {
        self.transaction.get_best_block()
    }

    async fn get_block_aux_data(
        &self,
        block_id: Id<Block>,
    ) -> Result<Option<BlockAuxData>, ApiServerStorageError> {
        self.transaction.get_block_aux_data(block_id)
    }

    async fn get_main_chain_block_id(
        &self,
        block_height: BlockHeight,
    ) -> Result<Option<Id<Block>>, ApiServerStorageError> {
        self.transaction.get_main_chain_block_id(block_height)
    }

    async fn get_pool_data(
        &self,
        pool_id: PoolId,
    ) -> Result<Option<PoolData>, ApiServerStorageError> {
        self.transaction.get_pool_data(pool_id)
    }

    async fn get_utxo(
        &self,
        outpoint: UtxoOutPoint,
    ) -> Result<Option<Utxo>, ApiServerStorageError> {
        self.transaction.get_utxo(outpoint)
    }

    async fn get_address_available_utxos(
        &self,
        address: &str,
    ) -> Result<Vec<(UtxoOutPoint, TxOutput)>, ApiServerStorageError> {
        self.transaction.get_address_available_utxos(address)
    }

    async fn get_delegations_from_address(
        &self,
        address: &Destination,
    ) -> Result<Vec<(DelegationId, Delegation)>, ApiServerStorageError> {
        self.transaction.get_delegations_from_address(address)
    }

    async fn get_fungible_token_issuance(
        &self,
        token_id: TokenId,
    ) -> Result<Option<FungibleTokenData>, ApiServerStorageError> {
        self.transaction.get_fungible_token_issuance(token_id)
    }

    async fn get_nft_token_issuance(
        &self,
        token_id: TokenId,
    ) -> Result<Option<NftIssuance>, ApiServerStorageError> {
        self.transaction.get_nft_token_issuance(token_id)
    }
}
