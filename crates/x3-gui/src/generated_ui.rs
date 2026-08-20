//! Generated Slint UI glue — the only locally-permitted `unsafe` boundary.
//!
//! `slint::include_modules!()` expands to the Slint compiler's item-tree
//! code which contains audited `unsafe` internals. Handwritten modules
//! (`main`, `worker`, `presentation`, `projection`, `app_settings`) inherit
//! the crate-level `deny(unsafe_code)` and must stay safe.

#![allow(unsafe_code)]

slint::include_modules!();
