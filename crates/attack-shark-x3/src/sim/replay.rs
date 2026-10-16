//! Byte-exact recorded-image replay for sim phase 1.
//!
//! [`SimReplay`] serves the profile images a scenario captured. Reads are
//! byte-exact; writes verify against the scenario's recorded post-write
//! image when one exists, and report a [`SimGap`] naming the missing
//! capture otherwise — replay never guesses resulting state.

use std::path::Path;

use super::fixtures::{Scenario, load_scenario};
use super::{ProfileImage, SimBackend, SimGap};
use crate::{
    ButtonsReport, DpiReport, PreferencesFraming, PreferencesReport, ProfileId, TransportKind,
};

/// Byte-exact replay of one scenario endpoint.
#[derive(Clone, Debug)]
pub struct SimReplay {
    /// Source identifier for gap messages (`scenario <name>` or a path).
    source: String,
    /// Transports of every endpoint in the scenario (this instance's
    /// endpoint selects one entry).
    endpoint_transports: Vec<TransportKind>,
    /// Transport this instance presents as.
    transport: TransportKind,
    /// Recorded profile images by one-based profile number.
    profiles: std::collections::BTreeMap<u8, ProfileImage>,
    /// Recorded post-write images by one-based profile number.
    post_write: std::collections::BTreeMap<u8, ProfileImage>,
    /// Recorded battery level (`None` = no battery reporting).
    battery: Option<u8>,
}

impl SimReplay {
    /// Tag prefix for device-list rows served by this backend.
    pub const REPLAY_TAG: &'static str = "[SIM-REPLAY]";

    /// Loads scenario `scenario` (registry name or file path) as a
    /// single-endpoint backend: the first endpoint.
    ///
    /// # Errors
    ///
    /// Returns a [`SimGap`] naming the missing scenario or capture when
    /// the scenario cannot be found or parsed.
    pub fn load(scenario: &str) -> Result<Self, SimGap> {
        Self::load_endpoint(scenario, 0)
    }

    /// Loads endpoint `endpoint` (zero-based) of scenario `scenario`.
    ///
    /// # Errors
    ///
    /// Returns a [`SimGap`] naming the missing scenario, capture, or
    /// endpoint index when the scenario cannot be found, parsed, or
    /// has no such endpoint.
    pub fn load_endpoint(scenario: &str, endpoint: usize) -> Result<Self, SimGap> {
        let path = resolve_scenario_path(scenario)?;
        let loaded = load_scenario(&path)?;
        Self::from_scenario(&loaded, scenario, endpoint)
    }

    /// Transports a scenario exposes as discovery endpoints.
    ///
    /// # Errors
    ///
    /// Returns a [`SimGap`] naming the missing scenario or capture when
    /// the scenario cannot be found or parsed.
    pub fn endpoint_transports(scenario: &str) -> Result<Vec<TransportKind>, SimGap> {
        let path = resolve_scenario_path(scenario)?;
        let loaded = load_scenario(&path)?;
        Ok(loaded
            .endpoints
            .iter()
            .map(|endpoint| endpoint.transport)
            .collect())
    }

    fn from_scenario(loaded: &Scenario, scenario: &str, endpoint: usize) -> Result<Self, SimGap> {
        let transports: Vec<TransportKind> = loaded
            .endpoints
            .iter()
            .map(|entry| entry.transport)
            .collect();
        let transport = transports.get(endpoint).copied().ok_or_else(|| {
            SimGap(format!(
                "sim-replay: scenario {scenario:?} has {} endpoint(s); no endpoint {endpoint}",
                transports.len()
            ))
        })?;
        Ok(Self {
            source: format!("scenario {scenario:?}"),
            endpoint_transports: transports,
            transport,
            profiles: loaded.profiles.clone(),
            post_write: loaded.post_write.clone(),
            battery: loaded.battery,
        })
    }

    /// Transports of every endpoint in this scenario (this instance
    /// serves the entry at its endpoint index).
    #[must_use]
    pub fn scenario_transports(&self) -> &[TransportKind] {
        &self.endpoint_transports
    }
}

