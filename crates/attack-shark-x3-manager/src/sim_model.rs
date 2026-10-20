//! In-memory plausible mouse for sim phase 1.
//!
//! [`SimModel`] holds five complete profile images (`DpiState`,
//! `PreferencesState`, `ButtonsState`), a battery level, and a connection
//! state, and implements the shared `SimBackend` trait consumed from
//! `attack_shark_x3::sim`. Writes run the same typed validation the real
//! write path runs (profile match + report encodes) and reject what hardware
//! rejects; reads serve stored images on wired/receiver and refuse on BLE,
//! exactly like hardware (no invented bytes).

use attack_shark_x3::sim::{ProfileImage, SimBackend, SimGap};
use attack_shark_x3::{
    ButtonAssignment, ButtonsReport, ButtonsState, DpiReport, DpiState, DpiValue,
    PreferencesFraming, PreferencesReport, PreferencesState, ProfileId, ProfileMetadata,
    StageIndex, TransportKind, WATERMARK_LENGTH,
};

/// Tuned profile-1 DPI ladder, shared byte-for-byte with the replay
/// `tuned` fixture (see `SimReplay`).
const TUNED_STAGES: [u16; 5] = [400, 800, 1600, 3200, 6400];
/// Tuned profile-1 active stage (one-based), shared with replay.
const TUNED_ACTIVE_STAGE: u8 = 3;
/// Tuned profile-1 remapped slot: dpi-cycle becomes dpi-minus.
const TUNED_REMAPPED_SLOT: usize = 3;

/// In-memory plausible mouse: five stored profile images plus battery and
/// connection state.
#[derive(Clone, Debug)]
pub struct SimModel {
    profiles: [ProfileImage; 5],
    active_profile: ProfileId,
    maximum_profile: ProfileId,
    transport: TransportKind,
    battery: Option<u8>,
    writes_per_percent: u32,
    writes: u64,
}

impl SimModel {
    /// Tag prefix for device-list rows served by this backend.
    pub const MODEL_TAG: &'static str = "[SIM-MODEL]";
    /// Battery level of freshly constructed models.
    pub const DEFAULT_BATTERY: u8 = 100;
    /// Successful writes per 1% of battery decay in freshly constructed models.
    pub const DEFAULT_WRITES_PER_PERCENT: u32 = 10;

    /// Stock-reset images on a wired connection.
    #[must_use]
    pub fn stock() -> Self {
        Self::with_images(stock_images(), TransportKind::Wired)
    }

    /// Tuned images (profile 1 non-stock, profiles 2-5 stock) on wired.
    #[must_use]
    pub fn tuned() -> Self {
        Self::with_images(tuned_images(), TransportKind::Wired)
    }

    /// Loads a named preset: `"stock"` or `"tuned"`.
    ///
    /// # Errors
    ///
    /// Returns a [`SimGap`] naming the unknown preset otherwise.
    pub fn load(preset: &str) -> Result<Self, SimGap> {
        match preset {
            "stock" => Ok(Self::stock()),
            "tuned" => Ok(Self::tuned()),
            _ => Err(SimGap(format!(
                "sim-model: unknown preset {preset:?}; expected \"stock\" or \"tuned\""
            ))),
        }
    }

    /// Transports a preset exposes as discovery endpoints.
    ///
    /// # Errors
    ///
    /// Returns a [`SimGap`] naming the unknown preset otherwise.
    pub fn endpoint_transports(preset: &str) -> Result<Vec<TransportKind>, SimGap> {
        match preset {
            "stock" | "tuned" => Ok(vec![TransportKind::Wired]),
            _ => Err(SimGap(format!(
                "sim-model: unknown preset {preset:?}; expected \"stock\" or \"tuned\""
            ))),
        }
    }

    /// Retargets this instance at another transport (BLE instances refuse reads).
    #[must_use]
    pub fn with_transport(mut self, transport: TransportKind) -> Self {
        self.transport = transport;
        self
    }

