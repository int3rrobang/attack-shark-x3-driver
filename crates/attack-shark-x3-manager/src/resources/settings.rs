use attack_shark_x3::{
    ButtonsState, DpiState, PollingRate, PreferencesState, ProfileId, TransportKind,
};
use serde::{Deserialize, Serialize};

use crate::backend::SessionWrite;
use crate::device::DeviceId;
use crate::error::ManagerError;
use crate::manager::DeviceManager;
use crate::operation::{
    BaselineSource, ResourceSnapshot, UpdatePolicy, VerificationMethod, WriteOutcome,
};
use crate::resources::state::reconcile_observed;
use crate::state::{DesiredSource, ProfileState, StateFile};

/// Partial profile preferences to merge into a complete image.
///
/// Every field is optional. Omitted fields remain exactly as supplied by the
/// manager-selected complete baseline.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreferencesDelta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light_mode: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deep_sleep: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_color: Option<[u8; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sleep_timer: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debounce: Option<u8>,
}

impl PreferencesDelta {
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.light_mode.is_none()
            && self.configuration.is_none()
            && self.deep_sleep.is_none()
            && self.host_color.is_none()
            && self.sleep_timer.is_none()
            && self.debounce.is_none()
    }
}

/// The complete desired image a safe report-`0x06` write must be matched
/// against.
///
/// Report `0x06` skips the profile loader and its deferred writer can persist
/// the complete live image under the target alias, so the safe path never
/// emits it unless every non-rate section of the freshly read live profile
/// exactly equals this desired image.
pub(crate) struct CompleteDesiredImage {
    pub(crate) dpi: DpiState,
    pub(crate) preferences: PreferencesState,
    pub(crate) buttons: ButtonsState,
}

fn missing_section_baseline(resource: &'static str, profile: ProfileId) -> ManagerError {
    ManagerError::MissingBaseline {
        resource,
        profile: Some(profile),
    }
}

fn missing_metadata_baseline() -> ManagerError {
    ManagerError::MissingBaseline {
        resource: "profile metadata",
        profile: None,
    }
}

impl DeviceManager {
    /// Reads one profile's complete preferences image and records USB readback evidence.
    pub async fn read_preferences(
        &self,
        device: &DeviceId,
        profile: ProfileId,
    ) -> Result<ResourceSnapshot<PreferencesState>, ManagerError> {
        let (_identity, session, _guard) = self.open_locked(device, "read_preferences").await?;
        let transport = session.transport();
        if transport == TransportKind::Ble {
            return Err(unsupported("read_preferences", transport));
        }

        let value = session.read_preferences(profile).await?;
        let now = self.now();
        let device_id = device.clone();
        let resource = self
            .store()
            .mutate_async(move |state| {
                let device_state = state
                    .devices
                    .get_mut(&device_id)
                    .ok_or_else(|| ManagerError::DeviceNotFound(device_id.clone()))?;
                let profile_state = device_state
                    .profiles
                    .entry(profile)
                    .or_insert_with(ProfileState::empty);
                reconcile_observed(&mut profile_state.preferences, value, now);
                Ok::<_, ManagerError>(profile_state.preferences.clone())
            })
            .await??;

        Ok(ResourceSnapshot { resource })
    }

    /// Writes a complete preferences image, honoring the requested
    /// verification level.
    ///
    /// `Transport` completes on the transport-level acknowledgment and works
    /// over both USB and BLE. `Readback` requires USB readback evidence; BLE
    /// has no byte-for-byte readback path and rejects it.
    pub async fn update_preferences(
        &self,
        device: &DeviceId,
        profile: ProfileId,
        desired: PreferencesState,
        policy: UpdatePolicy,
    ) -> Result<WriteOutcome<PreferencesState>, ManagerError> {
        if desired.profile != profile {
            return Err(ManagerError::InvalidUpdate(
                "preferences state profile does not match requested profile".to_owned(),
            ));
        }

        let (_identity, session, _guard) = self.open_locked(device, "update_preferences").await?;
        let transport = session.transport();

        if transport == TransportKind::Ble
            && !policy.allow_explicit_defaults
            && !has_preferences_baseline(&self.store().load_async().await?, device, profile)
        {
            return Err(ManagerError::MissingBaseline {
                resource: "preferences",
                profile: Some(profile),
            });
        }

        let write = session
            .write_preferences(desired, policy.verification)
            .await?;
        let now = self.now();
        let device_id = device.clone();
        let outcome = self
            .store()
            .mutate_async(move |state| {
                crate::resources::state::persist_write(
                    state,
                    &device_id,
                    profile,
                    |ps| &mut ps.preferences,
                    desired,
                    write,
                    now,
                )
            })
            .await??;
        crate::resources::state::finish_write(outcome, "preferences", profile)
    }

    /// Applies a sparse preferences update after resolving a complete
    /// baseline.
    ///
    /// The baseline source follows `policy.baseline`, which defaults to
    /// `BaselineSource::Live`: the current profile image is read
    /// immediately before merging on every transport. With
    /// `BaselineSource::Stored`, the stored desired evidence is used first,
    /// then stored observed evidence, and only the legacy captured image
    /// when authorized by `policy.allow_explicit_defaults`; USB and
    /// receiver transports may resolve a stored baseline instead of
    /// reading live, while BLE always resolves from the store.
    pub async fn update_preferences_delta(
        &self,
        device: &DeviceId,
        profile: ProfileId,
        delta: PreferencesDelta,
        policy: UpdatePolicy,
    ) -> Result<WriteOutcome<PreferencesState>, ManagerError> {
        if delta.is_empty() {
            return Err(ManagerError::InvalidUpdate(
                "preferences delta must provide at least one field".to_owned(),
            ));
        }

        let (_identity, session, _guard) =
            self.open_locked(device, "update_preferences_delta").await?;
        let baseline = match session.transport() {
            TransportKind::Ble => {
                self.load_stored_preferences_baseline(
                    device,
                    profile,
                    policy.allow_explicit_defaults,
                )
                .await?
            }
            TransportKind::Wired | TransportKind::Receiver => match policy.baseline {
                BaselineSource::Live => session.read_preferences(profile).await?,
                BaselineSource::Stored => {
                    self.load_stored_preferences_baseline(device, profile, false)
                        .await?
                }
            },
        };

        let desired = merge_preferences_delta(baseline, delta);
        let write = session
            .write_preferences(desired, policy.verification)
            .await?;
        let now = self.now();
        let device_id = device.clone();
        let outcome = self
            .store()
            .mutate_async(move |state| {
                crate::resources::state::persist_write(
                    state,
                    &device_id,
                    profile,
                    |ps| &mut ps.preferences,
                    desired,
                    write,
                    now,
                )
            })
            .await??;
        crate::resources::state::finish_write(outcome, "preferences", profile)
    }

