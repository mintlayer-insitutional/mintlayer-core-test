// Copyright (c) 2022 RBB S.r.l
// opensource@mintlayer.org
// SPDX-License-Identifier: MIT
// Licensed under the MIT License;
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://spdx.org/licenses/MIT
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
// Author(s): A. Altonen

//! Mintlayer peer manager
//!
//! TODO
//!
//!

use crate::{
    error::{P2pError, PeerError, ProtocolError},
    event,
    net::{self, ConnectivityService, NetworkingService},
};
use common::{chain::ChainConfig, primitives::version};
use futures::FutureExt;
use logging::log;
use std::{
    collections::HashMap,
    fmt::Debug,
    str::FromStr,
    sync::Arc,
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
use utils::ensure;

mod peerdb;

const MAX_ACTIVE_CONNECTIONS: usize = 32;
const PEER_MGR_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);



pub struct PeerManager<T>
where
    T: NetworkingService,
{
    /// Chain config
    config: Arc<ChainConfig>,

    /// Handle for sending/receiving connectivity events
    handle: T::ConnectivityHandle,

    /// Hashmap for peer information
    peers: HashMap<T::PeerId, peerdb::PeerContext<T>>,

    /// RX channel for receiving control events
    rx_swarm: mpsc::Receiver<event::SwarmEvent<T>>,

    /// TX channel for sending events to SyncManager
    tx_sync: mpsc::Sender<event::SyncControlEvent<T>>,

    /// Hashmap of pending outbound connections
    pending: HashMap<T::Address, Option<oneshot::Sender<crate::Result<()>>>>,

    /// Peer database
    peerdb: peerdb::PeerDb<T>,
}