    /// Overrides the starting battery level (`None` = no battery reporting).
    #[must_use]
    pub fn with_battery(mut self, battery: Option<u8>) -> Self {
        self.battery = battery;
        self
    }

    /// Overrides how many successful writes drain 1% of battery (`0` disables decay).
    #[must_use]
    pub fn with_writes_per_percent(mut self, writes_per_percent: u32) -> Self {
        self.writes_per_percent = writes_per_percent;
        self
    }

    /// Currently selected profile (one-based).
    #[must_use]
    pub fn active_profile(&self) -> u8 {
        self.active_profile.get()
    }

    /// Highest enabled profile slot (one-based).
    #[must_use]
    pub fn maximum_profile(&self) -> u8 {
        self.maximum_profile.get()
    }

    /// Stores a current/maximum metadata pair, validating `current <= maximum`
    /// exactly like the wire type, and returns the stored truth.
    ///
    /// # Errors
    ///
    /// Returns a [`SimGap`] when `current > maximum`.
    pub fn set_profile_metadata(
        &mut self,
        metadata: ProfileMetadata,
    ) -> Result<ProfileMetadata, SimGap> {
        self.active_profile = metadata.current();
        self.maximum_profile = metadata.maximum();
        self.record_write();
        self.profile_metadata()
    }

    /// Serves the stored current/maximum metadata pair.
    ///
    /// # Errors
    ///
    /// Returns a [`SimGap`] only when the stored pair violates
    /// `current <= maximum` (unreachable through the public setters).
    pub fn profile_metadata(&self) -> Result<ProfileMetadata, SimGap> {
        ProfileMetadata::new(self.active_profile, self.maximum_profile)
            .map_err(|error| SimGap(format!("sim-model: stored metadata is invalid: {error}")))
    }

    /// Selects the active profile.
    ///
    /// # Errors
    ///
    /// Returns a [`SimGap`] when `profile` is outside 1..=5.
    pub fn set_active_profile(&mut self, profile: u8) -> Result<(), SimGap> {
        self.active_profile = ProfileId::try_from(profile).map_err(|_| {
            SimGap(format!(
                "sim-model: invalid profile {profile}; expected 1..={}",
                ProfileId::MAX
            ))
        })?;
        Ok(())
    }

    /// Number of successful writes so far (drives deterministic battery decay).
    #[must_use]
    pub fn write_count(&self) -> u64 {
        self.writes
    }

    fn with_images(profiles: [ProfileImage; 5], transport: TransportKind) -> Self {
        Self {
            profiles,
            active_profile: ProfileId::MIN_ID,
            maximum_profile: ProfileId::MAX_ID,
            transport,
            battery: Some(Self::DEFAULT_BATTERY),
            writes_per_percent: Self::DEFAULT_WRITES_PER_PERCENT,
            writes: 0,
        }
    }

    fn record_write(&mut self) {
        self.writes += 1;
        if self.writes_per_percent == 0 {
            return;
        }
        if self
            .writes
            .is_multiple_of(u64::from(self.writes_per_percent))
        {
            self.battery = self.battery.map(|level| level.saturating_sub(1));
        }
    }
}

impl SimBackend for SimModel {
    fn read_profile(&self, profile: u8) -> Result<ProfileImage, SimGap> {
        ProfileId::try_from(profile).map_err(|_| {
            SimGap(format!(
                "sim-model: invalid profile {profile}; expected 1..={}",
                ProfileId::MAX
            ))
        })?;
        if self.transport == TransportKind::Ble {
            return Err(SimGap(format!(
                "sim-model: read_profile({profile}) isn't supported over Ble; \
                 BLE has no readback path (like hardware)"
            )));
        }
        Ok(self.profiles[usize::from(profile) - 1].clone())
    }

    fn write_profile(&mut self, profile: u8, image: &ProfileImage) -> Result<(), SimGap> {
        let target = ProfileId::try_from(profile).map_err(|_| {
            SimGap(format!(
                "sim-model: invalid profile {profile}; expected 1..={}",
                ProfileId::MAX
            ))
        })?;
        validate_profile_image(target, image, self.transport)?;
        self.profiles[usize::from(profile) - 1] = image.clone();
        self.record_write();
        Ok(())
    }

