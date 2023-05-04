use crate::{
    CycleHistoryDeserializer, CycleHistorySerializer, CycleInfo, DeferredCreditsDeserializer,
    DeferredCreditsSerializer, PoSChanges, PosError, PosResult, ProductionStats,
    SelectorController,
};
use crate::{DeferredCredits, PoSConfig};
use bitvec::vec::BitVec;
use massa_db::{
    DBBatch, MassaDB, CF_ERROR, CYCLE_HISTORY_DESER_ERROR, CYCLE_HISTORY_PREFIX,
    CYCLE_HISTORY_SER_ERROR, DEFERRED_CREDITS_DESER_ERROR, DEFERRED_CREDITS_PREFIX,
    DEFERRED_CREDITS_SER_ERROR, STATE_CF,
};
use massa_hash::Hash;
use massa_models::amount::Amount;
use massa_models::{address::Address, prehash::PreHashMap, slot::Slot};
use massa_serialization::{DeserializeError, Deserializer, Serializer, U64VarIntSerializer};
use parking_lot::RwLock;
use rocksdb::{Direction, IteratorMode};
use std::collections::VecDeque;
use std::ops::Bound::{Excluded, Included};
use std::ops::RangeBounds;
use std::sync::Arc;
use std::{collections::BTreeMap, path::PathBuf};
use tracing::debug;

// General cycle info idents
const COMPLETE_IDENT: u8 = 0u8;
const RNG_SEED_IDENT: u8 = 1u8;
const FINAL_STATE_HASH_SNAPSHOT_IDENT: u8 = 2u8;
const ROLL_COUNT_IDENT: u8 = 3u8;
const PROD_STATS_IDENT: u8 = 4u8;

// Production stats idents
const PROD_STATS_FAIL_IDENT: u8 = 0u8;
const PROD_STATS_SUCCESS_IDENT: u8 = 1u8;

/// Complete key formatting macro
#[macro_export]
macro_rules! complete_key {
    ($cycle_prefix:expr) => {
        [&$cycle_prefix[..], &[COMPLETE_IDENT]].concat()
    };
}

/// Rng seed key formatting macro
#[macro_export]
macro_rules! rng_seed_key {
    ($cycle_prefix:expr) => {
        [&$cycle_prefix[..], &[RNG_SEED_IDENT]].concat()
    };
}

/// Final state hash snapshot key formatting macro
#[macro_export]
macro_rules! final_state_hash_snapshot_key {
    ($cycle_prefix:expr) => {
        [&$cycle_prefix[..], &[FINAL_STATE_HASH_SNAPSHOT_IDENT]].concat()
    };
}

/// Roll count key prefix macro
#[macro_export]
macro_rules! roll_count_prefix {
    ($cycle_prefix:expr) => {
        [&$cycle_prefix[..], &[ROLL_COUNT_IDENT]].concat()
    };
}

/// Roll count key formatting macro
#[macro_export]
macro_rules! roll_count_key {
    ($cycle_prefix:expr, $addr:expr) => {
        [
            &$cycle_prefix[..],
            &[ROLL_COUNT_IDENT],
            &$addr.prefixed_bytes()[..],
        ]
        .concat()
    };
}

/// Production stats prefix macro
#[macro_export]
macro_rules! prod_stats_prefix {
    ($cycle_prefix:expr) => {
        [&$cycle_prefix[..], &[PROD_STATS_IDENT]].concat()
    };
}

/// Production stats fail key formatting macro
#[macro_export]
macro_rules! prod_stats_fail_key {
    ($cycle_prefix:expr, $addr:expr) => {
        [
            &$cycle_prefix[..],
            &[PROD_STATS_IDENT],
            &$addr.prefixed_bytes()[..],
            &[PROD_STATS_FAIL_IDENT],
        ]
        .concat()
    };
}

/// Production stats success key formatting macro
#[macro_export]
macro_rules! prod_stats_success_key {
    ($cycle_prefix:expr, $addr:expr) => {
        [
            &$cycle_prefix[..],
            &[PROD_STATS_IDENT],
            &$addr.prefixed_bytes()[..],
            &[PROD_STATS_SUCCESS_IDENT],
        ]
        .concat()
    };
}

/// Deferred credits key formatting macro
#[macro_export]
macro_rules! deferred_credits_key {
    ($id:expr) => {
        [&DEFERRED_CREDITS_PREFIX.as_bytes(), &$id[..]].concat()
    };
}

#[derive(Clone)]
#[allow(missing_docs)]
/// Final state of PoS
pub struct PoSFinalState {
    /// proof-of-stake configuration
    pub config: PoSConfig,
    /// Access to the RocksDB database
    pub db: Arc<RwLock<MassaDB>>,
    /// contiguous cycle history, back = newest
    pub cycle_history_cache: VecDeque<(u64, bool)>,
    /// selector controller
    pub selector: Box<dyn SelectorController>,
    /// initial rolls, used for negative cycle look back
    pub initial_rolls: BTreeMap<Address, u64>,
    /// initial seeds, used for negative cycle look back (cycles -2, -1 in that order)
    pub initial_seeds: Vec<Hash>,
    /// initial ledger hash, used for seed computation
    pub initial_ledger_hash: Hash,
    pub deferred_credits_serializer: DeferredCreditsSerializer,
    pub deferred_credits_deserializer: DeferredCreditsDeserializer,
    pub cycle_info_serializer: CycleHistorySerializer,
    pub cycle_info_deserializer: CycleHistoryDeserializer,
}

