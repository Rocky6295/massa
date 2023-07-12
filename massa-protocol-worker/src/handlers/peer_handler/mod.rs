use std::cmp::Reverse;
use std::net::IpAddr;
use std::{collections::HashMap, net::SocketAddr, thread::JoinHandle, time::Duration};

use crossbeam::channel::tick;
use crossbeam::select;
use massa_channel::MassaChannel;
use massa_channel::{receiver::MassaReceiver, sender::MassaSender};
use massa_hash::Hash;
use massa_models::config::SIGNATURE_DESER_SIZE;
use massa_models::version::{Version, VersionDeserializer, VersionSerializer};
use massa_protocol_exports::{
    BootstrapPeers, PeerId, PeerIdDeserializer, PeerIdSerializer, ProtocolConfig,
};
use massa_serialization::{DeserializeError, Deserializer, Serializer};
use massa_signature::Signature;
use peernet::context::Context as _;
use peernet::messages::MessagesSerializer as _;
use rand::{rngs::StdRng, RngCore, SeedableRng};

use peernet::{
    error::{PeerNetError, PeerNetResult},
    messages::MessagesHandler as PeerNetMessagesHandler,
    peer::InitConnectionHandler,
    transports::{endpoint::Endpoint, TransportType},
};
use tracing::log::{debug, error, info, warn};

use crate::context::Context;
use crate::handlers::peer_handler::models::PeerState;
use crate::messages::{Message, MessagesHandler, MessagesSerializer};
use crate::wrap_network::ActiveConnectionsTrait;

use self::models::PeerInfo;
use self::{
    models::{
        InitialPeers, PeerManagementChannel, PeerManagementCmd, PeerMessageTuple, SharedPeerDB,
    },
    tester::Tester,
};

use self::{
    announcement::{
        Announcement, AnnouncementDeserializer, AnnouncementDeserializerArgs,
        AnnouncementSerializer,
    },
    messages::{PeerManagementMessageDeserializer, PeerManagementMessageDeserializerArgs},
};

/// This file contains the definition of the peer management handler
/// This handler is here to check that announcements we receive are valid and
/// that all the endpoints we received are active.
mod announcement;
mod messages;
pub mod models;
mod tester;

pub(crate) use messages::{PeerManagementMessage, PeerManagementMessageSerializer};

pub struct PeerManagementHandler {
    pub peer_db: SharedPeerDB,
    pub thread_join: Option<JoinHandle<()>>,
    pub sender: PeerManagementChannel,
    testers: Vec<Tester>,
}

