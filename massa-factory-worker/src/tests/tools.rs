use crossbeam_channel::Receiver;
use massa_consensus_exports::test_exports::{
    ConsensusControllerImpl, ConsensusEventReceiver, MockConsensusControllerMessage,
};
use massa_models::config::MIP_STORE_STATS_BLOCK_CONSIDERED;
use massa_models::config::MIP_STORE_STATS_COUNTERS_MAX;
use massa_versioning::versioning::MipStatsConfig;
use massa_versioning::versioning::MipStore;
use parking_lot::RwLock;
use std::{sync::Arc, thread::sleep, time::Duration};

use massa_factory_exports::{
    test_exports::create_empty_block, FactoryChannels, FactoryConfig, FactoryManager,
};
use massa_models::{
    address::Address, block_id::BlockId, config::ENDORSEMENT_COUNT,
    endorsement::SecureShareEndorsement, operation::SecureShareOperation, prehash::PreHashMap,
    slot::Slot, test_exports::get_next_slot_instant,
};
use massa_pool_exports::test_exports::{
    MockPoolController, MockPoolControllerMessage, PoolEventReceiver,
};
use massa_pos_exports::{
    test_exports::{MockSelectorController, MockSelectorControllerMessage},
    Selection,
};
use massa_protocol_exports::MockProtocolController;
use massa_signature::KeyPair;
use massa_storage::Storage;
use massa_time::MassaTime;

use crate::start_factory;
use massa_wallet::test_exports::create_test_wallet;

/// This structure store all information and links to creates tests for the factory.
/// The factory will ask that to the the pool, consensus and factory and then will send the block to the consensus.
/// You can use the method `new` to build all the mocks and make the connections
/// Then you can use the method `get_next_created_block` that will manage the answers from the mock to the factory depending on the parameters you gave.
#[allow(dead_code)]
pub struct TestFactory {
    consensus_event_receiver: Option<ConsensusEventReceiver>,
    pub(crate) pool_receiver: PoolEventReceiver,
    pub(crate) selector_receiver: Option<Receiver<MockSelectorControllerMessage>>,
    factory_config: FactoryConfig,
    factory_manager: Box<dyn FactoryManager>,
    genesis_blocks: Vec<(BlockId, u64)>,
    pub(crate) storage: Storage,
    keypair: KeyPair,
}

impl TestFactory {
    /// Initialize a new factory and all mocks with default data
    /// Arguments:
    /// - `keypair`: this keypair will be the one added to the wallet that will be used to produce all blocks
    ///
    /// Returns
    /// - `TestFactory`: the structure that will be used to manage the tests
    pub fn new(default_keypair: &KeyPair) -> TestFactory {
        let (selector_controller, selector_receiver) = MockSelectorController::new_with_receiver();
        let (consensus_controller, consensus_event_receiver) =
            ConsensusControllerImpl::new_with_receiver();
        let (pool_controller, pool_receiver) = MockPoolController::new_with_receiver();
        let mut storage = Storage::create_root();
        let mut factory_config = FactoryConfig::default();
        let protocol_controller = MockProtocolController::new();
        let producer_keypair = default_keypair;
        let producer_address = Address::from_public_key(&producer_keypair.get_public_key());
        let mut accounts = PreHashMap::default();

        let mut genesis_blocks = vec![];
        for i in 0..factory_config.thread_count {
            let block = create_empty_block(producer_keypair, &Slot::new(0, i));
            genesis_blocks.push((block.id, 0));
            storage.store_block(block);
        }

        accounts.insert(producer_address, producer_keypair.clone());
        factory_config.t0 = MassaTime::from_millis(400);
        factory_config.genesis_timestamp = factory_config
            .genesis_timestamp
            .checked_sub(factory_config.t0)
            .unwrap();

        // create an empty default store
        let mip_stats_config = MipStatsConfig {
            block_count_considered: MIP_STORE_STATS_BLOCK_CONSIDERED,
            counters_max: MIP_STORE_STATS_COUNTERS_MAX,
        };
        let mip_store =
            MipStore::try_from(([], mip_stats_config)).expect("Cannot create an empty MIP store");

        let factory_manager = start_factory(
            factory_config.clone(),
            Arc::new(RwLock::new(create_test_wallet(Some(accounts)))),
            FactoryChannels {
                selector: selector_controller.clone(),
                consensus: consensus_controller,
                pool: pool_controller.clone(),
                protocol: Box::new(protocol_controller),
                storage: storage.clone_without_refs(),
            },
            mip_store,
        );

        TestFactory {
            consensus_event_receiver: Some(consensus_event_receiver),
            pool_receiver,
            selector_receiver: Some(selector_receiver),
            factory_config,
            factory_manager,
            genesis_blocks,
            storage,
            keypair: default_keypair.clone(),
        }
    }

