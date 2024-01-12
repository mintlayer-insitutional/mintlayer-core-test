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

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::Duration,
};

use logging::log;
use tokio::sync::mpsc::{error::TryRecvError, UnboundedReceiver, UnboundedSender};

use common::{
    chain::{self, config, ChainConfig},
    primitives::user_agent::mintlayer_core_user_agent,
};
use p2p_test_utils::{expect_future_val, expect_no_recv, P2pBasicTestTimeGetter};
use p2p_types::socket_address::SocketAddress;
use test_utils::assert_matches;

use crate::{
    config::{NodeType, P2pConfig},
    message::{AddrListRequest, AnnounceAddrRequest, PeerManagerMessage},
    net::{
        default_backend::{
            transport::{MpscChannelTransport, TcpTransportSocket},
            types::{CategorizedMessage, Command, Message},
            ConnectivityHandle, DefaultNetworkingService,
        },
        types::{ConnectivityEvent, PeerInfo, Role},
        ConnectivityService, NetworkingService,
    },
    peer_manager::{
        self,
        tests::{
            make_peer_manager_custom,
            utils::{cmd_to_peer_man_msg, make_peer_info},
        },
        OutboundConnectType, PeerManager, DNS_SEED_QUERY_INTERVAL,
    },
    testing_utils::{
        peerdb_inmemory_store, test_p2p_config, TestAddressMaker, TestTransportChannel,
        TestTransportMaker, TestTransportTcp, TEST_PROTOCOL_VERSION,
    },
    tests::helpers::TestDnsSeed,
    types::peer_id::PeerId,
    utils::oneshot_nofail,
    PeerManagerEvent,
};

async fn test_address_rate_limiter<A, T>()
where
    A: TestTransportMaker<Transport = T::Transport>,
    T: NetworkingService + 'static + std::fmt::Debug,
    T::ConnectivityHandle: ConnectivityService<T>,
{
    let bind_address = A::make_address();
    let config = Arc::new(config::create_mainnet());
    let p2p_config = Arc::new(test_p2p_config());
    let time_getter = P2pBasicTestTimeGetter::new();
    let (mut pm, _peer_mgr_event_sender, _shutdown_sender, _subscribers_sender) =
        make_peer_manager_custom::<T>(
            A::make_transport(),
            bind_address,
            Arc::clone(&config),
            p2p_config,
            time_getter.get_time_getter(),
        )
        .await;

    let address = TestAddressMaker::new_random_address();
    let peer_id = PeerId::new();
    let peer_info = PeerInfo {
        peer_id,
        protocol_version: TEST_PROTOCOL_VERSION,
        network: *config.magic_bytes(),
        software_version: *config.software_version(),
        user_agent: mintlayer_core_user_agent(),
        common_services: NodeType::Full.into(),
    };
    pm.accept_connection(address, bind_address, Role::Inbound, peer_info, None);
    assert_eq!(pm.peers.len(), 1);

    let get_new_public_address = || loop {
        let address = TestAddressMaker::new_random_address().as_peer_address();
        if SocketAddress::from_peer_address(&address, false).is_some() {
            return address;
        }
    };

    // Check that nodes are allowed to send own address immediately after connecting
    let address = get_new_public_address();
    pm.handle_announce_addr_request(peer_id, address);
    let accepted_count = pm.peerdb.known_addresses().count();
    assert_eq!(accepted_count, 1);

    for _ in 0..120 {
        time_getter.advance_time(Duration::from_secs(1));
        for _ in 0..100 {
            pm.handle_announce_addr_request(
                peer_id,
                TestAddressMaker::new_random_address().as_peer_address(),
            );
        }
    }
    let accepted_count = pm.peerdb.known_addresses().count();
    // The average expected count is 13 (1 + 120 * 0.1), but the exact number is not very important
    assert!(
        (5..25).contains(&accepted_count),
        "Unexpected accepted address count: {accepted_count}"
    );
}

// Test only TestTransportChannel because actual networking is not used
#[tracing::instrument]
#[tokio::test]
async fn test_address_rate_limiter_channels() {
    test_address_rate_limiter::<
        TestTransportChannel,
        DefaultNetworkingService<MpscChannelTransport>,
    >()
    .await;
}