    /// Resolves the complete preferences baseline from the durable store
    /// without any live read.
    ///
    /// The stored desired evidence is used first (unless it is explicit
    /// defaults and not authorized by `allow_explicit_defaults`), then the
    /// stored observed evidence, and only the legacy captured image when
    /// authorized by `allow_explicit_defaults`. With no usable stored
    /// baseline and no capture authorization, `ManagerError::MissingBaseline`
    /// is returned.
    pub(crate) async fn load_stored_preferences_baseline(
        &self,
        device: &DeviceId,
        profile: ProfileId,
        allow_explicit_defaults: bool,
    ) -> Result<PreferencesState, ManagerError> {
        let state = self.store().load_async().await?;
        let resource = state
            .devices
            .get(device)
            .and_then(|device_state| device_state.profiles.get(&profile))
            .map(|profile_state| &profile_state.preferences);

        if let Some(resource) = resource {
            if let Some(desired) = resource.desired.as_ref()
                && (allow_explicit_defaults || desired.source != DesiredSource::ExplicitDefaults)
            {
                return Ok(desired.value);
            }
            if let Some(observed) = resource.observed.as_ref() {
                return Ok(observed.value);
            }
        }

        if allow_explicit_defaults {
            return Ok(captured_evidence_preferences(profile));
        }

        Err(ManagerError::MissingBaseline {
            resource: "preferences",
            profile: Some(profile),
        })
    }

    /// Reads the live polling rate and records USB readback evidence.
    ///
    /// Report `0x06` skips the profile loader: the supplied alias is a wire
    /// side effect while the returned rate comes from the current live image.
    /// This method first loads the complete target profile in the same guarded
    /// session, then reads and persists the live rate under that profile.
    pub async fn read_polling_rate(
        &self,
        device: &DeviceId,
        profile: ProfileId,
    ) -> Result<ResourceSnapshot<PollingRate>, ManagerError> {
        let (_identity, session, _guard) = self.open_locked(device, "read_polling_rate").await?;
        let transport = session.transport();
        if transport == TransportKind::Ble {
            return Err(unsupported("read_live_polling_rate", transport));
        }
        session.read_profile(profile).await?;
        let value = session.read_live_polling_rate(profile).await?;
        let now = self.now();
        let device_id = device.clone();
        let resource = self
            .store()
            .mutate_async(move |state| {
                let device_state = state
                    .devices
                    .get_mut(&device_id)
                    .ok_or_else(|| ManagerError::DeviceNotFound(device_id.clone()))?;
                let profile_state = device_state
                    .profiles
                    .entry(profile)
                    .or_insert_with(ProfileState::empty);
                reconcile_observed(&mut profile_state.polling_rate, value, now);
                Ok::<_, ManagerError>(profile_state.polling_rate.clone())
            })
            .await??;

        Ok(ResourceSnapshot { resource })
    }

    /// Writes the polling rate for the explicit profile through the
    /// safe-by-default USB path.
    ///
    /// Report `0x06` skips the profile loader and its deferred writer can
    /// persist the complete live image under the target alias. This method
    /// therefore writes only after resolving the complete desired
    /// DPI/preferences/buttons image for the target profile, requiring the
    /// persistent profile metadata to name the target as current, freshly
    /// reading the complete profile in this same session, and comparing every
    /// non-rate section exactly. When the live rate already equals the
    /// desired rate, no hardware write occurs and the fresh read is recorded
    /// as matching observed evidence. BLE has no such preflight and is
    /// refused until the caller opts into
    /// [`update_polling_rate_unverified_ble`](Self::update_polling_rate_unverified_ble).
    pub async fn update_polling_rate(
        &self,
        device: &DeviceId,
        profile: ProfileId,
        desired: PollingRate,
        policy: UpdatePolicy,
    ) -> Result<WriteOutcome<PollingRate>, ManagerError> {
        let (_identity, session, _guard) = self.open_locked(device, "update_polling_rate").await?;
        let transport = session.transport();
        if transport == TransportKind::Ble {
            return Err(ManagerError::ExplicitAuthorizationRequired {
                operation: "update_polling_rate",
                transport: TransportKind::Ble,
            });
        }

        // Resolve the complete desired image from durable manager state
        // before any hardware traffic: an observed-only targeted rate read is
        // not trustworthy provenance for a write that can persist the live
        // image under the target alias.
        let image = self.require_complete_desired_image(device, profile).await?;

        // Fresh complete profile read in the same session; every non-rate
        // section must exactly match the desired image.
        let snapshot = session.read_profile(profile).await?;
        if snapshot.dpi != image.dpi {
            return Err(missing_section_baseline("DPI", profile));
        }
        if snapshot.preferences != image.preferences {
            return Err(missing_section_baseline("preferences", profile));
        }
        if snapshot.buttons != image.buttons {
            return Err(missing_section_baseline("buttons", profile));
        }

        // Avoid a redundant 0x06 write when the live rate already matches:
        // the fresh read is honest matching observed evidence.
        let current = session.read_live_polling_rate(profile).await?;
        if current == desired {
            let write = SessionWrite::ReadbackVerified(current);
            let now = self.now();
            let device_id = device.clone();
            let outcome = self
                .store()
                .mutate_async(move |state| {
                    crate::resources::state::persist_write(
                        state,
                        &device_id,
                        profile,
                        |ps| &mut ps.polling_rate,
                        desired,
                        write,
                        now,
                    )
                })
                .await??;
            return crate::resources::state::finish_write(outcome, "polling rate", profile);
        }

        let write = session
            .write_polling_rate_unchecked(profile, desired, policy.verification)
            .await?;
        let now = self.now();
        let device_id = device.clone();
        let outcome = self
            .store()
            .mutate_async(move |state| {
                crate::resources::state::persist_write(
                    state,
                    &device_id,
                    profile,
                    |ps| &mut ps.polling_rate,
                    desired,
                    write,
                    now,
                )
            })
            .await??;
        crate::resources::state::finish_write(outcome, "polling rate", profile)
    }