impl<T> PeerManager<T>
where
    T: NetworkingService + 'static,
    T::ConnectivityHandle: ConnectivityService<T>,
    <T as NetworkingService>::Address: FromStr,
    <<T as NetworkingService>::Address as FromStr>::Err: Debug,
{
    pub fn new(
        config: Arc<ChainConfig>,
        handle: T::ConnectivityHandle,
        rx_swarm: mpsc::Receiver<event::SwarmEvent<T>>,
        tx_sync: mpsc::Sender<event::SyncControlEvent<T>>,
    ) -> Self {
        Self {
            config,
            handle,
            rx_swarm,
            tx_sync,
            peerdb: peerdb::PeerDb::new(),
            peers: HashMap::with_capacity(MAX_ACTIVE_CONNECTIONS),
            pending: HashMap::new(),
        }
    }

    /// Update the list of known peers or known peer's list of addresses
    fn peer_discovered(&mut self, peers: &[net::types::AddrInfo<T>]) {
        self.peerdb.discover_peers(peers)
    }

    /// Update the list of known peers or known peer's list of addresses
    fn peer_expired(&mut self, peers: &[net::types::AddrInfo<T>]) {
        self.peerdb.expire_peers(peers)
    }

    /// Validate address
    ///
    /// Verify that the IP address is not banned
    fn validate_address(&self, address: &T::Address) -> bool {
        self.peerdb.is_address_banned(address)
    }

    /// Validate peer ID
    ///
    /// Verify that the peer ID is not banned
    fn validate_peer_id(&self, peer_id: &T::PeerId) -> bool {
        self.peerdb.is_id_banned(peer_id)
    }

    /// Verify protocol compatibility
    ///
    /// Make sure that remote peer supports protocols same protocols that we do and that
    /// they support the mandatory protocols which for now are configured to be:
    ///
    /// - `/meshsub/1.1.0`
    /// - `/meshsub/1.0.0`
    /// - `/ipfs/ping/1.0.0`
    /// - `/ipfs/id/1.0.0`
    /// - `/ipfs/id/push/1.0.0`
    /// - `/mintlayer/sync/0.1.0`
    ///
    /// If any of the procols are missing or if any of them have a different version,
    /// the validation fails and connection must be closed.
    ///
    /// Either peer may support additional protocols which are not known to the other
    /// peer and that is totally fine. As long as the aforementioned protocols with
    /// matching versions are found, the protocol set has been validated successfully.
    // TODO: create generic versions of the protocols when mock interface is supported again
    // TODO: convert `protocols` to a hashset
    fn validate_supported_protocols(&self, protocols: &[T::ProtocolId]) -> bool {
        const REQUIRED: &[&'static str] = &[
            "/meshsub/1.1.0",
            "/meshsub/1.0.0",
            "/ipfs/ping/1.0.0",
            "/ipfs/id/1.0.0",
            "/ipfs/id/push/1.0.0",
            "/mintlayer/sync/0.1.0",
        ];

        // TODO: zzz
        for required_proto in REQUIRED {
            if !protocols.iter().find(|proto| &proto.to_string() == required_proto).is_some() {
                return false;
            }
        }

        true
    }

    /// Verify software version compatibility
    ///
    /// Make sure that local and remote peer have the same software version
    fn validate_version(&self, version: &version::SemVer) -> bool {
        version == self.config.version()
    }

    /// Handle connection established event
    ///
    /// The event is received from the networking backend and it's either a result of an incoming
    /// connection from a remote peer or a response to a outbound connection that was initiated
    /// by the node as result of swarm mai
    async fn validate_connection(&mut self, info: net::types::PeerInfo<T>) -> crate::Result<()> {
        log::debug!("{}", info);

        ensure!(
            info.magic_bytes == *self.config.magic_bytes(),
            P2pError::ProtocolError(ProtocolError::DifferentNetwork(
                *self.config.magic_bytes(),
                info.magic_bytes,
            ))
        );
        ensure!(
            self.validate_version(&info.version),
            P2pError::ProtocolError(ProtocolError::InvalidVersion(
                *self.config.version(),
                info.version
            ))
        );
        ensure!(
            self.validate_supported_protocols(&info.protocols),
            P2pError::ProtocolError(ProtocolError::Incompatible),
        );
        ensure!(
            self.peers.get(&info.peer_id).is_none(),
            P2pError::PeerError(PeerError::PeerAlreadyExists)
        );

        let peer_id = info.peer_id;
        self.peers.insert(info.peer_id, peerdb::PeerContext { _info: info });
        self.tx_sync
            .send(event::SyncControlEvent::Connected(peer_id))
            .await
            .map_err(P2pError::from)
    }

    /// Validate inbound peer connection
    ///
    /// As inbound connections are received, they are prevalidated before passed
    /// on to the generic connection validator as these connections haven't gone
    /// through the same validation as outbound connections.
    ///
    /// This function verify that neither address the nor the peer ID are on the
    /// list of banned IPs/peer IDs.
    async fn validate_inbound_connection(
        &mut self,
        address: T::Address,
        info: net::types::PeerInfo<T>,
    ) -> crate::Result<()> {
        log::debug!("validate inbound connection, inbound address {}", address);

        ensure!(
            self.validate_address(&address),
            P2pError::PeerError(PeerError::BannedAddress(address.to_string())),
        );
        ensure!(
            self.validate_peer_id(&info.peer_id),
            P2pError::PeerError(PeerError::BannedPeer(info.peer_id.to_string())),
        );

        self.validate_connection(info).await
    }

    /// Close connection to a remote node
    ///
    /// The decision to close the connection is made either by the user via RPC
    /// or by the [`PeerManager::heartbeat()`] function which has decided to cull
    /// this connection in favor of another potential connection.
    async fn close_connection(&mut self, peer_id: T::PeerId) -> crate::Result<()> {
        log::debug!("connection closed for peer {}", peer_id);

        self.tx_sync.send(event::SyncControlEvent::Disconnected(peer_id)).await?;
        self.peers.remove(&peer_id);
        Ok(())
    }

    /// Handle outbound connection error
    ///
    /// The outbound connection was dialed successfully but the remote either didn't not respond
    /// (at all or in time) or it didn't support the handshaking which forced the connection closed.
    ///
    /// If the connection was initiated by the user via RPC, inform them that the connection failed.
    /// Inform the [`PeerDb`] about the address failure.
    fn handle_outbound_error(&mut self, address: T::Address, error: P2pError) -> crate::Result<()> {
        if let Some(Some(channel)) = self.pending.remove(&address) {
            channel.send(Err(error)).map_err(|_| P2pError::ChannelClosed)?;
        }

        // TODO: report error to peerdb
        Ok(())
    }

    /// Attempt to establish an outbound connection
    ///
    /// This function doesn't block on the call but sends a command to the
    /// networking backend which then reports at some point in the future
    /// whether the connection failed or succeeded.
    async fn connect(&mut self, address: T::Address) -> crate::Result<()> {
        // TODO: verify that the peer is not already part of our swarm (needs peerdb)
        ensure!(
            !self.pending.contains_key(&address),
            P2pError::PeerError(PeerError::Pending(address.to_string()))
        );
        ensure!(
            self.validate_address(&address),
            P2pError::PeerError(PeerError::BannedAddress(address.to_string())),
        );

        self.handle.connect(address).await
    }

    /// Try to establish new outbound connections if the total number of
    /// active connections the local node has is below threshold
    ///
    // TODO: ugly, refactor
    // TODO: move this to its own file?
    #[allow(dead_code)]
    async fn auto_connect(&mut self) -> crate::Result<()> {
        // we have enough active connections
        if self.peers.len() >= MAX_ACTIVE_CONNECTIONS {
            return Ok(());
        }
        log::debug!("try to establish more outbound connections");

        // we don't know of any peers
        // if self.discovered.is_empty() {
        //     log::error!(
        //         "# of connections below threshold ({} < {}) but no peers",
        //         self.peers.len(),
        //         MAX_ACTIVE_CONNECTIONS,
        //     );
        //     return Err(P2pError::PeerError(PeerError::NoPeers));
        // }

        // let npeers = std::cmp::min(
        //     self.discovered.len(),
        //     MAX_ACTIVE_CONNECTIONS - self.peers.len(),
        // );

        // // TODO: improve peer selection
        // let mut iter = self.discovered.iter();

        // #[allow(clippy::needless_collect)]
        // let _peers: Vec<(T::PeerId, Arc<T::Address>)> = (0..npeers)
        //     .map(|i| {
        //         let peer_info = iter.nth(i).expect("Peer to exist");

        //         let (ip4, ip6) = match peer_info.1 {
        //             PeerAddrInfo::Raw { ip4, ip6 } => (ip4, ip6),
        //         };
        //         assert!(!ip4.is_empty() || !ip6.is_empty());

        //         // TODO: let user specify their preference?
        //         let addr = if ip6.is_empty() {
        //             Arc::clone(ip4.iter().next().expect("ip4 empty"))
        //         } else {
        //             Arc::clone(ip6.iter().next().expect("ip6 empty"))
        //         };

        //         (*peer_info.0, addr)
        //     })
        //     .collect::<_>();

        // for (id, addr) in peers {
        //     log::trace!("try to connect to peer {:?}, address {:?}", id, addr);
        //     // TODO: don't remove entry but modify it
        //     self.discovered.remove(&id);
        //     self.handle
        //         .connect((*addr).clone())
        //         .await
        //         .map(|_info| {
        //             let id = _info.peer_id;
        //             match self.peers.insert(id, PeerContext { _info }) {
        //                 Some(_) => panic!("peer already exists"),
        //                 None => {}
        //             }
        //         })
        //         .map_err(|err| {
        //             log::error!("failed to establish outbound connection: {:?}", err);
        //             err
        //         })?;
        // }

        Ok(())
    }

    /// Maintain the swarm state
    ///
    /// [`PeerManager::heartbeat()`] is called every time a network/control event is received
    /// or the heartbeat interval of the event loop expires. In other words, the swarm state
    /// is checked and updated at least once every 30 seconds. In high-traffic scenarios the
    /// update interval is clamped to a sensible lower bound.
    ///
    /// This function maintains the overall connectivity state of the swarm by culling
    /// low-reputation peers and establishing new connections with peers that have higher
    /// reputation. It also checks if there are outbound connections whose timer has run out,
    /// meaning they have failed. It also updates peer scores and forgets those peers that are
    /// no longer needed.
    ///
    /// TODO: IP address diversity check?
    /// TODO: clamp update interval
    ///
    /// The process starts by first checking if there
    async fn heartbeat(&mut self) -> crate::Result<()> {
        Ok(())
    }

    /// Handle the result of a control/network event
    ///
    /// Currently only subsystem/channel-related errors are considered fatal.
    /// Other errors are logged as warnings and `Ok(())` is returned as they should
    /// not distrub the operation of [`PeerManager`].
    async fn handle_error(&mut self, result: crate::Result<()>) -> crate::Result<()> {
        match result {
            Ok(_) => Ok(()),
            Err(P2pError::ChannelClosed | P2pError::SubsystemFailure) => {
                log::error!("connection lost with subsystem wrapper/p2p subsystem");
                result
            }
            Err(err) => {
                log::warn!("non-fatal error occurred: {}", err);
                Ok(())
            }
        }
    }

    /// Run the `PeerManager` event loop
    ///
    /// The event loop has three main responsibilities:
    /// - listening to and handling control events from [`crate::sync::SyncManager`]/
    /// [`crate::pubsub::PubSubMessageHandler`]/RPC
    /// - listening to network events
    /// - updating internal state
    ///
    ///
    pub async fn run(&mut self) -> crate::Result<()> {
        loop {
            let result = tokio::select! {
                event = self.rx_swarm.recv().fuse() => match event.ok_or(P2pError::ChannelClosed)? {
                    event::SwarmEvent::Connect(addr, response) => {
                        log::debug!(
                            "try to establish outbound connection to peer at address {:?}",
                            addr
                        );

                        self.connect(addr.clone()).await.map(|_| {
                            self.pending.insert(addr.clone(), Some(response));
                        })
                        .map_err(|err| {
                            log::error!("failed to dial peer at address {}: {}", addr, err);
                            err
                        })
                    }
                    // TODO: pending disconnection events
                    event::SwarmEvent::Disconnect(peer_id, response) => {
                        log::debug!("disconnect peer {} from the swarm", peer_id);

                        response
                            .send(self.handle.disconnect(peer_id).await)
                            .map_err(|_| P2pError::ChannelClosed)
                    }
                    event::SwarmEvent::GetPeerCount(response) => {
                        response.send(self.peers.len()).map_err(|_| P2pError::ChannelClosed)
                    }
                    event::SwarmEvent::GetBindAddress(response) => response
                        .send(self.handle.local_addr().to_string())
                        .map_err(|_| P2pError::ChannelClosed),
                    event::SwarmEvent::GetPeerId(response) => response
                        .send(self.handle.peer_id().to_string())
                        .map_err(|_| P2pError::ChannelClosed),
                    event::SwarmEvent::GetConnectedPeers(response) => {
                        let peers = self.peers.iter().map(|(id, _)| id.to_string()).collect::<Vec<_>>();
                        response.send(peers).map_err(|_| P2pError::ChannelClosed)
                    }
                },
                event = self.handle.poll_next() => match event {
                    Ok(event) => match event {
                        net::types::ConnectivityEvent::IncomingConnection { peer_info, addr } => {
                            // TODO: report rejection to networking backend
                            self.validate_inbound_connection(addr, peer_info).await
                        }
                        net::types::ConnectivityEvent::ConnectionAccepted { peer_info } => {
                            self.validate_connection(peer_info).await
                        }
                        net::types::ConnectivityEvent::ConnectionClosed { peer_id } => {
                            self.close_connection(peer_id).await
                        }
                        net::types::ConnectivityEvent::ConnectionError { addr, error } => {
                            self.handle_outbound_error(addr, error)
                        }
                        net::types::ConnectivityEvent::Discovered { peers } => {
                            self.peer_discovered(&peers);
                            Ok(())
                        }
                        net::types::ConnectivityEvent::Expired { peers } => {
                            self.peer_expired(&peers);
                            Ok(())
                        }
                        net::types::ConnectivityEvent::Disconnected { .. } => {
                            Ok(())
                        }
                        net::types::ConnectivityEvent::Misbehaved { .. } => {
                            Ok(())
                        }
                        net::types::ConnectivityEvent::Error { .. } => {
                            Ok(())
                        }
                    }
                    Err(err) => {
                        log::error!("failed to read network event: {:?}", err);
                        Err(err)
                    }
                },
                _event = tokio::time::sleep(PEER_MGR_HEARTBEAT_INTERVAL) => { Ok(()) }
            };

            // handle error, exit on fatal errors and finally update peer manager state
            self.handle_error(result).await?;
            self.heartbeat().await?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{DialError, P2pError};
    use common::chain::config;
    use libp2p::{multiaddr::Protocol, Multiaddr, PeerId};
    use net::{libp2p::Libp2pService, mock::MockService, ConnectivityService};
    use std::net::SocketAddr;

    async fn make_swarm_manager<T>(
        addr: T::Address,
        config: Arc<common::chain::ChainConfig>,
    ) -> PeerManager<T>
    where
        T: NetworkingService + 'static,
        T::ConnectivityHandle: ConnectivityService<T>,
        <T as NetworkingService>::Address: FromStr,
        <<T as NetworkingService>::Address as FromStr>::Err: Debug,
    {
        let (conn, _, _) = T::start(
            addr,
            &[],
            &[],
            Arc::clone(&config),
            std::time::Duration::from_secs(10),
        )
        .await
        .unwrap();
        let (_, rx) = tokio::sync::mpsc::channel(16);
        let (tx_sync, mut rx_sync) = tokio::sync::mpsc::channel(16);

        tokio::spawn(async move {
            loop {
                let _ = rx_sync.recv().await;
            }
        });

        PeerManager::<T>::new(Arc::clone(&config), conn, rx, tx_sync)
    }

    // try to connect to an address that no one listening on and verify it fails
    #[tokio::test]
    async fn test_swarm_connect_mock() {
        let addr: SocketAddr = test_utils::make_address("[::1]:");
        let config = Arc::new(config::create_mainnet());
        let mut swarm = make_swarm_manager::<MockService>(addr, config).await;

        let addr: SocketAddr = "[::1]:1".parse().unwrap();
        // TODO:
        let _ = swarm.connect(addr).await;
    }

    // try to connect to an address that no one listening on and verify it fails
    #[tokio::test]
    async fn test_swarm_connect_libp2p() {
        let addr: Multiaddr = test_utils::make_address("/ip6/::1/tcp/");
        let config = Arc::new(config::create_mainnet());
        let mut swarm = make_swarm_manager::<Libp2pService>(addr, config).await;

        let addr: Multiaddr =
            "/ip6/::1/tcp/6666/p2p/12D3KooWRn14SemPVxwzdQNg8e8Trythiww1FWrNfPbukYBmZEbJ"
                .parse()
                .unwrap();
        swarm.connect(addr).await.unwrap();
        assert!(std::matches!(
            swarm.handle.poll_next().await,
            Ok(net::types::ConnectivityEvent::ConnectionError {
                addr,
                error: P2pError::DialError(DialError::IoError(
                    std::io::ErrorKind::ConnectionRefused
                ))
            })
        ));
    }

    // verify that if the node is aware of any peers on the network,
    // call to `auto_connect()` will establish a connection with them
    #[tokio::test]
    async fn test_auto_connect_mock() {
        let config = Arc::new(config::create_mainnet());
        let addr: Multiaddr = test_utils::make_address("/ip6/::1/tcp/");
        let mut swarm = make_swarm_manager::<Libp2pService>(addr, config.clone()).await;
        let mut swarm2 =
            make_swarm_manager::<Libp2pService>(test_utils::make_address("/ip6/::1/tcp/"), config)
                .await;

        let addr = swarm2.handle.local_addr().clone();
        let id: PeerId = if let Some(Protocol::P2p(peer)) = addr.iter().last() {
            PeerId::from_multihash(peer).unwrap()
        } else {
            panic!("invalid multiaddr");
        };

        tokio::spawn(async move {
            log::debug!("staring libp2p service");
            loop {
                assert!(swarm2.handle.poll_next().await.is_ok());
            }
        });

        // "discover" the other libp2p service
        swarm.peer_discovered(&[net::types::AddrInfo {
            id,
            ip4: vec![],
            ip6: vec![Arc::new(addr)],
        }]);
        swarm.auto_connect().await.unwrap();
        assert_eq!(swarm.peers.len(), 1);
    }

    #[tokio::test]
    async fn connect_outbound_same_network() {
        let config = Arc::new(config::create_mainnet());
        let mut swarm1 = make_swarm_manager::<Libp2pService>(
            test_utils::make_address("/ip6/::1/tcp/"),
            config.clone(),
        )
        .await;
        let mut swarm2 =
            make_swarm_manager::<Libp2pService>(test_utils::make_address("/ip6/::1/tcp/"), config)
                .await;

        let (conn1_res, _conn2_res) = tokio::join!(
            swarm1.handle.connect(swarm2.handle.local_addr().clone()),
            swarm2.handle.poll_next()
        );

        assert!(std::matches!(
            swarm1.handle.poll_next().await,
            Ok(net::types::ConnectivityEvent::ConnectionAccepted { peer_info: _ })
        ));
    }

    #[tokio::test]
    async fn test_validate_supported_protocols() {
        let config = Arc::new(config::create_mainnet());
        let swarm =
            make_swarm_manager::<Libp2pService>(test_utils::make_address("/ip6/::1/tcp/"), config)
                .await;

        // all needed protocols
        assert!(swarm.validate_supported_protocols(&[
            "/meshsub/1.1.0".to_string(),
            "/meshsub/1.0.0".to_string(),
            "/ipfs/ping/1.0.0".to_string(),
            "/ipfs/id/1.0.0".to_string(),
            "/ipfs/id/push/1.0.0".to_string(),
            "/mintlayer/sync/0.1.0".to_string(),
        ]));

        // all needed protocols + 2 extra
        assert!(swarm.validate_supported_protocols(&[
            "/meshsub/1.1.0".to_string(),
            "/meshsub/1.0.0".to_string(),
            "/ipfs/ping/1.0.0".to_string(),
            "/ipfs/id/1.0.0".to_string(),
            "/ipfs/id/push/1.0.0".to_string(),
            "/mintlayer/sync/0.1.0".to_string(),
            "/mintlayer/extra/0.1.0".to_string(),
            "/mintlayer/extra-test/0.2.0".to_string(),
        ]));

        // all needed protocols but wrong version for sync
        assert!(!swarm.validate_supported_protocols(&[
            "/meshsub/1.1.0".to_string(),
            "/meshsub/1.0.0".to_string(),
            "/ipfs/ping/1.0.0".to_string(),
            "/ipfs/id/1.0.0".to_string(),
            "/ipfs/id/push/1.0.0".to_string(),
            "/mintlayer/sync/0.2.0".to_string(),
        ]));

        // ping protocol missing
        assert!(!swarm.validate_supported_protocols(&[
            "/meshsub/1.1.0".to_string(),
            "/meshsub/1.0.0".to_string(),
            "/ipfs/id/1.0.0".to_string(),
            "/ipfs/id/push/1.0.0".to_string(),
            "/mintlayer/sync/0.1.0".to_string(),
        ]));
    }

    #[tokio::test]
    async fn connect_outbound_different_network() {
        let config = Arc::new(config::create_mainnet());
        let mut swarm1 = make_swarm_manager::<Libp2pService>(
            test_utils::make_address("/ip6/::1/tcp/"),
            Arc::clone(&config),
        )
        .await;
        let mut swarm2 = make_swarm_manager::<Libp2pService>(
            test_utils::make_address("/ip6/::1/tcp/"),
            Arc::new(
                common::chain::config::TestChainConfig::new()
                    .with_magic_bytes([1, 2, 3, 4])
                    .build(),
            ),
        )
        .await;

        let addr = swarm2.handle.local_addr().clone();
        tokio::spawn(async move { swarm2.handle.poll_next().await.unwrap() });
        swarm1.handle.connect(addr).await.unwrap();

        if let Ok(net::types::ConnectivityEvent::ConnectionAccepted { peer_info }) =
            swarm1.handle.poll_next().await
        {
            assert_ne!(peer_info.magic_bytes, *config.magic_bytes());
        }
    }

    #[tokio::test]
    async fn connect_inbound_same_network() {
        let config = Arc::new(config::create_mainnet());
        let mut swarm1 = make_swarm_manager::<Libp2pService>(
            test_utils::make_address("/ip6/::1/tcp/"),
            config.clone(),
        )
        .await;
        let mut swarm2 =
            make_swarm_manager::<Libp2pService>(test_utils::make_address("/ip6/::1/tcp/"), config)
                .await;

        let (_conn1_res, conn2_res) = tokio::join!(
            swarm1.handle.connect(swarm2.handle.local_addr().clone()),
            swarm2.handle.poll_next()
        );
        let conn2_res: net::types::ConnectivityEvent<Libp2pService> = conn2_res.unwrap();
        if let net::types::ConnectivityEvent::IncomingConnection { peer_info, addr } = conn2_res {
            assert_eq!(
                swarm2.validate_inbound_connection(addr, peer_info).await,
                Ok(())
            );
        } else {
            panic!("invalid event received");
        }
    }

    #[tokio::test]
    async fn connect_inbound_different_network() {
        let mut swarm1 = make_swarm_manager::<Libp2pService>(
            test_utils::make_address("/ip6/::1/tcp/"),
            Arc::new(config::create_mainnet()),
        )
        .await;
        let mut swarm2 = make_swarm_manager::<Libp2pService>(
            test_utils::make_address("/ip6/::1/tcp/"),
            Arc::new(
                common::chain::config::TestChainConfig::new()
                    .with_magic_bytes([1, 2, 3, 4])
                    .build(),
            ),
        )
        .await;

        let (_conn1_res, conn2_res) = tokio::join!(
            swarm1.handle.connect(swarm2.handle.local_addr().clone()),
            swarm2.handle.poll_next()
        );
        let conn2_res: net::types::ConnectivityEvent<Libp2pService> = conn2_res.unwrap();

        if let net::types::ConnectivityEvent::IncomingConnection { peer_info, addr } = conn2_res {
            assert_eq!(
                swarm2.validate_inbound_connection(addr, peer_info).await,
                Err(P2pError::ProtocolError(ProtocolError::DifferentNetwork(
                    [1, 2, 3, 4],
                    *config::create_mainnet().magic_bytes(),
                )))
            );
        } else {
            panic!("invalid event received");
        }
    }

    #[tokio::test]
    async fn remote_closes_connection() {
        let mut swarm1 = make_swarm_manager::<Libp2pService>(
            test_utils::make_address("/ip6/::1/tcp/"),
            Arc::new(config::create_mainnet()),
        )
        .await;
        let mut swarm2 = make_swarm_manager::<Libp2pService>(
            test_utils::make_address("/ip6/::1/tcp/"),
            Arc::new(config::create_mainnet()),
        )
        .await;
        let (_conn1_res, conn2_res) = tokio::join!(
            swarm1.handle.connect(swarm2.handle.local_addr().clone()),
            swarm2.handle.poll_next()
        );
        let conn2_res: net::types::ConnectivityEvent<Libp2pService> = conn2_res.unwrap();
        assert!(std::matches!(
            conn2_res,
            net::types::ConnectivityEvent::IncomingConnection { .. }
        ));
        assert!(std::matches!(
            swarm1.handle.poll_next().await,
            Ok(net::types::ConnectivityEvent::ConnectionAccepted { .. })
        ));

        assert_eq!(
            swarm2.handle.disconnect(*swarm1.handle.peer_id()).await,
            Ok(())
        );
        assert!(std::matches!(
            swarm1.handle.poll_next().await,
            Ok(net::types::ConnectivityEvent::ConnectionClosed { .. })
        ));
    }
}