impl PeerManagementHandler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        initial_peers: InitialPeers,
        peer_id: PeerId,
        peer_db: SharedPeerDB,
        (sender_msg, receiver_msg): (
            MassaSender<PeerMessageTuple>,
            MassaReceiver<PeerMessageTuple>,
        ),
        (sender_cmd, receiver_cmd): (
            MassaSender<PeerManagementCmd>,
            MassaReceiver<PeerManagementCmd>,
        ),
        messages_handler: MessagesHandler,
        mut active_connections: Box<dyn ActiveConnectionsTrait>,
        target_out_connections: HashMap<String, (Vec<IpAddr>, usize)>,
        default_target_out_connections: usize,
        config: &ProtocolConfig,
    ) -> Self {
        let message_serializer = PeerManagementMessageSerializer::new();

        let ((test_sender, test_receiver), testers) = Tester::run(
            config,
            active_connections.clone(),
            peer_db.clone(),
            messages_handler.clone(),
            target_out_connections.clone(),
            default_target_out_connections,
        );

        let connect_sender = PeerManagementHandler::start_thread_listener(
            config,
            5,
            messages_handler,
            peer_db.clone(),
        );

        let thread_join = std::thread::Builder::new()
        .name("protocol-peer-handler".to_string())
        .spawn({
            let peer_db = peer_db.clone();
            let ticker = tick(Duration::from_secs(10));
            let config = config.clone();
            let message_serializer = MessagesSerializer::new()
                .with_peer_management_message_serializer(PeerManagementMessageSerializer::new());
            let message_deserializer =
                PeerManagementMessageDeserializer::new(PeerManagementMessageDeserializerArgs {
                    max_peers_per_announcement: config.max_size_peers_announcement,
                    max_listeners_per_peer: config.max_size_listeners_per_peer,
                });
                let announcement_deser = AnnouncementDeserializer::new(
                    AnnouncementDeserializerArgs {
                        max_listeners: config.max_size_listeners_per_peer,
                    },
                );

                let mut peer_try_connect = 0;
            move || {
                loop {
                    select! {
                        recv(ticker) -> _ => {
                            let peers_to_send = peer_db.read().get_rand_peers_to_send(100);
                            if peers_to_send.is_empty() {
                                continue;
                            }

                            let msg = PeerManagementMessage::ListPeers(peers_to_send);

                            for peer_id in &active_connections.get_peer_ids_connected() {
                                if let Err(e) = active_connections
                                    .send_to_peer(peer_id, &message_serializer, msg.clone().into(), false) {
                                    error!("error sending ListPeers message to peer: {:?}", e);
                               }
                            }
                        }
                        recv(receiver_cmd) -> cmd => {
                            receiver_cmd.update_metrics();
                            // internal command
                           match cmd {
                             Ok(PeerManagementCmd::Ban(peer_ids)) => {
                                // remove running handshake ?
                                for peer_id in peer_ids {
                                    active_connections.shutdown_connection(&peer_id);

                                    // update peer_db
                                    peer_db.write().ban_peer(&peer_id);
                                }
                            },
                             Ok(PeerManagementCmd::Unban(peer_ids)) => {
                                for peer_id in peer_ids {
                                    peer_db.write().unban_peer(&peer_id);
                                }
                            },
                             Ok(PeerManagementCmd::GetBootstrapPeers { responder }) => {
                                let mut peers = peer_db.read().get_rand_peers_to_send(100);
                                // Add myself
                                if let Some(routable_ip) = config.routable_ip {
                                    let listeners = config.listeners.iter().map(|(addr, ty)| {
                                        (SocketAddr::new(routable_ip, addr.port()), *ty)
                                    }).collect();
                                    peers.push((peer_id.clone(), listeners));
                                }
                                if let Err(err) = responder.try_send(BootstrapPeers(peers)) {
                                    warn!("error sending bootstrap peers: {:?}", err);
                                }
                             },
                             Ok(PeerManagementCmd::Stop) => {
                                while let Ok(_msg) = test_receiver.try_recv() {
                                    // nothing to do just clean the channel
                                }
                                return;
                             },
                            Err(e) => {
                                error!("error receiving command: {:?}", e);
                            }
                           }
                        },
                        recv(receiver_msg) -> msg => {
                            receiver_msg.update_metrics();
                            let (peer_id, message) = match msg {
                                Ok((peer_id, message)) => (peer_id, message),
                                Err(_) => {
                                    return;
                                }
                            };
                            // check if peer is banned
                            if let Some(peer) = peer_db.read().peers.get(&peer_id) {
                                if peer.state == PeerState::Banned {
                                    warn!("Banned peer sent us a message: {:?}", peer_id);
                                    continue;
                                }
                            }
                            let (rest, message) = match message_deserializer
                                .deserialize::<DeserializeError>(&message) {
                                Ok((rest, message)) => (rest, message),
                                Err(e) => {
                                    warn!("error when deserializing message: {:?}", e);
                                    continue;
                                }
                            };
                            if !rest.is_empty() {
                                warn!("message not fully deserialized");
                                continue;
                            }
                            match message {
                                PeerManagementMessage::NewPeerConnected((peer_id, listeners)) => {
                                    debug!("Received peer message: NewPeerConnected from {}", peer_id);
                                    // if let Some((addr, _)) = listeners.iter().next() {
                                    //     let deser = announcement_deser.clone();
                                    //     let handler = messages_handler.clone();
                                    //     let db =peer_db.clone();
                                    //     peer_try_connect += 1;
                                    //     dbg!(peer_try_connect);
                                    //     let address = *addr;
                                    //     std::thread::spawn(move || {
                                    //         let res = Tester::tcp_handshake(
                                    //             handler,
                                    //             db,
                                    //             deser,
                                    //             VersionDeserializer::new(),
                                    //             PeerIdDeserializer::new(),
                                    //             address,
                                    //             config.version,
                                    //         );
                                    //         dbg!(res);
                                    //     });
                                    
                               
                     
                                    // } else  {
                                    //     // if let Err(e) = test_sender.try_send((peer_id, listeners)) {
                                    //     //     debug!("error when sending msg to peer tester : {}", e);
                                    //     // }
                                    // }
                                        if let Err(e) = connect_sender.try_send((peer_id, listeners)) {
                                            debug!("error when sending msg to peer connect : {}", e);
                                        }


                                  
                                }
                                PeerManagementMessage::ListPeers(peers) => {
                                    debug!("Received peer message: List peers from {}", peer_id);
                                    for (peer_id, listeners) in peers.into_iter() {
                                        if let Err(e) = test_sender.try_send((peer_id, listeners)) {
                                            debug!("error when sending msg to peer tester : {}", e);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }).expect("OS failed to start peer management thread");

        for (peer_id, listeners) in &initial_peers {
            let mut message = Vec::new();
            message_serializer
                .serialize(
                    &PeerManagementMessage::NewPeerConnected((peer_id.clone(), listeners.clone())),
                    &mut message,
                )
                .unwrap();
            sender_msg.try_send((peer_id.clone(), message)).unwrap();
        }

        Self {
            peer_db,
            thread_join: Some(thread_join),
            sender: PeerManagementChannel {
                msg_sender: sender_msg,
                command_sender: sender_cmd,
            },
            testers,
        }
    }

    fn start_thread_listener(
        protocol_config: &ProtocolConfig,
        nb_thread: usize,
        messages_handler: MessagesHandler,
        peer_db: SharedPeerDB,
    ) -> MassaSender<(PeerId, HashMap<SocketAddr, TransportType>)> {
        let (connect_sender, connect_receiver) =
            MassaChannel::new::<(PeerId, HashMap<SocketAddr, TransportType>)>(
                "connect_listener".to_string(),
                Some(1000),
            );

        let announcement_deser = AnnouncementDeserializer::new(AnnouncementDeserializerArgs {
            max_listeners: protocol_config.max_size_listeners_per_peer,
        });


        for _ in 0..nb_thread {
            let connect_receiver = connect_receiver.clone();
            let messages_handler = messages_handler.clone();
            let peer_db = peer_db.clone();
            let announcement_deserializer = announcement_deser.clone();
            let version = protocol_config.version;
            std::thread::spawn(move || {
                while let Ok((_peer_id, listeners)) = connect_receiver.recv() {
                    if let Some((addr, ty)) = listeners.iter().next() {
                        let _ = Tester::tcp_handshake(
                            messages_handler.clone(),
                            peer_db.clone(),
                            announcement_deserializer.clone(),
                            VersionDeserializer::new(),
                            PeerIdDeserializer::new(),
                            *addr,
                            version,
                        );
                    }
                }
            });
        }

        connect_sender
    }

    pub fn stop(&mut self) {
        self.sender
            .command_sender
            .send(PeerManagementCmd::Stop)
            .unwrap();

        // waiting for all threads to finish
        self.testers.iter_mut().for_each(|tester| {
            if let Some(join_handle) = tester.handler.take() {
                join_handle.join().expect("Failed to join tester thread");
            }
        });
    }
}

#[derive(Clone)]
pub struct MassaHandshake {
    pub announcement_serializer: AnnouncementSerializer,
    pub announcement_deserializer: AnnouncementDeserializer,
    pub version_serializer: VersionSerializer,
    pub version_deserializer: VersionDeserializer,
    pub config: ProtocolConfig,
    pub peer_db: SharedPeerDB,
    peer_mngt_msg_serializer: MessagesSerializer,
    peer_id_serializer: PeerIdSerializer,
    peer_id_deserializer: PeerIdDeserializer,
    message_handlers: MessagesHandler,
}

impl MassaHandshake {
    pub fn new(
        peer_db: SharedPeerDB,
        config: ProtocolConfig,
        message_handlers: MessagesHandler,
    ) -> Self {
        Self {
            peer_db,
            announcement_serializer: AnnouncementSerializer::new(),
            announcement_deserializer: AnnouncementDeserializer::new(
                AnnouncementDeserializerArgs {
                    max_listeners: config.max_size_listeners_per_peer,
                },
            ),
            version_serializer: VersionSerializer::new(),
            version_deserializer: VersionDeserializer::new(),
            config,
            peer_id_serializer: PeerIdSerializer::new(),
            peer_id_deserializer: PeerIdDeserializer::new(),
            peer_mngt_msg_serializer: MessagesSerializer::new()
                .with_peer_management_message_serializer(PeerManagementMessageSerializer::new()),
            message_handlers,
        }
    }
}

impl InitConnectionHandler<PeerId, Context, MessagesHandler> for MassaHandshake {
    fn perform_handshake(
        &mut self,
        context: &Context,
        endpoint: &mut Endpoint,
        listeners: &HashMap<SocketAddr, TransportType>,
        messages_handler: MessagesHandler,
    ) -> PeerNetResult<PeerId> {
        let mut bytes = vec![];
        self.peer_id_serializer
            .serialize(&context.get_peer_id(), &mut bytes)
            .map_err(|err| {
                PeerNetError::HandshakeError.error(
                    "Massa Handshake",
                    Some(format!("Failed to serialize  peer_id: {}", err)),
                )
            })?;
        self.version_serializer
            .serialize(&self.config.version, &mut bytes)
            .map_err(|err| {
                PeerNetError::HandshakeError.error(
                    "Massa Handshake",
                    Some(format!("Failed to serialize version: {}", err)),
                )
            })?;
        bytes.push(0);
        let listeners_announcement = Announcement::new(
            listeners.clone(),
            self.config.routable_ip,
            &context.our_keypair,
        )
        .unwrap();
        self.announcement_serializer
            .serialize(&listeners_announcement, &mut bytes)
            .map_err(|err| {
                PeerNetError::HandshakeError.error(
                    "Massa Handshake",
                    Some(format!("Failed to serialize announcement: {}", err)),
                )
            })?;
        endpoint.send::<PeerId>(&bytes)?;
        let received = endpoint.receive::<PeerId>()?;
        if received.len() < 32 {
            return Err(PeerNetError::HandshakeError.error(
                "Massa Handshake",
                Some(format!("Received too short message len:{}", received.len())),
            ));
        }
        let (received, peer_id) = self
            .peer_id_deserializer
            .deserialize::<DeserializeError>(&received)
            .map_err(|err| {
                PeerNetError::HandshakeError.error(
                    "Massa Handshake",
                    Some(format!("Failed to deserialize peer id: {}", err)),
                )
            })?;
        {
            let peer_db_read = self.peer_db.read();
            if let Some(info) = peer_db_read.peers.get(&peer_id) {
                if info.state == PeerState::Banned {
                    debug!("Banned peer tried to connect: {:?}", peer_id);
                }
            }
        }

        let res = {
            {
                let mut peer_db_write = self.peer_db.write();
                peer_db_write
                    .peers
                    .entry(peer_id.clone())
                    .and_modify(|info| {
                        info.state = PeerState::InHandshake;
                    });
            }

            let (received, version) = self
                .version_deserializer
                .deserialize::<DeserializeError>(received)
                .map_err(|err| {
                    PeerNetError::HandshakeError.error(
                        "Massa Handshake",
                        Some(format!("Failed to deserialize version: {}", err)),
                    )
                })?;
            if !self.config.version.is_compatible(&version) {
                return Err(PeerNetError::HandshakeError.error(
                    "Massa Handshake",
                    Some(format!("Received version incompatible: {}", version)),
                ));
            }
            let id = received.first().ok_or(
                PeerNetError::HandshakeError
                    .error("Massa Handshake", Some("Failed to get id".to_string())),
            )?;
            match id {
                0 => {
                    let (_, announcement) = self
                        .announcement_deserializer
                        .deserialize::<DeserializeError>(
                            received.get(1..).ok_or(PeerNetError::HandshakeError.error(
                                "Massa Handshake",
                                Some("Failed to get data".to_string()),
                            ))?,
                        )
                        .map_err(|err| {
                            PeerNetError::HandshakeError.error(
                                "Massa Handshake",
                                Some(format!("Failed to deserialize announcement: {}", err)),
                            )
                        })?;
                    if peer_id
                        .verify_signature(&announcement.hash, &announcement.signature)
                        .is_err()
                    {
                        return Err(PeerNetError::HandshakeError
                            .error("Massa Handshake", Some("Invalid signature".to_string())));
                    }
                    let message = PeerManagementMessage::NewPeerConnected((
                        peer_id.clone(),
                        announcement.clone().listeners,
                    ));
                    let mut bytes = Vec::new();
                    let peer_management_message_serializer = MessagesSerializer::new()
                        .with_peer_management_message_serializer(
                            PeerManagementMessageSerializer::new(),
                        );
                    peer_management_message_serializer
                        .serialize(&Message::PeerManagement(Box::new(message)), &mut bytes)
                        .map_err(|err| {
                            PeerNetError::HandshakeError.error(
                                "Massa Handshake",
                                Some(format!("Failed to serialize announcement: {}", err)),
                            )
                        })?;
                    messages_handler.handle(&bytes, &peer_id)?;
                    let mut self_random_bytes = [0u8; 32];
                    StdRng::from_entropy().fill_bytes(&mut self_random_bytes);
                    let self_random_hash = Hash::compute_from(&self_random_bytes);
                    let mut bytes = [0u8; 32];
                    bytes[..32].copy_from_slice(&self_random_bytes);

                    endpoint.send::<PeerId>(&bytes)?;
                    let received = endpoint.receive::<PeerId>()?;
                    let other_random_bytes: &[u8; 32] =
                        received.as_slice().try_into().map_err(|_| {
                            PeerNetError::HandshakeError.error(
                                "Massa Handshake",
                                Some("Failed to deserialize random bytes".to_string()),
                            )
                        })?;

                    // sign their random bytes
                    let other_random_hash = Hash::compute_from(other_random_bytes);
                    let self_signature =
                        context.our_keypair.sign(&other_random_hash).map_err(|_| {
                            PeerNetError::HandshakeError.error(
                                "Massa Handshake",
                                Some("Failed to sign random bytes".to_string()),
                            )
                        })?;

                    let mut bytes = [0u8; SIGNATURE_DESER_SIZE];
                    bytes.copy_from_slice(&self_signature.to_bytes());

                    endpoint.send::<PeerId>(&bytes)?;
                    let received = endpoint.receive::<PeerId>()?;

                    let other_signature =
                        Signature::from_bytes(received.as_slice()).map_err(|_| {
                            PeerNetError::HandshakeError.error(
                                "Massa Handshake",
                                Some("Failed to sign 2 random bytes".to_string()),
                            )
                        })?;

                    // check their signature
                    peer_id
                        .verify_signature(&self_random_hash, &other_signature)
                        .map_err(|err| {
                            PeerNetError::HandshakeError
                                .error("Massa Handshake", Some(format!("Signature error {}", err)))
                        })?;
                    Ok((peer_id.clone(), Some(announcement)))
                }
                1 => {
                    self.message_handlers.handle(
                        received.get(1..).ok_or(
                            PeerNetError::HandshakeError
                                .error("Massa Handshake", Some("Failed to get data".to_string())),
                        )?,
                        &peer_id,
                    )?;
                    Ok((peer_id.clone(), None))
                }
                _ => Err(PeerNetError::HandshakeError
                    .error("Massa Handshake", Some("Invalid message id".to_string()))),
            }
        };
        {
            let mut peer_db_write = self.peer_db.write();
            // if handshake failed, we set the peer state to HandshakeFailed
            match &res {
                Ok((peer_id, Some(announcement))) => {
                    info!("Peer connected: {:?}", peer_id);
                    //TODO: Hacky organize better when multiple ip/listeners
                    if !announcement.listeners.is_empty() {
                        peer_db_write
                            .index_by_newest
                            .retain(|(_, peer_id_stored)| peer_id_stored != peer_id);
                        peer_db_write
                            .index_by_newest
                            .insert((Reverse(announcement.timestamp), peer_id.clone()));
                    }
                    peer_db_write
                        .peers
                        .entry(peer_id.clone())
                        .and_modify(|info| {
                            info.last_announce = announcement.clone();
                            info.state = PeerState::Trusted;
                        })
                        .or_insert(PeerInfo {
                            last_announce: announcement.clone(),
                            state: PeerState::Trusted,
                        });
                }
                Ok((_peer_id, None)) => {
                    peer_db_write.peers.entry(peer_id).and_modify(|info| {
                        //TODO: Add the peerdb but for now impossible as we don't have announcement and we need one to place in peerdb
                        info.state = PeerState::HandshakeFailed;
                    });
                    return Err(PeerNetError::HandshakeError.error(
                        "Massa Handshake",
                        Some("Distant peer don't have slot for us.".to_string()),
                    ));
                }
                Err(_) => {
                    peer_db_write.peers.entry(peer_id).and_modify(|info| {
                        //TODO: Add the peerdb but for now impossible as we don't have announcement and we need one to place in peerdb
                        info.state = PeerState::HandshakeFailed;
                    });
                }
            }
        }

        // Send 100 peers to the other peer
        let peers_to_send = {
            let peer_db_read = self.peer_db.read();
            peer_db_read.get_rand_peers_to_send(100)
        };
        let mut buf = Vec::new();
        let msg = PeerManagementMessage::ListPeers(peers_to_send).into();

        self.peer_mngt_msg_serializer.serialize(&msg, &mut buf)?;
        endpoint.send::<PeerId>(buf.as_slice())?;

        res.map(|(id, _)| id)
    }

    fn fallback_function(
        &mut self,
        context: &Context,
        endpoint: &mut Endpoint,
        _listeners: &HashMap<SocketAddr, TransportType>,
    ) -> PeerNetResult<()> {
        //TODO: Fix this clone
        let context = context.clone();
        let mut endpoint = endpoint.try_clone()?;
        let db = self.peer_db.clone();
        let serializer = self.peer_mngt_msg_serializer.clone();
        let version_serializer = self.version_serializer.clone();
        let peer_id_serializer = self.peer_id_serializer.clone();
        let version = self.config.version;
        std::thread::spawn(move || {
            let peers_to_send = db.read().get_rand_peers_to_send(100);
            let mut buf = vec![];
            if let Err(err) = peer_id_serializer.serialize(&context.get_peer_id(), &mut buf) {
                warn!("{}", err.to_string());
                return;
            }
            if let Err(err) = version_serializer
                .serialize(&version, &mut buf)
                .map_err(|err| {
                    PeerNetError::HandshakeError.error(
                        "Massa Handshake",
                        Some(format!(
                            "Failed serialize version, Err: {:?}",
                            err.to_string()
                        )),
                    )
                })
            {
                warn!("{}", err.to_string());
                return;
            }
            buf.push(1);
            let msg = PeerManagementMessage::ListPeers(peers_to_send).into();
            if let Err(err) = serializer.serialize(&msg, &mut buf) {
                warn!("Failed to serialize message: {}", err);
                return;
            }
            if let Err(err) =
                endpoint.send_timeout::<PeerId>(buf.as_slice(), Duration::from_millis(200))
            {
                warn!("Failed to send message: {}", err);
                return;
            }
            endpoint.shutdown();
        });
        Ok(())
    }
}
