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

use common::{
    chain::{
        tokens::{NftIssuance, TokenId},
        Destination, GenBlock, TxOutput,
    },
    primitives::{Amount, BlockHeight, CoinOrTokenId, Id},
};

use crate::storage::{
    impls::postgres::queries::QueryFromConnection,
    storage_api::{
        block_aux_data::BlockAuxData, ApiServerStorageError, ApiServerStorageRead, Delegation,
        FungibleTokenData, Utxo,
    },
};
use std::collections::BTreeMap;

use common::chain::{DelegationId, PoolId, UtxoOutPoint};
use pos_accounting::PoolData;

use super::{ApiServerPostgresTransactionalRo, CONN_ERR};

#[async_trait::async_trait]
impl<'a> ApiServerStorageRead for ApiServerPostgresTransactionalRo<'a> {
    async fn is_initialized(&self) -> Result<bool, ApiServerStorageError> {
        let mut conn = QueryFromConnection::new(self.connection.as_ref().expect(CONN_ERR));
        let res = conn.is_initialized().await?;

        Ok(res)
    }

    async fn get_storage_version(&self) -> Result<Option<u32>, ApiServerStorageError> {
        let mut conn = QueryFromConnection::new(self.connection.as_ref().expect(CONN_ERR));
        let res = conn.get_storage_version().await?;

        Ok(res)
    }

    async fn get_address_balance(
        &self,
        address: &str,
        coin_or_token_id: CoinOrTokenId,
    ) -> Result<Option<Amount>, ApiServerStorageError> {
        let conn = QueryFromConnection::new(self.connection.as_ref().expect(CONN_ERR));
        let res = conn.get_address_balance(address, coin_or_token_id).await?;

        Ok(res)
    }

    async fn get_address_transactions(
        &self,
        address: &str,
    ) -> Result<Vec<Id<common::chain::Transaction>>, ApiServerStorageError> {
        let conn = QueryFromConnection::new(self.connection.as_ref().expect(CONN_ERR));
        let res = conn.get_address_transactions(address).await?;

        Ok(res)
    }

    async fn get_best_block(&self) -> Result<(BlockHeight, Id<GenBlock>), ApiServerStorageError> {
        let mut conn = QueryFromConnection::new(self.connection.as_ref().expect(CONN_ERR));
        let res = conn.get_best_block().await?;

        Ok(res)
    }

    async fn get_block(
        &self,
        block_id: Id<common::chain::Block>,
    ) -> Result<Option<common::chain::Block>, ApiServerStorageError> {
        let mut conn = QueryFromConnection::new(self.connection.as_ref().expect(CONN_ERR));
        let res = conn.get_block(block_id).await?;

        Ok(res)
    }

    async fn get_block_aux_data(
        &self,
        block_id: Id<common::chain::Block>,
    ) -> Result<
        Option<crate::storage::storage_api::block_aux_data::BlockAuxData>,
        ApiServerStorageError,
    > {
        let mut conn = QueryFromConnection::new(self.connection.as_ref().expect(CONN_ERR));
        let res = conn.get_block_aux_data(block_id).await?;

        Ok(res)
    }

    async fn get_delegation(
        &self,
        delegation_id: DelegationId,
    ) -> Result<Option<Delegation>, crate::storage::storage_api::ApiServerStorageError> {
        let mut conn = QueryFromConnection::new(self.connection.as_ref().expect(CONN_ERR));
        let res = conn.get_delegation(delegation_id).await?;

        Ok(res)
    }

    async fn get_pool_delegations(
        &self,
        pool_id: PoolId,
    ) -> Result<BTreeMap<DelegationId, Delegation>, ApiServerStorageError> {
        let mut conn = QueryFromConnection::new(self.connection.as_ref().expect(CONN_ERR));
        let res = conn.get_pool_delegation_shares(pool_id).await?;

        Ok(res)
    }

