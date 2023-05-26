use crossbeam::{
    channel::{Receiver, Sender},
    select,
};
use massa_consensus_exports::ConsensusController;
use massa_models::stats::NetworkStats;
use massa_pool_exports::PoolController;
use massa_protocol_exports::{PeerCategoryInfo, ProtocolConfig, ProtocolError};
use massa_storage::Storage;
use parking_lot::RwLock;
use peernet::{peer::PeerConnectionType, transports::OutConnectionConfig};
use peernet::{peer_id::PeerId, transports::TcpOutConnectionConfig};
use std::net::SocketAddr;
use std::{collections::HashMap, net::IpAddr};
use std::{num::NonZeroUsize, sync::Arc};
use std::{thread::JoinHandle, time::Duration};
use tracing::{info, warn};

use crate::{
    handlers::peer_handler::models::{InitialPeers, PeerState, SharedPeerDB},
    worker::ProtocolChannels,
};
use crate::{handlers::peer_handler::PeerManagementHandler, messages::MessagesHandler};
use crate::{
    handlers::{
        block_handler::{cache::BlockCache, BlockHandler},
        endorsement_handler::{cache::EndorsementCache, EndorsementHandler},
        operation_handler::{cache::OperationCache, OperationHandler},
        peer_handler::models::PeerMessageTuple,
    },
    wrap_network::NetworkController,
};

