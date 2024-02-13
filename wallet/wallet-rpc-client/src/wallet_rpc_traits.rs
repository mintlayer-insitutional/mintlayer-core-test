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

use chainstate::ChainInfo;
use common::{
    chain::{Block, SignedTransaction, Transaction, UtxoOutPoint},
    primitives::{DecimalAmount, Id},
};
use p2p_types::{
    bannable_address::BannableAddress, ip_or_socket_address::IpOrSocketAddress,
    socket_address::SocketAddress,
};
use serialization::hex_encoded::HexEncoded;
use wallet_controller::{ConnectedPeer, ControllerConfig};
use wallet_rpc_lib::types::{
    AccountIndexArg, AddressInfo, AddressWithUsageInfo, Balances, BlockInfo, DelegationInfo,
    NewAccountInfo, NewDelegation, NewTransaction, NftMetadata, NodeVersion, PoolInfo,
    PublicKeyInfo, RpcTokenId, SeedPhrase, StakePoolBalance, StakingStatus, TokenMetadata,
    TxOptionsOverrides, VrfPublicKeyInfo,
};
use wallet_types::{
    utxo_types::{UtxoStates, UtxoTypes},
    with_locked::WithLocked,
};

#[async_trait::async_trait]
pub trait ColdWalletInterface {
    type Error: std::error::Error + Send + Sync + 'static;

    async fn shutdown(&mut self) -> Result<(), Self::Error>;

    async fn create_wallet(
        &self,
        path: String,
        store_seed_phrase: bool,
        mnemonic: Option<String>,
    ) -> Result<(), Self::Error>;

    async fn open_wallet(&self, path: String, password: Option<String>) -> Result<(), Self::Error>;

    async fn close_wallet(&self) -> Result<(), Self::Error>;

    async fn sync(&self) -> Result<(), Self::Error>;

    async fn rescan(&self) -> Result<(), Self::Error>;

    async fn get_seed_phrase(&self) -> Result<SeedPhrase, Self::Error>;

    async fn purge_seed_phrase(&self) -> Result<SeedPhrase, Self::Error>;

    async fn set_lookahead_size(
        &self,
        lookahead_size: u32,
        i_know_what_i_am_doing: bool,
    ) -> Result<(), Self::Error>;

    async fn encrypt_private_keys(&self, password: String) -> Result<(), Self::Error>;

    async fn remove_private_key_encryption(&self) -> Result<(), Self::Error>;

    async fn unlock_private_keys(&self, password: String) -> Result<(), Self::Error>;

    async fn lock_private_key_encryption(&self) -> Result<(), Self::Error>;

    async fn best_block(&self) -> Result<BlockInfo, Self::Error>;

    async fn create_account(&self, name: Option<String>) -> Result<NewAccountInfo, Self::Error>;

    async fn get_issued_addresses(
        &self,
        options: AccountIndexArg,
    ) -> Result<Vec<AddressWithUsageInfo>, Self::Error>;

    async fn issue_address(
        &self,
        account_index: AccountIndexArg,
    ) -> Result<AddressInfo, Self::Error>;

    async fn reveal_public_key(
        &self,
        account_index: AccountIndexArg,
        address: String,
    ) -> Result<PublicKeyInfo, Self::Error>;

    async fn get_balance(
        &self,
        account_index: AccountIndexArg,
        utxo_states: UtxoStates,
        with_locked: WithLocked,
    ) -> Result<Balances, Self::Error>;

    async fn get_utxos(
        &self,
        account_index: AccountIndexArg,
        utxo_types: UtxoTypes,
        utxo_states: UtxoStates,
        with_locked: WithLocked,
    ) -> Result<Vec<serde_json::Value>, Self::Error>;

    async fn submit_raw_transaction(
        &self,
        tx: HexEncoded<SignedTransaction>,
        do_not_store: bool,
        options: TxOptionsOverrides,
    ) -> Result<NewTransaction, Self::Error>;

    async fn send_coins(
        &self,
        account_index: AccountIndexArg,
        address: String,
        amount: DecimalAmount,
        selected_utxos: Vec<UtxoOutPoint>,
        config: ControllerConfig,
    ) -> Result<NewTransaction, Self::Error>;

    async fn create_stake_pool(
        &self,
        account_index: AccountIndexArg,
        amount: DecimalAmount,
        cost_per_block: DecimalAmount,
        margin_ratio_per_thousand: String,
        decommission_address: String,
        config: ControllerConfig,
    ) -> Result<NewTransaction, Self::Error>;

    async fn decommission_stake_pool(
        &self,
        account_index: AccountIndexArg,
        pool_id: String,
        output_address: Option<String>,
        config: ControllerConfig,
    ) -> Result<NewTransaction, Self::Error>;

    async fn create_delegation(
        &self,
        account_index: AccountIndexArg,
        address: String,
        pool_id: String,
        config: ControllerConfig,
    ) -> Result<NewDelegation, Self::Error>;

    async fn delegate_staking(
        &self,
        account_index: AccountIndexArg,
        amount: DecimalAmount,
        delegation_id: String,
        config: ControllerConfig,
    ) -> Result<NewTransaction, Self::Error>;

    async fn withdraw_from_delegation(
        &self,
        account_index: AccountIndexArg,
        address: String,
        amount: DecimalAmount,
        delegation_id: String,
        config: ControllerConfig,
    ) -> Result<NewTransaction, Self::Error>;

    async fn start_staking(&self, account_index: AccountIndexArg) -> Result<(), Self::Error>;

    async fn stop_staking(&self, account_index: AccountIndexArg) -> Result<(), Self::Error>;

