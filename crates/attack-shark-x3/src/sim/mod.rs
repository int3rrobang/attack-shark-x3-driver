//! Simulated mouse backends for development and conformance testing.
//!
//! Two backends implement the shared [`SimBackend`] contract: a capture
//! replay (slice 1, `sim/replay.rs`) and an in-memory model (manager-owned
//! `sim_model.rs`, which cannot live here without a dependency cycle).
//! Discovery registers sim endpoints through the normal hardware path while
//! [`SIM_ENV_VAR`] is set; unset, this module contributes nothing to
//! discovery and no special-casing exists downstream.

pub mod fixtures;
pub mod replay;

use crate::{ButtonsState, DpiState, PreferencesState, TransportKind};

pub use fixtures::{Scenario, ScenarioEndpoint, load_scenario, parse_scenario};
pub use replay::SimReplay;

/// Device-list tag prefix for replay-backed sim endpoints.
pub const SIM_REPLAY_TAG: &str = "[SIM-REPLAY]";
/// Device-list tag prefix for model-backed sim endpoints.
pub const SIM_MODEL_TAG: &str = "[SIM-MODEL]";
/// Environment variable selecting a sim scenario. Unset means hardware only.
pub const SIM_ENV_VAR: &str = "X3_SIM";

/// One full profile image: the existing (DPI, preferences, buttons) triple.
/// Reuses manager types; no new state shapes.
pub type ProfileImage = (DpiState, PreferencesState, ButtonsState);

/// Loud failure naming the missing capture or behavior.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("sim gap: {0}")]
pub struct SimGap(pub String);

/// Shared contract implemented by the replay and model backends.
///
/// Transport rules are real: BLE instances refuse reads exactly like
/// hardware (via [`SimGap`]); wired instances serve them.
pub trait SimBackend {
    /// Serves the recorded/stored image for `profile` (1-based, like hardware).
    fn read_profile(&self, profile: u8) -> Result<ProfileImage, SimGap>;
    /// Stores `image` into `profile`; gaps fail loudly, never guessed.
    fn write_profile(&mut self, profile: u8, image: &ProfileImage) -> Result<(), SimGap>;
    /// Recorded/configured battery level, or `None` when unavailable.
    fn battery(&self) -> Option<u8>;
    /// The transport this instance behaves as.
    fn transport(&self) -> TransportKind;
    /// Device-list tag: [`SIM_REPLAY_TAG`] or [`SIM_MODEL_TAG`].
    fn tag(&self) -> &'static str;
}

/// Which sim backend (and dataset) `X3_SIM` selects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SimSpec {
    /// Capture replay of the named scenario (`replay:name`, or a bare path).
    Replay { scenario: String },
    /// In-memory model from the named preset (`model:name`).
    Model { preset: String },
}

/// Parses one `X3_SIM` value. Blank means unset (`None`); an unknown
/// `kind:` prefix is an error (unless the whole value looks like a scenario
/// path, e.g. a Windows `C:\…` path, which stays a bare replay scenario).
pub fn parse_sim_spec(raw: &str) -> Result<Option<SimSpec>, SimGap> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let Some((kind, name)) = raw.split_once(':') else {
        return Ok(Some(SimSpec::Replay {
            scenario: raw.to_owned(),
        }));
    };
    let (kind, name) = (kind.trim(), name.trim());
    match kind {
        "replay" => {
            if name.is_empty() {
                return Err(SimGap(format!(
                    "sim: {SIM_ENV_VAR} replay needs a scenario name, got {raw:?}"
                )));
            }
            Ok(Some(SimSpec::Replay {
                scenario: name.to_owned(),
            }))
        }
        "model" => {
            if name.is_empty() {
                return Err(SimGap(format!(
                    "sim: {SIM_ENV_VAR} model needs a preset name, got {raw:?}"
                )));
            }
            Ok(Some(SimSpec::Model {
                preset: name.to_owned(),
            }))
        }
        _ if looks_like_path(raw) => Ok(Some(SimSpec::Replay {
            scenario: raw.to_owned(),
        })),
        _ => Err(SimGap(format!(
            "sim: {SIM_ENV_VAR} has unknown backend {kind:?} in {raw:?}; \
             expected replay:<scenario>, model:<preset>, or a scenario path"
        ))),
    }
}

