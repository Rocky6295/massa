use massa_hash::Hash;
use massa_models::prehash::BuildHashMapper;
use schnellru::{ByLength, LruMap};
use tracing::{debug, warn};

use crate::types::ModuleInfo;

/// `LruMap` specialization for `PreHashed` keys
pub(crate) type PreHashLruMap<K, V> = LruMap<K, V, ByLength, BuildHashMapper<K>>;

/// RAM stored LRU cache.
/// The LRU caching scheme is to remove the least recently used module when the cache is full.
///
/// It is composed of:
/// * key: raw bytecode (which is hashed on insertion in LruMap)
/// * value.0: corresponding compiled module
/// * value.1: instance initialization cost
pub(crate) struct LRUCache {
    cache: PreHashLruMap<Hash, ModuleInfo>,
}

impl LRUCache {
    /// Create a new `LRUCache` with the given size
    pub(crate) fn new(cache_size: u32) -> Self {
        LRUCache {
            cache: LruMap::with_hasher(ByLength::new(cache_size), BuildHashMapper::default()),
        }
    }

    /// If the module is contained in the cache:
    /// * retrieve a copy of it
    /// * move it up in the LRU cache
    pub(crate) fn get(&mut self, hash: Hash) -> Option<ModuleInfo> {
        self.cache.get(&hash).cloned()
    }

    /// Save a module in the LRU cache
    pub(crate) fn insert(&mut self, hash: Hash, module_info: ModuleInfo) {
        self.cache.insert(hash, module_info);
        debug!("(LRU insert) length is: {}", self.cache.len());
    }

    /// Set the initialization cost of a LRU cached module
    pub(crate) fn set_init_cost(&mut self, hash: Hash, init_cost: u64) {
        if let Some(content) = self.cache.get(&hash) {
            match content {
                ModuleInfo::Module(module) => {
                    *content = ModuleInfo::ModuleAndDelta((module.clone(), init_cost))
                }
                ModuleInfo::ModuleAndDelta((_module, delta)) => *delta = init_cost,
                ModuleInfo::Invalid => {
                    warn!("tried to set the init cost of an invalid module");
                }
            }
        }
    }

    /// Set a module as invalid
    pub(crate) fn set_invalid(&mut self, hash: Hash) {
        if let Some(content) = self.cache.get(&hash) {
            *content = ModuleInfo::Invalid;
        }
    }
}