/// Resolves a scenario name to a fixture path: `stock`, `tuned`,
/// `foreign-token`, and `two-ble-unknown` map to the shipped fixtures;
/// anything else is treated as a direct file path.
fn resolve_scenario_path(scenario: &str) -> Result<std::path::PathBuf, SimGap> {
    match scenario {
        "stock" | "tuned" | "foreign-token" | "two-ble-unknown" => {
            Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/sim")
                .join(format!("{scenario}.toml")))
        }
        path => {
            let candidate = Path::new(path);
            if candidate.is_file() {
                Ok(candidate.to_owned())
            } else {
                Err(SimGap(format!(
                    "sim-replay: unknown scenario {scenario:?}; expected \"stock\", \"tuned\", \
                     \"foreign-token\", \"two-ble-unknown\", or a scenario file path"
                )))
            }
        }
    }
}

impl SimBackend for SimReplay {
    fn read_profile(&self, profile: u8) -> Result<ProfileImage, SimGap> {
        ProfileId::try_from(profile).map_err(|_| {
            SimGap(format!(
                "{}: invalid profile {profile}; expected 1..={}",
                self.source,
                ProfileId::MAX
            ))
        })?;
        if self.transport == TransportKind::Ble {
            return Err(SimGap(format!(
                "{}: read_profile({profile}) isn't supported over Ble; \
                 BLE has no readback path (like hardware)",
                self.source
            )));
        }
        self.profiles.get(&profile).cloned().ok_or_else(|| {
            SimGap(format!(
                "{}: no recorded image for profile {profile}; \
                 missing capture (replay serves recorded images only)",
                self.source
            ))
        })
    }

    fn write_profile(&mut self, profile: u8, image: &ProfileImage) -> Result<(), SimGap> {
        ProfileId::try_from(profile).map_err(|_| {
            SimGap(format!(
                "{}: invalid profile {profile}; expected 1..={}",
                self.source,
                ProfileId::MAX
            ))
        })?;
        let expected = self.post_write.get(&profile).ok_or_else(|| {
            SimGap(format!(
                "{}: no recorded post-write image for profile {profile}; \
                 missing capture for write verification (replay never guesses)",
                self.source
            ))
        })?;
        if image != expected {
            let describe = |image: &ProfileImage| {
                let dpi = DpiReport::encode(&image.0, self.transport)
                    .map(|report| hex_bytes(report.as_bytes()))
                    .unwrap_or_else(|error| format!("unencodable: {error}"));
                let preferences = PreferencesReport::encode_framed_for_transport(
                    &image.1,
                    self.transport,
                    PreferencesFraming::Compact,
                )
                .map(|report| hex_bytes(report.as_bytes()))
                .unwrap_or_else(|error| format!("unencodable: {error}"));
                let buttons = hex_bytes(ButtonsReport::encode(&image.2).as_bytes());
                format!("dpi={dpi} preferences={preferences} buttons={buttons}")
            };
            return Err(SimGap(format!(
                "{}: write to profile {profile} doesn't match the recorded post-write image; \
                 got {} expected {}",
                self.source,
                describe(image),
                describe(expected)
            )));
        }
        Ok(())
    }

    fn battery(&self) -> Option<u8> {
        self.battery
    }

    fn transport(&self) -> TransportKind {
        self.transport
    }

