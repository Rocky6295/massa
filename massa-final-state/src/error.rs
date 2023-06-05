//! Copyright (c) 2022 MASSA LABS <info@massa.net>

//! This file defines all error types for final state management

use displaydoc::Display;
use thiserror::Error;

use massa_versioning::versioning::ExtendFromDbError;

/// Final state error
#[non_exhaustive]
#[derive(Display, Error, Debug)]
pub enum FinalStateError {
    /// invalid slot: {0}
    InvalidSlot(String),
    /// ledger error: {0}
    LedgerError(String),
    /// PoS error: {0}
    PosError(String),
    /// Snapshot error: {0}
    SnapshotError(String),
    /// ExtendFromDbError
    MipStoreError(#[from] ExtendFromDbError),
}