/// Reads [`SIM_ENV_VAR`]. Unset (or blank) is `Ok(None)`: zero behavior change.
pub fn sim_spec_from_env() -> Result<Option<SimSpec>, SimGap> {
    sim_spec_from_var(std::env::var(SIM_ENV_VAR))
}

/// Env-lookup seam: the variable's lookup result maps onto a [`SimSpec`].
///
/// An absent variable is `Ok(None)` (hardware only); a non-Unicode value is a
/// loud gap. Kept separate from the process environment so the unset and
/// garbage paths are testable without mutating process state.
fn sim_spec_from_var(
    lookup: Result<String, std::env::VarError>,
) -> Result<Option<SimSpec>, SimGap> {
    match lookup {
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => Err(SimGap(format!("sim: {SIM_ENV_VAR} is not valid Unicode"))),
        Ok(raw) => parse_sim_spec(&raw),
    }
}

fn looks_like_path(value: &str) -> bool {
    value.contains('/') || value.contains('\\') || value.ends_with(".toml")
}

#[cfg(test)]
mod tests {
    use super::{SIM_ENV_VAR, SIM_MODEL_TAG, SIM_REPLAY_TAG, SimSpec, parse_sim_spec};

    #[test]
    fn tag_strings_are_exact() {
        assert_eq!(SIM_REPLAY_TAG, "[SIM-REPLAY]");
        assert_eq!(SIM_MODEL_TAG, "[SIM-MODEL]");
    }

    #[test]
    fn parses_prefixed_and_bare_specs() {
        assert_eq!(
            parse_sim_spec("replay:stock"),
            Ok(Some(SimSpec::Replay {
                scenario: "stock".to_owned()
            }))
        );
        assert_eq!(
            parse_sim_spec("model:tuned"),
            Ok(Some(SimSpec::Model {
                preset: "tuned".to_owned()
            }))
        );
        assert_eq!(
            parse_sim_spec("scenarios/stock.toml"),
            Ok(Some(SimSpec::Replay {
                scenario: "scenarios/stock.toml".to_owned()
            }))
        );
        assert_eq!(
            parse_sim_spec(r"C:\sims\stock.toml"),
            Ok(Some(SimSpec::Replay {
                scenario: r"C:\sims\stock.toml".to_owned()
            }))
        );
    }

    #[test]
    fn blank_means_unset() {
        assert_eq!(parse_sim_spec(""), Ok(None));
        assert_eq!(parse_sim_spec("   "), Ok(None));
    }

    #[test]
    fn garbage_is_loud() {
        for bad in ["bogus:thing", "replay:", "model:", ":stock"] {
            let error = parse_sim_spec(bad).expect_err("garbage must fail");
            assert!(
                format!("{error}").contains("sim"),
                "gap must name sim for {bad:?}"
            );
        }
    }

    #[test]
    fn env_lookup_maps_through_the_parser() {
        use std::env::VarError;
        assert_eq!(
            super::sim_spec_from_var(Err(VarError::NotPresent)),
            Ok(None)
        );
        assert_eq!(
            super::sim_spec_from_var(Ok("replay:stock".to_owned())),
            Ok(Some(SimSpec::Replay {
                scenario: "stock".to_owned()
            }))
        );
        assert_eq!(
            super::sim_spec_from_var(Ok("   ".to_owned())),
            Ok(None),
            "blank is unset"
        );
        let non_unicode =
            super::sim_spec_from_var(Err(VarError::NotUnicode(std::ffi::OsString::from("x"))))
                .expect_err("non-unicode must fail");
        assert!(format!("{non_unicode}").contains(SIM_ENV_VAR));
        let garbage =
            super::sim_spec_from_var(Ok("bogus:thing".to_owned())).expect_err("garbage must fail");
        assert!(format!("{garbage}").contains("bogus"));
    }
}
