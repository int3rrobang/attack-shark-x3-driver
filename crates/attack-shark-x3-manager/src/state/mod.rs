//! Durable desired/observed state for the manager.
//!
//! This module owns the on-disk state contract at schema 5:
//! a product-neutral `state.json`, a sibling cross-process `state.lock`,
//! per-device operation locks named `device-<id>.lock`, keyed by logical
//! `mouse-N`, schema validation, allocation via `nextDeviceNumber`, and
//! atomic transactional writes. Schema 5 adds installation-level identity
//! mode (legacy or persistent), optional per-device physical watermark ids,
//! and a durable resumable identity-setup journal. No migration from schema 4
//! exists; schema 4 documents are rejected.

pub mod model;
pub mod store;

pub use model::{
    ApplicationVerification, CapturedProfileImage, DesiredSource, DesiredState, DeviceState,
    IdentityMode, IdentitySetupJournal, IdentitySetupPhase, IdentitySetupStage,
    IdentitySetupSubject, IdentityStampProgress, MAX_PROFILE_NAME_CHARS, ObservationSource,
    ObservedState, PersistenceVerification, ProfileState, ResourceState, SCHEMA_VERSION, StateFile,
    Timestamp, Verification,
};
pub use store::{DeviceOperationGuard, StatePaths, StateReset, StateStore, StateTransaction};