impl PoSFinalState {
    /// create a new `PoSFinalState`
    pub fn new(
        config: PoSConfig,
        initial_seed_string: &str,
        initial_rolls_path: &PathBuf,
        selector: Box<dyn SelectorController>,
        initial_ledger_hash: Hash,
        db: Arc<RwLock<MassaDB>>,
    ) -> Result<Self, PosError> {
        // load get initial rolls from file
        let initial_rolls = serde_json::from_str::<BTreeMap<Address, u64>>(
            &std::fs::read_to_string(initial_rolls_path).map_err(|err| {
                PosError::RollsFileLoadingError(format!("error while deserializing: {}", err))
            })?,
        )
        .map_err(|err| PosError::RollsFileLoadingError(format!("error opening file: {}", err)))?;

        // Seeds used as the initial seeds for negative cycles (-2 and -1 respectively)
        let init_seed = Hash::compute_from(initial_seed_string.as_bytes());
        let initial_seeds = vec![Hash::compute_from(init_seed.to_bytes()), init_seed];

        let deferred_credits_deserializer =
            DeferredCreditsDeserializer::new(config.thread_count, config.max_credit_length, true);
        let cycle_info_deserializer = CycleHistoryDeserializer::new(
            config.cycle_history_length as u64,
            config.max_rolls_length,
            config.max_production_stats_length,
        );

        let mut pos_state = Self {
            config,
            db,
            cycle_history_cache: Default::default(),
            selector,
            initial_rolls,
            initial_seeds,
            initial_ledger_hash,
            deferred_credits_serializer: DeferredCreditsSerializer::new(),
            deferred_credits_deserializer,
            cycle_info_serializer: CycleHistorySerializer::new(),
            cycle_info_deserializer,
        };

        pos_state.cycle_history_cache = pos_state.get_cycle_history_cycles().into();

        Ok(pos_state)
    }

    /// Reset the state of the PoS final state
    ///
    /// USED ONLY FOR BOOTSTRAP
    pub fn reset(&self) {
        let db = self.db.read();
        db.delete_prefix(CYCLE_HISTORY_PREFIX);
        db.delete_prefix(DEFERRED_CREDITS_PREFIX);
    }

    /// Create the initial cycle based off the initial rolls.
    ///
    /// This should be called only if bootstrap did not happen.
    pub fn create_initial_cycle(&mut self) {
        let mut rng_seed = BitVec::with_capacity(
            self.config
                .periods_per_cycle
                .saturating_mul(self.config.thread_count as u64)
                .try_into()
                .unwrap(),
        );
        rng_seed.extend(vec![false; self.config.thread_count as usize]);

        self.put_new_cycle_info(&CycleInfo::new_with_hash(
            0,
            false,
            self.initial_rolls.clone(),
            rng_seed,
            PreHashMap::default(),
        ));
    }

    /// Put a new CycleInfo to RocksDB, and update the cycle_history cache
    pub fn put_new_cycle_info(&mut self, cycle_info: &CycleInfo) {
        let db = self.db.read();

        let mut batch = DBBatch::new(db.get_db_hash());
        self.put_cycle_history_complete(cycle_info.cycle, cycle_info.complete, &mut batch);
        self.put_cycle_history_rng_seed(cycle_info.cycle, cycle_info.rng_seed.clone(), &mut batch);
        self.put_cycle_history_final_state_hash_snapshot(
            cycle_info.cycle,
            cycle_info.final_state_hash_snapshot,
            &mut batch,
        );

        for (address, roll) in cycle_info.roll_counts.iter() {
            self.put_cycle_history_address_entry(
                cycle_info.cycle,
                address,
                Some(roll),
                None,
                &mut batch,
            );
        }
        for (address, prod_stats) in cycle_info.production_stats.iter() {
            self.put_cycle_history_address_entry(
                cycle_info.cycle,
                address,
                None,
                Some(prod_stats),
                &mut batch,
            );
        }

        db.write_batch(batch);

        self.cycle_history_cache
            .push_back((cycle_info.cycle, cycle_info.complete));
    }

    /// Deletes a given cycle from RocksDB
    pub fn delete_cycle_info(&mut self, cycle: u64) {
        let db = self.db.read();
        let handle = db.0.cf_handle(STATE_CF).expect(CF_ERROR);
        let mut batch = DBBatch::new(db.get_db_hash());

        let prefix = self.cycle_history_cycle_prefix(cycle);

        for (serialized_key, _) in db.0.prefix_iterator_cf(handle, prefix).flatten() {
            db.delete_key(handle, &mut batch, serialized_key.to_vec());
        }

        db.write_batch(batch);
    }

    /// Create the a cycle based off of another cycle_info. Used for downtime interpolation,
    /// when restarting from a snapshot.
    ///
    pub fn create_new_cycle_from_last(
        &mut self,
        last_cycle_info: &CycleInfo,
        first_slot: Slot,
        last_slot: Slot,
    ) -> Result<(), PosError> {
        let mut rng_seed = if first_slot.is_first_of_cycle(self.config.periods_per_cycle) {
            BitVec::with_capacity(
                self.config
                    .periods_per_cycle
                    .saturating_mul(self.config.thread_count as u64)
                    .try_into()
                    .unwrap(),
            )
        } else {
            last_cycle_info.rng_seed.clone()
        };

        let cycle = last_slot.get_cycle(self.config.periods_per_cycle);

        let num_slots = last_slot
            .slots_since(&first_slot, self.config.thread_count)
            .expect("Error in slot ordering")
            .saturating_add(1);

        rng_seed.extend(vec![false; num_slots as usize]);

        let complete =
            last_slot.is_last_of_cycle(self.config.periods_per_cycle, self.config.thread_count);

        self.put_new_cycle_info(&CycleInfo::new_with_hash(
            cycle,
            complete,
            last_cycle_info.roll_counts.clone(),
            rng_seed,
            last_cycle_info.production_stats.clone(),
        ));

        Ok(())
    }

