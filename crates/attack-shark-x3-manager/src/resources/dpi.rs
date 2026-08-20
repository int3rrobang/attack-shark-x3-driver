use attack_shark_x3::{DpiState, DpiValue, LiftOffDistance, ProfileId, StageIndex, TransportKind};
use serde::{Deserialize, Serialize};

use crate::backend::{DeviceSession, SessionWrite};
use crate::device::DeviceId;
use crate::error::ManagerError;
use crate::manager::DeviceManager;
use crate::operation::{BaselineSource, ResourceSnapshot, UpdatePolicy, WriteOutcome};
use crate::resources::state::{reconcile_observed, record_ack, record_readback};
use crate::state::{ApplicationVerification, DesiredSource, ProfileState, StateFile};

/// Partial DPI settings to merge into a complete profile image.
///
/// Every field is optional. Omitted fields, including unresolved DPI tail
/// bytes, are retained from the baseline selected by the manager.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DpiDelta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stages: Option<Vec<DpiValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_stage: Option<StageIndex>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensor: Option<SensorOptionsDelta>,
}

impl DpiDelta {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.stages.is_none()
            && self.active_stage.is_none()
            && match self.sensor {
                None => true,
                Some(sensor) => sensor.is_empty(),
            }
    }
}

/// Partial sensor settings nested in [`DpiDelta`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SensorOptionsDelta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lift_off_distance: Option<LiftOffDistance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ripple_control: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle_snap: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion_sync: Option<bool>,
}

impl SensorOptionsDelta {
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.lift_off_distance.is_none()
            && self.ripple_control.is_none()
            && self.angle_snap.is_none()
            && self.motion_sync.is_none()
    }
}

