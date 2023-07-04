//! # General description
//!
//! TODO

#![feature(let_chains)]

mod config;
mod controller;
mod error;
mod key;
mod ledger_changes;
mod ledger_entry;
mod mapping_grpc;
mod types;

pub use config::LedgerConfig;
pub use controller::LedgerController;
pub use error::LedgerError;
pub use key::{
    datastore_prefix_from_address, Key, KeyDeserializer, KeySerializer, KeyType, BALANCE_IDENT,
    BYTECODE_IDENT, DATASTORE_IDENT, VERSION_IDENT,
};
pub use ledger_changes::{
    DatastoreUpdateDeserializer, DatastoreUpdateSerializer, LedgerChanges,
    LedgerChangesDeserializer, LedgerChangesSerializer, LedgerEntryUpdate,
    LedgerEntryUpdateDeserializer, LedgerEntryUpdateSerializer,
};
pub use ledger_entry::{LedgerEntry, LedgerEntryDeserializer, LedgerEntrySerializer};
pub use types::{
    Applicable, SetOrDelete, SetOrKeep, SetOrKeepDeserializer, SetOrKeepSerializer,
    SetUpdateOrDelete, SetUpdateOrDeleteDeserializer, SetUpdateOrDeleteSerializer,
};

#[cfg(feature = "testing")]
pub mod test_exports;