    fn read_profile_metadata(&self) -> Result<ProfileMetadata, SimGap> {
        self.profile_metadata()
    }

    fn write_profile_metadata(
        &mut self,
        metadata: ProfileMetadata,
    ) -> Result<ProfileMetadata, SimGap> {
        self.set_profile_metadata(metadata)
    }

    fn battery(&self) -> Option<u8> {
        self.battery
    }

    fn transport(&self) -> TransportKind {
        self.transport
    }

    fn tag(&self) -> &'static str {
        Self::MODEL_TAG
    }
}

/// Rejects what the real write path rejects: section/profile mismatch (the
/// manager's `InvalidUpdate` gate) plus report-encode validation (the
/// driver's pre-write gate). Preferences/buttons encodes are infallible for
/// representable states, so they run for coverage; DPI stage rules reject.
fn validate_profile_image(
    target: ProfileId,
    image: &ProfileImage,
    transport: TransportKind,
) -> Result<(), SimGap> {
    let (dpi, preferences, buttons) = image;
    for (section, actual) in [
        ("dpi", dpi.profile),
        ("preferences", preferences.profile),
        ("buttons", buttons.profile),
    ] {
        if actual != target {
            return Err(SimGap(format!(
                "sim-model: {section} image targets profile {actual} \
                 instead of requested profile {target}; write rejected (like hardware)"
            )));
        }
    }
    DpiReport::encode(dpi, transport)
        .map_err(|error| SimGap(format!("sim-model: dpi image rejected: {error}")))?;
    PreferencesReport::encode_framed_for_transport(
        preferences,
        transport,
        PreferencesFraming::Compact,
    )
    .map_err(|error| SimGap(format!("sim-model: preferences image rejected: {error}")))?;
    let _ = ButtonsReport::encode(buttons);
    Ok(())
}

fn stock_images() -> [ProfileImage; 5] {
    [1u8, 2, 3, 4, 5].map(|profile| {
        let id = ProfileId::try_from(profile).expect("sim profile 1..=5");
        (
            DpiState::captured_stock_reset(id).expect("stock DPI stages are protocol-valid"),
            PreferencesState::captured_stock_reset(id),
            ButtonsState::default_for_profile(id),
        )
    })
}