    /// Sends the current draw inputs (initial or bootstrapped) to the selector.
    /// Waits for the initial draws to be performed.
    pub fn compute_initial_draws(&mut self) -> PosResult<()> {
        // if cycle_history starts at a cycle that is strictly higher than 0, do not feed cycles 0, 1 to selector
        let history_starts_late = self
            .cycle_history_cache
            .front()
            .map(|c_info| c_info.0 > 0)
            .unwrap_or(false);

        let mut max_cycle = None;

        // feed cycles 0, 1 to selector if necessary
        if !history_starts_late {
            for draw_cycle in 0u64..=1 {
                self.feed_selector(draw_cycle)?;
                max_cycle = Some(draw_cycle);
            }
        }

        // feed cycles available from history
        for (idx, hist_item) in self.cycle_history_cache.iter().enumerate() {
            if !hist_item.1 {
                break;
            }
            if history_starts_late && idx == 0 {
                // If the history starts late, the first RNG seed cannot be used to draw
                // because the roll distribution which should be provided by the previous element is absent.
                continue;
            }
            let draw_cycle = hist_item.0.checked_add(2).ok_or_else(|| {
                PosError::OverflowError("cycle overflow in give_selector_controller".into())
            })?;
            self.feed_selector(draw_cycle)?;
            max_cycle = Some(draw_cycle);
        }

        // wait for all fed cycles to be drawn
        if let Some(wait_cycle) = max_cycle {
            self.selector.as_mut().wait_for_draws(wait_cycle)?;
        }
        Ok(())
    }

    /// Technical specification of `apply_changes`:
    ///
    /// set `self.last_final_slot` = C
    /// if cycle C is absent from `self.cycle_history`:
    ///     `push` a new empty `CycleInfo` at the back of `self.cycle_history` and set its cycle = C
    ///     `pop_front` from `cycle_history` until front() represents cycle C-4 or later (not C-3 because we might need older endorsement draws on the limit between 2 cycles)
    /// for the cycle C entry of `cycle_history`:
    ///     extend `seed_bits` with `changes.seed_bits`
    ///     extend `roll_counts` with `changes.roll_changes`
    ///         delete all entries from `roll_counts` for which the roll count is zero
    ///     add each element of `changes.production_stats` to the cycle's `production_stats`
    /// for each `changes.deferred_credits` targeting cycle Ct:
    ///     overwrite `self.deferred_credits` entries of cycle Ct in `cycle_history` with the ones from change
    ///         remove entries for which Amount = 0
    /// if slot S was the last of cycle C:
    ///     set complete=true for cycle C in the history
    ///     compute the seed hash and notifies the `PoSDrawer` for cycle `C+3`
    ///
    pub fn apply_changes_to_batch(
        &mut self,
        changes: PoSChanges,
        slot: Slot,
        feed_selector: bool,
        batch: &mut DBBatch,
    ) -> PosResult<()> {
        let slots_per_cycle: usize = self
            .config
            .periods_per_cycle
            .saturating_mul(self.config.thread_count as u64)
            .try_into()
            .unwrap();

        // compute the current cycle from the given slot
        let cycle = slot.get_cycle(self.config.periods_per_cycle);

        // if cycle C is absent from self.cycle_history:
        // push a new empty CycleInfo at the back of self.cycle_history and set its cycle = C
        // pop_front from cycle_history until front() represents cycle C-4 or later
        // (not C-3 because we might need older endorsement draws on the limit between 2 cycles)
        if let Some(info) = self.cycle_history_cache.back() {
            if cycle == info.0 && !info.1 {
                // extend the last incomplete cycle
            } else if info.0.checked_add(1) == Some(cycle) && info.1 {
                // the previous cycle is complete, push a new incomplete/empty one to extend

                let roll_counts = self.get_all_roll_counts(info.0);
                self.put_new_cycle_info(&CycleInfo::new_with_hash(
                    cycle,
                    false,
                    roll_counts,
                    BitVec::with_capacity(slots_per_cycle),
                    PreHashMap::default(),
                ));
                while self.cycle_history_cache.len() > self.config.cycle_history_length {
                    if let Some((cycle, _)) = self.cycle_history_cache.pop_front() {
                        self.delete_cycle_info(cycle);
                    }
                }
            } else {
                return Err(PosError::OverflowError(
                    "invalid cycle sequence in PoS final state".into(),
                ));
            }
        } else {
            return Err(PosError::ContainerInconsistency(
                "PoS history should never be empty here".into(),
            ));
        }

        let complete =
            slot.is_last_of_cycle(self.config.periods_per_cycle, self.config.thread_count);
        self.put_cycle_history_complete(cycle, complete, batch);

        // OPTIM: we could avoid reading the previous seed bits with a cache or with an update function

        let mut rng_seed = self.get_cycle_history_rng_seed(cycle);
        rng_seed.extend(changes.seed_bits);
        self.put_cycle_history_rng_seed(cycle, rng_seed.clone(), batch);

        // extend roll counts
        for (addr, roll_count) in changes.roll_changes {
            self.put_cycle_history_address_entry(cycle, &addr, Some(&roll_count), None, batch);
        }

        // extend production stats
        for (addr, stats) in changes.production_stats {
            self.put_cycle_history_address_entry(cycle, &addr, None, Some(&stats), batch);
        }

        // if the cycle just completed, check that it has the right number of seed bits
        if complete && rng_seed.len() != slots_per_cycle {
            panic!(
                "cycle completed with incorrect number of seed bits: {} instead of {}",
                rng_seed.len(),
                slots_per_cycle
            );
        }

        // extent deferred_credits with changes.deferred_credits
        for (slot, credits) in changes.deferred_credits.credits.iter() {
            for (address, amount) in credits.iter() {
                self.put_deferred_credits_entry(slot, address, amount, batch);
            }
        }

        // remove zero-valued credits
        self.remove_deferred_credits_zeros(batch);

        // feed the cycle if it is complete
        // notify the PoSDrawer about the newly ready draw data
        // to draw cycle + 2, we use the rng data from cycle - 1 and the seed from cycle
        debug!(
            "After slot {} PoS cycle list is {:?}",
            slot, self.cycle_history_cache
        );
        if complete && feed_selector {
            self.feed_selector(cycle.checked_add(2).ok_or_else(|| {
                PosError::OverflowError("cycle overflow when feeding selector".into())
            })?)
        } else {
            Ok(())
        }
    }