    /// This functions wait until it's time to create the next block to be sync with the factory.
    /// It will answers to all the asks of the factory with mocks and data you provide as parameters.
    ///
    /// Arguments:
    /// - `operations`: Optional list of operations to include in the block
    /// - `endorsements`: Optional list of endorsements to include in the block
    pub fn get_next_created_block(
        &mut self,
        operations: Option<Vec<SecureShareOperation>>,
        endorsements: Option<Vec<SecureShareEndorsement>>,
    ) -> (BlockId, Storage) {
        let now = MassaTime::now().expect("could not get current time");
        let next_slot_instant = get_next_slot_instant(
            self.factory_config.genesis_timestamp,
            self.factory_config.thread_count,
            self.factory_config.t0,
        );
        sleep(next_slot_instant.checked_sub(now).unwrap().to_duration());
        let producer_address = Address::from_public_key(&self.keypair.get_public_key());
        loop {
            match self
                .selector_receiver
                .as_ref()
                .unwrap()
                .recv_timeout(Duration::from_millis(100))
            {
                Ok(MockSelectorControllerMessage::GetProducer {
                    slot: _,
                    response_tx,
                }) => {
                    println!("test in receiver");
                    response_tx.send(Ok(producer_address)).unwrap();
                }
                Ok(MockSelectorControllerMessage::GetSelection {
                    slot: _,
                    response_tx,
                }) => {
                    println!("test in receiver2");
                    response_tx
                        .send(Ok(Selection {
                            producer: producer_address,
                            endorsements: vec![producer_address; ENDORSEMENT_COUNT as usize],
                        }))
                        .unwrap();
                }
                Err(_) => {
                    break;
                }
                _ => panic!("unexpected message"),
            }
        }
        if let Some(consensus_event_receiver) = self.consensus_event_receiver.as_mut() {
            consensus_event_receiver
                .wait_command(MassaTime::from_millis(100), |command| {
                    if let MockConsensusControllerMessage::GetBestParents { response_tx } = command
                    {
                        response_tx.send(self.genesis_blocks.clone()).unwrap();
                        Some(())
                    } else {
                        None
                    }
                })
                .unwrap();
        }
        self.pool_receiver
            .wait_command(MassaTime::from_millis(100), |command| match command {
                MockPoolControllerMessage::GetBlockEndorsements {
                    block_id: _,
                    slot: _,
                    response_tx,
                } => {
                    if let Some(endorsements) = &endorsements {
                        let ids = endorsements.iter().map(|endo| Some(endo.id)).collect();
                        let mut storage = self.storage.clone_without_refs();
                        storage.store_endorsements(endorsements.clone());
                        response_tx.send((ids, self.storage.clone())).unwrap();
                        Some(())
                    } else {
                        response_tx.send((vec![], Storage::create_root())).unwrap();
                        Some(())
                    }
                }
                _ => panic!("unexpected message"),
            })
            .unwrap();

        self.pool_receiver
            .wait_command(MassaTime::from_millis(100), |command| match command {
                MockPoolControllerMessage::GetBlockOperations {
                    slot: _,
                    response_tx,
                } => {
                    if let Some(operations) = &operations {
                        let ids = operations.iter().map(|op| op.id).collect();
                        let mut storage = self.storage.clone_without_refs();
                        storage.store_operations(operations.clone());
                        response_tx.send((ids, storage.clone())).unwrap();
                        Some(())
                    } else {
                        response_tx.send((vec![], Storage::create_root())).unwrap();
                        Some(())
                    }
                }
                _ => panic!("unexpected message"),
            })
            .unwrap();

        if let Some(consensus_event_receiver) = self.consensus_event_receiver.as_mut() {
            consensus_event_receiver
                .wait_command(MassaTime::from_millis(100), |command| {
                    if let MockConsensusControllerMessage::RegisterBlock {
                        block_id,
                        block_storage,
                        slot: _,
                        created: _,
                    } = command
                    {
                        Some((block_id, block_storage))
                    } else {
                        None
                    }
                })
                .unwrap()
        } else {
            panic!()
        }
    }
}

impl Drop for TestFactory {
    fn drop(&mut self) {
        // Need this otherwise factory_manager is stuck while waiting for block & endorsement factory
        // to join
        // For instance, block factory is waiting for selector.get_producer(...)
        //               endorsement factory is waiting for selector.get_selection(...)
        // Note: that this will make the 2 threads panic
        // TODO: find a better way to resolve this
        if let Some(selector_receiver) = self.selector_receiver.take() {
            drop(selector_receiver);
        }

        if let Some(consensus_receiver) = self.consensus_event_receiver.take() {
            drop(consensus_receiver);
        }

        self.factory_manager.stop();
    }
}