    /// Performs a direct, unverified BLE polling-rate write as the explicit
    /// enthusiast escape hatch.
    ///
    /// This is the dangerous counterpart to
    /// [`update_polling_rate`](Self::update_polling_rate): it accepts
    /// transport validation only, performs the direct packet write, and
    /// returns ACK-only evidence with persistence `Unknown`. BLE has no
    /// readback path, so the parser ACK proves submission only. Non-BLE
    /// sessions and readback requests are rejected before any write.
    pub async fn update_polling_rate_unverified_ble(
        &self,
        device: &DeviceId,
        profile: ProfileId,
        desired: PollingRate,
        policy: UpdatePolicy,
    ) -> Result<WriteOutcome<PollingRate>, ManagerError> {
        let (_identity, session, _guard) = self
            .open_locked(device, "update_polling_rate_unverified_ble")
            .await?;
        let transport = session.transport();
        if transport != TransportKind::Ble {
            return Err(ManagerError::UnsupportedOperation {
                operation: "update_polling_rate_unverified_ble",
                transport,
            });
        }
        if policy.verification == VerificationMethod::Readback {
            return Err(ManagerError::UnsupportedOperation {
                operation: "update_polling_rate_unverified_ble",
                transport: TransportKind::Ble,
            });
        }

        let write = session
            .write_polling_rate_unchecked(profile, desired, policy.verification)
            .await?;
        let now = self.now();
        let device_id = device.clone();
        let outcome = self
            .store()
            .mutate_async(move |state| {
                crate::resources::state::persist_write(
                    state,
                    &device_id,
                    profile,
                    |ps| &mut ps.polling_rate,
                    desired,
                    write,
                    now,
                )
            })
            .await??;
        crate::resources::state::finish_write(outcome, "polling rate", profile)
    }

    /// Resolves the complete desired image a safe polling-rate write must be
    /// matched against.
    ///
    /// Requires desired DPI, preferences, and buttons state for the target
    /// profile plus persistent profile metadata naming the target as the
    /// current profile. Each missing section is reported by name before any
    /// write.
    pub(crate) async fn require_complete_desired_image(
        &self,
        device: &DeviceId,
        profile: ProfileId,
    ) -> Result<CompleteDesiredImage, ManagerError> {
        let state = self.store().load_async().await?;
        let device_state = state
            .devices
            .get(device)
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
        let profile_state = device_state.profiles.get(&profile);

        let dpi = profile_state
            .and_then(|profile_state| profile_state.dpi.desired.as_ref())
            .map(|desired| desired.value.clone())
            .ok_or_else(|| missing_section_baseline("DPI", profile))?;
        let preferences = profile_state
            .and_then(|profile_state| profile_state.preferences.desired.as_ref())
            .map(|desired| desired.value)
            .ok_or_else(|| missing_section_baseline("preferences", profile))?;
        let buttons = profile_state
            .and_then(|profile_state| profile_state.buttons.desired.as_ref())
            .map(|desired| desired.value)
            .ok_or_else(|| missing_section_baseline("buttons", profile))?;

        // The persistent metadata must show the target as the current
        // profile: the deferred writer serializes the live image into the
        // target alias.
        let metadata = device_state
            .profile_metadata
            .observed
            .as_ref()
            .map(|observed| observed.value)
            .or_else(|| {
                device_state
                    .profile_metadata
                    .desired
                    .as_ref()
                    .map(|desired| desired.value)
            })
            .ok_or_else(missing_metadata_baseline)?;
        if metadata.current() != profile {
            return Err(missing_metadata_baseline());
        }

        Ok(CompleteDesiredImage {
            dpi,
            preferences,
            buttons,
        })
    }
}

fn unsupported(operation: &'static str, transport: TransportKind) -> ManagerError {
    ManagerError::UnsupportedOperation {
        operation,
        transport,
    }
}

// Legacy capture-qualified preferences image. These bytes are only used by
// `captured_evidence_preferences` after explicit policy authorization; they
// are deliberately not a `Default` implementation.
const CAPTURED_EVIDENCE_LIGHT_MODE: u8 = 0x02;
const CAPTURED_EVIDENCE_CONFIGURATION: u8 = 0x01;
const CAPTURED_EVIDENCE_DEEP_SLEEP: u8 = 0x00;
const CAPTURED_EVIDENCE_HOST_COLOR: [u8; 3] = [0xff, 0x00, 0x00];
const CAPTURED_EVIDENCE_SLEEP_TIMER: u8 = 5;
const CAPTURED_EVIDENCE_DEBOUNCE: u8 = 0x00;

pub(crate) fn captured_evidence_preferences(profile: ProfileId) -> PreferencesState {
    PreferencesState::new(
        profile,
        CAPTURED_EVIDENCE_LIGHT_MODE,
        CAPTURED_EVIDENCE_CONFIGURATION,
        CAPTURED_EVIDENCE_DEEP_SLEEP,
        CAPTURED_EVIDENCE_HOST_COLOR,
        CAPTURED_EVIDENCE_SLEEP_TIMER,
        CAPTURED_EVIDENCE_DEBOUNCE,
    )
}