    /// Feeds the selector targeting a given draw cycle
    pub fn feed_selector(&self, draw_cycle: u64) -> PosResult<()> {
        // get roll lookback

        let (lookback_rolls, lookback_state_hash) = match draw_cycle.checked_sub(3) {
            // looking back in history
            Some(c) => {
                let index = self
                    .get_cycle_index(c)
                    .ok_or(PosError::CycleUnavailable(c))?;
                let cycle_info = &self.cycle_history_cache[index];
                if !cycle_info.1 {
                    return Err(PosError::CycleUnfinished(c));
                }
                // take the final_state_hash_snapshot at cycle - 3
                // it will later be combined with rng_seed from cycle - 2 to determine the selection seed
                // do this here to avoid a potential attacker manipulating the selections
                let state_hash = self.get_cycle_history_final_state_hash_snapshot(cycle_info.0);
                (
                    self.get_all_roll_counts(cycle_info.0),
                    state_hash.expect(
                        "critical: a complete cycle must contain a final state hash snapshot",
                    ),
                )
            }
            // looking back to negative cycles
            None => (self.initial_rolls.clone(), self.initial_ledger_hash),
        };

        // get seed lookback
        let lookback_seed = match draw_cycle.checked_sub(2) {
            // looking back in history
            Some(c) => {
                let index = self
                    .get_cycle_index(c)
                    .ok_or(PosError::CycleUnavailable(c))?;
                let cycle_info = &self.cycle_history_cache[index];
                if !cycle_info.1 {
                    return Err(PosError::CycleUnfinished(c));
                }
                let u64_ser = U64VarIntSerializer::new();
                let mut seed = Vec::new();
                u64_ser.serialize(&c, &mut seed).unwrap();
                seed.extend(self.get_cycle_history_rng_seed(cycle_info.0).into_vec());
                seed.extend(lookback_state_hash.to_bytes());
                Hash::compute_from(&seed)
            }
            // looking back to negative cycles
            None => self.initial_seeds[draw_cycle as usize],
        };

        // feed selector
        self.selector
            .as_ref()
            .feed_cycle(draw_cycle, lookback_rolls, lookback_seed)
    }

    /// Feeds the selector targeting a given draw cycle
    pub fn feed_cycle_state_hash(&self, cycle: u64, final_state_hash: Hash) {
        if self.get_cycle_index(cycle).is_some() {
            let db = self.db.read();

            let mut batch = DBBatch::new(db.get_db_hash());
            self.put_cycle_history_final_state_hash_snapshot(
                cycle,
                Some(final_state_hash),
                &mut batch,
            );

            db.write_batch(batch);
        } else {
            panic!("cycle {} should be contained here", cycle);
        }
    }

    /// Retrieves the amount of rolls a given address has at the latest cycle
    pub fn get_rolls_for(&self, addr: &Address) -> u64 {
        self.cycle_history_cache
            .back()
            .and_then(|info| {
                let cycle = info.0;
                let db = self.db.read();
                let handle = db.0.cf_handle(STATE_CF).expect(CF_ERROR);

                let key = roll_count_key!(self.cycle_history_cycle_prefix(cycle), addr);

                if let Some(serialized_value) =
                    db.0.get_cf(handle, key).expect(CYCLE_HISTORY_DESER_ERROR)
                {
                    let (_, amount) = self
                        .cycle_info_deserializer
                        .cycle_info_deserializer
                        .rolls_deser
                        .u64_deserializer
                        .deserialize::<DeserializeError>(&serialized_value)
                        .expect(CYCLE_HISTORY_DESER_ERROR);

                    Some(amount)
                } else {
                    None
                }
            })
            .unwrap_or_default()
    }

