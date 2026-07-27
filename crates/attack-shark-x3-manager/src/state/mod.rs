//! Durable desired/observed state for the manager.
//!
//! This module owns the on-disk state contract described in the refactor
//! roadmap (§6): a product-neutral `state.json`, a sibling cross-process
//! `state.lock`, schema validation, and atomic transactional writes.

pub mod model;
pub mod store;

pub use model::*;
pub use store::{StatePaths, StateStore, StateTransaction};
