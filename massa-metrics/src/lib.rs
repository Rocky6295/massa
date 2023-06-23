//! this library is used to collect metrics from the node and expose them to the prometheus server
//!
//! the metrics are collected from the node and from the survey
//! the survey is a separate thread that is used to collect metrics from the network (active connections)
//!

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, RwLock},
    time::Duration,
};

use lazy_static::lazy_static;
use prometheus::{register_int_gauge, Gauge, IntCounter, IntGauge};
use survey::MassaSurvey;
use tracing::warn;

#[cfg(not(feature = "testing"))]
mod server;

mod survey;

lazy_static! {
    // use lazy_static for these metrics because they are used in storage which implement default
    static ref OPERATIONS_COUNTER: IntGauge = register_int_gauge!(
        "operations_storage_counter",
        "operations storage counter len"
    )
    .unwrap();
    static ref BLOCKS_COUNTER: IntGauge =
        register_int_gauge!("blocks_storage_counter", "blocks storage counter len").unwrap();
    static ref ENDORSEMENTS_COUNTER: IntGauge =
        register_int_gauge!("endorsements_storage_counter", "endorsements storage counter len").unwrap();
}

pub fn set_blocks_counter(val: usize) {
    BLOCKS_COUNTER.set(val as i64);
}

pub fn set_endorsements_counter(val: usize) {
    ENDORSEMENTS_COUNTER.set(val as i64);
}

pub fn set_operations_counter(val: usize) {
    OPERATIONS_COUNTER.set(val as i64);
}

#[derive(Clone)]
pub struct MassaMetrics {
    /// enable metrics
    enabled: bool,

    /// consensus period for each thread
    /// index 0 = thread 0 ...
    consensus_vec: Vec<Gauge>,

    /// total bytes receive by peernet manager
    peernet_total_bytes_receive: IntCounter,
    /// total bytes sent by peernet manager
    peernet_total_bytes_sent: IntCounter,

    /// total block in graph
    block_graph_counter: IntCounter,
    /// total time to add block to graph
    block_graph_ms: IntCounter,

    /// active in connections peer
    active_in_connections: IntGauge,
    /// active out connections peer
    active_out_connections: IntGauge,

    /// sum of stored_operations
    retrieval_thread_stored_operations_sum: IntGauge,

    /// counter of operations for final slot
    operations_final_counter: IntCounter,

    // block_cache
    block_cache_checked_headers_size: IntGauge,
    block_cache_blocks_known_by_peer: IntGauge,

    // Operation cache
    operation_cache_checked_operations: IntGauge,
    operation_cache_checked_operations_prefix: IntGauge,
    operation_cache_ops_know_by_peer: IntGauge,

    // Consensus state
    consensus_state_active_index: IntGauge,
    consensus_state_active_index_without_ops: IntGauge,
    consensus_state_incoming_index: IntGauge,
    consensus_state_discarded_index: IntGauge,
    consensus_state_block_statuses: IntGauge,

    // endorsement cache
    endorsement_cache_checked_endorsements: IntGauge,
    endorsement_cache_known_by_peer: IntGauge,

    // cursor
    active_cursor_thread: IntGauge,
    active_cursor_period: IntGauge,

    final_cursor_thread: IntGauge,
    final_cursor_period: IntGauge,

    // peer bandwidth (bytes sent, bytes received)
    peers_bandwidth: Arc<RwLock<HashMap<String, (IntCounter, IntCounter)>>>,

    pub tick_delay: Duration,
}