impl DeviceManager {
    /// Reads one profile's complete DPI image and records USB readback evidence.
    pub async fn read_dpi(
        &self,
        device: &DeviceId,
        profile: ProfileId,
    ) -> Result<ResourceSnapshot<DpiState>, ManagerError> {
        let (_identity, _endpoint, session, _guard) = self.open_locked(device, "read_dpi").await?;
        let transport = session.transport();
        if transport == TransportKind::Ble {
            return Err(unsupported("read_dpi", transport));
        }

        let value = session.read_dpi(profile).await?;
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
                reconcile_observed(&mut profile_state.dpi, value, now);
                Ok::<_, ManagerError>(profile_state.dpi.clone())
            })
            .await??;

        Ok(ResourceSnapshot { resource })
    }

    /// Writes a complete DPI image, using USB readback or a BLE ACK as evidence.
    pub async fn update_dpi(
        &self,
        device: &DeviceId,
        profile: ProfileId,
        desired: DpiState,
        policy: UpdatePolicy,
    ) -> Result<WriteOutcome<DpiState>, ManagerError> {
        if desired.profile != profile {
            return Err(ManagerError::InvalidUpdate(
                "DPI state profile does not match requested profile".to_owned(),
            ));
        }

        let (_identity, _endpoint, session, _guard) =
            self.open_locked(device, "update_dpi").await?;
        let transport = session.transport();

        if transport == TransportKind::Ble && !policy.allow_explicit_defaults {
            let state = self.store().load_async().await?;
            if !has_dpi_baseline(&state, device, profile) {
                return Err(ManagerError::MissingBaseline {
                    resource: "DPI",
                    profile: Some(profile),
                });
            }
        }

        self.write_dpi_with_session(
            device,
            profile,
            desired,
            policy.verification,
            session.as_ref(),
        )
        .await
    }
    /// Applies a sparse DPI update after resolving a complete baseline.
    ///
    /// By default USB resolves the baseline with a live read of the current
    /// profile image immediately before merging; `BaselineSource::Stored`
    /// merges against durable desired/observed evidence without a live read.
    /// BLE uses stored desired evidence first, then stored observed evidence,
    /// and only uses the explicitly captured profile image when authorized by
    /// `policy.allow_explicit_defaults`.
    pub async fn update_dpi_delta(
        &self,
        device: &DeviceId,
        profile: ProfileId,
        delta: DpiDelta,
        policy: UpdatePolicy,
    ) -> Result<WriteOutcome<DpiState>, ManagerError> {
        if delta.is_empty() {
            return Err(ManagerError::InvalidUpdate(
                "DPI delta must provide at least one field".to_owned(),
            ));
        }

        let (_identity, _endpoint, session, _guard) =
            self.open_locked(device, "update_dpi_delta").await?;
        let baseline = match session.transport() {
            TransportKind::Ble => {
                self.load_stored_dpi_baseline(device, profile, policy.allow_explicit_defaults)
                    .await?
            }
            TransportKind::Wired | TransportKind::Receiver => match policy.baseline {
                BaselineSource::Live => session.read_dpi(profile).await?,
                BaselineSource::Stored => {
                    self.load_stored_dpi_baseline(device, profile, false)
                        .await?
                }
            },
        };
        if baseline.profile != profile {
            return Err(ManagerError::InvalidUpdate(format!(
                "DPI baseline targets profile {} instead of requested profile {}",
                baseline.profile, profile
            )));
        }

        let desired = merge_dpi_delta(baseline, &delta)?;
        self.write_dpi_with_session(
            device,
            profile,
            desired,
            policy.verification,
            session.as_ref(),
        )
        .await
    }

    async fn write_dpi_with_session(
        &self,
        device: &DeviceId,
        profile: ProfileId,
        desired: DpiState,
        verification: crate::operation::VerificationMethod,
        session: &dyn DeviceSession,
    ) -> Result<WriteOutcome<DpiState>, ManagerError> {
        if desired.profile != profile {
            return Err(ManagerError::InvalidUpdate(
                "DPI state profile does not match requested profile".to_owned(),
            ));
        }
        let write = session.write_dpi(desired.clone(), verification).await?;
        let now = self.now();
        let device_id = device.clone();
        let desired_owned = desired.clone();
        let outcome = self
            .store()
            .mutate_async(move |state| {
                persist_dpi_write(state, &device_id, profile, desired_owned, write, now)
            })
            .await??;
        finish_dpi_write(outcome, profile)
    }

    /// Resolves the complete DPI baseline from the durable store without any
    /// live read.
    ///
    /// Returns the stored desired evidence (unless it is explicit defaults
    /// and not authorized by `allow_explicit_defaults`), then the stored
    /// observed evidence, and only the captured evidence image when
    /// authorized by `allow_explicit_defaults`. With no usable stored
    /// baseline and no capture authorization, `ManagerError::MissingBaseline`
    /// is returned.
    pub(crate) async fn load_stored_dpi_baseline(
        &self,
        device: &DeviceId,
        profile: ProfileId,
        allow_explicit_defaults: bool,
    ) -> Result<DpiState, ManagerError> {
        let state = self.store().load_async().await?;
        let resource = state
            .devices
            .get(device)
            .and_then(|device_state| device_state.profiles.get(&profile))
            .map(|profile_state| &profile_state.dpi);

        if let Some(resource) = resource {
            if let Some(desired) = resource.desired.as_ref()
                && (allow_explicit_defaults || desired.source != DesiredSource::ExplicitDefaults)
            {
                return Ok(desired.value.clone());
            }
            if let Some(observed) = resource.observed.as_ref() {
                return Ok(observed.value.clone());
            }
        }

        if allow_explicit_defaults {
            return Self::captured_evidence_dpi(profile);
        }

        Err(ManagerError::MissingBaseline {
            resource: "DPI",
            profile: Some(profile),
        })
    }

    /// This tail is captured evidence for the stock empty profile-1 image,
    /// not a universal device default. Authorization is enforced by the
    /// caller before this helper is reached.
    fn captured_evidence_dpi(profile: ProfileId) -> Result<DpiState, ManagerError> {
        let stage = DpiValue::new(800)
            .ok_or_else(|| ManagerError::InvalidUpdate("invalid captured DPI stage".to_owned()))?;
        let active_stage = StageIndex::new(1).ok_or_else(|| {
            ManagerError::InvalidUpdate("invalid captured DPI active stage".to_owned())
        })?;
        let captured =
            DpiState::captured_empty_profile_one(vec![stage], active_stage).map_err(|source| {
                ManagerError::Protocol {
                    operation: "dpi state",
                    source,
                }
            })?;
        DpiState::new(
            profile,
            captured.stages,
            captured.active_stage,
            captured.preserved_tail,
        )
        .map_err(|source| ManagerError::Protocol {
            operation: "dpi state",
            source,
        })
    }
}

