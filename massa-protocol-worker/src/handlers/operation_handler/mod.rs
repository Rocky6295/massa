use std::thread::JoinHandle;

use massa_channel::{receiver::MassaReceiver, sender::MassaSender};
use massa_metrics::MassaMetrics;
use massa_pool_exports::PoolController;
use massa_protocol_exports::ProtocolConfig;
use massa_storage::Storage;

use crate::wrap_network::ActiveConnectionsTrait;

use self::{
    cache::SharedOperationCache, commands_propagation::OperationHandlerPropagationCommand,
    commands_retrieval::OperationHandlerRetrievalCommand, propagation::start_propagation_thread,
    retrieval::start_retrieval_thread,
};

pub mod cache;
pub mod commands_propagation;
pub mod commands_retrieval;
mod messages;
mod propagation;
mod retrieval;

pub(crate) use messages::{OperationMessage, OperationMessageSerializer};
pub(crate) use retrieval::note_operations_from_peer;

use super::peer_handler::models::{PeerManagementCmd, PeerMessageTuple};

pub struct OperationHandler {
    pub operation_retrieval_thread: Option<(
        MassaSender<OperationHandlerRetrievalCommand>,
        JoinHandle<()>,
    )>,
    pub operation_propagation_thread: Option<(
        MassaSender<OperationHandlerPropagationCommand>,
        JoinHandle<()>,
    )>,
}

impl OperationHandler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool_controller: Box<dyn PoolController>,
        storage: Storage,
        config: ProtocolConfig,
        cache: SharedOperationCache,
        active_connections: Box<dyn ActiveConnectionsTrait>,
        receiver_network: MassaReceiver<PeerMessageTuple>,
        sender_retrieval_ext: MassaSender<OperationHandlerRetrievalCommand>,
        receiver_retrieval_ext: MassaReceiver<OperationHandlerRetrievalCommand>,
        local_sender: MassaSender<OperationHandlerPropagationCommand>,
        local_receiver: MassaReceiver<OperationHandlerPropagationCommand>,
        peer_cmd_sender: MassaSender<PeerManagementCmd>,
        massa_metrics: MassaMetrics,
    ) -> Self {
        let operation_retrieval_thread = start_retrieval_thread(
            receiver_network,
            pool_controller,
            storage.clone_without_refs(),
            config.clone(),
            cache.clone(),
            active_connections.clone(),
            receiver_retrieval_ext,
            local_sender.clone(),
            peer_cmd_sender,
            massa_metrics.clone(),
        );

        let operation_propagation_thread = start_propagation_thread(
            local_receiver,
            active_connections,
            config,
            cache,
            storage.clone_without_refs(),
            massa_metrics,
        );
        Self {
            operation_retrieval_thread: Some((sender_retrieval_ext, operation_retrieval_thread)),
            operation_propagation_thread: Some((local_sender, operation_propagation_thread)),
        }
    }

    pub fn stop(&mut self) {
        if let Some((tx, thread)) = self.operation_retrieval_thread.take() {
            let _ = tx.send(OperationHandlerRetrievalCommand::Stop);
            thread.join().unwrap();
        }
        if let Some((tx, thread)) = self.operation_propagation_thread.take() {
            let _ = tx.send(OperationHandlerPropagationCommand::Stop);
            thread.join().unwrap();
        }
    }
}