    /// Retrieves the amount of rolls a given address has at a given cycle
    pub fn get_address_active_rolls(&self, addr: &Address, cycle: u64) -> Option<u64> {
        match cycle.checked_sub(3) {
            Some(lookback_cycle) => {
                let db = self.db.read();
                let handle = db.0.cf_handle(STATE_CF).expect(CF_ERROR);

                let key = roll_count_key!(self.cycle_history_cycle_prefix(lookback_cycle), addr);

                if let Some(serialized_value) =
                    db.0.get_cf(handle, key).expect(CYCLE_HISTORY_DESER_ERROR)
                {
                    let (_, amount) = self
                        .cycle_info_deserializer
                        .cycle_info_deserializer
                        .rolls_deser
                        .u64_deserializer
                        .deserialize::<DeserializeError>(&serialized_value)
                        .expect(CYCLE_HISTORY_DESER_ERROR);

                    Some(amount)
                } else {
                    None
                }
            }
            None => self.initial_rolls.get(addr).cloned(),
        }
    }

    /// Retrieves every deferred credit in a slot range
    pub fn get_deferred_credits_range<R>(&self, range: R) -> DeferredCredits
    where
        R: RangeBounds<Slot>,
    {
        let db = self.db.read();
        let handle = db.0.cf_handle(STATE_CF).expect(CF_ERROR);

        let mut deferred_credits = DeferredCredits::new_without_hash();

        let mut start_key_buffer = Vec::new();
        start_key_buffer.extend_from_slice(DEFERRED_CREDITS_PREFIX.as_bytes());

        match range.start_bound() {
            Included(slot) => {
                self.deferred_credits_serializer
                    .slot_ser
                    .serialize(slot, &mut start_key_buffer)
                    .expect(DEFERRED_CREDITS_SER_ERROR);
            }
            Excluded(slot) => {
                self.deferred_credits_serializer
                    .slot_ser
                    .serialize(
                        &slot
                            .get_next_slot(self.config.thread_count)
                            .expect(DEFERRED_CREDITS_SER_ERROR),
                        &mut start_key_buffer,
                    )
                    .expect(DEFERRED_CREDITS_SER_ERROR);
            }
            _ => {}
        };

        for (serialized_key, serialized_value) in
            db.0.iterator_cf(
                handle,
                IteratorMode::From(&start_key_buffer, Direction::Forward),
            )
            .flatten()
        {
            if !serialized_key.starts_with(DEFERRED_CREDITS_PREFIX.as_bytes()) {
                break;
            }
            let (rest, slot) = self
                .deferred_credits_deserializer
                .slot_deserializer
                .deserialize::<DeserializeError>(&serialized_key[DEFERRED_CREDITS_PREFIX.len()..])
                .expect(DEFERRED_CREDITS_DESER_ERROR);
            if !range.contains(&slot) {
                break;
            }

            let (_, address) = self
                .deferred_credits_deserializer
                .credit_deserializer
                .address_deserializer
                .deserialize::<DeserializeError>(rest)
                .expect(DEFERRED_CREDITS_DESER_ERROR);

            let (_, amount) = self
                .deferred_credits_deserializer
                .credit_deserializer
                .amount_deserializer
                .deserialize::<DeserializeError>(&serialized_value)
                .expect(DEFERRED_CREDITS_DESER_ERROR);

            deferred_credits.insert(slot, address, amount);
        }

        deferred_credits
    }

    /// Gets the index of a cycle in history
    pub fn get_cycle_index(&self, cycle: u64) -> Option<usize> {
        let first_cycle = match self.cycle_history_cache.front() {
            Some(c) => c.0,
            None => return None, // history empty
        };
        if cycle < first_cycle {
            return None; // in the past
        }
        let index: usize = match (cycle - first_cycle).try_into() {
            Ok(v) => v,
            Err(_) => return None, // usize overflow
        };
        if index >= self.cycle_history_cache.len() {
            return None; // in the future
        }
        Some(index)
    }

    fn put_cycle_history_complete(&self, cycle: u64, value: bool, batch: &mut DBBatch) {
        let db = self.db.read();
        let handle = db.0.cf_handle(STATE_CF).expect(CF_ERROR);

        let prefix = self.cycle_history_cycle_prefix(cycle);

        let serialized_value = if value { &[1] } else { &[0] };

        db.put_or_update_entry_value(handle, batch, complete_key!(prefix), serialized_value);
    }

    fn is_cycle_complete(&self, cycle: u64) -> bool {
        let db = self.db.read();
        let handle = db.0.cf_handle(STATE_CF).expect(CF_ERROR);

        let prefix = self.cycle_history_cycle_prefix(cycle);

        if let Ok(Some(complete_value)) = db.0.get_cf(handle, complete_key!(prefix)) {
            complete_value.len() == 1 && complete_value[0] == 1
        } else {
            false
        }
    }

    fn put_cycle_history_final_state_hash_snapshot(
        &self,
        cycle: u64,
        value: Option<Hash>,
        batch: &mut DBBatch,
    ) {
        let db = self.db.read();
        let handle = db.0.cf_handle(STATE_CF).expect(CF_ERROR);

        let prefix = self.cycle_history_cycle_prefix(cycle);

        let mut serialized_value = Vec::new();
        self.cycle_info_serializer
            .cycle_info_serializer
            .opt_hash_ser
            .serialize(&value, &mut serialized_value)
            .expect(CYCLE_HISTORY_SER_ERROR);

        db.put_or_update_entry_value(
            handle,
            batch,
            final_state_hash_snapshot_key!(prefix),
            &serialized_value,
        );
    }

