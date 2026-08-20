//! Durable desired/observed state for the manager.
//!
//! This module owns the on-disk state contract at schema 4:
//! a product-neutral `state.json`, a sibling cross-process `state.lock`,
//! per-device `*.operation.lock` files keyed by logical `mouse-N`, schema
//! validation, allocation via `nextDeviceNumber`, and atomic transactional
//! writes. No migration from schema 3 exists.

pub mod model;
pub mod store;

pub use model::{
    ApplicationVerification, DesiredSource, DesiredState, DeviceState, MAX_PROFILE_NAME_CHARS,
    ObservationSource, ObservedState, PersistenceVerification, ProfileState, ResourceState,
    SCHEMA_VERSION, StateFile, Timestamp, Verification,
};
pub use store::{DeviceOperationGuard, StatePaths, StateReset, StateStore, StateTransaction};