impl MassaMetrics {
    #[allow(unused_variables)]
    pub fn new(enabled: bool, addr: SocketAddr, nb_thread: u8, tick_delay: Duration) -> Self {
        // TODO unwrap

        let mut consensus_vec = vec![];
        for i in 0..nb_thread {
            let gauge = Gauge::new(
                format!("consensus_thread_{}", i),
                "consensus thread actual period",
            )
            .expect("Failed to create gauge");
            #[cfg(not(feature = "testing"))]
            {
                let _ = prometheus::register(Box::new(gauge.clone()));
            }

            consensus_vec.push(gauge);
        }

        // active cursor
        let active_cursor_thread =
            IntGauge::new("active_cursor_thread", "execution active cursor thread").unwrap();
        let active_cursor_period =
            IntGauge::new("active_cursor_period", "execution active cursor period").unwrap();

        // final cursor
        let final_cursor_thread =
            IntGauge::new("final_cursor_thread", "execution final cursor thread").unwrap();
        let final_cursor_period =
            IntGauge::new("final_cursor_period", "execution final cursor period").unwrap();

        // active connections IN
        let active_in_connections =
            IntGauge::new("active_in_connections", "active connections IN len").unwrap();

        // active connections OUT
        let active_out_connections =
            IntGauge::new("active_out_connections", "active connections OUT len").unwrap();

        // block cache
        let block_cache_checked_headers_size = IntGauge::new(
            "block_cache_checked_headers_size",
            "size of BlockCache checked_headers",
        )
        .unwrap();

        let block_cache_blocks_known_by_peer = IntGauge::new(
            "block_cache_blocks_known_by_peer_size",
            "size of BlockCache blocks_known_by_peer",
        )
        .unwrap();

        // operation cache
        let operation_cache_checked_operations = IntGauge::new(
            "operation_cache_checked_operations",
            "size of OperationCache checked_operations",
        )
        .unwrap();

        let operation_cache_checked_operations_prefix = IntGauge::new(
            "operation_cache_checked_operations_prefix",
            "size of OperationCache checked_operations_prefix",
        )
        .unwrap();

        let operation_cache_ops_know_by_peer = IntGauge::new(
            "operation_cache_ops_know_by_peer",
            "size of OperationCache operation_cache_ops_know_by_peer",
        )
        .unwrap();

        // from retrieval thread of operation_handler
        let retrieval_thread_stored_operations_sum = IntGauge::new(
            "retrieval_thread_stored_operations_sum_size",
            "sum of retrieval_thread_stored_operations",
        )
        .unwrap();

        // consensus state from tick.rs
        let consensus_state_active_index = IntGauge::new(
            "consensus_state_active_index",
            "consensus state active index size",
        )
        .unwrap();

        let consensus_state_active_index_without_ops = IntGauge::new(
            "consensus_state_active_index_without_ops",
            "consensus state active index without ops size",
        )
        .unwrap();

        let consensus_state_incoming_index = IntGauge::new(
            "consensus_state_incoming_index",
            "consensus state incoming index size",
        )
        .unwrap();

        let consensus_state_discarded_index = IntGauge::new(
            "consensus_state_discarded_index",
            "consensus state discarded index size",
        )
        .unwrap();

        let consensus_state_block_statuses = IntGauge::new(
            "consensus_state_block_statuses",
            "consensus state block statuses size",
        )
        .unwrap();

        let endorsement_cache_checked_endorsements = IntGauge::new(
            "endorsement_cache_checked_endorsements",
            "endorsement cache checked endorsements size",
        )
        .unwrap();

        let endorsement_cache_known_by_peer = IntGauge::new(
            "endorsement_cache_known_by_peer",
            "endorsement cache know by peer size",
        )
        .unwrap();

        let block_graph_counter =
            IntCounter::new("block_slot_graph_counter", "total block in graph").unwrap();
        let block_graph_ms = IntCounter::new(
            "block_slot_graph_ms",
            "sum of delta in ms between block inclusion in graph and block slot",
        )
        .unwrap();

        let peernet_total_bytes_receive = IntCounter::new(
            "peernet_total_bytes_receive",
            "total byte received by peernet",
        )
        .unwrap();

        let peernet_total_bytes_sent =
            IntCounter::new("peernet_total_bytes_sent", "total byte sent by peernet").unwrap();

        let operations_final_counter =
            IntCounter::new("operations_final_counter", "total final operations").unwrap();

        if enabled {
            #[cfg(not(feature = "testing"))]
            {
                server::bind_metrics(addr);

                let _ = prometheus::register(Box::new(final_cursor_thread.clone()));
                let _ = prometheus::register(Box::new(final_cursor_period.clone()));
                let _ = prometheus::register(Box::new(active_cursor_thread.clone()));
                let _ = prometheus::register(Box::new(active_cursor_period.clone()));
                let _ = prometheus::register(Box::new(active_out_connections.clone()));
                let _ = prometheus::register(Box::new(block_cache_blocks_known_by_peer.clone()));
                let _ = prometheus::register(Box::new(block_cache_checked_headers_size.clone()));
                let _ = prometheus::register(Box::new(operation_cache_checked_operations.clone()));
                let _ = prometheus::register(Box::new(active_in_connections.clone()));
                let _ = prometheus::register(Box::new(operation_cache_ops_know_by_peer.clone()));
                let _ =
                    prometheus::register(Box::new(retrieval_thread_stored_operations_sum.clone()));
                let _ = prometheus::register(Box::new(consensus_state_active_index.clone()));
                let _ = prometheus::register(Box::new(
                    consensus_state_active_index_without_ops.clone(),
                ));
                let _ = prometheus::register(Box::new(consensus_state_incoming_index.clone()));
                let _ = prometheus::register(Box::new(consensus_state_discarded_index.clone()));
                let _ = prometheus::register(Box::new(consensus_state_block_statuses.clone()));
                let _ = prometheus::register(Box::new(
                    operation_cache_checked_operations_prefix.clone(),
                ));
                let _ =
                    prometheus::register(Box::new(endorsement_cache_checked_endorsements.clone()));
                let _ = prometheus::register(Box::new(endorsement_cache_known_by_peer.clone()));
                let _ = prometheus::register(Box::new(block_graph_counter.clone()));
                let _ = prometheus::register(Box::new(block_graph_ms.clone()));
                let _ = prometheus::register(Box::new(peernet_total_bytes_receive.clone()));
                let _ = prometheus::register(Box::new(peernet_total_bytes_sent.clone()));
                let _ = prometheus::register(Box::new(operations_final_counter.clone()));
            }

            MassaSurvey::run(
                tick_delay,
                active_in_connections.clone(),
                active_out_connections.clone(),
                peernet_total_bytes_sent.clone(),
                peernet_total_bytes_receive.clone(),
            );
        }

        MassaMetrics {
            enabled,
            consensus_vec,
            peernet_total_bytes_receive,
            peernet_total_bytes_sent,
            block_graph_counter,
            block_graph_ms,
            active_in_connections,
            active_out_connections,
            retrieval_thread_stored_operations_sum,
            operations_final_counter,
            block_cache_checked_headers_size,
            block_cache_blocks_known_by_peer,
            operation_cache_checked_operations,
            operation_cache_checked_operations_prefix,
            operation_cache_ops_know_by_peer,
            consensus_state_active_index,
            consensus_state_active_index_without_ops,
            consensus_state_incoming_index,
            consensus_state_discarded_index,
            consensus_state_block_statuses,
            endorsement_cache_checked_endorsements,
            endorsement_cache_known_by_peer,
            // blocks_counter,
            // endorsements_counter,
            // operations_counter,
            active_cursor_thread,
            active_cursor_period,
            final_cursor_thread,
            final_cursor_period,
            peers_bandwidth: Arc::new(RwLock::new(HashMap::new())),
            tick_delay,
        }
    }