pub(crate) fn merge_preferences_delta(
    baseline: PreferencesState,
    delta: PreferencesDelta,
) -> PreferencesState {
    PreferencesState::new(
        baseline.profile,
        delta.light_mode.unwrap_or(baseline.light_mode),
        delta.configuration.unwrap_or(baseline.configuration),
        delta.deep_sleep.unwrap_or(baseline.deep_sleep),
        delta.host_color.unwrap_or(baseline.host_color),
        delta.sleep_timer.unwrap_or(baseline.sleep_timer),
        delta.debounce.unwrap_or(baseline.debounce),
    )
}

pub(crate) fn has_preferences_baseline(
    state: &StateFile,
    device: &DeviceId,
    profile: ProfileId,
) -> bool {
    state
        .devices
        .get(device)
        .and_then(|device_state| device_state.profiles.get(&profile))
        .is_some_and(|profile_state| {
            profile_state.preferences.desired.is_some()
                || profile_state.preferences.observed.is_some()
        })
}

#[cfg(test)]
mod tests {
    use super::{DeviceManager, PreferencesDelta};
    use crate::backend::{ScriptedFakeFactory, ScriptedFakeSession, ScriptedWrite};
    use crate::device::DeviceIdentity;
    use crate::error::ManagerError;
    use crate::state::{
        ApplicationVerification, DesiredSource, DesiredState, DeviceState, ObservationSource,
        ObservedState, PersistenceVerification, StatePaths, StateStore, Timestamp, Verification,
    };
    use crate::{BaselineSource, UpdatePolicy, VerificationMethod};
    use attack_shark_x3::{
        ButtonAssignment, ButtonsState, DpiState, DpiValue, PollingRate, PreferencesState,
        ProfileId, ProfileMetadata, StageIndex, TransportKind,
    };
    use std::sync::Arc;

    fn store(dir: &tempfile::TempDir) -> StateStore {
        StateStore::open(StatePaths::new(dir.path().join("state.json")))
    }

    fn usb_identity() -> DeviceIdentity {
        DeviceIdentity::test_usb(
            TransportKind::Wired,
            0x1d57,
            0xfa61,
            Some("SETTINGS-TEST"),
            r"\\?\hid#settings-test",
            Some("Settings test"),
        )
        .expect("valid USB identity")
    }

    fn ble_identity() -> DeviceIdentity {
        DeviceIdentity::test_ble("settings-ble-test", Some("Settings BLE test"))
            .expect("valid BLE identity")
    }

    fn snapshot(
        profile: ProfileId,
        preferences: PreferencesState,
    ) -> attack_shark_x3::driver::ProfileSnapshot {
        attack_shark_x3::driver::ProfileSnapshot {
            target_profile: profile,
            persistent_metadata: ProfileMetadata::new(profile, profile).expect("metadata"),
            dpi: DpiState::new(
                profile,
                vec![DpiValue::new(800).expect("DPI")],
                StageIndex::new(1).expect("stage"),
                [0; 25],
            )
            .expect("DPI state"),
            preferences,
            buttons: ButtonsState::new(
                profile,
                [ButtonAssignment::default();
                    attack_shark_x3::protocol::buttons::BUTTON_SLOT_COUNT],
            ),
        }
    }

    /// A fixed complete image distinct from the seeded baseline values.
    fn complete_image(profile: ProfileId) -> (DpiState, PreferencesState, ButtonsState) {
        let dpi = DpiState::new(
            profile,
            vec![DpiValue::new(800).expect("DPI")],
            StageIndex::new(1).expect("stage"),
            [0; 25],
        )
        .expect("DPI state");
        let preferences = PreferencesState::new(profile, 2, 1, 0, [0xff, 0, 0], 5, 4);
        let mut slots =
            [ButtonAssignment::default(); attack_shark_x3::protocol::buttons::BUTTON_SLOT_COUNT];
        slots[0] = ButtonAssignment::new(0x01, 0x02, 0x03);
        (dpi, preferences, ButtonsState::new(profile, slots))
    }

    fn matching_snapshot(
        profile: ProfileId,
        dpi: DpiState,
        preferences: PreferencesState,
        buttons: ButtonsState,
    ) -> attack_shark_x3::driver::ProfileSnapshot {
        attack_shark_x3::driver::ProfileSnapshot {
            target_profile: profile,
            persistent_metadata: ProfileMetadata::new(profile, profile).expect("metadata"),
            dpi,
            preferences,
            buttons,
        }
    }

    /// Seeds durable desired DPI/preferences/buttons plus profile metadata for
    /// the target profile, as a safe polling-rate write requires. The device
    /// entry is created with its exact identity when not yet registered.
    fn seed_complete_desired(
        store: &StateStore,
        identity: &DeviceIdentity,
        profile: ProfileId,
        metadata: ProfileMetadata,
        dpi: DpiState,
        preferences: PreferencesState,
        buttons: ButtonsState,
    ) {
        let mut transaction = store.transaction().unwrap();
        // Ensure nextDeviceNumber respects allocation semantics for the shared
        // mouse-999 fixture before commit.
        if let Some(num) = identity.id.number()
            && transaction.state().next_device_number <= num
        {
            transaction.state_mut().next_device_number = num + 1;
            assert!(
                transaction.state().next_device_number != 0,
                "nextDeviceNumber overflow"
            );
        }
        let device_state = transaction
            .state_mut()
            .devices
            .entry(identity.id.clone())
            .or_insert_with(|| DeviceState::new(identity.clone()));
        device_state.profile_metadata.desired = Some(DesiredState {
            value: metadata,
            source: DesiredSource::UserWrite,
            verification: Verification::not_sent(),
            updated_at: Timestamp::default(),
        });
        let profile_state = device_state.profiles.entry(profile).or_default();
        profile_state.dpi.desired = Some(DesiredState {
            value: dpi,
            source: DesiredSource::UserWrite,
            verification: Verification::not_sent(),
            updated_at: Timestamp::default(),
        });
        profile_state.preferences.desired = Some(DesiredState {
            value: preferences,
            source: DesiredSource::UserWrite,
            verification: Verification::not_sent(),
            updated_at: Timestamp::default(),
        });
        profile_state.buttons.desired = Some(DesiredState {
            value: buttons,
            source: DesiredSource::UserWrite,
            verification: Verification::not_sent(),
            updated_at: Timestamp::default(),
        });
        transaction.state().validate().unwrap();
        transaction.commit().unwrap();
    }