fn unsupported(operation: &'static str, transport: TransportKind) -> ManagerError {
    ManagerError::UnsupportedOperation {
        operation,
        transport,
    }
}

pub(crate) fn merge_dpi_delta(
    baseline: DpiState,
    delta: &DpiDelta,
) -> Result<DpiState, ManagerError> {
    let mut merged = baseline;
    if let Some(stages) = delta.stages.as_ref() {
        merged.stages = stages.clone();
    }
    if let Some(active_stage) = delta.active_stage {
        merged.active_stage = active_stage;
    }
    if let Some(sensor_delta) = delta.sensor {
        let mut sensor = merged.sensor;
        if let Some(lift_off_distance) = sensor_delta.lift_off_distance {
            sensor.lift_off_distance = lift_off_distance;
        }
        if let Some(ripple_control) = sensor_delta.ripple_control {
            sensor.ripple_control = ripple_control;
        }
        if let Some(angle_snap) = sensor_delta.angle_snap {
            sensor.angle_snap = angle_snap;
        }
        if let Some(motion_sync) = sensor_delta.motion_sync {
            sensor.motion_sync = motion_sync;
        }
        merged.sensor = sensor;
    }
    let sensor = merged.sensor;
    let validated = DpiState::new(
        merged.profile,
        merged.stages,
        merged.active_stage,
        merged.preserved_tail,
    )
    .map_err(|source| ManagerError::Protocol {
        operation: "dpi state",
        source,
    })?
    .with_sensor(sensor);
    Ok(validated)
}

pub(crate) fn has_dpi_baseline(state: &StateFile, device: &DeviceId, profile: ProfileId) -> bool {
    state
        .devices
        .get(device)
        .and_then(|device_state| device_state.profiles.get(&profile))
        .is_some_and(|profile_state| {
            profile_state.dpi.desired.is_some() || profile_state.dpi.observed.is_some()
        })
}