    fn put_cycle_history_rng_seed(&self, cycle: u64, value: BitVec<u8>, batch: &mut DBBatch) {
        let db = self.db.read();
        let handle = db.0.cf_handle(STATE_CF).expect(CF_ERROR);

        let prefix = self.cycle_history_cycle_prefix(cycle);

        let mut serialized_value = Vec::new();
        self.cycle_info_serializer
            .cycle_info_serializer
            .bitvec_ser
            .serialize(&value, &mut serialized_value)
            .expect(CYCLE_HISTORY_SER_ERROR);

        db.put_or_update_entry_value(handle, batch, rng_seed_key!(prefix), &serialized_value);
    }

    /// Internal function to put an entry and perform the hash XORs
    fn put_cycle_history_address_entry(
        &self,
        cycle: u64,
        address: &Address,
        roll_count: Option<&u64>,
        production_stats: Option<&ProductionStats>,
        batch: &mut DBBatch,
    ) {
        let db = self.db.read();
        let handle = db.0.cf_handle(STATE_CF).expect(CF_ERROR);

        let prefix = self.cycle_history_cycle_prefix(cycle);

        // Roll count
        if let Some(roll_count) = roll_count {
            let mut serialized_roll_count = Vec::new();
            self.cycle_info_serializer
                .cycle_info_serializer
                .u64_ser
                .serialize(roll_count, &mut serialized_roll_count)
                .expect(CYCLE_HISTORY_SER_ERROR);
            db.put_or_update_entry_value(
                handle,
                batch,
                roll_count_key!(prefix, address),
                &serialized_roll_count,
            );
        }

        // Production stats
        if let Some(production_stats) = production_stats {
            let mut serialized_prod_stats_fail = Vec::new();
            self.cycle_info_serializer
                .cycle_info_serializer
                .u64_ser
                .serialize(
                    &production_stats.block_failure_count,
                    &mut serialized_prod_stats_fail,
                )
                .expect(CYCLE_HISTORY_SER_ERROR);
            db.put_or_update_entry_value(
                handle,
                batch,
                prod_stats_fail_key!(prefix, address),
                &serialized_prod_stats_fail,
            );

            // Production stats success
            let mut serialized_prod_stats_success = Vec::new();
            self.cycle_info_serializer
                .cycle_info_serializer
                .u64_ser
                .serialize(
                    &production_stats.block_success_count,
                    &mut serialized_prod_stats_success,
                )
                .expect(CYCLE_HISTORY_SER_ERROR);
            db.put_or_update_entry_value(
                handle,
                batch,
                prod_stats_success_key!(prefix, address),
                &serialized_prod_stats_success,
            );
        }
    }

    /// Internal function to put an entry and perform the hash XORs
    pub fn put_deferred_credits_entry(
        &self,
        slot: &Slot,
        address: &Address,
        amount: &Amount,
        batch: &mut DBBatch,
    ) {
        let db = self.db.read();
        let handle = db.0.cf_handle(STATE_CF).expect(CF_ERROR);

        let mut serialized_key = Vec::new();
        self.deferred_credits_serializer
            .slot_ser
            .serialize(slot, &mut serialized_key)
            .expect(DEFERRED_CREDITS_SER_ERROR);
        self.deferred_credits_serializer
            .credits_ser
            .address_ser
            .serialize(address, &mut serialized_key)
            .expect(DEFERRED_CREDITS_SER_ERROR);

        let mut serialized_amount = Vec::new();
        self.deferred_credits_serializer
            .credits_ser
            .amount_ser
            .serialize(amount, &mut serialized_amount)
            .expect(DEFERRED_CREDITS_SER_ERROR);

        db.put_or_update_entry_value(
            handle,
            batch,
            deferred_credits_key!(serialized_key),
            &serialized_amount,
        );
    }

    /// Internal function to remove the zeros from the deferred_credits
    fn remove_deferred_credits_zeros(&self, batch: &mut DBBatch) {
        let db = self.db.read();
        let handle = db.0.cf_handle(STATE_CF).expect(CF_ERROR);

        for (serialized_key, serialized_value) in
            db.0.iterator_cf(handle, IteratorMode::Start).flatten()
        {
            let (_, amount) = self
                .deferred_credits_deserializer
                .credit_deserializer
                .amount_deserializer
                .deserialize::<DeserializeError>(&serialized_value)
                .expect(DEFERRED_CREDITS_DESER_ERROR);

            if amount.is_zero() {
                db.delete_key(handle, batch, serialized_key.to_vec());
            }
        }
    }
}

/// Helpers for key management
impl PoSFinalState {
    fn cycle_history_cycle_prefix(&self, cycle: u64) -> Vec<u8> {
        let mut serialized_key = Vec::new();
        serialized_key.extend_from_slice(CYCLE_HISTORY_PREFIX.as_bytes());
        self.cycle_info_serializer
            .cycle_info_serializer
            .u64_ser
            .serialize(&cycle, &mut serialized_key)
            .expect(CYCLE_HISTORY_SER_ERROR);
        serialized_key
    }