    #[tokio::test]
    async fn usb_preferences_readback_write_records_observed_evidence() {
        let dir = tempfile::tempdir().unwrap();

        let store = store(&dir);
        let identity = usb_identity();
        let device = identity.id.clone();
        let profile = ProfileId::new(1).unwrap();
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            ScriptedFakeSession::usb(),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        let desired = PreferencesState::new(profile, 1, 2, 3, [4, 5, 6], 7, 8);
        let outcome = manager
            .update_preferences(
                &device,
                profile,
                desired,
                UpdatePolicy {
                    verification: VerificationMethod::Readback,
                    ..UpdatePolicy::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(outcome.observed, Some(desired));
        assert_eq!(
            outcome.verification.application,
            ApplicationVerification::ReadbackVerified
        );
        assert_eq!(
            store.load().unwrap().devices[&device].profiles[&profile]
                .preferences
                .observed
                .as_ref()
                .unwrap()
                .source,
            ObservationSource::UsbReadback
        );
    }

    #[tokio::test]
    async fn usb_preferences_transport_write_records_acknowledged() {
        let dir = tempfile::tempdir().unwrap();

        let store = store(&dir);
        let identity = usb_identity();
        let device = identity.id.clone();
        let profile = ProfileId::new(1).unwrap();
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            ScriptedFakeSession::usb(),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        let desired = PreferencesState::new(profile, 1, 2, 3, [4, 5, 6], 7, 8);
        let outcome = manager
            .update_preferences(&device, profile, desired, UpdatePolicy::default())
            .await
            .unwrap();
        assert_eq!(outcome.observed, None);
        assert_eq!(
            outcome.verification.application,
            ApplicationVerification::Acknowledged
        );
        let persisted = store.load().unwrap();
        let preferences = &persisted.devices[&device].profiles[&profile].preferences;
        assert_eq!(preferences.desired.as_ref().unwrap().value, desired,);
        assert!(
            preferences.observed.is_none(),
            "transport acceptance must not fabricate readback evidence"
        );
    }

    #[tokio::test]
    async fn usb_preferences_delta_preserves_unmodified_fields() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = usb_identity();
        let device = identity.id.clone();
        let profile = ProfileId::new(1).unwrap();
        let baseline = PreferencesState::new(profile, 2, 1, 0, [0xff, 0, 0], 5, 4);
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            ScriptedFakeSession::usb().with_profile(snapshot(profile, baseline)),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        manager
            .update_preferences_delta(
                &device,
                profile,
                PreferencesDelta {
                    host_color: Some([1, 2, 3]),
                    ..PreferencesDelta::default()
                },
                UpdatePolicy::default(),
            )
            .await
            .unwrap();

        let actual = store.load().unwrap().devices[&device].profiles[&profile]
            .preferences
            .desired
            .as_ref()
            .unwrap()
            .value;
        assert_eq!(
            actual,
            PreferencesState::new(profile, 2, 1, 0, [1, 2, 3], 5, 4)
        );
    }

    #[tokio::test]
    async fn ble_preferences_delta_prefers_stored_desired_over_observed() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = ble_identity();
        let device = identity.id.clone();
        let profile = ProfileId::new(1).unwrap();
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            ScriptedFakeSession::ble(),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        let desired_baseline = PreferencesState::new(profile, 2, 1, 0, [0xff, 0, 0], 5, 4);
        let observed_baseline = PreferencesState::new(profile, 7, 8, 9, [1, 2, 3], 10, 11);
        let mut transaction = store.transaction().unwrap();
        let profile_state = transaction
            .state_mut()
            .devices
            .get_mut(&device)
            .unwrap()
            .profiles
            .entry(profile)
            .or_default();
        profile_state.preferences.desired = Some(DesiredState {
            value: desired_baseline,
            source: DesiredSource::UserWrite,
            verification: Verification::not_sent(),
            updated_at: Timestamp::default(),
        });
        profile_state.preferences.observed = Some(ObservedState {
            value: observed_baseline,
            source: ObservationSource::UsbReadback,
            observed_at: Timestamp::default(),
        });
        transaction.commit().unwrap();

        manager
            .update_preferences_delta(
                &device,
                profile,
                PreferencesDelta {
                    debounce: Some(9),
                    ..PreferencesDelta::default()
                },
                UpdatePolicy::default(),
            )
            .await
            .unwrap();

        let actual = store.load().unwrap().devices[&device].profiles[&profile]
            .preferences
            .desired
            .as_ref()
            .unwrap()
            .value;
        assert_eq!(
            actual,
            PreferencesState::new(profile, 2, 1, 0, [0xff, 0, 0], 5, 9)
        );
    }

    #[tokio::test]
    async fn ble_preferences_delta_requires_baseline_or_explicit_authorization() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = ble_identity();
        let device = identity.id.clone();
        let profile = ProfileId::new(1).unwrap();
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            ScriptedFakeSession::ble(),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();
        let delta = PreferencesDelta {
            light_mode: Some(7),
            ..PreferencesDelta::default()
        };

