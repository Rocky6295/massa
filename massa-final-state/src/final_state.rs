//! Copyright (c) 2022 MASSA LABS <info@massa.net>

//! This file defines the final state of the node, which includes
//! the final ledger and asynchronous message pool that are kept at
//! the output of a given final slot (the latest executed final slot),
//! and need to be bootstrapped by nodes joining the network.

use crate::{config::FinalStateConfig, error::FinalStateError, state_changes::StateChanges};

use massa_async_pool::AsyncPool;
use massa_db::{DBBatch, MassaDB, CHANGE_ID_DESER_ERROR, MIP_STORE_PREFIX};
use massa_db::{
    ASYNC_POOL_PREFIX, CYCLE_HISTORY_PREFIX, DEFERRED_CREDITS_PREFIX,
    EXECUTED_DENUNCIATIONS_PREFIX, EXECUTED_OPS_PREFIX, LEDGER_PREFIX, STATE_CF,
};
use massa_executed_ops::ExecutedDenunciations;
use massa_executed_ops::ExecutedOps;
use massa_ledger_exports::LedgerController;
use massa_models::config::PERIODS_BETWEEN_BACKUPS;
use massa_models::slot::Slot;
use massa_pos_exports::{PoSFinalState, SelectorController};
use massa_versioning::versioning::{MipComponent, MipStore};

use parking_lot::RwLock;
use rocksdb::IteratorMode;
use tracing::{debug, info, warn};

use massa_models::timeslots::get_block_slot_timestamp;
use massa_time::MassaTime;
use std::sync::Arc;

/// Represents a final state `(ledger, async pool, executed_ops, executed_de and the state of the PoS)`
pub struct FinalState {
    /// execution state configuration
    pub(crate) config: FinalStateConfig,
    /// final ledger associating addresses to their balance, executable bytecode and data
    pub ledger: Box<dyn LedgerController>,
    /// asynchronous pool containing messages sorted by priority and their data
    pub async_pool: AsyncPool,
    /// proof of stake state containing cycle history and deferred credits
    pub pos_state: PoSFinalState,
    /// executed operations
    pub executed_ops: ExecutedOps,
    /// executed denunciations
    pub executed_denunciations: ExecutedDenunciations,
    /// MIP store
    pub mip_store: MipStore,
    /// last_start_period
    /// * If start new network: set to 0
    /// * If from snapshot: retrieve from args
    /// * If from bootstrap: set during bootstrap
    pub last_start_period: u64,
    /// last_slot_before_downtime
    /// * None if start new network
    /// * If from snapshot: retrieve from the slot attached to the snapshot
    /// * If from bootstrap: set during bootstrap
    pub last_slot_before_downtime: Option<Slot>,
    /// the rocksdb instance used to write every final_state struct on disk
    pub db: Arc<RwLock<MassaDB>>,
}

impl FinalState {
    /// Initializes a new `FinalState`
    ///
    /// # Arguments
    /// * `config`: the configuration of the final state to use for initialization
    /// * `ledger`: the instance of the ledger on disk. Used to apply changes to the ledger.
    /// * `selector`: the pos selector. Used to send draw inputs when a new cycle is completed.
    /// * `reset_final_state`: if true, we only keep the ledger, and we reset the other fields of the final state
    pub fn new(
        db: Arc<RwLock<MassaDB>>,
        config: FinalStateConfig,
        ledger: Box<dyn LedgerController>,
        selector: Box<dyn SelectorController>,
        mut mip_store: MipStore,
        reset_final_state: bool,
    ) -> Result<Self, FinalStateError> {
        let db_slot = db
            .read()
            .get_change_id()
            .map_err(|_| FinalStateError::InvalidSlot(String::from("Could not get slot in db")))?;

        // create the pos state
        let pos_state = PoSFinalState::new(
            config.pos_config.clone(),
            &config.initial_seed_string,
            &config.initial_rolls_path,
            selector,
            db.clone(),
        )
        .map_err(|err| FinalStateError::PosError(format!("PoS final state init error: {}", err)))?;

        // attach at the output of the latest initial final slot, that is the last genesis slot
        let slot = if reset_final_state {
            Slot::new(0, config.thread_count.saturating_sub(1))
        } else {
            db_slot
        };

        // create the async pool
        let async_pool = AsyncPool::new(config.async_pool_config.clone(), db.clone());

        // create a default executed ops
        let executed_ops = ExecutedOps::new(config.executed_ops_config.clone(), db.clone());

        // create a default executed denunciations
        let executed_denunciations =
            ExecutedDenunciations::new(config.executed_denunciations_config.clone(), db.clone());

        // init MIP store by reading from the db
        mip_store
            .extend_from_db(db.clone())
            .map_err(FinalStateError::from)?;

        let mut final_state = FinalState {
            ledger,
            async_pool,
            pos_state,
            config,
            executed_ops,
            executed_denunciations,
            mip_store,
            last_start_period: 0,
            last_slot_before_downtime: None,
            db,
        };

        if reset_final_state {
            let only_use_xor = final_state.get_only_use_xor(&slot);

            final_state.async_pool.reset(only_use_xor);
            final_state.pos_state.reset(only_use_xor);
            final_state.executed_ops.reset(only_use_xor);
            final_state.executed_denunciations.reset(only_use_xor);
            final_state.db.read().set_initial_change_id(slot);
        }

        info!(
            "final_state hash at slot {}: {}",
            slot,
            final_state.db.read().get_db_hash()
        );

        // create the final state
        Ok(final_state)
    }