    async fn get_main_chain_block_id(
        &self,
        block_height: BlockHeight,
    ) -> Result<Option<Id<common::chain::Block>>, ApiServerStorageError> {
        let mut conn = QueryFromConnection::new(self.connection.as_ref().expect(CONN_ERR));
        let res = conn.get_main_chain_block_id(block_height).await?;

        Ok(res)
    }

    async fn get_transaction_with_block(
        &self,
        transaction_id: Id<common::chain::Transaction>,
    ) -> Result<
        Option<(Option<BlockAuxData>, common::chain::SignedTransaction)>,
        ApiServerStorageError,
    > {
        let mut conn = QueryFromConnection::new(self.connection.as_ref().expect(CONN_ERR));
        let res = conn.get_transaction_with_block(transaction_id).await?;

        Ok(res)
    }

    async fn get_pool_data(
        &self,
        pool_id: common::chain::PoolId,
    ) -> Result<Option<PoolData>, crate::storage::storage_api::ApiServerStorageError> {
        let mut conn = QueryFromConnection::new(self.connection.as_ref().expect(CONN_ERR));
        let res = conn.get_pool_data(pool_id).await?;

        Ok(res)
    }

    async fn get_latest_pool_data(
        &self,
        len: u32,
        offset: u32,
    ) -> Result<Vec<(PoolId, PoolData)>, ApiServerStorageError> {
        let conn = QueryFromConnection::new(self.connection.as_ref().expect(CONN_ERR));
        let res = conn.get_latest_pool_data(len, offset).await?;

        Ok(res)
    }

    async fn get_pool_data_with_largest_pledge(
        &self,
        len: u32,
        offset: u32,
    ) -> Result<Vec<(PoolId, PoolData)>, ApiServerStorageError> {
        let conn = QueryFromConnection::new(self.connection.as_ref().expect(CONN_ERR));
        let res = conn.get_pool_data_with_largest_pledge(len, offset).await?;

        Ok(res)
    }

    async fn get_transaction(
        &self,
        transaction_id: Id<common::chain::Transaction>,
    ) -> Result<
        Option<(
            Option<Id<common::chain::Block>>,
            common::chain::SignedTransaction,
        )>,
        ApiServerStorageError,
    > {
        let mut conn = QueryFromConnection::new(self.connection.as_ref().expect(CONN_ERR));
        let res = conn.get_transaction(transaction_id).await?;

        Ok(res)
    }

    async fn get_utxo(
        &self,
        outpoint: UtxoOutPoint,
    ) -> Result<Option<Utxo>, ApiServerStorageError> {
        let mut conn = QueryFromConnection::new(self.connection.as_ref().expect(CONN_ERR));
        let res = conn.get_utxo(outpoint).await?;

        Ok(res)
    }

    async fn get_address_available_utxos(
        &self,
        address: &str,
    ) -> Result<Vec<(UtxoOutPoint, TxOutput)>, ApiServerStorageError> {
        let mut conn = QueryFromConnection::new(self.connection.as_ref().expect(CONN_ERR));
        let res = conn.get_address_available_utxos(address).await?;

        Ok(res)
    }

    async fn get_delegations_from_address(
        &self,
        address: &Destination,
    ) -> Result<Vec<(DelegationId, Delegation)>, ApiServerStorageError> {
        let mut conn = QueryFromConnection::new(self.connection.as_ref().expect(CONN_ERR));
        let res = conn.get_delegations_from_address(address).await?;

        Ok(res)
    }

    async fn get_fungible_token_issuance(
        &self,
        token_id: TokenId,
    ) -> Result<Option<FungibleTokenData>, ApiServerStorageError> {
        let conn = QueryFromConnection::new(self.connection.as_ref().expect(CONN_ERR));
        let res = conn.get_fungible_token_issuance(token_id).await?;

        Ok(res)
    }

    async fn get_nft_token_issuance(
        &self,
        token_id: TokenId,
    ) -> Result<Option<NftIssuance>, ApiServerStorageError> {
        let conn = QueryFromConnection::new(self.connection.as_ref().expect(CONN_ERR));
        let res = conn.get_nft_token_issuance(token_id).await?;

        Ok(res)
    }
}
