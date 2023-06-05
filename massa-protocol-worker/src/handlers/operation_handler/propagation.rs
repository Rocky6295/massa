use std::{mem, thread::JoinHandle};

use crossbeam::channel::{Receiver, RecvTimeoutError};
use massa_logging::massa_trace;
use massa_models::operation::OperationId;
use massa_protocol_exports::PeerId;
use massa_protocol_exports::ProtocolConfig;
use tracing::{debug, info, log::warn};

use crate::{
    handlers::operation_handler::OperationMessage, messages::MessagesSerializer,
    wrap_network::ActiveConnectionsTrait,
};

use super::{
    cache::SharedOperationCache, commands_propagation::OperationHandlerPropagationCommand,
    OperationMessageSerializer,
};

struct PropagationThread {
    internal_receiver: Receiver<OperationHandlerPropagationCommand>,
    active_connections: Box<dyn ActiveConnectionsTrait>,
    operations_to_announce: Vec<OperationId>,
    config: ProtocolConfig,
    cache: SharedOperationCache,
    operation_message_serializer: MessagesSerializer,
}

impl PropagationThread {
    fn run(&mut self) {
        let mut next_announce = std::time::Instant::now()
            .checked_add(self.config.operation_announcement_interval.to_duration())
            .expect("Can't init interval op propagation");
        loop {
            match self.internal_receiver.recv_deadline(next_announce) {
                Ok(internal_message) => {
                    match internal_message {
                        OperationHandlerPropagationCommand::AnnounceOperations(operations_ids) => {
                            // Note operations as checked.
                            {
                                let mut cache_write = self.cache.write();
                                for op_id in operations_ids.iter().copied() {
                                    cache_write.insert_checked_operation(op_id);
                                }
                            }
                            self.operations_to_announce.extend(operations_ids);
                            if self.operations_to_announce.len()
                                > self.config.operation_announcement_buffer_capacity
                            {
                                self.announce_ops();
                                next_announce = std::time::Instant::now()
                                    .checked_add(
                                        self.config.operation_announcement_interval.to_duration(),
                                    )
                                    .expect("Can't init interval op propagation");
                            }
                        }
                        OperationHandlerPropagationCommand::Stop => {
                            info!("Stop operation propagation thread");
                            return;
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.announce_ops();
                    next_announce = std::time::Instant::now()
                        .checked_add(self.config.operation_announcement_interval.to_duration())
                        .expect("Can't init interval op propagation");
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return;
                }
            }
        }
    }

    fn announce_ops(&mut self) {
        // Quit if empty  to avoid iterating on nodes
        if self.operations_to_announce.is_empty() {
            return;
        }
        let operation_ids = mem::take(&mut self.operations_to_announce);
        massa_trace!("protocol.protocol_worker.announce_ops.begin", {
            "operation_ids": operation_ids
        });
        {
            let mut cache_write = self.cache.write();
            let peers_connected = self.active_connections.get_peer_ids_connected();
            cache_write.update_cache(
                peers_connected,
                self.config
                    .max_node_known_ops_size
                    .try_into()
                    .expect("max_node_known_ops_size is too big"),
            );

            // Propagate to peers
            let all_keys: Vec<PeerId> = cache_write
                .ops_known_by_peer
                .iter()
                .map(|(k, _)| k)
                .cloned()
                .collect();
            for peer_id in all_keys {
                let ops = cache_write.ops_known_by_peer.peek_mut(&peer_id).unwrap();
                let new_ops: Vec<OperationId> = operation_ids
                    .iter()
                    .filter(|id| ops.peek(&id.prefix()).is_none())
                    .copied()
                    .collect();
                if !new_ops.is_empty() {
                    for id in &new_ops {
                        ops.insert(id.prefix(), ());
                    }
                    debug!(
                        "Send operations announcement of len {} to {}",
                        new_ops.len(),
                        peer_id
                    );
                    for sub_list in new_ops.chunks(self.config.max_operations_per_message as usize)
                    {
                        if let Err(err) = self.active_connections.send_to_peer(
                            &peer_id,
                            &self.operation_message_serializer,
                            OperationMessage::OperationsAnnouncement(
                                sub_list.iter().map(|id| id.into_prefix()).collect(),
                            )
                            .into(),
                            false,
                        ) {
                            warn!(
                                "Failed to send OperationsAnnouncement message to peer: {}",
                                err
                            );
                        }
                    }
                }
            }
        }
    }
}

pub fn start_propagation_thread(
    internal_receiver: Receiver<OperationHandlerPropagationCommand>,
    active_connections: Box<dyn ActiveConnectionsTrait>,
    config: ProtocolConfig,
    cache: SharedOperationCache,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("protocol-operation-handler-propagation".to_string())
        .spawn(move || {
            let mut propagation_thread = PropagationThread {
                internal_receiver,
                active_connections,
                operations_to_announce: Vec::new(),
                config,
                cache,
                operation_message_serializer: MessagesSerializer::new()
                    .with_operation_message_serializer(OperationMessageSerializer::new()),
            };
            propagation_thread.run();
        })
        .expect("OS failed to start operation propagation thread")
}