    /// Get all the roll counts for a given cycle
    pub fn get_all_roll_counts(&self, cycle: u64) -> BTreeMap<Address, u64> {
        let db = self.db.read();
        let handle = db.0.cf_handle(STATE_CF).expect(CF_ERROR);

        let mut roll_counts: BTreeMap<Address, u64> = BTreeMap::new();

        let prefix = roll_count_prefix!(self.cycle_history_cycle_prefix(cycle));
        for (serialized_key, serialized_value) in db.0.prefix_iterator_cf(handle, prefix).flatten()
        {
            let (rest, _cycle) = self
                .cycle_info_deserializer
                .cycle_info_deserializer
                .u64_deser
                .deserialize::<DeserializeError>(&serialized_key[CYCLE_HISTORY_PREFIX.len()..])
                .expect(CYCLE_HISTORY_DESER_ERROR);

            let (_, address) = self
                .cycle_info_deserializer
                .cycle_info_deserializer
                .rolls_deser
                .address_deserializer
                .deserialize::<DeserializeError>(&rest[1..])
                .expect(CYCLE_HISTORY_DESER_ERROR);

            let (_, amount) = self
                .cycle_info_deserializer
                .cycle_info_deserializer
                .rolls_deser
                .u64_deserializer
                .deserialize::<DeserializeError>(&serialized_value)
                .expect(CYCLE_HISTORY_DESER_ERROR);

            roll_counts.insert(address, amount);
        }

        roll_counts
    }

    /// Retrieves the productions statistics for all addresses on a given cycle
    pub fn get_all_production_stats(
        &self,
        cycle: u64,
    ) -> Option<PreHashMap<Address, ProductionStats>> {
        let db = self.db.read();
        let handle = db.0.cf_handle(STATE_CF).expect(CF_ERROR);

        let mut production_stats: PreHashMap<Address, ProductionStats> = PreHashMap::default();
        let mut cur_production_stat = ProductionStats::default();
        let mut cur_address = None;

        let prefix = prod_stats_prefix!(self.cycle_history_cycle_prefix(cycle));
        for (serialized_key, serialized_value) in db.0.prefix_iterator_cf(handle, prefix).flatten()
        {
            let (rest, _cycle) = self
                .cycle_info_deserializer
                .cycle_info_deserializer
                .u64_deser
                .deserialize::<DeserializeError>(&serialized_key[CYCLE_HISTORY_PREFIX.len()..])
                .expect(CYCLE_HISTORY_DESER_ERROR);

            let (rest, address) = self
                .cycle_info_deserializer
                .cycle_info_deserializer
                .production_stats_deser
                .address_deserializer
                .deserialize::<DeserializeError>(&rest[1..])
                .expect(CYCLE_HISTORY_DESER_ERROR);

            if cur_address != Some(address) {
                cur_address = Some(address);
                cur_production_stat = ProductionStats::default();
            }

            let (_, value) = self
                .cycle_info_deserializer
                .cycle_info_deserializer
                .production_stats_deser
                .u64_deserializer
                .deserialize::<DeserializeError>(&serialized_value)
                .expect(CYCLE_HISTORY_DESER_ERROR);

            if rest.len() == 1 && rest[0] == PROD_STATS_FAIL_IDENT {
                cur_production_stat.block_failure_count = value;
            } else if rest.len() == 1 && rest[0] == PROD_STATS_SUCCESS_IDENT {
                cur_production_stat.block_success_count = value;
            } else {
                panic!("{}", CYCLE_HISTORY_DESER_ERROR);
            }

            production_stats.insert(address, cur_production_stat);
        }

        match production_stats.is_empty() {
            true => None,
            false => Some(production_stats),
        }
    }

    fn get_cycle_history_rng_seed(&self, cycle: u64) -> BitVec<u8> {
        let db = self.db.read();
        let handle = db.0.cf_handle(STATE_CF).expect(CF_ERROR);

        let serialized_rng_seed =
            db.0.get_cf(
                handle,
                rng_seed_key!(self.cycle_history_cycle_prefix(cycle)),
            )
            .expect(CYCLE_HISTORY_DESER_ERROR)
            .expect(CYCLE_HISTORY_DESER_ERROR);

        let (_, rng_seed) = self
            .cycle_info_deserializer
            .cycle_info_deserializer
            .bitvec_deser
            .deserialize::<DeserializeError>(&serialized_rng_seed)
            .expect(CYCLE_HISTORY_DESER_ERROR);

        rng_seed
    }

    fn get_cycle_history_final_state_hash_snapshot(&self, cycle: u64) -> Option<Hash> {
        let db = self.db.read();
        let handle = db.0.cf_handle(STATE_CF).expect(CF_ERROR);

        let serialized_state_hash =
            db.0.get_cf(
                handle,
                final_state_hash_snapshot_key!(self.cycle_history_cycle_prefix(cycle)),
            )
            .expect(CYCLE_HISTORY_DESER_ERROR)
            .expect(CYCLE_HISTORY_DESER_ERROR);
        let (_, state_hash) = self
            .cycle_info_deserializer
            .cycle_info_deserializer
            .opt_hash_deser
            .deserialize::<DeserializeError>(&serialized_state_hash)
            .expect(CYCLE_HISTORY_DESER_ERROR);
        state_hash
    }

