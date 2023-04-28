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

use std::{sync::Arc, time::Duration};

use chainstate::ChainInfo;
use common::{
    chain::{Block, ChainConfig, GenBlock},
    primitives::{BlockHeight, Id},
};
use logging::log;
use node_comm::node_traits::NodeInterface;
use serialization::hex::HexEncode;
use tokio::{sync::mpsc, task::JoinHandle};
use wallet::DefaultWallet;

// Disabled until wallet implements required API
const BLOCK_SYNC_ENABLED: bool = false;

struct NextBlockInfo {
    common_block_id: Id<GenBlock>,
    block_id: Id<Block>,
    block_height: BlockHeight,
}

struct FetchedBlock {
    block: Block,
    block_height: BlockHeight,
}

#[derive(thiserror::Error, Debug)]
enum FetchBlockError<T: NodeInterface> {
    #[error("Unexpected RPC error: {0}")]
    UnexpectedRpcError(T::Error),
    #[error("Unexpected genesis block received at height {0}")]
    UnexpectedGenesisBlock(BlockHeight),
    #[error("There is no block at height {0}")]
    NoBlockAtHeight(BlockHeight),
    #[error("Block with id {0} not found")]
    BlockNotFound(Id<Block>),
    #[error("Invalid prev block id: {0}, expected: {1}")]
    InvalidPrevBlockId(Id<GenBlock>, Id<GenBlock>),
}

type BlockFetchResult<T> = Result<FetchedBlock, FetchBlockError<T>>;

pub struct BlockSyncing<T: NodeInterface> {
    chain_config: Arc<ChainConfig>,

    rpc_client: T,

    node_state_rx: mpsc::Receiver<ChainInfo>,

    /// Last known chain state information of the remote node.
    /// Used to start block synchronization when a new block is found.
    node_chain_info: Option<ChainInfo>,

    state_sync_task: JoinHandle<()>,

    /// Handle of the background block fetch task, if started.
    /// If successful, the wallet will be updated.
    /// If there was an error, the block sync process will be retried later.
    block_fetch_task: Option<JoinHandle<BlockFetchResult<T>>>,
}

impl<T: NodeInterface + Clone + Send + Sync + 'static> BlockSyncing<T> {
    pub fn new(chain_config: Arc<ChainConfig>, rpc_client: T) -> Self {
        let (node_state_tx, node_state_rx) = mpsc::channel(1);
        let state_sync_task = tokio::spawn(run_state_sync(node_state_tx, rpc_client.clone()));

        Self {
            chain_config,
            rpc_client,
            node_state_rx,
            node_chain_info: None,
            state_sync_task,
            block_fetch_task: None,
        }
    }

    fn handle_node_state_change(&mut self, chain_info: ChainInfo) {
        log::info!(
            "Node chainstate updated, best block height: {}, best block id: {}",
            chain_info.best_block_height,
            chain_info.best_block_id.hex_encode()
        );
        self.node_chain_info = Some(chain_info);
    }

    fn start_block_fetch_if_needed(&mut self, wallet: &mut DefaultWallet) {
        if !BLOCK_SYNC_ENABLED {
            return;
        }

        if self.block_fetch_task.is_some() {
            return;
        }

        let (node_block_id, node_block_height) = match self.node_chain_info.as_ref() {
            Some(info) => (info.best_block_id, info.best_block_height),
            None => return,
        };

        let (wallet_block_id, wallet_block_height) =
            wallet.get_best_block().expect("`get_best_block` should not fail normally");

        // Wait until the node has enough block height.
        // Block sync may not work correctly otherwise.
        if node_block_id == wallet_block_id || node_block_height < wallet_block_height {
            return;
        }

        let chain_config = Arc::clone(&self.chain_config);
        let mut rpc_client = self.rpc_client.clone();

        self.block_fetch_task = Some(tokio::spawn(async move {
            let sync_res = fetch_new_block(
                &chain_config,
                &mut rpc_client,
                node_block_id,
                node_block_height,
                wallet_block_id,
                wallet_block_height,
            )
            .await;

            if let Err(e) = &sync_res {
                log::error!("Block fetch failed: {e}");
                // Wait a bit to not spam constantly if the node is unreachable
                tokio::time::sleep(Duration::from_secs(10)).await;
            }

            sync_res
        }));
    }

    fn handle_block_fetch_result(&mut self, res: BlockFetchResult<T>, wallet: &mut DefaultWallet) {
        if let Ok(FetchedBlock {
            block,
            block_height,
        }) = res
        {
            let scan_res = wallet.scan_new_blocks(block_height, vec![block]);
            if let Err(e) = scan_res {
                log::error!("Block scan failed: {e}");
            }
        }
    }

    async fn recv_block_fetch_result(
        block_fetch_task: &mut Option<JoinHandle<BlockFetchResult<T>>>,
    ) -> BlockFetchResult<T> {
        // This must be cancel safe!
        match block_fetch_task {
            Some(task) => {
                let res = task.await.expect("Block fetch should not panic");
                *block_fetch_task = None;
                res
            }
            None => std::future::pending().await,
        }
    }

    pub async fn run(&mut self, wallet: &mut DefaultWallet) {
        // This must be cancel safe!
        loop {
            self.start_block_fetch_if_needed(wallet);

            tokio::select! {
                chain_info_opt = self.node_state_rx.recv() => {
                    // Channel is always open because [run_tip_sync] does not return
                    self.handle_node_state_change(chain_info_opt.expect("Channel must be open"));
                }
                sync_result = Self::recv_block_fetch_result(&mut self.block_fetch_task) => {
                    self.handle_block_fetch_result(sync_result, wallet);
                }
            }
        }
    }
}

