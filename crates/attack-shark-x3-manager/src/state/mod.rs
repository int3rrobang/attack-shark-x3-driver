//! Durable desired/observed state for the manager.
//!
//! This module owns the on-disk state contract at schema 6:
//! a product-neutral `state.json`, a sibling cross-process `state.lock`,
//! per-device operation locks named `device-<id>.lock`, keyed by logical
//! `mouse-N`, schema validation, allocation via `nextDeviceNumber`, and
//! atomic transactional writes. Schema 5 added installation-level identity
//! mode (legacy or persistent), optional per-device physical watermark ids,
//! and a durable resumable identity-setup journal; schema 6 adds the
//! per-device presentation-only shell colour. No migration from an earlier
//! schema exists; older documents are rejected.

pub mod model;
pub mod store;

pub use model::{
    ApplicationVerification, CapturedProfileImage, DesiredSource, DesiredState, DeviceState,
    EndpointMapping, IdentityMode, IdentitySetupJournal, IdentitySetupPhase, IdentitySetupStage,
    IdentitySetupSubject, IdentityStampProgress, MAX_PROFILE_NAME_CHARS, ObservationSource,
    ObservedState, PersistenceVerification, ProfileState, ResourceState, SCHEMA_VERSION,
    ShellColor, StateFile, Timestamp, Verification,
};
pub use store::{DeviceOperationGuard, StatePaths, StateReset, StateStore, StateTransaction};