    pub fn set_active_connections(&self, in_connections: usize, out_connections: usize) {
        self.active_in_connections.set(in_connections as i64);
        self.active_out_connections.set(out_connections as i64);
    }

    pub fn set_active_cursor(&self, period: u64, thread: u8) {
        self.active_cursor_thread.set(thread as i64);
        self.active_cursor_period.set(period as i64);
    }

    pub fn set_final_cursor(&self, period: u64, thread: u8) {
        self.final_cursor_thread.set(thread as i64);
        self.final_cursor_period.set(period as i64);
    }

    pub fn set_consensus_period(&self, thread: usize, period: u64) {
        if let Some(g) = self.consensus_vec.get(thread) {
            g.set(period as f64);
        }
    }

    pub fn set_consensus_state(
        &self,
        active_index: usize,
        incoming_index: usize,
        discarded_index: usize,
        block_statuses: usize,
        active_index_without_ops: usize,
    ) {
        self.consensus_state_active_index.set(active_index as i64);
        self.consensus_state_incoming_index
            .set(incoming_index as i64);
        self.consensus_state_discarded_index
            .set(discarded_index as i64);
        self.consensus_state_block_statuses
            .set(block_statuses as i64);
        self.consensus_state_active_index_without_ops
            .set(active_index_without_ops as i64);
    }

    pub fn set_block_cache_metrics(&self, checked_header_size: usize, blocks_known_by_peer: usize) {
        self.block_cache_checked_headers_size
            .set(checked_header_size as i64);
        self.block_cache_blocks_known_by_peer
            .set(blocks_known_by_peer as i64);
    }

    pub fn set_retrieval_thread_stored_operations_sum(&self, sum: usize) {
        self.retrieval_thread_stored_operations_sum.set(sum as i64);
    }

    pub fn set_operations_cache_metrics(
        &self,
        checked_operations: usize,
        checked_operations_prefix: usize,
        ops_know_by_peer: usize,
    ) {
        self.operation_cache_checked_operations
            .set(checked_operations as i64);
        self.operation_cache_checked_operations_prefix
            .set(checked_operations_prefix as i64);
        self.operation_cache_ops_know_by_peer
            .set(ops_know_by_peer as i64);
    }

    pub fn set_endorsements_cache_metrics(
        &self,
        checked_endorsements: usize,
        known_by_peer: usize,
    ) {
        self.endorsement_cache_checked_endorsements
            .set(checked_endorsements as i64);
        self.endorsement_cache_known_by_peer
            .set(known_by_peer as i64);
    }

    pub fn inc_block_graph_ms(&self, diff: u64) {
        self.block_graph_ms.inc_by(diff);
    }

    pub fn inc_block_graph_counter(&self) {
        self.block_graph_counter.inc();
    }

