#![feature(variant_count)]
#![feature(assert_matches)]
// Copyright (c) 2022 MASSA LABS <info@massa.net>

//! # General description
//! MIP = Massa Improvement proposal (similar to Bitcoin Improvement Proposal - BIP or Ethereum - EIP)
//!
//! MIPInfo -> represent a MIP (name, versions, time ranges)
//! MIPState -> Deployment state of a MIPInfo
//! MIPStore -> A map of MIPInfo -> MipState
//!
//!
//! # Notes on MipInfo versions
//!
//! There is 2 different 'versions':
//! * version == Network version -> This is the network version to announce and thus to store in block header
//! * component_version -> This is the version for the associated component and is used in VersioningFactory (e.g KeyPair, Address, VM)
//!
//! # Notes on MipInfo timings and stats
//!
//! So in the execution module and after a block become final, we update the MipStore stats (in a blocking way).
//! By updating the stats, we mean sending: (Slot timestamp, Option<(current: u32, advertising: Option)>).
//! Using the slot timestamp, ensure that the trigger (and the trigger timeout) is a consensus by all nodes
//! (This means that the trigger and the trigger timeout are not timer based).
//! In order to have all nodes in sync (because of node various delays), the state is set to active
//! after an activation delay (duration is required to be > 1 cycle).
//!
//! # Notes on MipState
//!
//! MipState has:
//! * A state machine (stores the current state of deployment for a MipInfo)
//! * A history (stores a list of `Advance` message that 'really' updated the state machine)
//!
//! A auto generated graph of the state machine can be found here:
//! * dot -Tpng ./target/machine/componentstate.dot > ./target/machine/componentstate.png
//! * xdg-open ./target/machine/componentstate.png
//!
//! History is there in order to:
//! * Query the state at any time, so you can query MipStore and ask the best version at any time
//! * Used a lot when merging 2 MipStore:
//!   * By replaying the history of the states of the received MipStore (bootstrap), we can safely updates in the bootstrap process
//!   * + When we init MipStore (at startup), this ensures that we have a time ranges & versions coherent list of MipInfo
//!     * For instance, this can avoid to have 2 MipInfo with the same name
//!
//! # Versioning Factory
//!
//! A Factory trait is there to ease the development of factory for Versioned component (e.g. address, block)
//!
//! All factories should query MIPStore in order to create a component with correct version; default implementation
//! are provided by the trait to avoid re writing these query functions.
//!
//! Unit tests in versioning_factory.rs shows a basic but realistic implementation of a AddressFactory (impl the Factory trait)

pub mod address_factory;
pub mod grpc_mapping;
pub mod keypair_factory;
pub mod mips;
pub mod versioning;
pub mod versioning_factory;
pub mod versioning_ser_der;

/// Test utils
//#[cfg(any(test, feature = "testing"))]
pub mod test_helpers;