pub enum ConnectivityCommand {
    Stop,
    GetStats {
        #[allow(clippy::type_complexity)]
        responder: Sender<(
            NetworkStats,
            HashMap<PeerId, (SocketAddr, PeerConnectionType)>,
        )>,
    },
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn start_connectivity_thread(
    peer_id: PeerId,
    mut network_controller: Box<dyn NetworkController>,
    consensus_controller: Box<dyn ConsensusController>,
    pool_controller: Box<dyn PoolController>,
    channel_blocks: (Sender<PeerMessageTuple>, Receiver<PeerMessageTuple>),
    channel_endorsements: (Sender<PeerMessageTuple>, Receiver<PeerMessageTuple>),
    channel_operations: (Sender<PeerMessageTuple>, Receiver<PeerMessageTuple>),
    channel_peers: (Sender<PeerMessageTuple>, Receiver<PeerMessageTuple>),
    initial_peers: InitialPeers,
    peer_db: SharedPeerDB,
    storage: Storage,
    protocol_channels: ProtocolChannels,
    messages_handler: MessagesHandler,
    peer_categories: HashMap<String, (Vec<IpAddr>, PeerCategoryInfo)>,
    _default_category: PeerCategoryInfo,
    config: ProtocolConfig,
) -> Result<(Sender<ConnectivityCommand>, JoinHandle<()>), ProtocolError> {
    let handle = std::thread::Builder::new()
    .name("protocol-connectivity".to_string())
    .spawn({
        let sender_endorsements_propagation_ext = protocol_channels.endorsement_handler_propagation.0.clone();
        let sender_blocks_retrieval_ext = protocol_channels.block_handler_retrieval.0.clone();
        let sender_blocks_propagation_ext = protocol_channels.block_handler_propagation.0.clone();
        let sender_operations_propagation_ext = protocol_channels.operation_handler_propagation.0.clone();
        move || {
            for (addr, transport) in &config.listeners {
                network_controller
                    .start_listener(*transport, *addr)
                    .unwrap_or_else(|_| panic!(
                        "Failed to start listener {:?} of transport {:?} in protocol",
                        addr, transport
                    ));
            }

            // Little hack to be sure that listeners are started before trying to connect to peers
            std::thread::sleep(Duration::from_millis(100));

            // Create cache outside of the op handler because it could be used by other handlers
            let total_in_slots = config.peers_categories.values().map(|v| v.max_in_connections_post_handshake).sum::<usize>() + config.default_category_info.max_in_connections_post_handshake;
            let total_out_slots = config.peers_categories.values().map(| v| v.target_out_connections).sum::<usize>() + config.default_category_info.target_out_connections;
            let operation_cache = Arc::new(RwLock::new(OperationCache::new(
                NonZeroUsize::new(config.max_known_ops_size).unwrap(),
                NonZeroUsize::new(total_in_slots + total_out_slots).unwrap(),
            )));
            let endorsement_cache = Arc::new(RwLock::new(EndorsementCache::new(
                NonZeroUsize::new(config.max_known_endorsements_size).unwrap(),
                NonZeroUsize::new(total_in_slots + total_out_slots).unwrap(),
            )));

            let block_cache = Arc::new(RwLock::new(BlockCache::new(
                NonZeroUsize::new(config.max_known_blocks_size).unwrap(),
                NonZeroUsize::new(total_in_slots + total_out_slots).unwrap(),
            )));

            // Start handlers
            let mut peer_management_handler = PeerManagementHandler::new(
                initial_peers,
                peer_id,
                peer_db.clone(),
                channel_peers,
                protocol_channels.peer_management_handler,
                messages_handler,
                network_controller.get_active_connections(),
                peer_categories.iter().map(|(key, value)|(key.clone(), (value.0.clone(), value.1.target_out_connections))).collect(),
                config.default_category_info.target_out_connections,
                &config,
            );

            let mut operation_handler = OperationHandler::new(
                pool_controller.clone(),
                storage.clone_without_refs(),
                config.clone(),
                operation_cache.clone(),
                network_controller.get_active_connections(),
                channel_operations.1,
                protocol_channels.operation_handler_retrieval.0.clone(),
                protocol_channels.operation_handler_retrieval.1.clone(),
                sender_operations_propagation_ext.clone(),
                protocol_channels.operation_handler_propagation.1.clone(),
                peer_management_handler.sender.command_sender.clone(),
            );
            let mut endorsement_handler = EndorsementHandler::new(
                pool_controller.clone(),
                endorsement_cache.clone(),
                storage.clone_without_refs(),
                config.clone(),
                network_controller.get_active_connections(),
                channel_endorsements.1,
                protocol_channels.endorsement_handler_retrieval.0.clone(),
                protocol_channels.endorsement_handler_retrieval.1.clone(),
                sender_endorsements_propagation_ext,
                protocol_channels.endorsement_handler_propagation.1.clone(),
                peer_management_handler.sender.command_sender.clone(),
            );
            let mut block_handler = BlockHandler::new(
                network_controller.get_active_connections(),
                consensus_controller,
                pool_controller,
                channel_blocks.1,
                sender_blocks_retrieval_ext,
                protocol_channels.block_handler_retrieval.1.clone(),
                protocol_channels.block_handler_propagation.1.clone(),
                sender_blocks_propagation_ext,
                sender_operations_propagation_ext,
                peer_management_handler.sender.command_sender.clone(),
                config.clone(),
                endorsement_cache,
                operation_cache,
                block_cache,
                storage.clone_without_refs(),
            );

            //Try to connect to peers
            loop {
                select! {
                        recv(protocol_channels.connectivity_thread.1) -> msg => {
                            match msg {
                                Ok(ConnectivityCommand::Stop) => {
                                    println!("Stopping protocol");
                                    drop(network_controller);
                                    println!("Stoppeed network controller");
                                    operation_handler.stop();
                                    println!("Stopped operation handler");
                                    endorsement_handler.stop();
                                    println!("Stopped endorsement handler");
                                    block_handler.stop();
                                    println!("Stopped block handler");
                                    peer_management_handler.stop();
                                    println!("Stopped peer handler");
                                    break;
                                },
                                Ok(ConnectivityCommand::GetStats { responder }) => {
                                    let active_node_count = network_controller.get_active_connections().get_peer_ids_connected().len() as u64;
                                    let in_connection_count = network_controller.get_active_connections().get_nb_in_connections() as u64;
                                    let out_connection_count = network_controller.get_active_connections().get_nb_out_connections() as u64;
                                    let (banned_peer_count, known_peer_count) = {
                                        let peer_db_read = peer_db.read();
                                        (peer_db_read.get_banned_peer_count(), peer_db_read.peers.len() as u64)
                                    };
                                    let stats = NetworkStats {
                                        active_node_count,
                                        in_connection_count,
                                        out_connection_count,
                                        banned_peer_count,
                                        known_peer_count,
                                    };
                                    let peers: HashMap<PeerId, (SocketAddr, PeerConnectionType)> = network_controller.get_active_connections().get_peers_connected().into_iter().map(|(peer_id, peer)| {
                                        (peer_id, (peer.0, peer.1))
                                    }).collect();
                                    responder.send((stats, peers)).unwrap_or_else(|_| warn!("Failed to send stats to responder"));
                                }
                                Err(_) => {
                                    warn!("Channel to connectivity thread is closed. Stopping the protocol");
                                    break;
                                }
                            }
                        }
                    default(config.try_connection_timer.to_duration()) => {
                        let peers_connected = network_controller.get_active_connections().get_peers_connected();
                        let mut slots_per_category: Vec<(String, usize)> = peer_categories.iter().map(|(category, category_infos)| {
                            (category.clone(), category_infos.1.target_out_connections.saturating_sub(peers_connected.iter().filter(|(_, peer)| {
                                if peer.1 == PeerConnectionType::OUT && let Some(peer_category) = &peer.2 {
                                    category == peer_category
                                } else {
                                    false
                                }
                            }).count()))
                        }).collect();
                        let mut slot_default_category = config.default_category_info.target_out_connections.saturating_sub(peers_connected.iter().filter(|(_, peer)| {
                            peer.1 == PeerConnectionType::OUT && peer.2.is_none()
                        }).count());
                        let mut addresses_to_connect: Vec<SocketAddr> = Vec::new();
                        {
                            let peer_db_read = peer_db.read();
                            for (_, peer_id) in &peer_db_read.index_by_newest {
                                if peers_connected.contains_key(peer_id) {
                                    continue;
                                }
                                if let Some(peer_info) = peer_db_read.peers.get(peer_id).and_then(|peer| {
                                    if peer.state == PeerState::Trusted {
                                        Some(peer.clone())
                                    } else {
                                        None
                                    }
                                }) {
                                    if peer_info.last_announce.listeners.is_empty() {
                                        continue;
                                    }
                                    //TODO: Adapt for multiple listeners
                                    let (addr, _) = peer_info.last_announce.listeners.iter().next().unwrap();
                                    let canonical_ip = addr.ip().to_canonical();
                                    if cfg!(feature = "local_network") {
                                        let allowed = match canonical_ip {
                                            std::net::IpAddr::V4(ip) => ip.is_global() || ip.is_private(),
                                            std::net::IpAddr::V6(ip) => ip.is_global() || ip.is_unique_local()
                                        };
                                        if !allowed {
                                            continue;
                                        }
                                    } else {
                                        if !canonical_ip.is_global()  {
                                            continue;
                                        }
                                    }
                                    // Check if the peer is in a category and we didn't reached out target yet
                                    let mut category_found = None;
                                    for (name, (ips, _)) in &peer_categories {
                                        if ips.contains(&canonical_ip) {
                                            category_found = Some(name);
                                        }
                                    }

                                    if let Some(category) = category_found {
                                        for (name, category_infos) in &mut slots_per_category {
                                            if name == category && category_infos > &mut 0 {
                                                addresses_to_connect.push(*addr);
                                                *category_infos -= 1;
                                            }
                                        }
                                    } else if slot_default_category > 0 {
                                        addresses_to_connect.push(*addr);
                                        slot_default_category -= 1;
                                    }


                                    // IF all slots are filled, stop
                                    if slot_default_category == 0 && slots_per_category.iter().all(|(_, slots)| *slots == 0) {
                                        break;
                                    }
                                }
                            }
                        }
                        for addr in addresses_to_connect {
                            info!("Trying to connect to addr {}", addr);
                            // We only manage TCP for now
                            if let Err(err) = network_controller.try_connect(addr, config.timeout_connection.to_duration(), &OutConnectionConfig::Tcp(Box::new(TcpOutConnectionConfig::new(config.read_write_limit_bytes_per_second / 10, Duration::from_millis(100))))) {
                                warn!("Failed to connect to peer {:?}: {:?}", addr, err);
                            }
                        }
                    }
                }
            }
        }
    }).expect("OS failed to start connectivity thread");

    // Start controller
    Ok((protocol_channels.connectivity_thread.0, handle))
}
