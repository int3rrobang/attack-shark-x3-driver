use std::{io, path::PathBuf};

use thiserror::Error;

use crate::state::SCHEMA_VERSION;

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