    /// Initializes a `FinalState` from a snapshot. Currently, we do not use the final_state from the ledger,
    /// we just create a new one. This will be changed in the follow-up.
    ///
    /// # Arguments
    /// * `config`: the configuration of the final state to use for initialization
    /// * `ledger`: the instance of the ledger on disk. Used to apply changes to the ledger.
    /// * `selector`: the pos selector. Used to send draw inputs when a new cycle is completed.
    /// * `last_start_period`: at what period we should attach the final_state
    pub fn new_derived_from_snapshot(
        db: Arc<RwLock<MassaDB>>,
        config: FinalStateConfig,
        ledger: Box<dyn LedgerController>,
        selector: Box<dyn SelectorController>,
        mip_store: MipStore,
        last_start_period: u64,
    ) -> Result<Self, FinalStateError> {
        info!("Restarting from snapshot");

        let mut final_state =
            FinalState::new(db, config.clone(), ledger, selector, mip_store, false)?;

        let recovered_slot =
            final_state.db.read().get_change_id().map_err(|_| {
                FinalStateError::InvalidSlot(String::from("Could not get slot in db"))
            })?;

        // This is needed for `test_bootstrap_server` to work
        if cfg!(feature = "testing") {
            let mut batch = DBBatch::new();
            final_state.pos_state.create_initial_cycle(&mut batch);
            final_state.db.write().write_batch(
                batch,
                Default::default(),
                Some(recovered_slot),
                false,
            );
        }

        final_state.last_slot_before_downtime = Some(recovered_slot);

        // Check that MIP store is coherent with the network shutdown time range
        // Assume that the final state has been edited during network shutdown
        let shutdown_start = recovered_slot
            .get_next_slot(config.thread_count)
            .map_err(|e| {
                FinalStateError::InvalidSlot(format!(
                    "Unable to get next slot from recovered slot: {:?}",
                    e
                ))
            })?;
        let shutdown_end = Slot::new(last_start_period, 0)
            .get_prev_slot(config.thread_count)
            .map_err(|e| {
                FinalStateError::InvalidSlot(format!(
                    "Unable to compute prev slot from last start period: {:?}",
                    e
                ))
            })?;
        debug!(
            "Checking if MIP store is coherent against shutdown period: {} - {}",
            shutdown_start, shutdown_end
        );

        if !final_state
            .mip_store
            .is_coherent_with_shutdown_period(
                shutdown_start,
                shutdown_end,
                config.thread_count,
                config.t0,
                config.genesis_timestamp,
            )
            .unwrap_or(false)
        {
            return Err(FinalStateError::InvalidSlot(
                "MIP store is Not coherent".to_string(),
            ));
        }

        debug!(
            "Latest consistent slot found in snapshot data: {}",
            recovered_slot
        );

        info!(
            "final_state hash at slot {}: {}",
            recovered_slot,
            final_state.db.read().get_db_hash()
        );

        // Then, interpolate the downtime, to attach at end_slot;
        final_state.last_start_period = last_start_period;

        final_state.recompute_caches();

        // We compute the draws here because we need to feed_cycles when interpolating
        final_state.compute_initial_draws()?;

        let only_use_xor = final_state.get_only_use_xor(&recovered_slot);

        final_state.interpolate_downtime(only_use_xor)?;

        Ok(final_state)
    }