    fn tag(&self) -> &'static str {
        Self::REPLAY_TAG
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DpiState, PreferencesState, StageIndex};

    fn replay(scenario: &str) -> SimReplay {
        SimReplay::load(scenario).expect("shipped scenario loads")
    }

    #[test]
    fn served_images_match_fixture_bytes() {
        let backend = replay("stock");
        let scenario = load_scenario(&resolve_scenario_path("stock").expect("stock resolves"))
            .expect("stock parses");
        for profile in 1u8..=5 {
            let served = backend.read_profile(profile).expect("profile serves");
            let recorded = scenario
                .profiles
                .get(&profile)
                .expect("fixture has profile");
            assert_eq!(&served, recorded, "profile {profile} served byte-exact");
            let transport = backend.transport();
            assert_eq!(
                DpiReport::encode(&served.0, transport)
                    .expect("encodes")
                    .as_bytes(),
                DpiReport::encode(&recorded.0, transport)
                    .expect("encodes")
                    .as_bytes(),
                "profile {profile} DPI bytes identical"
            );
            assert_eq!(
                ButtonsReport::encode(&served.2).as_bytes(),
                ButtonsReport::encode(&recorded.2).as_bytes(),
                "profile {profile} button bytes identical"
            );
        }
    }

    #[test]
    fn stock_matches_capture_helpers() {
        let backend = replay("stock");
        let id = ProfileId::try_from(1u8).expect("profile 1");
        let expected_dpi = DpiState::captured_stock_reset(id).expect("stock DPI");
        let expected_prefs = PreferencesState::captured_stock_reset(id);
        let expected_buttons = crate::ButtonsState::default_for_profile(id);
        let served = backend.read_profile(1).expect("profile 1 serves");
        assert_eq!(served.0, expected_dpi);
        assert_eq!(served.1, expected_prefs);
        assert_eq!(served.2, expected_buttons);
    }

    #[test]
    fn tuned_profile_one_matches_model_contract() {
        let backend = replay("tuned");
        let served = backend.read_profile(1).expect("tuned profile 1 serves");
        let stages: Vec<u16> = served.0.stages.iter().map(|stage| stage.get()).collect();
        assert_eq!(stages, vec![400, 800, 1600, 3200, 6400]);
        assert_eq!(
            served.0.active_stage,
            StageIndex::try_from(3u8).expect("stage 3")
        );
        assert_eq!(served.0.preserved_tail, [0u8; WATERMARK_LENGTH_TEST]);
        assert_eq!(
            served.2.slots[3],
            crate::ButtonAssignment::new(0x0f, 0x00, 0x00)
        );
    }

    #[test]
    fn foreign_token_profile_one_has_watermark() {
        let backend = replay("foreign-token");
        let served = backend.read_profile(1).expect("profile 1 serves");
        let id = served
            .0
            .physical_id()
            .physical_id()
            .expect("foreign token decodes");
        assert_eq!(
            id.token_bytes(),
            [
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, //
                0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
            ]
        );
    }

    #[test]
    fn ble_endpoints_refuse_reads_like_hardware() {
        let backend = SimReplay::load_endpoint("two-ble-unknown", 0).expect("endpoint 0 loads");
        assert_eq!(backend.transport(), TransportKind::Ble);
        let error = backend.read_profile(1).expect_err("BLE refuses reads");
        let rendered = format!("{error:?}");
        assert!(rendered.contains("Ble"), "gap names transport: {rendered}");
        let second = SimReplay::load_endpoint("two-ble-unknown", 1).expect("endpoint 1 loads");
        assert_eq!(second.transport(), TransportKind::Ble);
        assert!(second.read_profile(1).is_err());
        assert_eq!(
            SimReplay::endpoint_transports("two-ble-unknown").expect("transports"),
            vec![TransportKind::Ble, TransportKind::Ble]
        );
    }

    #[test]
    fn missing_capture_gaps_are_loud_and_named() {
        let backend = SimReplay::load_endpoint("two-ble-unknown", 0).expect("ble loads");
        assert_eq!(backend.battery(), None);
        // BLE refuses first (like hardware); a wired backend with no
        // captures names the missing profile image instead.
        let rendered = format!("{:?}", backend.read_profile(1).expect_err("refused"));
        assert!(rendered.contains("Ble"), "gap names transport: {rendered}");

        let mut wired = replay("stock");
        // No post-write captures ship: every write gaps loudly.
        let image = wired.read_profile(1).expect("read serves");
        let error = wired
            .write_profile(1, &image)
            .expect_err("write without post-write capture gaps");
        let rendered = format!("{error:?}");
        assert!(
            rendered.contains("post-write"),
            "gap names capture: {rendered}"
        );
        assert!(
            rendered.contains("profile 1"),
            "gap names profile: {rendered}"
        );

        // Unknown scenario names the scenario.
        let error = SimReplay::load("nope").expect_err("unknown scenario");
        assert!(
            format!("{error:?}").contains("nope"),
            "gap names scenario: {error:?}"
        );
        // Endpoint past the end names the index.
        let error = SimReplay::load_endpoint("stock", 3).expect_err("missing endpoint");
        assert!(
            format!("{error:?}").contains("endpoint 3"),
            "gap names endpoint: {error:?}"
        );
        // Invalid profile numbers rejected.
        assert!(wired.read_profile(0).is_err());
        assert!(wired.read_profile(6).is_err());
    }

    #[test]
    fn tag_string_exact() {
        assert_eq!(replay("stock").tag(), "[SIM-REPLAY]");
    }

    #[test]
    fn battery_serves_recorded_levels() {
        assert_eq!(replay("stock").battery(), Some(87));
        assert_eq!(replay("tuned").battery(), Some(64));
        assert_eq!(replay("foreign-token").battery(), Some(42));
    }

    const WATERMARK_LENGTH_TEST: usize = crate::WATERMARK_LENGTH;
}