async fn run_state_sync<T: NodeInterface>(state_tx: mpsc::Sender<ChainInfo>, rpc_client: T) {
    let mut last_state = None;

    while !state_tx.is_closed() {
        let state_res = rpc_client.chainstate_info().await;
        match state_res {
            Ok(state) => {
                if last_state.as_ref() != Some(&state) {
                    _ = state_tx.send(state.clone()).await;
                    last_state = Some(state);
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(e) => {
                logging::log::error!("Node state sync error: {}", e);
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        }
    }
}

impl<T: NodeInterface> Drop for BlockSyncing<T> {
    fn drop(&mut self) {
        self.state_sync_task.abort();
        self.block_fetch_task.as_ref().map(JoinHandle::abort);
    }
}

// TODO: For security reasons, the wallet should probably keep track of latest blocks
// and not allow very large reorgs (for example, the Monero wallet allows reorgs of up to 100 blocks).
async fn get_next_block_info<T: NodeInterface>(
    chain_config: &ChainConfig,
    rpc_client: &mut T,
    node_block_id: Id<GenBlock>,
    node_block_height: BlockHeight,
    wallet_block_id: Id<GenBlock>,
    wallet_block_height: BlockHeight,
) -> Result<NextBlockInfo, FetchBlockError<T>> {
    assert!(node_block_id != wallet_block_id);
    assert!(node_block_height >= wallet_block_height);

    let common_block_opt = rpc_client
        .get_last_common_ancestor(wallet_block_id, node_block_id)
        .await
        .map_err(FetchBlockError::UnexpectedRpcError)?;

    let (common_block_id, common_block_height) = match common_block_opt {
        // Common branch is found
        Some(common_block) => common_block,
        // Common branch not found, restart from genesis block.
        // This happens when:
        // 1. The node is downloading blocks.
        // 2. Blocks in the blockchain were pruned, so the block the wallet knows about is now unrecognized in the block tree.
        None => (chain_config.genesis_block_id(), BlockHeight::zero()),
    };

    let block_height = common_block_height.next_height();

    let gen_block_id = rpc_client
        .get_block_id_at_height(block_height)
        .await
        .map_err(FetchBlockError::UnexpectedRpcError)?
        .ok_or(FetchBlockError::NoBlockAtHeight(block_height))?;

    // This must not be genesis, but we don't want to trust the remote node and give it power to panic the wallet with expect.
    // TODO: we should mark this node as malicious if this happens to be genesis.
    let block_id = match gen_block_id.classify(chain_config) {
        common::chain::GenBlockId::Genesis(_) => {
            return Err(FetchBlockError::UnexpectedGenesisBlock(wallet_block_height))
        }
        common::chain::GenBlockId::Block(id) => id,
    };

    Ok(NextBlockInfo {
        common_block_id,
        block_id,
        block_height,
    })
}

async fn fetch_new_block<T: NodeInterface>(
    chain_config: &ChainConfig,
    rpc_client: &mut T,
    node_block_id: Id<GenBlock>,
    node_block_height: BlockHeight,
    wallet_block_id: Id<GenBlock>,
    wallet_block_height: BlockHeight,
) -> Result<FetchedBlock, FetchBlockError<T>> {
    let NextBlockInfo {
        common_block_id,
        block_id,
        block_height,
    } = get_next_block_info(
        chain_config,
        rpc_client,
        node_block_id,
        node_block_height,
        wallet_block_id,
        wallet_block_height,
    )
    .await?;

    let block = rpc_client
        .get_block(block_id)
        .await
        .map_err(FetchBlockError::UnexpectedRpcError)?
        .ok_or(FetchBlockError::BlockNotFound(block_id))?;
    utils::ensure!(
        *block.header().prev_block_id() == common_block_id,
        FetchBlockError::InvalidPrevBlockId(*block.header().prev_block_id(), common_block_id)
    );

    Ok(FetchedBlock {
        block,
        block_height,
    })
}
