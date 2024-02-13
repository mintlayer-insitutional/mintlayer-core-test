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

pub mod client_impl;

use crypto::key::hdkd::u31::U31;
use rpc::new_http_client;
use rpc::RpcAuthData;
use rpc::RpcHttpClient;

use crate::wallet_rpc_traits::WalletInterface;

// use crate::wallet_rpc_traits::ColdWalletInterface;

#[derive(thiserror::Error, Debug)]
pub enum WalletRpcError {
    #[error("Initialization error: {0}")]
    InitializationError(Box<WalletRpcError>),
    #[error("Decoding error: {0}")]
    DecodingError(#[from] serialization::hex::HexError),
    #[error("Client creation error: {0}")]
    ClientCreationError(jsonrpsee::core::ClientError),
    #[error("Response error: {0}")]
    ResponseError(jsonrpsee::core::ClientError),
}

#[derive(Clone, Debug)]
pub struct ClientWalletRpc {
    http_client: RpcHttpClient,
}

impl ClientWalletRpc {
    pub async fn new(
        remote_socket_address: String,
        rpc_auth: RpcAuthData,
    ) -> Result<Self, WalletRpcError> {
        let host = format!("http://{remote_socket_address}");

        let http_client =
            new_http_client(host, rpc_auth).map_err(WalletRpcError::ClientCreationError)?;

        let client = Self { http_client };

        client
            .get_issued_addresses(U31::ZERO)
            .await
            .map_err(|e| WalletRpcError::InitializationError(Box::new(e)))?;

        Ok(client)
    }
}