        let error = manager
            .update_preferences_delta(&device, profile, delta, UpdatePolicy::default())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ManagerError::MissingBaseline {
                resource: "preferences",
                profile: Some(p),
            } if p == profile
        ));

        manager
            .update_preferences_delta(
                &device,
                profile,
                delta,
                UpdatePolicy {
                    allow_explicit_defaults: true,
                    ..UpdatePolicy::default()
                },
            )
            .await
            .unwrap();
        let actual = store.load().unwrap().devices[&device].profiles[&profile]
            .preferences
            .desired
            .as_ref()
            .unwrap()
            .value;
        assert_eq!(
            actual,
            PreferencesState::new(profile, 7, 1, 0, [0xff, 0, 0], 5, 0)
        );
    }

    #[tokio::test]
    async fn usb_preferences_delta_stored_baseline_skips_live_read() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = usb_identity();
        let device = identity.id.clone();
        let profile = ProfileId::new(1).unwrap();
        // No scripted profile: any live `read_preferences` would fail, so the
        // update can only succeed by resolving the stored desired baseline.
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            ScriptedFakeSession::usb(),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        let stored_baseline = PreferencesState::new(profile, 2, 1, 0, [0xff, 0, 0], 5, 4);
        let mut transaction = store.transaction().unwrap();
        let profile_state = transaction
            .state_mut()
            .devices
            .get_mut(&device)
            .unwrap()
            .profiles
            .entry(profile)
            .or_default();
        profile_state.preferences.desired = Some(DesiredState {
            value: stored_baseline,
            source: DesiredSource::UserWrite,
            verification: Verification::not_sent(),
            updated_at: Timestamp::default(),
        });
        transaction.commit().unwrap();

        manager
            .update_preferences_delta(
                &device,
                profile,
                PreferencesDelta {
                    host_color: Some([1, 2, 3]),
                    ..PreferencesDelta::default()
                },
                UpdatePolicy {
                    baseline: BaselineSource::Stored,
                    ..UpdatePolicy::default()
                },
            )
            .await
            .unwrap();

        let actual = store.load().unwrap().devices[&device].profiles[&profile]
            .preferences
            .desired
            .as_ref()
            .unwrap()
            .value;
        assert_eq!(
            actual,
            PreferencesState::new(profile, 2, 1, 0, [1, 2, 3], 5, 4)
        );
    }

    #[tokio::test]
    async fn usb_preferences_delta_stored_baseline_requires_stored_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = usb_identity();
        let device = identity.id.clone();
        let profile = ProfileId::new(1).unwrap();
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            ScriptedFakeSession::usb(),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        let error = manager
            .update_preferences_delta(
                &device,
                profile,
                PreferencesDelta {
                    light_mode: Some(7),
                    ..PreferencesDelta::default()
                },
                UpdatePolicy {
                    baseline: BaselineSource::Stored,
                    ..UpdatePolicy::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ManagerError::MissingBaseline {
                resource: "preferences",
                profile: Some(p),
            } if p == profile
        ));
    }

    #[tokio::test]
    async fn preferences_delta_rejects_empty_update() {
        let dir = tempfile::tempdir().unwrap();
        let identity = ble_identity();
        let device = identity.id.clone();
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            ScriptedFakeSession::ble(),
        ));
        let manager = DeviceManager::with_store_and_factory(store(&dir), factory);
        manager.register_device(identity).unwrap();

        let error = manager
            .update_preferences_delta(
                &device,
                ProfileId::new(1).unwrap(),
                PreferencesDelta::default(),
                UpdatePolicy::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ManagerError::InvalidUpdate(_)));
    }

    #[tokio::test]
    async fn safe_ble_polling_write_requires_explicit_authorization() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = ble_identity();
        let device = identity.id.clone();
        let profile = ProfileId::new(1).unwrap();
        let session = ScriptedFakeSession::ble();
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            session.clone(),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        let error = manager
            .update_polling_rate(
                &device,
                profile,
                PollingRate::Hz500,
                UpdatePolicy::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ManagerError::ExplicitAuthorizationRequired {
                operation: "update_polling_rate",
                transport: TransportKind::Ble,
            }
        ));
        assert!(
            session.writes().is_empty(),
            "safe BLE refusal must not emit a polling write"
        );
        assert!(store.load().unwrap().devices[&device].profiles.is_empty());
    }

    #[tokio::test]
    async fn dangerous_ble_polling_write_rejects_readback() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = ble_identity();
        let device = identity.id.clone();
        let profile = ProfileId::new(1).unwrap();
        let session = ScriptedFakeSession::ble();
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            session.clone(),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        let error = manager
            .update_polling_rate_unverified_ble(
                &device,
                profile,
                PollingRate::Hz500,
                UpdatePolicy {
                    verification: VerificationMethod::Readback,
                    ..UpdatePolicy::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ManagerError::UnsupportedOperation {
                operation: "update_polling_rate_unverified_ble",
                transport: TransportKind::Ble,
            }
        ));
        assert!(
            session.writes().is_empty(),
            "readback rejection must not emit a polling write"
        );
        assert!(store.load().unwrap().devices[&device].profiles.is_empty());
    }

    #[tokio::test]
    async fn dangerous_ble_polling_write_rejects_non_ble_transport() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = usb_identity();
        let device = identity.id.clone();
        let profile = ProfileId::new(1).unwrap();
        let session = ScriptedFakeSession::usb();
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            session.clone(),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        let error = manager
            .update_polling_rate_unverified_ble(
                &device,
                profile,
                PollingRate::Hz500,
                UpdatePolicy::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ManagerError::UnsupportedOperation {
                operation: "update_polling_rate_unverified_ble",
                transport: TransportKind::Wired,
            }
        ));
        assert!(
            session.writes().is_empty(),
            "non-BLE rejection must not emit a polling write"
        );
    }

    #[tokio::test]
    async fn dangerous_ble_polling_write_logs_one_write_and_records_ack_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = ble_identity();
        let device = identity.id.clone();
        let profile = ProfileId::new(1).unwrap();
        let session = ScriptedFakeSession::ble();
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            session.clone(),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        let outcome = manager
            .update_polling_rate_unverified_ble(
                &device,
                profile,
                PollingRate::Hz500,
                UpdatePolicy::default(),
            )
            .await
            .unwrap();
        assert_eq!(outcome.desired, PollingRate::Hz500);
        assert_eq!(outcome.observed, None);
        assert_eq!(
            outcome.verification.application,
            ApplicationVerification::Acknowledged
        );
        assert_eq!(
            outcome.verification.persistence,
            PersistenceVerification::Unknown
        );
        assert_eq!(
            session.writes(),
            vec![ScriptedWrite::PollingRate(profile, PollingRate::Hz500)],
            "the dangerous BLE write must submit exactly one polling report"
        );

        let persisted = store.load().unwrap();
        let rate = &persisted.devices[&device].profiles[&profile].polling_rate;
        assert_eq!(rate.desired.as_ref().unwrap().value, PollingRate::Hz500);
        assert_eq!(
            rate.desired.as_ref().unwrap().source,
            DesiredSource::UserWrite
        );
        assert!(
            rate.observed.is_none(),
            "dangerous BLE ACK evidence must not fabricate a readback value"
        );
    }

    #[tokio::test]
    async fn safe_usb_polling_write_requires_complete_desired_image() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = usb_identity();
        let device = identity.id.clone();
        let profile = ProfileId::new(2).unwrap();
        let session = ScriptedFakeSession::usb().with_polling_rate(PollingRate::Hz1000);
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            session.clone(),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        // No complete desired image exists: the safe path must fail by name
        // before any 0x06 write.
        let error = manager
            .update_polling_rate(
                &device,
                profile,
                PollingRate::Hz500,
                UpdatePolicy::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ManagerError::MissingBaseline {
                resource: "DPI",
                profile: Some(p),
            } if p == profile
        ));
        assert!(
            session.writes().is_empty(),
            "missing baseline must fail before any polling write"
        );
        assert!(store.load().unwrap().devices[&device].profiles.is_empty());
    }

    #[tokio::test]
    async fn safe_usb_polling_write_fails_before_write_when_snapshot_mismatches() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = usb_identity();
        let device = identity.id.clone();
        let profile = ProfileId::new(2).unwrap();
        let (dpi, preferences, buttons) = complete_image(profile);
        seed_complete_desired(
            &store,
            &identity,
            profile,
            ProfileMetadata::new(profile, profile).unwrap(),
            dpi,
            preferences,
            buttons,
        );

        // The live device holds a different DPI image than the desired one.
        let deviant_dpi = DpiState::new(
            profile,
            vec![DpiValue::new(1600).expect("DPI")],
            StageIndex::new(1).expect("stage"),
            [0; 25],
        )
        .expect("DPI state");
        let session = ScriptedFakeSession::usb()
            .with_profile(matching_snapshot(
                profile,
                deviant_dpi,
                preferences,
                buttons,
            ))
            .with_polling_rate(PollingRate::Hz1000);
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            session.clone(),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        let error = manager
            .update_polling_rate(
                &device,
                profile,
                PollingRate::Hz500,
                UpdatePolicy::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ManagerError::MissingBaseline {
                resource: "DPI",
                profile: Some(p),
            } if p == profile
        ));
        assert!(
            session.writes().is_empty(),
            "image mismatch must fail before any polling write"
        );
    }

    #[tokio::test]
    async fn safe_usb_polling_write_requires_metadata_naming_target_as_current() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = usb_identity();
        let device = identity.id.clone();
        let profile = ProfileId::new(2).unwrap();
        let (dpi, preferences, buttons) = complete_image(profile);
        // Durable metadata names a different current profile than the target.
        let other = ProfileId::new(3).unwrap();
        seed_complete_desired(
            &store,
            &identity,
            profile,
            ProfileMetadata::new(other, other).unwrap(),
            dpi,
            preferences,
            buttons,
        );
        let session = ScriptedFakeSession::usb().with_polling_rate(PollingRate::Hz1000);
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            session.clone(),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        let error = manager
            .update_polling_rate(
                &device,
                profile,
                PollingRate::Hz500,
                UpdatePolicy::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ManagerError::MissingBaseline {
                resource: "profile metadata",
                profile: None,
            }
        ));
        assert!(
            session.writes().is_empty(),
            "metadata mismatch must fail before any polling write"
        );
    }

    #[tokio::test]
    async fn safe_usb_polling_write_noop_when_rate_already_equal() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = usb_identity();
        let device = identity.id.clone();
        let profile = ProfileId::new(2).unwrap();
        let (dpi, preferences, buttons) = complete_image(profile);
        seed_complete_desired(
            &store,
            &identity,
            profile,
            ProfileMetadata::new(profile, profile).unwrap(),
            dpi.clone(),
            preferences,
            buttons,
        );
        let session = ScriptedFakeSession::usb()
            .with_profile(matching_snapshot(profile, dpi, preferences, buttons))
            .with_polling_rate(PollingRate::Hz500);
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            session.clone(),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        let outcome = manager
            .update_polling_rate(
                &device,
                profile,
                PollingRate::Hz500,
                UpdatePolicy::default(),
            )
            .await
            .unwrap();
        assert_eq!(outcome.desired, PollingRate::Hz500);
        assert_eq!(outcome.observed, Some(PollingRate::Hz500));
        assert_eq!(
            outcome.verification.application,
            ApplicationVerification::ReadbackVerified
        );
        assert!(
            session.writes().is_empty(),
            "an equal live rate must not schedule a redundant polling write"
        );

        let persisted = store.load().unwrap();
        let rate = &persisted.devices[&device].profiles[&profile].polling_rate;
        assert_eq!(rate.observed.as_ref().unwrap().value, PollingRate::Hz500);
        assert_eq!(
            rate.observed.as_ref().unwrap().source,
            ObservationSource::UsbReadback
        );
    }

    #[tokio::test]
    async fn safe_usb_polling_transport_write_succeeds_with_complete_desired_image() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = usb_identity();
        let device = identity.id.clone();
        let profile = ProfileId::new(2).unwrap();
        let (dpi, preferences, buttons) = complete_image(profile);
        seed_complete_desired(
            &store,
            &identity,
            profile,
            ProfileMetadata::new(profile, profile).unwrap(),
            dpi.clone(),
            preferences,
            buttons,
        );
        let session = ScriptedFakeSession::usb()
            .with_profile(matching_snapshot(profile, dpi, preferences, buttons))
            .with_polling_rate(PollingRate::Hz1000);
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            session.clone(),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        let outcome = manager
            .update_polling_rate(
                &device,
                profile,
                PollingRate::Hz500,
                UpdatePolicy::default(),
            )
            .await
            .unwrap();
        assert_eq!(outcome.desired, PollingRate::Hz500);
        assert_eq!(outcome.observed, None);
        assert_eq!(
            outcome.verification.application,
            ApplicationVerification::Acknowledged
        );
        assert_eq!(
            outcome.verification.persistence,
            PersistenceVerification::Unknown
        );
        assert_eq!(
            session.writes(),
            vec![ScriptedWrite::PollingRate(profile, PollingRate::Hz500)],
            "a changed rate must submit exactly one polling report"
        );
    }

    #[tokio::test]
    async fn safe_usb_polling_readback_write_records_observed_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = usb_identity();
        let device = identity.id.clone();
        let profile = ProfileId::new(2).unwrap();
        let (dpi, preferences, buttons) = complete_image(profile);
        seed_complete_desired(
            &store,
            &identity,
            profile,
            ProfileMetadata::new(profile, profile).unwrap(),
            dpi.clone(),
            preferences,
            buttons,
        );
        let session = ScriptedFakeSession::usb()
            .with_profile(matching_snapshot(profile, dpi, preferences, buttons))
            .with_polling_rate(PollingRate::Hz1000);
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            session.clone(),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        let outcome = manager
            .update_polling_rate(
                &device,
                profile,
                PollingRate::Hz500,
                UpdatePolicy {
                    verification: VerificationMethod::Readback,
                    ..UpdatePolicy::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(outcome.desired, PollingRate::Hz500);
        assert_eq!(outcome.observed, Some(PollingRate::Hz500));
        assert_eq!(
            outcome.verification.application,
            ApplicationVerification::ReadbackVerified
        );
        assert_eq!(
            outcome.verification.persistence,
            PersistenceVerification::Unknown
        );
        assert_eq!(
            session.writes(),
            vec![ScriptedWrite::PollingRate(profile, PollingRate::Hz500)],
            "a changed rate must submit exactly one polling report"
        );

        let persisted = store.load().unwrap();
        let rate = &persisted.devices[&device].profiles[&profile].polling_rate;
        assert_eq!(rate.desired.as_ref().unwrap().value, PollingRate::Hz500);
        assert_eq!(rate.observed.as_ref().unwrap().value, PollingRate::Hz500);
        assert_eq!(
            rate.observed.as_ref().unwrap().source,
            ObservationSource::UsbReadback
        );
    }
    #[tokio::test]
    async fn read_polling_rate_loads_target_and_stores_under_target_not_live_alias() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = usb_identity();
        let device = identity.id.clone();
        let target = ProfileId::new(2).unwrap();
        let live_before = ProfileId::new(1).unwrap();
        let (dpi, preferences, buttons) = complete_image(target);
        // Per-profile rates: live_before = 125, target = 1000
        let session = ScriptedFakeSession::usb()
            .with_profile(matching_snapshot(target, dpi, preferences, buttons))
            .with_polling_rate_for(live_before, PollingRate::Hz125)
            .with_polling_rate_for(target, PollingRate::Hz1000)
            .with_live_profile(live_before);
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            session.clone(),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        let snapshot = manager.read_polling_rate(&device, target).await.unwrap();
        assert_eq!(
            snapshot.resource.observed.as_ref().unwrap().value,
            PollingRate::Hz1000
        );
        assert_eq!(session.last_polling_alias(), Some(target));
        let persisted = store.load().unwrap();
        // Stored under target
        assert_eq!(
            persisted.devices[&device].profiles[&target]
                .polling_rate
                .observed
                .as_ref()
                .unwrap()
                .value,
            PollingRate::Hz1000
        );
        // Live alias 1 must not be contaminated with 1000
        if let Some(other) = persisted.devices[&device].profiles.get(&live_before)
            && let Some(obs) = other.polling_rate.observed.as_ref()
        {
            assert_ne!(
                obs.value,
                PollingRate::Hz1000,
                "standalone live rate must not contaminate other profile"
            );
        }
    }

    #[tokio::test]
    async fn read_polling_rate_standalone_live_mismatch_cannot_contaminate() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = usb_identity();
        let device = identity.id.clone();
        let target = ProfileId::new(2).unwrap();
        let other = ProfileId::new(1).unwrap();
        let (dpi_target, pref_target, btn_target) = complete_image(target);
        // Snapshot for target correctly describes target; live before is other with rate 500, target rate 1000
        let session = ScriptedFakeSession::usb()
            .with_profile(matching_snapshot(
                other,
                dpi_target.clone(),
                pref_target,
                btn_target,
            ))
            .with_profile(matching_snapshot(
                target,
                dpi_target.clone(),
                pref_target,
                btn_target,
            ))
            .with_polling_rate_for(other, PollingRate::Hz500)
            .with_polling_rate_for(target, PollingRate::Hz1000)
            .with_live_profile(other);
        // Force live to other: after our safe path, we load target, so live becomes target and rate should be 1000, not 500
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            session.clone(),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        // Directly read polling for target; should get target's rate, not live's 500
        let snap = manager.read_polling_rate(&device, target).await.unwrap();
        assert_eq!(
            snap.resource.observed.as_ref().unwrap().value,
            PollingRate::Hz1000
        );
        let persisted = store.load().unwrap();
        assert_eq!(
            persisted.devices[&device].profiles[&target]
                .polling_rate
                .observed
                .as_ref()
                .unwrap()
                .value,
            PollingRate::Hz1000
        );
        // Ensure other not polluted
        if let Some(p) = persisted.devices[&device].profiles.get(&other)
            && let Some(obs) = &p.polling_rate.observed
        {
            assert_ne!(obs.value, PollingRate::Hz1000);
        }
    }

    #[tokio::test]
    async fn read_polling_rate_ble_is_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = ble_identity();
        let device = identity.id.clone();
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            ScriptedFakeSession::ble(),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();
        let err = manager
            .read_polling_rate(&device, ProfileId::new(1).unwrap())
            .await
            .expect_err("BLE must be unsupported");
        assert!(matches!(
            err,
            ManagerError::UnsupportedOperation {
                operation: "read_live_polling_rate",
                transport: TransportKind::Ble
            }
        ));
    }
}