    pub fn inc_peernet_total_bytes_receive(&self, diff: u64) {
        self.peernet_total_bytes_receive.inc_by(diff);
    }

    pub fn inc_peernet_total_bytes_sent(&self, diff: u64) {
        self.peernet_total_bytes_sent.inc_by(diff);
    }

    pub fn inc_operations_final_counter(&self, diff: u64) {
        self.operations_final_counter.inc_by(diff);
    }

    /// Update the bandwidth metrics for all peers
    /// HashMap<peer_id, (tx, rx)>
    pub fn update_peers_tx_rx(&self, data: HashMap<String, (u64, u64)>) {
        if self.enabled {
            // #[cfg(not(feature = "testing"))]
            // {

            let mut write = self.peers_bandwidth.write().unwrap();

            // metrics of peers that are not in the data HashMap are removed
            let missing_peer: Vec<String> = write
                .keys()
                .filter(|key| !data.contains_key(key.as_str()))
                .cloned()
                .collect();

            for key in missing_peer {
                // remove peer and unregister metrics
                if let Some((tx, rx)) = write.remove(&key) {
                    if let Err(e) = prometheus::unregister(Box::new(tx)) {
                        warn!("Failed to unregister tx metricfor peer {} : {}", key, e);
                    }

                    if let Err(e) = prometheus::unregister(Box::new(rx)) {
                        warn!("Failed to unregister rx metric for peer {} : {}", key, e);
                    }
                }
            }

            for (k, (tx_peernet, rx_peernet)) in data {
                if let Some((tx_metric, rx_metric)) = write.get_mut(&k) {
                    // peer metrics exist
                    // update tx and rx

                    let to_add = tx_peernet.saturating_sub(tx_metric.get());
                    tx_metric.inc_by(to_add);

                    let to_add = rx_peernet.saturating_sub(rx_metric.get());
                    rx_metric.inc_by(to_add);
                } else {
                    // peer metrics does not exist
                    let label_rx = format!("peer_total_bytes_receive_{}", k);
                    let label_tx = format!("peer_total_bytes_sent_{}", k);

                    let peer_total_bytes_receive =
                        IntCounter::new(label_rx, "total byte received by the peer").unwrap();

                    let peer_total_bytes_sent =
                        IntCounter::new(label_tx, "total byte sent by the peer").unwrap();

                    peer_total_bytes_sent.inc_by(tx_peernet);
                    peer_total_bytes_receive.inc_by(rx_peernet);

                    let _ = prometheus::register(Box::new(peer_total_bytes_receive.clone()));
                    let _ = prometheus::register(Box::new(peer_total_bytes_sent.clone()));

                    write.insert(k, (peer_total_bytes_sent, peer_total_bytes_receive));
                }
            }
        }
    }
}
// mod test {
//     use massa_channel::MassaChannel;

//     use crate::start_metrics_server;

//     #[tokio::test]
//     async fn test_channel_metrics() {
//         let addr = ([192, 168, 1, 183], 9898).into();

//         start_metrics_server(addr);
//         std::thread::sleep(std::time::Duration::from_millis(500));
//         let (sender, receiver) = MassaChannel::new("operations".to_string(), None);

//         let (sender2, receiver2) = MassaChannel::new("second_channel".to_string(), None);

//         sender2.send("hello_world".to_string()).unwrap();
//         let data = receiver2.recv().unwrap();
//         assert_eq!(data, "hello_world".to_string());

//         for i in 0..100 {
//             sender.send(i).unwrap();
//         }

//         for _i in 0..20 {
//             receiver.recv().unwrap();
//         }

//         assert_eq!(receiver.len(), 80);
//         std::thread::sleep(std::time::std::time::Duration::from_secs(5));
//         drop(sender2);
//         drop(receiver2);
//         std::thread::sleep(std::time::Duration::from_secs(100));
//     }

//     #[tokio::test]
//     async fn test_channel() {
//         let addr = ([192, 168, 1, 183], 9898).into();

//         start_metrics_server(addr);
//         std::thread::sleep(std::time::Duration::from_millis(500));

//         let (sender, receiver) = MassaChannel::new("test2".to_string(), None);

//         let cloned = receiver.clone();

//         sender.send("msg".to_string()).unwrap();

//         std::thread::spawn(move || {
//             dbg!("spawned");

//             loop {
//                 dbg!("loop");
//                 dbg!(receiver.recv().unwrap());
//                 std::thread::sleep(std::time::Duration::from_secs(1));
//             }
//         });
//         std::thread::sleep(std::time::Duration::from_secs(2));
//         std::thread::spawn(move || {
//             std::thread::sleep(std::time::std::time::Duration::from_secs(5));

//             drop(sender);
//         });

//         std::thread::sleep(std::time::Duration::from_secs(20));
//     }
// }
