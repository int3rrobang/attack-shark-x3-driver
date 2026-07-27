use attack_shark_x3::{PollingRate, PreferencesState, ProfileId, TransportKind};
use serde::{Deserialize, Serialize};

use crate::backend::SessionWrite;
use crate::device::DeviceId;
use crate::error::ManagerError;
use crate::manager::DeviceManager;
use crate::operation::{ResourceSnapshot, UpdatePolicy, WriteOutcome};
use crate::state::{
    ApplicationVerification, DesiredSource, DesiredState, ObservationSource, ObservedState,
    PersistenceVerification, ProfileState, StateFile, StateTransaction, Verification,
};

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

impl DeviceManager {
    /// Reads one profile's complete preferences image and records USB readback evidence.
    pub async fn read_preferences(
        &self,
        device: &DeviceId,
        profile: ProfileId,
    ) -> Result<ResourceSnapshot<PreferencesState>, ManagerError> {
        let (_identity, session) = self.open_session(device).await?;
        let transport = session.transport();
        if transport == TransportKind::Ble {
            return Err(unsupported("read_preferences", transport));
        }

        let value = session.read_preferences(profile).await?;
        let now = self.now();
        let mut transaction = self.store().transaction()?;
        let device_state = transaction
            .state_mut()
            .devices
            .get_mut(device)
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
        let profile_state = device_state
            .profiles
            .entry(profile)
            .or_insert_with(ProfileState::empty);
        profile_state.preferences.observed = Some(ObservedState {
            value,
            source: ObservationSource::UsbReadback,
            observed_at: now,
        });
        let resource = profile_state.preferences.clone();
        transaction.state().validate()?;
        transaction.commit()?;

        Ok(ResourceSnapshot { resource })
    }

    /// Writes a complete preferences image, using USB readback or a BLE ACK.
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

        let (_identity, session) = self.open_session(device).await?;
        let transport = session.transport();

        if transport == TransportKind::Ble && !policy.allow_explicit_defaults {
            {
                let state = self.store().load()?;
                if !has_preferences_baseline(&state, device, profile) {
                    return Err(ManagerError::MissingBaseline {
                        resource: "preferences",
                        profile: Some(profile),
                    });
                }
            }

            let write = session.write_preferences(desired).await?;
            let mut transaction = self.store().transaction()?;
            let outcome = persist_preferences_write(
                &mut transaction,
                device,
                profile,
                desired,
                write,
                self.now(),
            )?;
            transaction.state().validate()?;
            transaction.commit()?;
            return finish_preferences_write(outcome, profile);
        }