    /// Once we created a FinalState from a snapshot, we need to edit it to attach at the end_slot and handle the downtime.
    /// This basically recreates the history of the final_state, without executing the slots.
    fn interpolate_downtime(&mut self, only_use_xor: bool) -> Result<(), FinalStateError> {
        let current_slot =
            self.db.read().get_change_id().map_err(|_| {
                FinalStateError::InvalidSlot(String::from("Could not get slot in db"))
            })?;
        let current_slot_cycle = current_slot.get_cycle(self.config.periods_per_cycle);

        let end_slot = Slot::new(
            self.last_start_period,
            self.config.thread_count.saturating_sub(1),
        );
        let end_slot_cycle = end_slot.get_cycle(self.config.periods_per_cycle);

        if current_slot_cycle == end_slot_cycle {
            // In that case, we just complete the gap in the same cycle
            self.interpolate_single_cycle(current_slot, end_slot, only_use_xor)?;
        } else {
            // Here, we we also complete the cycle_infos in between
            self.interpolate_multiple_cycles(
                current_slot,
                end_slot,
                current_slot_cycle,
                end_slot_cycle,
                only_use_xor,
            )?;
        }

        // Recompute the hash with the updated data and feed it to POS_state.
        let final_state_hash = self.db.read().get_db_hash();

        info!(
            "final_state hash at slot {}: {}",
            end_slot, final_state_hash
        );

        // feed final_state_hash to the last cycle
        let cycle = end_slot.get_cycle(self.config.periods_per_cycle);

        self.pos_state
            .feed_cycle_state_hash(cycle, final_state_hash, only_use_xor);

        Ok(())
    }

    /// This helper function is to be called if the downtime does not span over multiple cycles
    fn interpolate_single_cycle(
        &mut self,
        current_slot: Slot,
        end_slot: Slot,
        only_use_xor: bool,
    ) -> Result<(), FinalStateError> {
        let latest_snapshot_cycle =
            self.pos_state
                .cycle_history_cache
                .pop_back()
                .ok_or(FinalStateError::SnapshotError(String::from(
                    "Invalid cycle_history",
                )))?;

        let latest_snapshot_cycle_info = self.pos_state.get_cycle_info(latest_snapshot_cycle.0);

        let mut batch = DBBatch::new();

        self.pos_state
            .delete_cycle_info(latest_snapshot_cycle.0, &mut batch);

        self.pos_state.db.write().write_batch(
            batch,
            Default::default(),
            Some(end_slot),
            only_use_xor,
        );

        let mut batch = DBBatch::new();

        self.pos_state
            .create_new_cycle_from_last(
                &latest_snapshot_cycle_info,
                current_slot
                    .get_next_slot(self.config.thread_count)
                    .expect("Cannot get next slot"),
                end_slot,
                &mut batch,
            )
            .map_err(|err| FinalStateError::PosError(format!("{}", err)))?;

        self.pos_state.db.write().write_batch(
            batch,
            Default::default(),
            Some(end_slot),
            only_use_xor,
        );

        Ok(())
    }

