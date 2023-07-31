use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use massa_channel::{receiver::MassaReceiver, sender::MassaSender, MassaChannel};
use massa_protocol_exports::{PeerId, ProtocolError};
use parking_lot::RwLock;
use peernet::{
    messages::{
        MessagesHandler as PeerNetMessagesHandler, MessagesSerializer as PeerNetMessagesSerializer,
    },
    peer::PeerConnectionType,
};

use crate::{
    handlers::{
        block_handler::BlockMessageSerializer,
        endorsement_handler::EndorsementMessageSerializer,
        operation_handler::OperationMessageSerializer,
        peer_handler::{
            models::{PeerInfo, PeerState, SharedPeerDB},
            PeerManagementMessageSerializer,
        },
    },
    messages::{Message, MessagesHandler, MessagesSerializer},
    wrap_network::{ActiveConnectionsTrait, NetworkController},
};

pub struct MockActiveConnections {
    pub connections: HashMap<PeerId, MassaSender<Message>>,
}

impl MockActiveConnections {
    pub fn new() -> Self {
        Self {
            connections: HashMap::new(),
        }
    }
}

type SharedMockActiveConnections = Arc<RwLock<MockActiveConnections>>;

impl ActiveConnectionsTrait for SharedMockActiveConnections {
    fn clone_box(&self) -> Box<dyn ActiveConnectionsTrait> {
        Box::new(self.clone())
    }

    fn get_nb_out_connections(&self) -> usize {
        //TODO: Place a coherent value
        0
    }

    fn get_nb_in_connections(&self) -> usize {
        //TODO: Place a coherent value
        0
    }

    fn get_peers_connected(
        &self,
    ) -> HashMap<PeerId, (std::net::SocketAddr, PeerConnectionType, Option<String>)> {
        self.read()
            .connections
            .keys()
            .map(|peer_id| {
                (
                    peer_id.clone(),
                    (
                        std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
                        PeerConnectionType::OUT,
                        None,
                    ),
                )
            })
            .collect()
    }

    fn get_peer_ids_connected(&self) -> std::collections::HashSet<PeerId> {
        self.read().connections.keys().cloned().collect()
    }

    fn send_to_peer(
        &self,
        peer_id: &PeerId,
        _message_serializer: &crate::messages::MessagesSerializer,
        message: Message,
        _high_priority: bool,
    ) -> Result<(), massa_protocol_exports::ProtocolError> {
        let _ = self
            .read()
            .connections
            .get(peer_id)
            .unwrap()
            .try_send(message);
        Ok(())
    }

    fn shutdown_connection(&mut self, peer_id: &PeerId) {
        self.write().connections.remove(peer_id);
    }

    fn get_peers_connections_bandwidth(&self) -> HashMap<String, (u64, u64)> {
        HashMap::new()
    }

    fn get_peer_ids_connection_queue(&self) -> HashSet<std::net::SocketAddr> {
        HashSet::new()
    }
}

pub struct MockNetworkController {
    connections: SharedMockActiveConnections,
    messages_handler: MessagesHandler,
    message_serializer: MessagesSerializer,
    peer_db: SharedPeerDB,
}

impl Clone for MockNetworkController {
    fn clone(&self) -> Self {
        Self {
            connections: self.connections.clone(),
            messages_handler: self.messages_handler.clone(),
            message_serializer: MessagesSerializer::new()
                .with_block_message_serializer(BlockMessageSerializer::new())
                .with_endorsement_message_serializer(EndorsementMessageSerializer::new())
                .with_operation_message_serializer(OperationMessageSerializer::new())
                .with_peer_management_message_serializer(PeerManagementMessageSerializer::new()),
            peer_db: self.peer_db.clone(),
        }
    }
}

impl MockNetworkController {
    pub fn new(messages_handler: MessagesHandler, peer_db: SharedPeerDB) -> Self {
        Self {
            connections: Arc::new(RwLock::new(MockActiveConnections::new())),
            messages_handler,
            message_serializer: MessagesSerializer::new()
                .with_block_message_serializer(BlockMessageSerializer::new())
                .with_endorsement_message_serializer(EndorsementMessageSerializer::new())
                .with_operation_message_serializer(OperationMessageSerializer::new())
                .with_peer_management_message_serializer(PeerManagementMessageSerializer::new()),
            peer_db,
        }
    }
}

impl MockNetworkController {
    pub fn create_fake_connection(&mut self, peer_id: PeerId) -> (PeerId, MassaReceiver<Message>) {
        let (sender, receiver) = MassaChannel::new("create_fake_connection".to_string(), None);

        // Don't fake connect if we are banned
        if let Some(peer_info) = self.peer_db.read().peers.get(&peer_id) {
            if peer_info.state == PeerState::Banned {
                return (peer_id, receiver);
            }
        }
        // Otherwise, add to active connections and to peer_db
        self.connections
            .write()
            .connections
            .insert(peer_id.clone(), sender);
        self.peer_db.write().peers.insert(
            peer_id.clone(),
            PeerInfo {
                last_announce: None,
                state: PeerState::Trusted,
            },
        );
        (peer_id, receiver)
    }

    pub fn remove_fake_connection(&mut self, peer_id: &PeerId) {
        self.connections.write().connections.remove(peer_id);
    }

    /// Simulate a peer that send a message to us
    pub fn send_from_peer(
        &mut self,
        peer_id: &PeerId,
        message: Message,
    ) -> Result<(), ProtocolError> {
        let peers_connected: HashSet<PeerId> = self
            .connections
            .read()
            .connections
            .keys()
            .cloned()
            .collect();
        if !peers_connected.contains(peer_id) {
            return Err(ProtocolError::GeneralProtocolError(
                "Peer not connected".to_string(),
            ));
        }
        let mut data = Vec::new();
        self.message_serializer
            .serialize(&message, &mut data)
            .map_err(|err| ProtocolError::GeneralProtocolError(err.to_string()))?;
        self.messages_handler
            .handle(&data, peer_id)
            .map_err(|err| ProtocolError::GeneralProtocolError(err.to_string()))?;
        Ok(())
    }

    pub fn get_connections(&self) -> SharedMockActiveConnections {
        self.connections.clone()
    }
}

impl NetworkController for MockNetworkController {
    fn start_listener(
        &mut self,
        _transport_type: peernet::transports::TransportType,
        _addr: std::net::SocketAddr,
    ) -> Result<(), massa_protocol_exports::ProtocolError> {
        Ok(())
    }

    fn stop_listener(
        &mut self,
        _transport_type: peernet::transports::TransportType,
        _addr: std::net::SocketAddr,
    ) -> Result<(), massa_protocol_exports::ProtocolError> {
        Ok(())
    }

    fn try_connect(
        &mut self,
        _addr: std::net::SocketAddr,
        _timeout: std::time::Duration,
    ) -> Result<(), massa_protocol_exports::ProtocolError> {
        Ok(())
    }

    fn get_active_connections(&self) -> Box<dyn crate::wrap_network::ActiveConnectionsTrait> {
        Box::new(self.connections.clone())
    }

    fn get_total_bytes_received(&self) -> u64 {
        0
    }

    fn get_total_bytes_sent(&self) -> u64 {
        0
    }
}
