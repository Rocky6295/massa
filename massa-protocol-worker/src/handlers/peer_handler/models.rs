use crossbeam::channel::Sender;
use massa_protocol_exports::{BootstrapPeers, ProtocolError};
use massa_time::MassaTime;
use parking_lot::RwLock;
use peernet::{peer_id::PeerId, transports::TransportType};
use rand::seq::SliceRandom;
use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tracing::log::info;

use super::announcement::Announcement;

const THREE_DAYS_MS: u128 = 3 * 24 * 60 * 60 * 1_000_000;

pub(crate) type InitialPeers = HashMap<PeerId, HashMap<SocketAddr, TransportType>>;

#[derive(Default)]
pub(crate) struct PeerDB {
    pub(crate) peers: HashMap<PeerId, PeerInfo>,
    /// last is the oldest value (only routable peers)
    pub(crate) index_by_newest: BTreeSet<(Reverse<u128>, PeerId)>,
    /// Tested addresses used to avoid testing the same address too often. //TODO: Need to be pruned
    pub(crate) tested_addresses: HashMap<SocketAddr, MassaTime>,
}

pub(crate) type SharedPeerDB = Arc<RwLock<PeerDB>>;

pub(crate) type PeerMessageTuple = (PeerId, u64, Vec<u8>);

#[derive(Clone, Debug)]
pub(crate) struct PeerInfo {
    pub(crate) last_announce: Announcement,
    pub(crate) state: PeerState,
}

#[warn(dead_code)]
#[derive(Eq, PartialEq, Clone, Debug)]
pub(crate) enum PeerState {
    Banned,
    InHandshake,
    HandshakeFailed,
    Trusted,
}

pub(crate) enum PeerManagementCmd {
    Ban(Vec<PeerId>),
    Unban(Vec<PeerId>),
    GetBootstrapPeers { responder: Sender<BootstrapPeers> },
    Stop,
}

pub(crate) struct PeerManagementChannel {
    pub(crate) msg_sender: Sender<PeerMessageTuple>,
    pub(crate) command_sender: Sender<PeerManagementCmd>,
}

impl PeerDB {
    pub(crate) fn ban_peer(&mut self, peer_id: &PeerId) {
        println!("peers: {:?}", self.peers);
        if let Some(peer) = self.peers.get_mut(peer_id) {
            peer.state = PeerState::Banned;
            info!("Banned peer: {:?}", peer_id);
        } else {
            info!("Tried to ban unknown peer: {:?}", peer_id);
        };
    }

    pub(crate) fn unban_peer(&mut self, peer_id: &PeerId) {
        if self.peers.contains_key(peer_id) {
            self.peers.remove(peer_id);
            info!("Unbanned peer: {:?}", peer_id);
        } else {
            info!("Tried to unban unknown peer: {:?}", peer_id);
        };
    }

    /// Retrieve the peer with the oldest test date.
    pub(crate) fn get_oldest_peer(&self, cooldown: Duration) -> Option<SocketAddr> {
        match self
            .tested_addresses
            .iter()
            .min_by_key(|(_, timestamp)| *(*timestamp))
        {
            Some((addr, timestamp)) => {
                if timestamp.estimate_instant().ok()?.elapsed() > cooldown {
                    Some(*addr)
                } else {
                    None
                }
            }
            None => None,
        }
    }

    /// Select max 100 peers to send to another peer
    /// The selected peers should has been online within the last 3 days
    pub(crate) fn get_rand_peers_to_send(
        &self,
        nb_peers: usize,
    ) -> Vec<(PeerId, HashMap<SocketAddr, TransportType>)> {
        //TODO: Add ourself
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backward")
            .as_millis();

        let min_time = now - THREE_DAYS_MS;

        let mut keys = self.peers.keys().cloned().collect::<Vec<_>>();
        let mut rng = rand::thread_rng();
        keys.shuffle(&mut rng);

        let mut result = Vec::new();

        for key in keys {
            if result.len() >= nb_peers {
                break;
            }
            if let Some(peer) = self.peers.get(&key) {
                // skip old peers
                if peer.last_announce.timestamp < min_time {
                    continue;
                }
                let listeners: HashMap<SocketAddr, TransportType> = peer
                    .last_announce
                    .listeners
                    .clone()
                    .into_iter()
                    .filter(|(addr, _)| addr.ip().to_canonical().is_global())
                    .collect();
                if listeners.is_empty() {
                    continue;
                }
                result.push((key, listeners));
            }
        }

        result
    }

    pub(crate) fn get_banned_peer_count(&self) -> u64 {
        self.peers
            .values()
            .filter(|peer| peer.state == PeerState::Banned)
            .count() as u64
    }

    // Flush PeerDB to disk ?
    fn _flush(&self) -> Result<(), ProtocolError> {
        unimplemented!()
    }
}