    /// This helper function is to be called if the downtime spans over multiple cycles
    fn interpolate_multiple_cycles(
        &mut self,
        current_slot: Slot,
        end_slot: Slot,
        current_slot_cycle: u64,
        end_slot_cycle: u64,
        only_use_xor: bool,
    ) -> Result<(), FinalStateError> {
        let latest_snapshot_cycle =
            self.pos_state
                .cycle_history_cache
                .pop_back()
                .ok_or(FinalStateError::SnapshotError(String::from(
                    "Invalid cycle_history",
                )))?;

        let latest_snapshot_cycle_info = self.pos_state.get_cycle_info(latest_snapshot_cycle.0);

        let mut batch = DBBatch::new();

        self.pos_state
            .delete_cycle_info(latest_snapshot_cycle.0, &mut batch);

        self.pos_state.db.write().write_batch(
            batch,
            Default::default(),
            Some(end_slot),
            only_use_xor,
        );

        // Firstly, complete the first cycle
        let last_slot = Slot::new_last_of_cycle(
            current_slot_cycle,
            self.config.periods_per_cycle,
            self.config.thread_count,
        )
        .map_err(|err| {
            FinalStateError::InvalidSlot(format!(
                "Cannot create slot for interpolating downtime: {}",
                err
            ))
        })?;

        let mut batch = DBBatch::new();

        self.pos_state
            .create_new_cycle_from_last(
                &latest_snapshot_cycle_info,
                current_slot
                    .get_next_slot(self.config.thread_count)
                    .expect("Cannot get next slot"),
                last_slot,
                &mut batch,
            )
            .map_err(|err| FinalStateError::PosError(format!("{}", err)))?;

        self.pos_state.db.write().write_batch(
            batch,
            Default::default(),
            Some(end_slot),
            only_use_xor,
        );

        // Feed final_state_hash to the completed cycle
        self.feed_cycle_hash_and_selector_for_interpolation(current_slot_cycle, only_use_xor)?;

        // TODO: Bring back the following optimisation (it fails because of selector)
        // Then, build all the completed cycles in betweens. If we have to build more cycles than the cycle_history_length, we only build the last ones.
        //let current_slot_cycle = (current_slot_cycle + 1)
        //    .max(end_slot_cycle.saturating_sub(self.config.pos_config.cycle_history_length as u64));
        let current_slot_cycle = current_slot_cycle + 1;

        for cycle in current_slot_cycle..end_slot_cycle {
            let first_slot = Slot::new_first_of_cycle(cycle, self.config.periods_per_cycle)
                .map_err(|err| {
                    FinalStateError::InvalidSlot(format!(
                        "Cannot create slot for interpolating downtime: {}",
                        err
                    ))
                })?;

            let last_slot = Slot::new_last_of_cycle(
                cycle,
                self.config.periods_per_cycle,
                self.config.thread_count,
            )
            .map_err(|err| {
                FinalStateError::InvalidSlot(format!(
                    "Cannot create slot for interpolating downtime: {}",
                    err
                ))
            })?;

            let mut batch = DBBatch::new();

            self.pos_state
                .create_new_cycle_from_last(
                    &latest_snapshot_cycle_info,
                    first_slot,
                    last_slot,
                    &mut batch,
                )
                .map_err(|err| FinalStateError::PosError(format!("{}", err)))?;

            self.pos_state.db.write().write_batch(
                batch,
                Default::default(),
                Some(end_slot),
                only_use_xor,
            );

            // Feed final_state_hash to the completed cycle
            self.feed_cycle_hash_and_selector_for_interpolation(cycle, only_use_xor)?;
        }

        // Then, build the last cycle
        let first_slot = Slot::new_first_of_cycle(end_slot_cycle, self.config.periods_per_cycle)
            .map_err(|err| {
                FinalStateError::InvalidSlot(format!(
                    "Cannot create slot for interpolating downtime: {}",
                    err
                ))
            })?;

        let mut batch = DBBatch::new();

        self.pos_state
            .create_new_cycle_from_last(
                &latest_snapshot_cycle_info,
                first_slot,
                end_slot,
                &mut batch,
            )
            .map_err(|err| FinalStateError::PosError(format!("{}", err)))?;

        // If the end_slot_cycle is completed
        if end_slot.is_last_of_cycle(self.config.periods_per_cycle, self.config.thread_count) {
            // Feed final_state_hash to the completed cycle
            self.feed_cycle_hash_and_selector_for_interpolation(end_slot_cycle, only_use_xor)?;
        }

        // We reduce the cycle_history len as needed
        while self.pos_state.cycle_history_cache.len() > self.pos_state.config.cycle_history_length
        {
            if let Some((cycle, _)) = self.pos_state.cycle_history_cache.pop_front() {
                self.pos_state.delete_cycle_info(cycle, &mut batch);
            }
        }

        self.db
            .write()
            .write_batch(batch, Default::default(), Some(end_slot), only_use_xor);

        Ok(())
    }

    /// Used during interpolation, when a new cycle is set as completed
    fn feed_cycle_hash_and_selector_for_interpolation(
        &mut self,
        cycle: u64,
        only_use_xor: bool,
    ) -> Result<(), FinalStateError> {
        let final_state_hash = self.db.read().get_db_hash();

        self.pos_state
            .feed_cycle_state_hash(cycle, final_state_hash, only_use_xor);

        self.pos_state
            .feed_selector(cycle.checked_add(2).ok_or_else(|| {
                FinalStateError::PosError("cycle overflow when feeding selector".into())
            })?)
            .map_err(|_| {
                FinalStateError::PosError("cycle overflow when feeding selector".into())
            })?;
        Ok(())
    }