pub(crate) fn persist_dpi_write(
    state: &mut StateFile,
    device: &DeviceId,
    profile: ProfileId,
    desired: DpiState,
    write: SessionWrite<DpiState>,
    now: crate::state::Timestamp,
) -> Result<WriteOutcome<DpiState>, ManagerError> {
    let device_state = state
        .devices
        .get_mut(device)
        .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
    let profile_state = device_state
        .profiles
        .entry(profile)
        .or_insert_with(crate::state::ProfileState::empty);
    let (observed, verification) = match write {
        SessionWrite::ReadbackVerified(readback) => {
            let observed = readback.clone();
            record_readback(&mut profile_state.dpi, desired.clone(), readback, now);
            let verification = profile_state
                .dpi
                .desired
                .as_ref()
                .expect("desired must exist after record_readback")
                .verification
                .clone();
            (Some(observed), verification)
        }
        SessionWrite::Acknowledged => {
            record_ack(&mut profile_state.dpi, desired.clone(), now);
            let verification = profile_state
                .dpi
                .desired
                .as_ref()
                .expect("desired must exist after record_ack")
                .verification
                .clone();
            (None, verification)
        }
    };

    Ok(WriteOutcome {
        desired,
        observed,
        verification,
    })
}
pub(crate) fn finish_dpi_write(
    outcome: WriteOutcome<DpiState>,
    profile: ProfileId,
) -> Result<WriteOutcome<DpiState>, ManagerError> {
    if outcome.verification.application == ApplicationVerification::Mismatch {
        return Err(ManagerError::VerificationMismatch {
            resource: "DPI",
            profile: Some(profile),
        });
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::{DeviceManager, DpiDelta, SensorOptionsDelta};
    use crate::backend::{ScriptedFakeFactory, ScriptedFakeSession};
    use crate::device::DeviceIdentity;
    use crate::error::ManagerError;
    use crate::state::{
        DesiredSource, DesiredState, ObservationSource, ObservedState, StatePaths, StateStore,
        Timestamp, Verification,
    };
    use crate::{BaselineSource, UpdatePolicy, VerificationMethod};
    use attack_shark_x3::{
        ButtonAssignment, ButtonsState, DpiState, DpiValue, PreferencesState, ProfileId,
        ProfileMetadata, StageIndex, TransportKind,
    };
    use std::sync::Arc;

    fn store(dir: &tempfile::TempDir) -> StateStore {
        StateStore::open(StatePaths::new(dir.path().join("state.json")))
    }
    fn usb_identity() -> DeviceIdentity {
        DeviceIdentity::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa61,
            Some("DPI-TEST"),
            r"\\?\hid#dpi-test",
            Some("DPI test"),
        )
        .expect("valid USB identity")
    }

    fn ble_identity() -> DeviceIdentity {
        DeviceIdentity::ble("dpi-ble-test", Some("DPI BLE test")).expect("valid BLE identity")
    }

    fn dpi(profile: ProfileId, value: u16) -> DpiState {
        DpiState::new(
            profile,
            vec![DpiValue::new(value).expect("valid DPI")],
            StageIndex::new(1).expect("valid stage"),
            [0; 25],
        )
        .expect("valid DPI state")
    }

    fn snapshot(profile: ProfileId, dpi: DpiState) -> attack_shark_x3::driver::ProfileSnapshot {
        attack_shark_x3::driver::ProfileSnapshot {
            target_profile: profile,
            persistent_metadata: ProfileMetadata::new(profile, profile).expect("metadata"),
            dpi,
            preferences: PreferencesState::new(profile, 1, 2, 3, [4, 5, 6], 7, 8),
            buttons: ButtonsState::new(
                profile,
                [ButtonAssignment::default();
                    attack_shark_x3::protocol::buttons::BUTTON_SLOT_COUNT],
            ),
        }
    }

    #[tokio::test]
    async fn usb_write_persists_readback_observed_state() {
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

        let desired = dpi(profile, 800);
        let outcome = manager
            .update_dpi(
                &device,
                profile,
                desired.clone(),
                UpdatePolicy {
                    verification: VerificationMethod::Readback,
                    ..UpdatePolicy::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(outcome.desired, desired);
        assert_eq!(outcome.observed, Some(desired.clone()));
        assert_eq!(
            outcome.verification.application,
            crate::ApplicationVerification::ReadbackVerified
        );
        let resource = &store.load().unwrap().devices[&device].profiles[&profile].dpi;
        assert_eq!(resource.observed.as_ref().unwrap().value, desired);
        assert_eq!(
            resource.observed.as_ref().unwrap().source,
            ObservationSource::UsbReadback
        );
    }

    #[tokio::test]
    async fn usb_transport_write_is_acknowledged_without_observed() {
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

        let desired = dpi(profile, 800);
        let outcome = manager
            .update_dpi(
                &device,
                profile,
                desired.clone(),
                UpdatePolicy {
                    verification: VerificationMethod::Transport,
                    ..UpdatePolicy::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(outcome.desired, desired);
        assert!(outcome.observed.is_none());
        assert_eq!(
            outcome.verification.application,
            crate::ApplicationVerification::Acknowledged
        );
        let resource = &store.load().unwrap().devices[&device].profiles[&profile].dpi;
        assert!(resource.observed.is_none());
        assert_eq!(resource.desired.as_ref().unwrap().value, desired);
    }

    #[tokio::test]
    async fn ble_ack_has_no_observed_value() {
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

        let desired = dpi(profile, 800);
        let outcome = manager
            .update_dpi(
                &device,
                profile,
                desired.clone(),
                UpdatePolicy {
                    allow_explicit_defaults: true,
                    ..UpdatePolicy::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(outcome.desired, desired);
        assert!(outcome.observed.is_none());
        assert_eq!(
            outcome.verification.application,
            crate::ApplicationVerification::Acknowledged
        );
        let resource = &store.load().unwrap().devices[&device].profiles[&profile].dpi;
        assert!(resource.observed.is_none());
        assert_eq!(resource.desired.as_ref().unwrap().value, desired);
    }

    #[tokio::test]
    async fn ble_readback_write_is_unsupported() {
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

        let error = manager
            .update_dpi(
                &device,
                profile,
                dpi(profile, 800),
                UpdatePolicy {
                    allow_explicit_defaults: true,
                    verification: VerificationMethod::Readback,
                    baseline: BaselineSource::Live,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ManagerError::UnsupportedOperation { .. }));
    }

    #[tokio::test]
    async fn ble_update_requires_profile_baseline_without_explicit_defaults() {
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
        let manager = DeviceManager::with_store_and_factory(store, factory);
        manager.register_device(identity).unwrap();

        let error = manager
            .update_dpi(&device, profile, dpi(profile, 800), UpdatePolicy::default())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ManagerError::MissingBaseline {
                resource: "DPI",
                profile: Some(p),
            } if p == profile
        ));
    }
    #[tokio::test]
    async fn usb_dpi_delta_preserves_unmodified_fields_and_tail() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = usb_identity();
        let device = identity.id.clone();
        let profile = ProfileId::new(1).unwrap();
        let mut baseline = dpi(profile, 800);
        baseline.stages = vec![DpiValue::new(800).unwrap(), DpiValue::new(1600).unwrap()];
        baseline.active_stage = StageIndex::new(2).unwrap();
        baseline.sensor.ripple_control = true;
        baseline.preserved_tail = [0xa5; 25];
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            ScriptedFakeSession::usb().with_profile(snapshot(profile, baseline.clone())),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        manager
            .update_dpi_delta(
                &device,
                profile,
                DpiDelta {
                    active_stage: Some(StageIndex::new(1).unwrap()),
                    ..DpiDelta::default()
                },
                UpdatePolicy::default(),
            )
            .await
            .unwrap();

        let actual = store.load().unwrap().devices[&device].profiles[&profile]
            .dpi
            .desired
            .as_ref()
            .unwrap()
            .value
            .clone();
        assert_eq!(actual.active_stage, StageIndex::new(1).unwrap());
        assert_eq!(actual.stages, baseline.stages);
        assert_eq!(actual.sensor, baseline.sensor);
        assert_eq!(actual.preserved_tail, baseline.preserved_tail);
    }

    #[tokio::test]
    async fn usb_dpi_delta_readback_verifies_merged_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = usb_identity();
        let device = identity.id.clone();
        let profile = ProfileId::new(1).unwrap();
        let baseline = dpi(profile, 800);
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            ScriptedFakeSession::usb().with_profile(snapshot(profile, baseline.clone())),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        let outcome = manager
            .update_dpi_delta(
                &device,
                profile,
                DpiDelta {
                    active_stage: Some(StageIndex::new(1).unwrap()),
                    ..DpiDelta::default()
                },
                UpdatePolicy {
                    verification: VerificationMethod::Readback,
                    ..UpdatePolicy::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            outcome.verification.application,
            crate::ApplicationVerification::ReadbackVerified
        );
        let observed = outcome.observed.expect("readback must produce observed");
        assert_eq!(observed.active_stage, StageIndex::new(1).unwrap());
        assert_eq!(observed.stages, baseline.stages);
        assert_eq!(observed.preserved_tail, baseline.preserved_tail);
        let resource = &store.load().unwrap().devices[&device].profiles[&profile].dpi;
        assert_eq!(resource.observed.as_ref().unwrap().value, outcome.desired);
    }

    #[tokio::test]
    async fn ble_dpi_delta_prefers_stored_desired_over_observed() {
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

        let mut desired_baseline = dpi(profile, 800);
        desired_baseline.preserved_tail = [0x11; 25];
        desired_baseline.sensor.ripple_control = true;
        let observed_baseline = dpi(profile, 1600);
        let mut transaction = store.transaction().unwrap();
        let profile_state = transaction
            .state_mut()
            .devices
            .get_mut(&device)
            .unwrap()
            .profiles
            .entry(profile)
            .or_default();
        profile_state.dpi.desired = Some(DesiredState {
            value: desired_baseline.clone(),
            source: DesiredSource::UserWrite,
            verification: Verification::not_sent(),
            updated_at: Timestamp::default(),
        });
        profile_state.dpi.observed = Some(ObservedState {
            value: observed_baseline,
            source: ObservationSource::UsbReadback,
            observed_at: Timestamp::default(),
        });
        transaction.commit().unwrap();

        manager
            .update_dpi_delta(
                &device,
                profile,
                DpiDelta {
                    sensor: Some(SensorOptionsDelta {
                        ripple_control: Some(false),
                        ..SensorOptionsDelta::default()
                    }),
                    ..DpiDelta::default()
                },
                UpdatePolicy::default(),
            )
            .await
            .unwrap();

        let actual = store.load().unwrap().devices[&device].profiles[&profile]
            .dpi
            .desired
            .as_ref()
            .unwrap()
            .value
            .clone();
        assert_eq!(actual.stages, desired_baseline.stages);
        assert_eq!(actual.preserved_tail, desired_baseline.preserved_tail);
        assert!(!actual.sensor.ripple_control);
    }

    #[tokio::test]
    async fn ble_dpi_delta_requires_baseline_or_explicit_authorization() {
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
        let delta = DpiDelta {
            active_stage: Some(StageIndex::new(1).unwrap()),
            ..DpiDelta::default()
        };

        let error = manager
            .update_dpi_delta(&device, profile, delta.clone(), UpdatePolicy::default())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ManagerError::MissingBaseline {
                resource: "DPI",
                profile: Some(p),
            } if p == profile
        ));

        manager
            .update_dpi_delta(
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
            .dpi
            .desired
            .as_ref()
            .unwrap()
            .value
            .clone();
        let captured = DpiState::captured_empty_profile_one(
            vec![DpiValue::new(800).unwrap()],
            StageIndex::new(1).unwrap(),
        )
        .unwrap();
        assert_eq!(actual.preserved_tail, captured.preserved_tail);
    }

    #[tokio::test]
    async fn dpi_delta_rejects_empty_update() {
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
            .update_dpi_delta(
                &device,
                ProfileId::new(1).unwrap(),
                DpiDelta::default(),
                UpdatePolicy::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ManagerError::InvalidUpdate(_)));
    }

    #[tokio::test]
    async fn stored_baseline_dpi_delta_merges_without_live_read() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = usb_identity();
        let device = identity.id.clone();
        let profile = ProfileId::new(1).unwrap();
        // No scripted profile: any live `read_dpi` would fail with
        // MissingBaseline, so a successful merge proves the baseline came
        // from the durable store.
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            ScriptedFakeSession::usb(),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        let mut stored_baseline = dpi(profile, 800);
        stored_baseline.preserved_tail = [0x22; 25];
        stored_baseline.sensor.ripple_control = true;
        let mut transaction = store.transaction().unwrap();
        let profile_state = transaction
            .state_mut()
            .devices
            .get_mut(&device)
            .unwrap()
            .profiles
            .entry(profile)
            .or_default();
        profile_state.dpi.desired = Some(DesiredState {
            value: stored_baseline.clone(),
            source: DesiredSource::UserWrite,
            verification: Verification::not_sent(),
            updated_at: Timestamp::default(),
        });
        transaction.commit().unwrap();

        let outcome = manager
            .update_dpi_delta(
                &device,
                profile,
                DpiDelta {
                    sensor: Some(SensorOptionsDelta {
                        motion_sync: Some(true),
                        ..SensorOptionsDelta::default()
                    }),
                    ..DpiDelta::default()
                },
                UpdatePolicy {
                    baseline: BaselineSource::Stored,
                    ..UpdatePolicy::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(outcome.desired.stages, stored_baseline.stages);
        assert_eq!(
            outcome.desired.preserved_tail,
            stored_baseline.preserved_tail
        );
        assert!(outcome.desired.sensor.ripple_control);
        assert!(outcome.desired.sensor.motion_sync);
    }

    #[tokio::test]
    async fn stored_baseline_dpi_delta_requires_stored_baseline() {
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
        let manager = DeviceManager::with_store_and_factory(store, factory);
        manager.register_device(identity).unwrap();

        let error = manager
            .update_dpi_delta(
                &device,
                profile,
                DpiDelta {
                    active_stage: Some(StageIndex::new(1).unwrap()),
                    ..DpiDelta::default()
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
                resource: "DPI",
                profile: Some(p),
            } if p == profile
        ));
    }

    #[tokio::test]
    async fn dpi_delta_live_baseline_uses_one_session() {
        use crate::backend::SessionFactory;
        use crate::device::DeviceEndpoint;
        use crate::operation::DiscoveredEndpoint;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingFactory {
            inner: ScriptedFakeFactory,
            opens: Arc<AtomicUsize>,
        }

        #[async_trait::async_trait(?Send)]
        impl SessionFactory for CountingFactory {
            async fn list(
                &self,
                selection: crate::device::TransportSelection,
            ) -> Result<Vec<DiscoveredEndpoint>, ManagerError> {
                self.inner.list(selection).await
            }

            async fn open(
                &self,
                endpoint: &DeviceEndpoint,
            ) -> Result<Box<dyn crate::backend::DeviceSession>, ManagerError> {
                self.opens.fetch_add(1, Ordering::SeqCst);
                self.inner.open(endpoint).await
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = usb_identity();
        let device = identity.id.clone();
        let profile = ProfileId::new(1).unwrap();
        let baseline = dpi(profile, 800);
        let opens = Arc::new(AtomicUsize::new(0));
        let inner = ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            ScriptedFakeSession::usb().with_profile(snapshot(profile, baseline.clone())),
        );
        let factory = Arc::new(CountingFactory {
            inner,
            opens: opens.clone(),
        });
        let manager = DeviceManager::with_store_and_factory(store, factory);
        manager.register_device(identity).unwrap();

        let result = manager
            .update_dpi_delta(
                &device,
                profile,
                DpiDelta {
                    active_stage: Some(StageIndex::new(1).unwrap()),
                    ..DpiDelta::default()
                },
                UpdatePolicy::default(),
            )
            .await;
        assert!(
            !matches!(
                result.as_ref().err(),
                Some(ManagerError::DeviceOperationBusy { .. })
            ),
            "live baseline must not trigger nested lock, got {result:?}"
        );
        result.unwrap();
        assert_eq!(
            opens.load(Ordering::SeqCst),
            1,
            "live DPI delta must open exactly one session"
        );
    }

    #[tokio::test]
    async fn dpi_delta_stored_baseline_uses_one_session() {
        use crate::backend::SessionFactory;
        use crate::device::DeviceEndpoint;
        use crate::operation::DiscoveredEndpoint;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingFactory {
            inner: ScriptedFakeFactory,
            opens: Arc<AtomicUsize>,
        }

        #[async_trait::async_trait(?Send)]
        impl SessionFactory for CountingFactory {
            async fn list(
                &self,
                selection: crate::device::TransportSelection,
            ) -> Result<Vec<DiscoveredEndpoint>, ManagerError> {
                self.inner.list(selection).await
            }

            async fn open(
                &self,
                endpoint: &DeviceEndpoint,
            ) -> Result<Box<dyn crate::backend::DeviceSession>, ManagerError> {
                self.opens.fetch_add(1, Ordering::SeqCst);
                self.inner.open(endpoint).await
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let identity = usb_identity();
        let device = identity.id.clone();
        let profile = ProfileId::new(1).unwrap();
        let mut stored_baseline = dpi(profile, 800);
        stored_baseline.preserved_tail = [0x22; 25];
        stored_baseline.sensor.ripple_control = true;
        let opens = Arc::new(AtomicUsize::new(0));
        let inner = ScriptedFakeFactory::new().with_identity(
            identity.clone(),
            true,
            ScriptedFakeSession::usb(),
        );
        let factory = Arc::new(CountingFactory {
            inner,
            opens: opens.clone(),
        });
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        let mut txn = store.transaction().unwrap();
        let profile_state = txn
            .state_mut()
            .devices
            .get_mut(&device)
            .unwrap()
            .profiles
            .entry(profile)
            .or_default();
        profile_state.dpi.desired = Some(DesiredState {
            value: stored_baseline.clone(),
            source: DesiredSource::UserWrite,
            verification: Verification::not_sent(),
            updated_at: Timestamp::default(),
        });
        txn.commit().unwrap();

        let result = manager
            .update_dpi_delta(
                &device,
                profile,
                DpiDelta {
                    sensor: Some(SensorOptionsDelta {
                        motion_sync: Some(true),
                        ..SensorOptionsDelta::default()
                    }),
                    ..DpiDelta::default()
                },
                UpdatePolicy {
                    baseline: BaselineSource::Stored,
                    ..UpdatePolicy::default()
                },
            )
            .await;
        assert!(
            !matches!(
                result.as_ref().err(),
                Some(ManagerError::DeviceOperationBusy { .. })
            ),
            "stored baseline must not trigger nested lock, got {result:?}"
        );
        let outcome = result.unwrap();
        assert!(outcome.desired.sensor.motion_sync);
        assert_eq!(
            opens.load(Ordering::SeqCst),
            1,
            "stored DPI delta must open exactly one session"
        );
    }

    #[test]
    fn merge_dpi_delta_invalid_stages_preserves_protocol_source() {
        use attack_shark_x3::ProtocolError;
        use std::error::Error;
        let profile = ProfileId::new(1).unwrap();
        let baseline = dpi(profile, 800);
        let delta = DpiDelta {
            stages: Some(vec![]),
            ..DpiDelta::default()
        };
        let err = super::merge_dpi_delta(baseline, &delta).expect_err("empty stages must fail");
        assert!(matches!(
            err,
            ManagerError::Protocol {
                operation: "dpi state",
                ..
            }
        ));
        if let ManagerError::Protocol { operation, source } = &err {
            assert_eq!(*operation, "dpi state");
            assert_eq!(*source, ProtocolError::InvalidStageCount { count: 0 });
        }
        // Display hides technical detail, source chain preserves it.
        assert_eq!(format!("{err}"), "invalid update for dpi state");
        let src = err.source().unwrap();
        assert!(src.downcast_ref::<ProtocolError>().is_some());
        assert!(!format!("{err}").contains("InvalidStageCount"));
        assert!(format!("{err:?}").contains("InvalidStageCount"));
    }

    #[test]
    fn captured_evidence_dpi_failure_is_protocol_not_string() {
        use attack_shark_x3::ProtocolError;
        use std::error::Error;
        // Directly exercise the mapping helpers used in captured_evidence_dpi.
        let source = ProtocolError::InvalidDpi { value: 9999 };
        let err = ManagerError::Protocol {
            operation: "dpi state",
            source,
        };
        assert_eq!(format!("{err}"), "invalid update for dpi state");
        assert!(err.source().is_some());
        let src = err
            .source()
            .unwrap()
            .downcast_ref::<ProtocolError>()
            .unwrap();
        assert_eq!(*src, ProtocolError::InvalidDpi { value: 9999 });
    }

    #[test]
    fn dpi_state_new_error_is_not_string_erased() {
        use attack_shark_x3::ProtocolError;
        use std::error::Error;
        // Ensure DpiState construction failures map to Protocol, not InvalidUpdate string.
        let profile = ProfileId::new(1).unwrap();
        let err = DpiState::new(
            profile,
            vec![attack_shark_x3::DpiValue::new(800).unwrap(); 9],
            StageIndex::new(1).unwrap(),
            [0; 25],
        )
        .map_err(|source| ManagerError::Protocol {
            operation: "dpi state",
            source,
        })
        .expect_err("9 stages must be invalid");
        assert!(matches!(err, ManagerError::Protocol { .. }));
        assert!(
            err.source()
                .unwrap()
                .downcast_ref::<ProtocolError>()
                .is_some()
        );
    }
}