    async fn staking_status(
        &self,
        account_index: AccountIndexArg,
    ) -> Result<StakingStatus, Self::Error>;

    async fn list_pool_ids(
        &self,
        account_index: AccountIndexArg,
    ) -> Result<Vec<PoolInfo>, Self::Error>;

    async fn stake_pool_balance(&self, pool_id: String) -> Result<StakePoolBalance, Self::Error>;

    async fn list_delegation_ids(
        &self,
        account_index: AccountIndexArg,
    ) -> Result<Vec<DelegationInfo>, Self::Error>;

    async fn list_created_blocks_ids(
        &self,
        account_index: AccountIndexArg,
    ) -> Result<Vec<BlockInfo>, Self::Error>;

    async fn new_vrf_public_key(
        &self,
        account_index: AccountIndexArg,
    ) -> Result<VrfPublicKeyInfo, Self::Error>;

    async fn get_vrf_public_key(
        &self,
        account_index: AccountIndexArg,
    ) -> Result<Vec<VrfPublicKeyInfo>, Self::Error>;

    async fn issue_new_nft(
        &self,
        account_index: AccountIndexArg,
        destination_address: String,
        metadata: NftMetadata,
        config: ControllerConfig,
    ) -> Result<RpcTokenId, Self::Error>;

    async fn issue_new_token(
        &self,
        account_index: AccountIndexArg,
        destination_address: String,
        metadata: TokenMetadata,
        config: ControllerConfig,
    ) -> Result<RpcTokenId, Self::Error>;

    async fn change_token_authority(
        &self,
        account_index: AccountIndexArg,
        token_id: String,
        address: String,
        config: ControllerConfig,
    ) -> Result<NewTransaction, Self::Error>;

    async fn mint_tokens(
        &self,
        account_index: AccountIndexArg,
        token_id: String,
        address: String,
        amount: DecimalAmount,
        config: ControllerConfig,
    ) -> Result<NewTransaction, Self::Error>;

    async fn unmint_tokens(
        &self,
        account_index: AccountIndexArg,
        token_id: String,
        amount: DecimalAmount,
        config: ControllerConfig,
    ) -> Result<NewTransaction, Self::Error>;

    async fn lock_token_supply(
        &self,
        account_index: AccountIndexArg,
        token_id: String,
        config: ControllerConfig,
    ) -> Result<NewTransaction, Self::Error>;

    async fn freeze_token(
        &self,
        account_index: AccountIndexArg,
        token_id: String,
        is_unfreezable: bool,
        config: ControllerConfig,
    ) -> Result<NewTransaction, Self::Error>;

    async fn unfreeze_token(
        &self,
        account_index: AccountIndexArg,
        token_id: String,
        config: ControllerConfig,
    ) -> Result<NewTransaction, Self::Error>;

    async fn send_tokens(
        &self,
        account_index: AccountIndexArg,
        token_id: String,
        address: String,
        amount: DecimalAmount,
        config: ControllerConfig,
    ) -> Result<NewTransaction, Self::Error>;

    async fn deposit_data(
        &self,
        account_index: AccountIndexArg,
        data: String,
        config: ControllerConfig,
    ) -> Result<NewTransaction, Self::Error>;

    async fn node_version(&self) -> Result<NodeVersion, Self::Error>;

    async fn node_shutdown(&self) -> Result<(), Self::Error>;

    async fn connect_to_peer(&self, address: IpOrSocketAddress) -> Result<(), Self::Error>;

    async fn disconnect_peer(&self, peer_id: u64) -> Result<(), Self::Error>;

    async fn list_banned(
        &self,
    ) -> Result<Vec<(BannableAddress, common::primitives::time::Time)>, Self::Error>;

    async fn ban_address(
        &self,
        address: BannableAddress,
        duration: std::time::Duration,
    ) -> Result<(), Self::Error>;

    async fn unban_address(&self, address: BannableAddress) -> Result<(), Self::Error>;

    async fn list_discouraged(
        &self,
    ) -> Result<Vec<(BannableAddress, common::primitives::time::Time)>, Self::Error>;

    async fn peer_count(&self) -> Result<usize, Self::Error>;

    async fn connected_peers(&self) -> Result<Vec<ConnectedPeer>, Self::Error>;

    async fn reserved_peers(&self) -> Result<Vec<SocketAddress>, Self::Error>;

    async fn add_reserved_peer(&self, address: IpOrSocketAddress) -> Result<(), Self::Error>;

    async fn remove_reserved_peer(&self, address: IpOrSocketAddress) -> Result<(), Self::Error>;

    async fn submit_block(&self, block: HexEncoded<Block>) -> Result<(), Self::Error>;

    async fn chainstate_info(&self) -> Result<ChainInfo, Self::Error>;

    async fn abandon_transaction(
        &self,
        account_index: AccountIndexArg,
        transaction_id: Id<Transaction>,
    ) -> Result<(), Self::Error>;

    async fn list_pending_transactions(
        &self,
        account_index: AccountIndexArg,
    ) -> Result<Vec<Id<Transaction>>, Self::Error>;

    async fn get_transaction(
        &self,
        account_index: AccountIndexArg,
        transaction_id: Id<Transaction>,
    ) -> Result<serde_json::Value, Self::Error>;

    async fn get_raw_transaction(
        &self,
        account_index: AccountIndexArg,
        transaction_id: Id<Transaction>,
    ) -> Result<String, Self::Error>;

    async fn get_raw_signed_transaction(
        &self,
        account_index: AccountIndexArg,
        transaction_id: Id<Transaction>,
    ) -> Result<String, Self::Error>;
}