    /// Reset the final state to the initial state.
    ///
    /// USED ONLY FOR BOOTSTRAP
    pub fn reset(&mut self) {
        let slot = Slot::new(0, self.config.thread_count.saturating_sub(1));
        let only_use_xor = self.get_only_use_xor(&slot);

        self.db.write().reset(slot);
        self.ledger.reset(only_use_xor);
        self.async_pool.reset(only_use_xor);
        self.pos_state.reset(only_use_xor);
        self.executed_ops.reset(only_use_xor);
        self.executed_denunciations.reset(only_use_xor);
        self.mip_store.reset_db(self.db.clone(), only_use_xor);
    }

    /// Performs the initial draws.
    pub fn compute_initial_draws(&mut self) -> Result<(), FinalStateError> {
        self.pos_state
            .compute_initial_draws()
            .map_err(|err| FinalStateError::PosError(err.to_string()))
    }

    /// Applies changes to the execution state at a given slot, and settles that slot forever.
    /// Once this is called, the state is attached at the output of the provided slot.
    ///
    /// Panics if the new slot is not the one coming just after the current one.
    pub fn finalize(&mut self, slot: Slot, changes: StateChanges) {
        let cur_slot = self.db.read().get_change_id().expect(CHANGE_ID_DESER_ERROR);
        // check slot consistency
        let next_slot = cur_slot
            .get_next_slot(self.config.thread_count)
            .expect("overflow in execution state slot");

        assert_eq!(
            slot, next_slot,
            "attempting to apply execution state changes at slot {} while the current slot is {}",
            slot, cur_slot
        );

        let mut db_batch = DBBatch::new();

        // apply the state changes to the batch

        self.async_pool
            .apply_changes_to_batch(&changes.async_pool_changes, &mut db_batch);
        self.pos_state
            .apply_changes_to_batch(changes.pos_changes.clone(), slot, true, &mut db_batch)
            .expect("could not settle slot in final state proof-of-stake");

        // TODO:
        // do not panic above, it might just mean that the lookback cycle is not available
        // bootstrap again instead
        self.ledger
            .apply_changes_to_batch(changes.ledger_changes.clone(), &mut db_batch);
        self.executed_ops.apply_changes_to_batch(
            changes.executed_ops_changes.clone(),
            slot,
            &mut db_batch,
        );

        self.executed_denunciations.apply_changes_to_batch(
            changes.executed_denunciations_changes.clone(),
            slot,
            &mut db_batch,
        );

        let only_use_xor = self.get_only_use_xor(&slot);
        
        println!("mip_status: {:?}", self.mip_store.get_mip_status());

        self.db
            .write()
            .write_batch(db_batch, Default::default(), Some(slot), only_use_xor);

        let final_state_hash = self.db.read().get_db_hash();

        // compute the final state hash
        info!("final_state hash at slot {}: {}", slot, final_state_hash);

        // Backup DB if needed
        if slot.period % PERIODS_BETWEEN_BACKUPS == 0 && slot.period != 0 && slot.thread == 0 {
            let state_slot = self.db.read().get_change_id();
            match state_slot {
                Ok(slot) => {
                    info!(
                        "Backuping db for slot {}, state slot: {}, state hash: {}",
                        slot, slot, final_state_hash
                    );
                }
                Err(e) => {
                    info!("{}", e);
                    info!(
                        "Backuping db for unknown state slot, state hash: {}",
                        final_state_hash
                    );
                }
            }

            self.db.read().backup_db(slot);
        }

        // feed final_state_hash to the last cycle
        let cycle = slot.get_cycle(self.config.periods_per_cycle);
        self.pos_state
            .feed_cycle_state_hash(cycle, final_state_hash, only_use_xor);
    }

    /// After bootstrap or load from disk, recompute all the caches.
    pub fn recompute_caches(&mut self) {
        self.async_pool.recompute_message_info_cache();
        self.executed_ops.recompute_sorted_ops_and_op_exec_status();
        self.executed_denunciations.recompute_sorted_denunciations();
        self.pos_state.recompute_pos_state_caches();
    }