    fn get_cycle_history_cycles(&self) -> Vec<(u64, bool)> {
        let db = self.db.read();
        let handle = db.0.cf_handle(STATE_CF).expect(CF_ERROR);

        let mut found_cycles: Vec<(u64, bool)> = Vec::new();

        while let Some(Ok((serialized_key, _))) = match found_cycles.last() {
            Some((prev_cycle, _)) => {
                db.0.iterator_cf(
                    handle,
                    IteratorMode::From(
                        &self.cycle_history_cycle_prefix(prev_cycle.saturating_add(1)),
                        Direction::Forward,
                    ),
                )
                .next()
            }
            None => db.0.iterator_cf(handle, IteratorMode::Start).next(),
        } {
            let (_, cycle) = self
                .cycle_info_deserializer
                .cycle_info_deserializer
                .u64_deser
                .deserialize::<DeserializeError>(&serialized_key[CYCLE_HISTORY_PREFIX.len()..])
                .expect(CYCLE_HISTORY_DESER_ERROR);

            found_cycles.push((cycle, self.is_cycle_complete(cycle)));
        }

        found_cycles
    }

    /// Queries a given cycle info in the database
    /// Panics if the cycle is not on disk
    pub fn get_cycle_info(&self, cycle: u64) -> CycleInfo {
        let complete = self.is_cycle_complete(cycle);
        let rng_seed = self.get_cycle_history_rng_seed(cycle);
        let final_state_hash_snapshot = self.get_cycle_history_final_state_hash_snapshot(cycle);

        let roll_counts = self.get_all_roll_counts(cycle);
        let production_stats = self
            .get_all_production_stats(cycle)
            .unwrap_or(PreHashMap::default());

        let mut cycle_info =
            CycleInfo::new_with_hash(cycle, complete, roll_counts, rng_seed, production_stats);
        cycle_info.final_state_hash_snapshot = final_state_hash_snapshot;
        cycle_info
    }

    /// Gets the deferred credits for a given address that will be credited at a given slot
    pub fn get_address_credits_for_slot(&self, addr: &Address, slot: &Slot) -> Option<Amount> {
        let db = self.db.read();
        let handle = db.0.cf_handle(STATE_CF).expect(CF_ERROR);

        let mut serialized_key = Vec::new();
        self.deferred_credits_serializer
            .slot_ser
            .serialize(slot, &mut serialized_key)
            .expect(DEFERRED_CREDITS_SER_ERROR);
        self.deferred_credits_serializer
            .credits_ser
            .address_ser
            .serialize(addr, &mut serialized_key)
            .expect(DEFERRED_CREDITS_SER_ERROR);

        match db.0.get_cf(handle, deferred_credits_key!(serialized_key)) {
            Ok(Some(serialized_amount)) => {
                let (_, amount) = self
                    .deferred_credits_deserializer
                    .credit_deserializer
                    .amount_deserializer
                    .deserialize::<DeserializeError>(&serialized_amount)
                    .expect(DEFERRED_CREDITS_DESER_ERROR);
                Some(amount)
            }
            _ => None,
        }
    }

    /// Gets the production stats for a given address
    pub fn get_production_stats_for_address(
        &self,
        cycle: u64,
        address: Address,
    ) -> Option<ProductionStats> {
        let db = self.db.read();
        let handle = db.0.cf_handle(STATE_CF).expect(CF_ERROR);

        let prefix = self.cycle_history_cycle_prefix(cycle);

        let query = vec![
            (handle, prod_stats_fail_key!(prefix, address)),
            (handle, prod_stats_success_key!(prefix, address)),
        ];

        let results = db.0.multi_get_cf(query);

        match (results.get(0), results.get(1)) {
            (Some(Ok(Some(serialized_fail))), Some(Ok(Some(serialized_success)))) => {
                let (_, fail) = self
                    .cycle_info_deserializer
                    .cycle_info_deserializer
                    .production_stats_deser
                    .u64_deserializer
                    .deserialize::<DeserializeError>(serialized_fail)
                    .expect(CYCLE_HISTORY_DESER_ERROR);
                let (_, success) = self
                    .cycle_info_deserializer
                    .cycle_info_deserializer
                    .production_stats_deser
                    .u64_deserializer
                    .deserialize::<DeserializeError>(serialized_success)
                    .expect(CYCLE_HISTORY_DESER_ERROR);

                Some(ProductionStats {
                    block_success_count: success,
                    block_failure_count: fail,
                })
            }
            _ => None,
        }
    }
}

/// Helpers for testing
#[cfg(feature = "testing")]
impl PoSFinalState {
    /// Queries all the deferred credits in the database
    pub fn get_deferred_credits(&self) -> DeferredCredits {
        let db = self.db.read();
        let handle = db.0.cf_handle(STATE_CF).expect(CF_ERROR);

        let mut deferred_credits = DeferredCredits::new_with_hash();

        for (serialized_key, serialized_value) in
            db.0.iterator_cf(handle, IteratorMode::Start).flatten()
        {
            let (rest, slot) = self
                .deferred_credits_deserializer
                .slot_deserializer
                .deserialize::<DeserializeError>(&serialized_key)
                .expect(DEFERRED_CREDITS_DESER_ERROR);
            let (_, address) = self
                .deferred_credits_deserializer
                .credit_deserializer
                .address_deserializer
                .deserialize::<DeserializeError>(&rest)
                .expect(DEFERRED_CREDITS_DESER_ERROR);

            let (_, amount) = self
                .deferred_credits_deserializer
                .credit_deserializer
                .amount_deserializer
                .deserialize::<DeserializeError>(&serialized_value)
                .expect(DEFERRED_CREDITS_DESER_ERROR);

            deferred_credits.insert(slot, address, amount);
        }
        deferred_credits
    }
}