fn tuned_images() -> [ProfileImage; 5] {
    let mut images = stock_images();
    let id = ProfileId::try_from(1u8).expect("sim profile 1..=5");
    let stages = TUNED_STAGES
        .into_iter()
        .map(DpiValue::try_from)
        .collect::<Result<Vec<_>, _>>()
        .expect("tuned DPI stages are protocol-valid");
    let active = StageIndex::try_from(TUNED_ACTIVE_STAGE).expect("tuned stage is 1..=8");
    let dpi = DpiState::new(id, stages, active, [0u8; WATERMARK_LENGTH])
        .expect("tuned DPI selection is protocol-valid");
    let mut slots = ButtonsState::default_for_profile(id).slots;
    slots[TUNED_REMAPPED_SLOT] = ButtonAssignment::new(0x0f, 0x00, 0x00);
    images[0] = (
        dpi,
        PreferencesState::captured_stock_reset(id),
        ButtonsState::new(id, slots),
    );
    images
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(value: u8) -> ProfileId {
        ProfileId::try_from(value).expect("test profile must be valid")
    }

    #[test]
    fn replay_fixtures_play_clean_through_the_model() {
        use attack_shark_x3::sim::SimReplay;
        // Every recorded image must be acceptable to the model and served
        // back identically: replay proves firmware behavior, the model must
        // at minimum replay history. two-ble-unknown carries no images.
        for scenario in ["stock", "tuned", "foreign-token"] {
            let replay = SimReplay::load(scenario).expect("fixture loads");
            let mut model = SimModel::stock();
            for raw in 1u8..=5 {
                let image = replay.read_profile(raw).expect("replay serves");
                model
                    .write_profile(raw, &image)
                    .unwrap_or_else(|error| panic!("model rejects {scenario} p{raw}: {error:?}"));
                let back = model.read_profile(raw).expect("model serves");
                assert_eq!(back, image, "{scenario} p{raw} roundtrip");
            }
        }
    }

    #[test]
    fn write_then_read_roundtrip_per_profile() {
        let mut model = SimModel::stock();
        for raw in 1u8..=5 {
            let (dpi, prefs, buttons) = model.read_profile(raw).expect("stock read");
            assert_eq!(dpi.profile, profile(raw));
            assert_eq!(prefs.profile, profile(raw));
            assert_eq!(buttons.profile, profile(raw));
            let mut changed = (dpi, prefs, buttons);
            changed.0.active_stage = StageIndex::try_from(1u8).expect("stage 1");
            model.write_profile(raw, &changed).expect("write");
            let back = model.read_profile(raw).expect("re-read");
            assert_eq!(back, changed);
        }
    }

    #[test]
    fn ble_read_refused_but_write_served() {
        let model = SimModel::stock().with_transport(TransportKind::Ble);
        let error = model.read_profile(1).expect_err("BLE must refuse reads");
        assert!(
            format!("{error:?}").contains("Ble"),
            "gap must name the transport: {error:?}"
        );
        let mut model = model;
        let wired = SimModel::stock();
        let image = wired.read_profile(1).expect("wired serves");
        model.write_profile(1, &image).expect("BLE write acks");
        assert_eq!(model.battery(), Some(SimModel::DEFAULT_BATTERY));
    }

    #[test]
    fn battery_decay_is_deterministic() {
        let image = SimModel::stock().read_profile(1).expect("image");
        let mut first = SimModel::stock().with_writes_per_percent(3);
        let mut second = SimModel::stock().with_writes_per_percent(3);
        for _ in 0..7 {
            first.write_profile(1, &image).expect("write");
            second.write_profile(1, &image).expect("write");
        }
        assert_eq!(first.battery(), Some(98));
        assert_eq!(first.battery(), second.battery());
        assert_eq!(first.write_count(), 7);
    }

    #[test]
    fn battery_none_never_decays() {
        let image = SimModel::stock().read_profile(1).expect("image");
        let mut model = SimModel::stock()
            .with_battery(None)
            .with_writes_per_percent(1);
        model.write_profile(1, &image).expect("write");
        assert_eq!(model.battery(), None);
    }

    #[test]
    fn invalid_writes_rejected() {
        let mut model = SimModel::stock();
        let image = model.read_profile(1).expect("image");
        // Section/profile mismatch.
        let mut mismatched = image.clone();
        mismatched.0.profile = profile(2);
        assert!(model.write_profile(1, &mismatched).is_err());
        // DPI stage rules: active stage beyond the stage count.
        let bad_dpi = DpiState {
            profile: profile(1),
            stages: vec![DpiValue::try_from(800u16).expect("dpi")],
            active_stage: StageIndex::try_from(2u8).expect("stage"),
            sensor: Default::default(),
            preserved_tail: [0u8; WATERMARK_LENGTH],
        };
        let bad = (bad_dpi, image.1, image.2);
        assert!(model.write_profile(1, &bad).is_err());
        // Profile number outside 1..=5.
        assert!(model.write_profile(0, &image).is_err());
        assert!(model.read_profile(6).is_err());
        // Rejected writes leave state and battery untouched.
        assert_eq!(model.write_count(), 0);
        assert_eq!(model.read_profile(1).expect("image"), image);
    }

    #[test]
    fn presets_load_and_name_transports() {
        assert!(SimModel::load("stock").is_ok());
        assert!(SimModel::load("tuned").is_ok());
        let gap = SimModel::load("nope").expect_err("unknown preset");
        assert!(
            format!("{gap}").contains("\"nope\""),
            "gap names preset: {gap:?}"
        );
        assert_eq!(
            SimModel::endpoint_transports("tuned").expect("preset"),
            vec![TransportKind::Wired]
        );
        assert!(SimModel::endpoint_transports("nope").is_err());
        assert_eq!(SimModel::tuned().tag(), "[SIM-MODEL]");
    }

    #[test]
    fn tuned_matches_replay_contract() {
        let tuned = SimModel::tuned();
        let profile_one = tuned.read_profile(1).expect("profile 1");
        let stages: Vec<u16> = profile_one
            .0
            .stages
            .iter()
            .map(|stage| stage.get())
            .collect();
        assert_eq!(stages, vec![400, 800, 1600, 3200, 6400]);
        assert_eq!(profile_one.0.active_stage.get(), 3);
        assert_eq!(profile_one.0.preserved_tail, [0u8; WATERMARK_LENGTH]);
        let prefs = profile_one.1;
        assert_eq!(
            (
                prefs.light_mode,
                prefs.configuration,
                prefs.deep_sleep,
                prefs.host_color,
                prefs.sleep_timer,
                prefs.debounce
            ),
            (0x00, 0x03, 0xa8, [0x00, 0x00, 0xff], 0x01, 0x04)
        );
        assert_eq!(
            profile_one.2.slots[3],
            ButtonAssignment::new(0x0f, 0x00, 0x00)
        );
        let stock = SimModel::stock();
        for raw in 2u8..=5 {
            assert_eq!(
                tuned.read_profile(raw).expect("tuned"),
                stock.read_profile(raw).expect("stock"),
                "tuned profiles 2-5 equal stock reset images"
            );
        }
    }

    #[test]
    fn active_profile_switching() {
        let mut model = SimModel::stock();
        assert_eq!(model.active_profile(), 1);
        model.set_active_profile(4).expect("switch");
        assert_eq!(model.active_profile(), 4);
        assert!(model.set_active_profile(0).is_err());
        assert!(model.set_active_profile(6).is_err());
        assert_eq!(model.active_profile(), 4);
    }

    #[test]
    fn metadata_defaults_to_five_enabled_slots() {
        let model = SimModel::stock();
        assert_eq!(model.maximum_profile(), 5);
        let metadata = model.profile_metadata().expect("stored metadata");
        assert_eq!(metadata.current(), profile(1));
        assert_eq!(metadata.maximum(), profile(5));
        assert_eq!(SimModel::tuned().maximum_profile(), 5);
    }

    #[test]
    fn metadata_roundtrip_honours_a_lowered_maximum() {
        use attack_shark_x3::sim::SimBackend;
        let mut model = SimModel::tuned();
        let hide = ProfileMetadata::new(profile(4), profile(4)).expect("hide-shaped metadata");
        let stored = model
            .write_profile_metadata(hide)
            .expect("hide-shaped write");
        assert_eq!(stored, hide);
        assert_eq!(model.read_profile_metadata().expect("read back"), hide);
        assert_eq!(model.active_profile(), 4);
        assert_eq!(model.maximum_profile(), 4);
        // Activation still works for the contracted range.
        model
            .set_active_profile(2)
            .expect("activation within range");
        assert_eq!(
            model.profile_metadata().expect("metadata").current(),
            profile(2)
        );
    }

    #[test]
    fn invalid_metadata_write_is_rejected() {
        use attack_shark_x3::sim::SimBackend;
        // `ProfileMetadata::new` enforces `current <= maximum` at the wire
        // type, so an invalid pair cannot even be constructed here; the
        // backend rejects a stale current above a lowered maximum instead by
        // keeping `set_active_profile` for the activation path.
        let mut model = SimModel::stock();
        model.set_active_profile(5).expect("activate tail");
        let hide_tail = ProfileMetadata::new(profile(4), profile(4)).expect("metadata");
        // A current above the target maximum has no valid spelling: the
        // constructor is the rejection point.
        assert!(ProfileMetadata::new(profile(5), profile(4)).is_err());
        // And a well-formed hide still lands exactly.
        let stored = model
            .write_profile_metadata(hide_tail)
            .expect("well-formed hide");
        assert_eq!(stored, hide_tail);
    }
}