    /// Deserialize the entire DB and check the data. Useful to check after bootstrap.
    pub fn is_db_valid(&self) -> bool {
        let db = self.db.read();
        let handle = db.db.cf_handle(STATE_CF).unwrap();

        for (serialized_key, serialized_value) in
            db.db.iterator_cf(handle, IteratorMode::Start).flatten()
        {
            if !serialized_key.starts_with(CYCLE_HISTORY_PREFIX.as_bytes())
                && !serialized_key.starts_with(DEFERRED_CREDITS_PREFIX.as_bytes())
                && !serialized_key.starts_with(ASYNC_POOL_PREFIX.as_bytes())
                && !serialized_key.starts_with(EXECUTED_OPS_PREFIX.as_bytes())
                && !serialized_key.starts_with(EXECUTED_DENUNCIATIONS_PREFIX.as_bytes())
                && !serialized_key.starts_with(LEDGER_PREFIX.as_bytes())
                && !serialized_key.starts_with(MIP_STORE_PREFIX.as_bytes())
            {
                warn!(
                    "Key/value does not correspond to any prefix: serialized_key: {:?}, serialized_value: {:?}",
                    serialized_key, serialized_value
                );
                return false;
            }

            if serialized_key.starts_with(CYCLE_HISTORY_PREFIX.as_bytes()) {
                if !self
                    .pos_state
                    .is_cycle_history_key_value_valid(&serialized_key, &serialized_value)
                {
                    warn!(
                        "Wrong key/value for CYCLE_HISTORY_KEY PREFIX serialized_key: {:?}, serialized_value: {:?}",
                        serialized_key, serialized_value
                    );
                    return false;
                }
            } else if serialized_key.starts_with(DEFERRED_CREDITS_PREFIX.as_bytes()) {
                if !self
                    .pos_state
                    .is_deferred_credits_key_value_valid(&serialized_key, &serialized_value)
                {
                    warn!(
                        "Wrong key/value for DEFERRED_CREDITS PREFIX serialized_key: {:?}, serialized_value: {:?}",
                        serialized_key, serialized_value
                    );
                    return false;
                }
            } else if serialized_key.starts_with(ASYNC_POOL_PREFIX.as_bytes()) {
                if !self
                    .async_pool
                    .is_key_value_valid(&serialized_key, &serialized_value)
                {
                    warn!(
                        "Wrong key/value for ASYNC_POOL PREFIX serialized_key: {:?}, serialized_value: {:?}",
                        serialized_key, serialized_value
                    );
                    return false;
                }
            } else if serialized_key.starts_with(EXECUTED_OPS_PREFIX.as_bytes()) {
                if !self
                    .executed_ops
                    .is_key_value_valid(&serialized_key, &serialized_value)
                {
                    warn!(
                        "Wrong key/value for EXECUTED_OPS PREFIX serialized_key: {:?}, serialized_value: {:?}",
                        serialized_key, serialized_value
                    );
                    return false;
                }
            } else if serialized_key.starts_with(EXECUTED_DENUNCIATIONS_PREFIX.as_bytes()) {
                if !self
                    .executed_denunciations
                    .is_key_value_valid(&serialized_key, &serialized_value)
                {
                    warn!("Wrong key/value for EXECUTED_DENUNCIATIONS PREFIX serialized_key: {:?}, serialized_value: {:?}", serialized_key, serialized_value);
                    return false;
                }
            } else if serialized_key.starts_with(LEDGER_PREFIX.as_bytes())
                && !self
                    .ledger
                    .is_key_value_valid(&serialized_key, &serialized_value)
            {
                warn!(
                    "Wrong key/value for LEDGER PREFIX serialized_key: {:?}, serialized_value: {:?}",
                    serialized_key, serialized_value
                );
                return false;
            }
        }

        true
    }

    /// Temporary getter to know if we should compute the lsm tree during db writes
    pub fn get_only_use_xor(&self, slot: &Slot) -> bool {
        let ts = get_block_slot_timestamp(
            self.config.thread_count,
            self.config.t0,
            self.config.genesis_timestamp,
            *slot,
        )
        .unwrap();
        self.get_hash_kind_version(ts) == 1
    }

    fn get_hash_kind_version(&self, ts: MassaTime) -> u32 {
        // Temp code
        // Return version for hash kind of final state: 0 -> LSM, 1 -> Xor
        // let now = MassaTime::now().expect("Cannot get current time");
        self.mip_store
            .get_latest_component_version_at(&MipComponent::FinalStateHashKind, ts)
    }
}