        let write = session.write_preferences(desired).await?;
        let mut transaction = self.store().transaction()?;
        let outcome = persist_preferences_write(
            &mut transaction,
            device,
            profile,
            desired,
            write,
            self.now(),
        )?;
        transaction.state().validate()?;
        transaction.commit()?;
        finish_preferences_write(outcome, profile)
    }

    /// Applies a sparse preferences update after resolving a complete
    /// baseline.
    ///
    /// USB always reads the current profile image immediately before
    /// merging. BLE uses stored desired evidence first, then stored observed
    /// evidence, and only uses the legacy captured image when authorized by
    /// `policy.allow_explicit_defaults`.
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

        let (_identity, session) = self.open_session(device).await?;
        let baseline = if session.transport() == TransportKind::Ble {
            self.load_ble_preferences_baseline(device, profile, policy.allow_explicit_defaults)?
        } else {
            session.read_preferences(profile).await?
        };
        if baseline.profile != profile {
            return Err(ManagerError::InvalidUpdate(format!(
                "preferences baseline targets profile {} instead of requested profile {}",
                baseline.profile, profile
            )));
        }

        let desired = merge_preferences_delta(baseline, delta);
        self.update_preferences(device, profile, desired, policy)
            .await
    }

    fn load_ble_preferences_baseline(
        &self,
        device: &DeviceId,
        profile: ProfileId,
        allow_explicit_defaults: bool,
    ) -> Result<PreferencesState, ManagerError> {
        let state = self.store().load()?;
        let resource = state
            .devices
            .get(device)
            .and_then(|device_state| device_state.profiles.get(&profile))
            .map(|profile_state| &profile_state.preferences);

        if let Some(resource) = resource {
            if let Some(desired) = resource.desired.as_ref() {
                if allow_explicit_defaults || desired.source != DesiredSource::ExplicitDefaults {
                    return Ok(desired.value);
                }
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

    /// Reads the global polling rate and records USB readback evidence.
    pub async fn read_polling_rate(
        &self,
        device: &DeviceId,
    ) -> Result<ResourceSnapshot<PollingRate>, ManagerError> {
        let (_identity, session) = self.open_session(device).await?;
        let transport = session.transport();
        if transport == TransportKind::Ble {
            return Err(unsupported("read_polling_rate", transport));
        }

        let value = session.read_polling_rate().await?;
        let now = self.now();
        let mut transaction = self.store().transaction()?;
        let device_state = transaction
            .state_mut()
            .devices
            .get_mut(device)
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
        device_state.polling_rate.observed = Some(ObservedState {
            value,
            source: ObservationSource::UsbReadback,
            observed_at: now,
        });
        let resource = device_state.polling_rate.clone();
        transaction.state().validate()?;
        transaction.commit()?;

        Ok(ResourceSnapshot { resource })
    }

    /// Writes the global polling rate, using USB readback or a BLE ACK.
    pub async fn update_polling_rate(
        &self,
        device: &DeviceId,
        desired: PollingRate,
        _policy: UpdatePolicy,
    ) -> Result<WriteOutcome<PollingRate>, ManagerError> {
        let (_identity, session) = self.open_session(device).await?;
        let write = session.write_polling_rate(desired).await?;
        let mut transaction = self.store().transaction()?;
        let outcome = persist_polling_write(&mut transaction, device, desired, write, self.now())?;
        transaction.state().validate()?;
        transaction.commit()?;
        finish_polling_write(outcome)
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

fn captured_evidence_preferences(profile: ProfileId) -> PreferencesState {
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

fn merge_preferences_delta(
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

fn has_preferences_baseline(state: &StateFile, device: &DeviceId, profile: ProfileId) -> bool {
    state
        .devices
        .get(device)
        .and_then(|device_state| device_state.profiles.get(&profile))
        .is_some_and(|profile_state| {
            profile_state.preferences.desired.is_some()
                || profile_state.preferences.observed.is_some()
        })
}

fn persist_preferences_write(
    transaction: &mut StateTransaction<'_>,
    device: &DeviceId,
    profile: ProfileId,
    desired: PreferencesState,
    write: SessionWrite<PreferencesState>,
    now: crate::state::Timestamp,
) -> Result<WriteOutcome<PreferencesState>, ManagerError> {
    let (observed, application) = match write {
        SessionWrite::ReadbackVerified(readback) => {
            let application = if readback == desired {
                ApplicationVerification::ReadbackVerified
            } else {
                ApplicationVerification::Mismatch
            };
            (Some(readback), application)
        }
        SessionWrite::Acknowledged => (None, ApplicationVerification::Acknowledged),
    };

    let device_state = transaction
        .state_mut()
        .devices
        .get_mut(device)
        .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
    let profile_state = device_state
        .profiles
        .entry(profile)
        .or_insert_with(ProfileState::empty);
    profile_state.preferences.desired = Some(DesiredState {
        value: desired,
        source: DesiredSource::UserWrite,
        verification: Verification {
            application,
            persistence: PersistenceVerification::Unknown,
        },
        updated_at: now,
    });
    if let Some(readback) = observed {
        profile_state.preferences.observed = Some(ObservedState {
            value: readback,
            source: ObservationSource::UsbReadback,
            observed_at: now,
        });
    }

    Ok(WriteOutcome {
        desired,
        observed,
        verification: Verification {
            application,
            persistence: PersistenceVerification::Unknown,
        },
    })
}

fn persist_polling_write(
    transaction: &mut StateTransaction<'_>,
    device: &DeviceId,
    desired: PollingRate,
    write: SessionWrite<PollingRate>,
    now: crate::state::Timestamp,
) -> Result<WriteOutcome<PollingRate>, ManagerError> {
    let (observed, application) = match write {
        SessionWrite::ReadbackVerified(readback) => {
            let application = if readback == desired {
                ApplicationVerification::ReadbackVerified
            } else {
                ApplicationVerification::Mismatch
            };
            (Some(readback), application)
        }
        SessionWrite::Acknowledged => (None, ApplicationVerification::Acknowledged),
    };

    let device_state = transaction
        .state_mut()
        .devices
        .get_mut(device)
        .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
    device_state.polling_rate.desired = Some(DesiredState {
        value: desired,
        source: DesiredSource::UserWrite,
        verification: Verification {
            application,
            persistence: PersistenceVerification::Unknown,
        },
        updated_at: now,
    });
    if let Some(readback) = observed {
        device_state.polling_rate.observed = Some(ObservedState {
            value: readback,
            source: ObservationSource::UsbReadback,
            observed_at: now,
        });
    }

    Ok(WriteOutcome {
        desired,
        observed,
        verification: Verification {
            application,
            persistence: PersistenceVerification::Unknown,
        },
    })
}

fn finish_preferences_write(
    outcome: WriteOutcome<PreferencesState>,
    profile: ProfileId,
) -> Result<WriteOutcome<PreferencesState>, ManagerError> {
    if outcome.verification.application == ApplicationVerification::Mismatch {
        return Err(ManagerError::VerificationMismatch {
            resource: "preferences",
            profile: Some(profile),
        });
    }
    Ok(outcome)
}

fn finish_polling_write(
    outcome: WriteOutcome<PollingRate>,
) -> Result<WriteOutcome<PollingRate>, ManagerError> {
    if outcome.verification.application == ApplicationVerification::Mismatch {
        return Err(ManagerError::VerificationMismatch {
            resource: "polling rate",
            profile: None,
        });
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::{DeviceManager, PreferencesDelta};
    use crate::UpdatePolicy;
    use crate::backend::{ScriptedFakeFactory, ScriptedFakeSession};
    use crate::device::DeviceIdentity;
    use crate::error::ManagerError;
    use crate::state::{
        DesiredSource, DesiredState, ObservationSource, ObservedState, StatePaths, StateStore,
        Timestamp, Verification,
    };
    use attack_shark_x3::{
        ButtonAssignment, ButtonsState, DpiState, DpiValue, PollingRate, PreferencesState,
        ProfileId, ProfileMetadata, StageIndex, TransportKind,
    };
    use std::sync::Arc;

    fn store(dir: &tempfile::TempDir) -> StateStore {
        StateStore::open(StatePaths {
            state_file: dir.path().join("state.json"),
            lock_file: dir.path().join("state.lock"),
        })
    }

    fn usb_identity() -> DeviceIdentity {
        DeviceIdentity::usb(
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
        DeviceIdentity::ble("settings-ble-test", Some("Settings BLE test"))
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
    #[tokio::test]
    async fn usb_preferences_write_records_readback() {
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
        assert_eq!(outcome.observed, Some(desired));
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
    async fn ble_polling_ack_has_no_observed_value() {
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

        let outcome = manager
            .update_polling_rate(&device, PollingRate::Hz500, UpdatePolicy::default())
            .await
            .unwrap();
        assert_eq!(outcome.desired, PollingRate::Hz500);
        assert!(outcome.observed.is_none());
        assert!(
            store.load().unwrap().devices[&device]
                .polling_rate
                .observed
                .is_none()
        );
    }
}
