// Copyright (c) 2021-2023 RBB S.r.l
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

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use tokio::sync::mpsc::UnboundedSender;

use logging::log;
use p2p_types::{bannable_address::BannableAddress, socket_address::SocketAddress, PeerId};

use crate::{
    net::types::{PeerInfo, PeerRole},
    peer_manager::{self, dns_seed::DnsSeed},
};

pub mod test_node;
pub mod test_node_group;

pub use test_node::*;
pub use test_node_group::*;

// TODO: test utilities related to peer manager should probably go into peer_manager/tests.
// Or perhaps we should have a dedicated test_helpers module, which wouldn't be specific to
// any particular kind of tests.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerManagerNotification {
    BanScoreAdjustment {
        address: SocketAddress,
        new_score: u32,
    },
    Ban {
        address: BannableAddress,
    },
    Discourage {
        address: BannableAddress,
    },
    Heartbeat,
    ConnectionAccepted {
        address: SocketAddress,
        peer_role: PeerRole,
    },
}

pub struct PeerManagerObserver {
    notification_sender: UnboundedSender<PeerManagerNotification>,
}

impl PeerManagerObserver {
    pub fn new(notification_sender: UnboundedSender<PeerManagerNotification>) -> Self {
        Self {
            notification_sender,
        }
    }

    fn send_notification(&self, notification: PeerManagerNotification) {
        let send_result = self.notification_sender.send(notification.clone());

        if let Err(err) = send_result {
            log::warn!("Error sending peer manager notification {notification:?}: {err}");
        }
    }
}

impl peer_manager::Observer for PeerManagerObserver {
    fn on_peer_ban_score_adjustment(&mut self, address: SocketAddress, new_score: u32) {
        self.send_notification(PeerManagerNotification::BanScoreAdjustment { address, new_score });
    }

    fn on_peer_ban(&mut self, address: BannableAddress) {
        self.send_notification(PeerManagerNotification::Ban { address });
    }

    fn on_peer_discouragement(&mut self, address: BannableAddress) {
        self.send_notification(PeerManagerNotification::Discourage { address });
    }

    fn on_heartbeat(&mut self) {
        self.send_notification(PeerManagerNotification::Heartbeat);
    }

    fn on_connection_accepted(&mut self, address: SocketAddress, peer_role: PeerRole) {
        self.send_notification(PeerManagerNotification::ConnectionAccepted { address, peer_role });
    }
}

#[derive(Debug)]
pub struct TestPeerInfo {
    pub info: PeerInfo,
    pub role: PeerRole,
}

#[derive(Debug)]
pub struct TestPeersInfo {
    pub info: BTreeMap<SocketAddress, TestPeerInfo>,
}

impl TestPeersInfo {
    pub fn from_peer_mgr_peer_contexts(
        contexts: &BTreeMap<PeerId, peer_manager::peer_context::PeerContext>,
    ) -> Self {
        let mut info = BTreeMap::new();

        for ctx in contexts.values() {
            info.insert(
                ctx.peer_address,
                TestPeerInfo {
                    info: ctx.info.clone(),
                    role: ctx.peer_role,
                },
            );
        }

        Self { info }
    }

    pub fn count_peers_by_role(&self, role: PeerRole) -> usize {
        self.info.iter().filter(|(_, info)| info.role == role).count()
    }
}

pub struct TestDnsSeed {
    addresses: Arc<Mutex<Vec<SocketAddress>>>,
}

impl TestDnsSeed {
    pub fn new(addresses: Arc<Mutex<Vec<SocketAddress>>>) -> Self {
        Self { addresses }
    }
}

#[async_trait]
impl DnsSeed for TestDnsSeed {
    async fn obtain_addresses(&self) -> Vec<SocketAddress> {
        self.addresses.lock().unwrap().clone()
    }
}