#[tracing::instrument]
#[test]
fn test_addr_list_handling_inbound() {
    type TestNetworkingService = DefaultNetworkingService<TcpTransportSocket>;

    let bind_address = TestTransportTcp::make_address();
    let chain_config = Arc::new(config::create_mainnet());
    let p2p_config = Arc::new(test_p2p_config());
    let (cmd_sender, mut cmd_receiver) = tokio::sync::mpsc::unbounded_channel();
    let (_conn_event_sender, conn_event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let (_peer_mgr_event_sender, peer_mgr_event_receiver) =
        tokio::sync::mpsc::unbounded_channel::<PeerManagerEvent>();
    let time_getter = P2pBasicTestTimeGetter::new();
    let connectivity_handle =
        ConnectivityHandle::<TestNetworkingService>::new(vec![], cmd_sender, conn_event_receiver);

    let mut pm = PeerManager::<TestNetworkingService, _>::new(
        Arc::clone(&chain_config),
        Arc::clone(&p2p_config),
        connectivity_handle,
        peer_mgr_event_receiver,
        time_getter.get_time_getter(),
        peerdb_inmemory_store(),
    )
    .unwrap();

    let peer_id_1 = PeerId::new();
    let peer_info = PeerInfo {
        peer_id: peer_id_1,
        protocol_version: TEST_PROTOCOL_VERSION,
        network: *chain_config.magic_bytes(),
        software_version: *chain_config.software_version(),
        user_agent: mintlayer_core_user_agent(),
        common_services: NodeType::Full.into(),
    };
    pm.accept_connection(
        TestAddressMaker::new_random_address(),
        bind_address,
        Role::Inbound,
        peer_info,
        None,
    );
    assert_eq!(pm.peers.len(), 1);

    // Peer is accepted by the peer manager
    match cmd_receiver.try_recv() {
        Ok(Command::Accept { peer_id }) if peer_id == peer_id_1 => {}
        v => panic!("unexpected result: {v:?}"),
    }

    // No more messages
    match cmd_receiver.try_recv() {
        Err(_) => {}
        v => panic!("unexpected result: {v:?}"),
    }

    // Peer manager sends response normally to first address list request
    pm.handle_addr_list_request(peer_id_1);
    let cmd = cmd_receiver.try_recv().unwrap();
    let (peer_id, peer_msg) = cmd_to_peer_man_msg(cmd);
    assert_eq!(peer_id, peer_id_1);
    assert_matches!(peer_msg, PeerManagerMessage::AddrListResponse(_));

    // No more messages
    match cmd_receiver.try_recv() {
        Err(_) => {}
        v => panic!("unexpected result: {v:?}"),
    }

    // Other requests are ignored but the peer is not scored
    pm.handle_addr_list_request(peer_id_1);
    // No more messages
    match cmd_receiver.try_recv() {
        Err(_) => {}
        v => panic!("unexpected result: {v:?}"),
    }
    assert_eq!(pm.peers.get(&peer_id_1).unwrap().score, 0);

    // Check that the peer is scored if it tries to send an unexpected address list response
    pm.handle_addr_list_response(
        peer_id_1,
        vec![TestAddressMaker::new_random_address().as_peer_address()],
    );
    assert_ne!(pm.peers.get(&peer_id_1).unwrap().score, 0);
}

#[tracing::instrument]
#[test]
fn test_addr_list_handling_outbound() {
    type TestNetworkingService = DefaultNetworkingService<TcpTransportSocket>;

    let bind_address = TestTransportTcp::make_address();
    let chain_config = Arc::new(config::create_mainnet());
    let p2p_config = Arc::new(test_p2p_config());
    let (cmd_sender, mut cmd_receiver) = tokio::sync::mpsc::unbounded_channel();
    let (_conn_event_sender, conn_event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let (_peer_mgr_event_sender, peer_mgr_event_receiver) =
        tokio::sync::mpsc::unbounded_channel::<PeerManagerEvent>();
    let time_getter = P2pBasicTestTimeGetter::new();
    let connectivity_handle =
        ConnectivityHandle::<TestNetworkingService>::new(vec![], cmd_sender, conn_event_receiver);

    let mut pm = PeerManager::<TestNetworkingService, _>::new(
        Arc::clone(&chain_config),
        Arc::clone(&p2p_config),
        connectivity_handle,
        peer_mgr_event_receiver,
        time_getter.get_time_getter(),
        peerdb_inmemory_store(),
    )
    .unwrap();

    let peer_id_1 = PeerId::new();
    let peer_address = TestAddressMaker::new_random_address();
    let peer_info = PeerInfo {
        peer_id: peer_id_1,
        protocol_version: TEST_PROTOCOL_VERSION,
        network: *chain_config.magic_bytes(),
        software_version: *chain_config.software_version(),
        user_agent: mintlayer_core_user_agent(),
        common_services: NodeType::Full.into(),
    };
    pm.connect(
        peer_address,
        OutboundConnectType::Automatic {
            block_relay_only: false,
        },
    );

    // New peer connection is requested
    match cmd_receiver.try_recv() {
        Ok(Command::Connect {
            address,
            local_services_override: _,
        }) if address == peer_address => {}
        v => panic!("unexpected result: {v:?}"),
    }

    pm.accept_connection(peer_address, bind_address, Role::Outbound, peer_info, None);
    assert_eq!(pm.peers.len(), 1);

    // Peer is accepted by the peer manager
    match cmd_receiver.try_recv() {
        Ok(Command::Accept { peer_id }) if peer_id == peer_id_1 => {}
        v => panic!("unexpected result: {v:?}"),
    }

    // Address list is requested from the connected peer
    let cmd = cmd_receiver.try_recv().unwrap();
    let (peer_id, peer_msg) = cmd_to_peer_man_msg(cmd);
    assert_eq!(peer_id, peer_id_1);
    assert_matches!(peer_msg, PeerManagerMessage::AddrListRequest(_));

    // No more messages
    match cmd_receiver.try_recv() {
        Err(_) => {}
        v => panic!("unexpected result: {v:?}"),
    }

    // Check that the address list response is processed normally and that the peer is not scored
    pm.handle_addr_list_response(
        peer_id_1,
        vec![TestAddressMaker::new_random_address().as_peer_address()],
    );
    assert_eq!(pm.peers.get(&peer_id_1).unwrap().score, 0);

    // No more messages
    match cmd_receiver.try_recv() {
        Err(_) => {}
        v => panic!("unexpected result: {v:?}"),
    }

    // Check that the peer is scored if it tries to send an unexpected address list response
    pm.handle_addr_list_response(
        peer_id_1,
        vec![TestAddressMaker::new_random_address().as_peer_address()],
    );
    assert_ne!(pm.peers.get(&peer_id_1).unwrap().score, 0);
}

// Verify that the node periodically resends its own address
#[tracing::instrument]
#[tokio::test]
async fn resend_own_addresses() {
    type TestNetworkingService = DefaultNetworkingService<TcpTransportSocket>;

    // The addresses on which the node is listening
    let listening_addresses: Vec<SocketAddress> =
        vec!["1.2.3.4:3031".parse().unwrap(), "[2001:bc8:1600::1]:3031".parse().unwrap()];

    // Outbound connections normally use random ports
    let outbound_addresses: Vec<SocketAddress> =
        vec!["1.2.3.4:12345".parse().unwrap(), "[2001:bc8:1600::1]:23456".parse().unwrap()];

    let chain_config = Arc::new(config::create_mainnet());
    let p2p_config = Arc::new(test_p2p_config());
    let (cmd_sender, mut cmd_receiver) = tokio::sync::mpsc::unbounded_channel();
    let (_conn_event_sender, conn_event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let (_peer_mgr_event_sender, peer_mgr_event_receiver) =
        tokio::sync::mpsc::unbounded_channel::<PeerManagerEvent>();
    let time_getter = P2pBasicTestTimeGetter::new();
    let connectivity_handle = ConnectivityHandle::<TestNetworkingService>::new(
        listening_addresses.clone(),
        cmd_sender,
        conn_event_receiver,
    );

    let mut pm = PeerManager::<TestNetworkingService, _>::new(
        Arc::clone(&chain_config),
        Arc::clone(&p2p_config),
        connectivity_handle,
        peer_mgr_event_receiver,
        time_getter.get_time_getter(),
        peerdb_inmemory_store(),
    )
    .unwrap();

    let peer_count = p2p_config.peer_manager_config.outbound_full_and_block_relay_count();
    for peer_index in 0..peer_count {
        let new_peer_id = PeerId::new();
        let peer_address = TestAddressMaker::new_random_address();
        let peer_info = PeerInfo {
            peer_id: new_peer_id,
            protocol_version: TEST_PROTOCOL_VERSION,
            network: *chain_config.magic_bytes(),
            software_version: *chain_config.software_version(),
            user_agent: mintlayer_core_user_agent(),
            common_services: NodeType::Full.into(),
        };
        pm.connect(peer_address, OutboundConnectType::Reserved);

        // New peer connection is requested
        while !matches!(
            cmd_receiver.try_recv().unwrap(),
            Command::Connect {
                address: _,
                local_services_override: _
            }
        ) {}

        let addr_idx = peer_index % 2;

        pm.accept_connection(
            peer_address,
            listening_addresses[addr_idx],
            Role::Outbound,
            peer_info,
            Some(outbound_addresses[addr_idx].as_peer_address()),
        );
    }
    assert_eq!(pm.peers.len(), peer_count);

    let (started_sender, started_receiver) = oneshot_nofail::channel();
    logging::spawn_in_current_span(async move { pm.run_internal(Some(started_sender)).await });
    started_receiver.await.unwrap();

    // Flush all pending messages
    while cmd_receiver.try_recv().is_ok() {}

    // Advance the current time by 5 days (resends are not deterministic, but this should be more than enough)
    time_getter.advance_time(Duration::from_secs(5 * 24 * 60 * 60));

    // PeerManager should resend own addresses
    let mut listening_addresses = listening_addresses.into_iter().collect::<BTreeSet<_>>();
    while !listening_addresses.is_empty() {
        let cmd = tokio::time::timeout(Duration::from_secs(60), cmd_receiver.recv())
            .await
            .unwrap()
            .unwrap();

        if let Command::SendMessage {
            peer_id: _,
            message,
        } = cmd
        {
            if let CategorizedMessage::PeerManagerMessage(
                PeerManagerMessage::AnnounceAddrRequest(AnnounceAddrRequest { address }),
            ) = message.categorize()
            {
                let announced_addr = SocketAddress::from_peer_address(&address, false).unwrap();
                listening_addresses.remove(&announced_addr);
            }
        }
    }
}

// Configure the peer manager with an empty dns seed and a predefined peer address.
// Check that it attempts to connect to the predefined address.
#[tracing::instrument]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_to_predefined_address_if_dns_seed_is_empty() {
    type TestNetworkingService = DefaultNetworkingService<TcpTransportSocket>;

    let predefined_peer_address = TestAddressMaker::new_random_address();

    let chain_config = Arc::new(
        chain::config::create_unit_test_config_builder()
            .predefined_peer_addresses(vec![predefined_peer_address.socket_addr()])
            .build(),
    );

    let p2p_config = Arc::new(test_p2p_config());
    let (cmd_sender, mut cmd_receiver) = tokio::sync::mpsc::unbounded_channel();
    let (conn_event_sender, conn_event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let (peer_mgr_event_sender, peer_mgr_event_receiver) =
        tokio::sync::mpsc::unbounded_channel::<PeerManagerEvent>();
    let time_getter = P2pBasicTestTimeGetter::new();
    let connectivity_handle =
        ConnectivityHandle::<TestNetworkingService>::new(vec![], cmd_sender, conn_event_receiver);

    let peer_mgr = PeerManager::<TestNetworkingService, _>::new_generic(
        Arc::clone(&chain_config),
        Arc::clone(&p2p_config),
        connectivity_handle,
        peer_mgr_event_receiver,
        time_getter.get_time_getter(),
        peerdb_inmemory_store(),
        None,
        Box::new(TestDnsSeed::new(Arc::new(Mutex::new(Vec::new())))),
    )
    .unwrap();

    let peer_mgr_join_handle = logging::spawn_in_current_span(async move {
        let mut peer_mgr = peer_mgr;
        let _ = peer_mgr.run_internal(None).await;
        peer_mgr
    });

    // Connection to predefined_peer_address is requested
    let cmd =
        expect_future_val!(recv_command_advance_time(&mut cmd_receiver, &time_getter)).unwrap();
    assert_matches!(
        cmd,
        Command::Connect {
            address,
            local_services_override: _,
        } if address == predefined_peer_address
    );

    expect_no_recv!(cmd_receiver);

    drop(conn_event_sender);
    drop(peer_mgr_event_sender);

    let peer_mgr = peer_mgr_join_handle.await.unwrap();
    let addresses: BTreeSet<_> = peer_mgr.peerdb().known_addresses().cloned().collect();
    assert!(addresses.get(&predefined_peer_address).is_some());
}

// Configure the peer manager with a non-empty dns seed and a predefined peer address.
// Check that it attempts to connect to the seeded address, but not to the predefined one.
#[tracing::instrument]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dont_connect_to_predefined_address_if_dns_seed_is_non_empty() {
    type TestNetworkingService = DefaultNetworkingService<TcpTransportSocket>;

    let seeded_peer_address = TestAddressMaker::new_random_address();
    let predefined_peer_address = TestAddressMaker::new_random_address();

    let chain_config = Arc::new(
        chain::config::create_unit_test_config_builder()
            .predefined_peer_addresses(vec![predefined_peer_address.socket_addr()])
            .build(),
    );

    let p2p_config = Arc::new(test_p2p_config());
    let (cmd_sender, mut cmd_receiver) = tokio::sync::mpsc::unbounded_channel();
    let (conn_event_sender, conn_event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let (peer_mgr_event_sender, peer_mgr_event_receiver) =
        tokio::sync::mpsc::unbounded_channel::<PeerManagerEvent>();
    let time_getter = P2pBasicTestTimeGetter::new();
    let connectivity_handle =
        ConnectivityHandle::<TestNetworkingService>::new(vec![], cmd_sender, conn_event_receiver);

    let peer_mgr = PeerManager::<TestNetworkingService, _>::new_generic(
        Arc::clone(&chain_config),
        Arc::clone(&p2p_config),
        connectivity_handle,
        peer_mgr_event_receiver,
        time_getter.get_time_getter(),
        peerdb_inmemory_store(),
        None,
        Box::new(TestDnsSeed::new(Arc::new(Mutex::new(vec![
            seeded_peer_address,
        ])))),
    )
    .unwrap();

    let peer_mgr_join_handle = logging::spawn_in_current_span(async move {
        let mut peer_mgr = peer_mgr;
        let _ = peer_mgr.run_internal(None).await;
        peer_mgr
    });

    // Connection to seeded_peer_address is requested
    let cmd =
        expect_future_val!(recv_command_advance_time(&mut cmd_receiver, &time_getter)).unwrap();
    assert_matches!(
        cmd,
        Command::Connect {
            address,
            local_services_override: _,
        } if address == seeded_peer_address
    );

    expect_no_recv!(cmd_receiver);

    drop(conn_event_sender);
    drop(peer_mgr_event_sender);

    let peer_mgr = peer_mgr_join_handle.await.unwrap();
    let addresses: BTreeSet<_> = peer_mgr.peerdb().known_addresses().cloned().collect();
    // seeded_peer_address is in the db, but predefined_peer_address is not.
    assert!(addresses.get(&seeded_peer_address).is_some());
    assert!(addresses.get(&predefined_peer_address).is_none());
}

// 1) Configure the peer manager with a non-empty dns seed and non-empty peerdb.
// 2) Check that it attempts to connect to the address in the peerdb, and not to the seeded one;
// make the connection succeed.
// 3) Advance the time, so that the peer manager might consider querying the dns seed.
// The dns seed should not be queried.
#[tracing::instrument]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dont_use_dns_seed_if_connections_exist() {
    type TestNetworkingService = DefaultNetworkingService<TcpTransportSocket>;

    let local_bind_address = TestAddressMaker::new_random_address();
    let existing_address = TestAddressMaker::new_random_address();
    let seeded_peer_address = TestAddressMaker::new_random_address();

    let chain_config = Arc::new(chain::config::create_unit_test_config());

    let p2p_config = Arc::new(P2pConfig {
        ping_check_period: Duration::ZERO.into(),

        bind_addresses: Default::default(),
        socks5_proxy: Default::default(),
        disable_noise: Default::default(),
        boot_nodes: Default::default(),
        reserved_nodes: Default::default(),
        whitelisted_addresses: Default::default(),
        ban_threshold: Default::default(),
        ban_duration: Default::default(),
        outbound_connection_timeout: Default::default(),
        ping_timeout: Default::default(),
        peer_handshake_timeout: Default::default(),
        max_clock_diff: Default::default(),
        node_type: Default::default(),
        allow_discover_private_ips: Default::default(),
        user_agent: mintlayer_core_user_agent(),
        sync_stalling_timeout: Default::default(),
        peer_manager_config: Default::default(),
        protocol_config: Default::default(),
    });
    let (cmd_sender, mut cmd_receiver) = tokio::sync::mpsc::unbounded_channel();
    let (conn_event_sender, conn_event_receiver) = tokio::sync::mpsc::unbounded_channel();
    let (peer_mgr_event_sender, peer_mgr_event_receiver) =
        tokio::sync::mpsc::unbounded_channel::<PeerManagerEvent>();
    let time_getter = P2pBasicTestTimeGetter::new();
    let connectivity_handle =
        ConnectivityHandle::<TestNetworkingService>::new(vec![], cmd_sender, conn_event_receiver);

    let mut peer_mgr = PeerManager::<TestNetworkingService, _>::new_generic(
        Arc::clone(&chain_config),
        Arc::clone(&p2p_config),
        connectivity_handle,
        peer_mgr_event_receiver,
        time_getter.get_time_getter(),
        peerdb_inmemory_store(),
        None,
        Box::new(TestDnsSeed::new(Arc::new(Mutex::new(vec![
            seeded_peer_address,
        ])))),
    )
    .unwrap();

    peer_mgr.peerdb.peer_discovered(existing_address);

    let peer_mgr_join_handle = logging::spawn_in_current_span(async move {
        let mut peer_mgr = peer_mgr;
        let _ = peer_mgr.run_internal(None).await;
        peer_mgr
    });

    // Connection to existing_address is requested
    let cmd =
        expect_future_val!(recv_command_advance_time(&mut cmd_receiver, &time_getter)).unwrap();
    assert_matches!(
        cmd,
        Command::Connect {
            address,
            local_services_override: _,
        } if address == existing_address
    );

    let _ = accept_outbound_connection(
        &conn_event_sender,
        &mut cmd_receiver,
        &existing_address,
        &local_bind_address,
        &chain_config,
    )
    .await;

    time_getter.advance_time(DNS_SEED_QUERY_INTERVAL * 2);

    expect_no_recv!(cmd_receiver);

    drop(conn_event_sender);
    drop(peer_mgr_event_sender);

    let peer_mgr = peer_mgr_join_handle.await.unwrap();
    let addresses: BTreeSet<_> = peer_mgr.peerdb().known_addresses().cloned().collect();
    // existing_address is in the db, but seeded_peer_address is not.
    assert!(addresses.get(&existing_address).is_some());
    assert!(addresses.get(&seeded_peer_address).is_none());
}

async fn recv_command_advance_time(
    cmd_receiver: &mut UnboundedReceiver<Command>,
    time_getter: &P2pBasicTestTimeGetter,
) -> Result<Command, TryRecvError> {
    super::utils::recv_command_advance_time(
        cmd_receiver,
        time_getter,
        peer_manager::HEARTBEAT_INTERVAL_MAX,
    )
    .await
}

async fn accept_outbound_connection(
    conn_event_sender: &UnboundedSender<ConnectivityEvent>,
    cmd_receiver: &mut UnboundedReceiver<Command>,
    peer_address: &SocketAddress,
    local_bind_address: &SocketAddress,
    chain_config: &ChainConfig,
) -> PeerId {
    let peer_id = PeerId::new();
    conn_event_sender
        .send(ConnectivityEvent::OutboundAccepted {
            peer_address: *peer_address,
            bind_address: *local_bind_address,
            peer_info: make_peer_info(peer_id, chain_config),
            node_address_as_seen_by_peer: None,
        })
        .unwrap();

    log::debug!("Expecting Command::Accept");
    let cmd = cmd_receiver.recv().await.unwrap();
    assert_eq!(cmd, Command::Accept { peer_id });

    log::debug!("Expecting AddrListRequest");
    let cmd = cmd_receiver.recv().await.unwrap();
    assert_eq!(
        cmd,
        Command::SendMessage {
            peer_id,
            message: Message::AddrListRequest(AddrListRequest {})
        }
    );

    peer_id
}
