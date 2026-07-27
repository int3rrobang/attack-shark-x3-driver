use std::{io, path::PathBuf};

use thiserror::Error;

use crate::{device::DeviceId, state::SCHEMA_VERSION};
use attack_shark_x3::{ProfileId, TransportKind};

/// Errors returned while loading, validating, or storing durable manager state.
#[derive(Debug, Error)]
pub enum StateError {
    /// The platform did not provide a usable base directory for manager state.
    #[error("unable to determine a base path for manager state: {reason}")]
    StatePathUnavailable { reason: String },

    /// An I/O operation failed at the given path.
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// The state document could not be encoded or decoded as JSON.
    #[error("invalid state JSON: {0}")]
    Serde(#[from] serde_json::Error),

    /// Another process currently owns the product-wide state lock.
    #[error("manager state is locked by another process at {path}")]
    LockBusy { path: PathBuf },

    /// The file uses a schema version this package does not understand.
    #[error("unsupported state schema version {found}; expected {expected}")]
    UnsupportedSchema { found: u32, expected: u32 },

    /// The document is syntactically valid but violates state invariants.
    #[error("invalid manager state: {0}")]
    InvalidState(String),
}

impl StateError {
    /// Creates an unavailable-state-path error.
    #[must_use]
    pub fn state_path_unavailable(reason: impl Into<String>) -> Self {
        Self::StatePathUnavailable {
            reason: reason.into(),
        }
    }

    /// Creates a path-qualified I/O error.
    #[must_use]
    pub fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    /// Creates an unsupported-schema error for the manager's current schema.
    #[must_use]
    pub const fn unsupported_schema(found: u32) -> Self {
        Self::UnsupportedSchema {
            found,
            expected: SCHEMA_VERSION,
        }
    }

    /// Creates an invalid-state error from a human-readable invariant detail.
    #[must_use]
    pub fn invalid_state(detail: impl Into<String>) -> Self {
        Self::InvalidState(detail.into())
    }
}

/// Errors returned by typed manager operations.
#[derive(Debug, Error)]
pub enum ManagerError {
    /// Durable manager state could not be loaded or stored.
    #[error(transparent)]
    State(#[from] StateError),

    /// A transport worker or USB driver operation failed.
    #[cfg(any(feature = "usb", feature = "ble"))]
    #[error(transparent)]
    Driver(#[from] attack_shark_x3::driver::DriverError),

    /// A BLE transport operation failed.
    #[cfg(feature = "ble")]
    #[error(transparent)]
    Ble(#[from] attack_shark_x3::BleError),

    /// The selected transport cannot perform the requested operation.
    #[error("operation {operation} is unsupported on {transport:?}")]
    UnsupportedOperation {
        operation: &'static str,
        transport: TransportKind,
    },

    /// A partial update has no complete baseline to merge against.
    #[error("missing {resource} baseline for profile {profile:?}")]
    MissingBaseline {
        resource: &'static str,
        profile: Option<ProfileId>,
    },

    /// The requested exact device is not currently available.
    #[error("device not found: {0}")]
    DeviceNotFound(DeviceId),

    /// Profile-reload verification has no distinct enabled profile to use.
    #[error("no alternate profile available for target {target} (maximum {maximum})")]
    NoAlternateProfile {
        target: ProfileId,
        maximum: ProfileId,
    },

    /// A normalized readback differs from the requested value.
    #[error("verification mismatch for {resource} on profile {profile:?}")]
    VerificationMismatch {
        resource: &'static str,
        profile: Option<ProfileId>,
    },

    /// The requested update is invalid before reaching a transport.
    #[error("invalid update: {0}")]
    InvalidUpdate(String),
}
