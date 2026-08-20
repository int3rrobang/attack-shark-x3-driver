use std::{io, path::PathBuf, time::Duration};

use thiserror::Error;

use crate::{
    device::{DeviceId, TransportSelection},
    state::SCHEMA_VERSION,
};
use attack_shark_x3::{ProfileId, ProtocolError, TransportKind};

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
    #[error("{operation} isn't supported over {transport:?}")]
    UnsupportedOperation {
        operation: &'static str,
        transport: TransportKind,
    },

    /// The operation is dangerous and the safe-by-default path refuses it
    /// until the user grants explicit authorization for this invocation.
    #[error("{operation} on {transport:?} needs your permission")]
    ExplicitAuthorizationRequired {
        operation: &'static str,
        transport: TransportKind,
    },

    /// A partial update has no complete baseline to merge against.
    #[error("no saved {resource} for profile {profile:?}")]
    MissingBaseline {
        resource: &'static str,
        profile: Option<ProfileId>,
    },

    /// The requested exact device is not currently available.
    #[error("device not found: {0}")]
    DeviceNotFound(DeviceId),
    /// More than one connected device matched a transport selection.
    #[error("multiple connected devices match transport selection {selection:?}: {candidates:?}")]
    AmbiguousDevice {
        selection: TransportSelection,
        candidates: Vec<DeviceId>,
    },

    /// No connected device matched a transport selection.
    #[error("no connected device matches transport selection {selection:?}")]
    NoDevice { selection: TransportSelection },

    /// An explicitly requested device was discovered but is disconnected.
    #[error("device is disconnected: {0}")]
    DeviceDisconnected(DeviceId),
    /// The exact USB device did not disappear within the interactive wait window.
    #[error(
        "timed out waiting for USB device {device} to disconnect during the power-cycle check after {timeout:?}"
    )]
    PowerCycleDisappearanceTimeout { device: DeviceId, timeout: Duration },

    /// The exact USB device did not return within the interactive wait window.
    #[error(
        "timed out waiting for USB device {device} to reconnect during the power-cycle check after {timeout:?}"
    )]
    PowerCycleReappearanceTimeout { device: DeviceId, timeout: Duration },

    /// Profile-reload verification has no distinct enabled profile to use.
    #[error("no alternate profile available for target {target} (maximum {maximum})")]
    NoAlternateProfile {
        target: ProfileId,
        maximum: ProfileId,
    },

    /// A normalized readback differs from the requested value.
    ///
    /// For USB profile metadata, `actual == target` is required for success;
    /// a mismatch is persisted as a mismatch observation and never reports
    /// success. For polling rate, report `0x06` carries the live image rate
    /// and is only associated after loading the target profile in the same
    /// guarded session.
    #[error("confirmation mismatch for {resource} on profile {profile:?}")]
    VerificationMismatch {
        resource: &'static str,
        profile: Option<ProfileId>,
    },
    /// Profile capture completed, but the original metadata could not be
    /// restored, so the device may remain expanded or switched.
    #[error(
        "read the profiles, but couldn't restore the original setup ({restore}); the mouse may be left on a different profile or with extra profiles enabled"
    )]
    RefreshRestorationFailed { restore: String },
    /// Profile capture and metadata restoration both failed, so the device
    /// may remain expanded or switched.
    #[error(
        "reading the profiles failed ({refresh}); restoring the original setup also failed ({restore}); the mouse may be left on a different profile or with extra profiles enabled"
    )]
    RefreshRestoreFailed { refresh: String, restore: String },

    /// A protocol-level validation or construction failure with operation context.
    ///
    /// The user-facing [`Display`] intentionally omits the technical [`ProtocolError`]
    /// detail to keep normal UI copy concise; the typed source remains available via
    /// [`std::error::Error::source`] and `Debug` for diagnostics and JSON output.
    #[error("invalid update for {operation}")]
    Protocol {
        operation: &'static str,
        #[source]
        source: ProtocolError,
    },

    /// The requested update is invalid before reaching a transport.
    #[error("invalid update: {0}")]
    InvalidUpdate(String),

    /// A per-device operation could not acquire its lock within the timeout.
    #[error(
        "device {device} is busy: operation {operation} timed out after {timeout:?} waiting for lock at {path}"
    )]
    DeviceOperationBusy {
        device: DeviceId,
        operation: &'static str,
        timeout: Duration,
        path: PathBuf,
    },
}
#[cfg(test)]
mod tests {
    use super::ManagerError;
    use attack_shark_x3::ProtocolError;
    use std::error::Error;

    #[test]
    fn protocol_error_preserves_typed_source_and_operation() {
        let source = ProtocolError::InvalidProfile { value: 99 };
        let err = ManagerError::Protocol {
            operation: "profile metadata",
            source,
        };
        assert!(matches!(
            err,
            ManagerError::Protocol {
                operation: "profile metadata",
                ..
            }
        ));
        let chained = err.source().expect("protocol error must have source");
        let typed = chained
            .downcast_ref::<ProtocolError>()
            .expect("source must be ProtocolError");
        assert_eq!(*typed, ProtocolError::InvalidProfile { value: 99 });
    }

    #[test]
    fn protocol_display_hides_technical_source_but_debug_shows_it() {
        let source = ProtocolError::InvalidPollingRate { value: 999 };
        let err = ManagerError::Protocol {
            operation: "dpi state",
            source,
        };
        let display = format!("{err}");
        assert_eq!(display, "invalid update for dpi state");
        assert!(
            !display.contains("999"),
            "Display must not leak technical source detail: {display}"
        );
        let debug = format!("{err:?}");
        assert!(
            debug.contains("InvalidPollingRate"),
            "Debug must retain source: {debug}"
        );
        assert!(debug.contains("dpi state"));
    }

    #[test]
    fn protocol_operation_context_is_preserved() {
        let err = ManagerError::Protocol {
            operation: "dpi: merge delta",
            source: ProtocolError::InvalidStageCount { count: 0 },
        };
        if let ManagerError::Protocol { operation, source } = &err {
            assert_eq!(*operation, "dpi: merge delta");
            assert_eq!(*source, ProtocolError::InvalidStageCount { count: 0 });
        } else {
            panic!("expected Protocol variant");
        }
        // source chaining via Error::source returns ProtocolError
        let src = err.source().unwrap();
        assert!(src.downcast_ref::<ProtocolError>().is_some());
    }

    #[test]
    fn invalid_update_string_variant_still_available() {
        let err = ManagerError::InvalidUpdate("custom message".to_owned());
        assert_eq!(format!("{err}"), "invalid update: custom message");
        assert!(err.source().is_none());
    }
}
