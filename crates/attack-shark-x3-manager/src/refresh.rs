use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use attack_shark_x3::{ProfileId, ProfileMetadata, TransportKind};

use crate::backend::DeviceSession;
use crate::device::DeviceId;
use crate::error::ManagerError;
use crate::manager::DeviceManager;
use crate::operation::{FullProfileRefreshOutcome, ProfileResourceKind, RefreshedProfile};
use crate::state::{
    CapturedProfileImage, ObservationSource, ObservedState, ProfileState, ResourceState, Timestamp,
};

/// Fresh complete profile configuration captured from a USB session.
///
/// Metadata plus all five complete profile images, with the original profile
/// metadata always restored exactly. Capturing has no durable side effect;
/// persistence is layered on by the caller.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProfileCapture {
    pub original_metadata: ProfileMetadata,
    pub restored_metadata: ProfileMetadata,
    pub temporarily_expanded: bool,
    pub profiles: BTreeMap<ProfileId, RefreshedProfile>,
}

impl ProfileCapture {
    /// Materializes the capture as observed-only evidence at `now`.
    ///
    /// The metadata and every profile image become fresh `UsbReadback`
    /// observations with no desired value or verification evidence. This is
    /// the form embedded in durable ceremony journals.
    pub(crate) fn into_evidence(self, now: Timestamp) -> CapturedProfileImage {
        let mut profiles = BTreeMap::new();
        for (&profile, refreshed) in &self.profiles {
            let mut state = ProfileState::empty();
            state.dpi = observed(refreshed.dpi.clone(), now);
            state.preferences = observed(refreshed.preferences, now);
            state.buttons = observed(refreshed.buttons, now);
            state.polling_rate = observed(refreshed.polling_rate, now);
            profiles.insert(profile, state);
        }
        CapturedProfileImage {
            profile_metadata: observed(self.restored_metadata, now),
            profiles,
        }
    }
}

impl DeviceManager {
    /// Captures the complete live profile configuration of a USB device and
    /// reconciles durable observations without replacing desired values.
    ///
    /// The capture itself is performed by [`capture_all_profiles`]; this
    /// operation layers persistence on that pure helper. This is not a
    /// passive read: profile metadata is written while expanding and
    /// activating slots. Persistence evidence is invalidated because this
    /// workflow does not include a power cycle.
    pub async fn refresh_all_profiles(
        &self,
        device: &DeviceId,
    ) -> Result<FullProfileRefreshOutcome, ManagerError> {
        let (_identity, session, _guard) = self.open_locked(device, "refresh_all_profiles").await?;
        let transport = session.transport();
        if transport == TransportKind::Ble {
            return Err(ManagerError::UnsupportedOperation {
                operation: "refresh-all-profiles",
                transport,
            });
        }

        let capture = capture_all_profiles(session.as_ref()).await?;
        self.persist_profile_refresh(device, capture).await
    }

    async fn persist_profile_refresh(
        &self,
        device: &DeviceId,
        capture: ProfileCapture,
    ) -> Result<FullProfileRefreshOutcome, ManagerError> {
        let now = self.now();
        let device = device.clone();
        self.store()
            .mutate_async(move |state| {
                let device_state = state
                    .devices
                    .get_mut(&device)
                    .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
                let profile_metadata_drift = device_state
                    .profile_metadata
                    .reconcile_observation(capture.restored_metadata, now);
                let mut drift = BTreeMap::new();
                for (&profile, refreshed) in &capture.profiles {
                    let st = device_state.profiles.entry(profile).or_default();
                    let mut mismatches = Vec::new();
                    if st.dpi.reconcile_observation(refreshed.dpi.clone(), now) {
                        mismatches.push(ProfileResourceKind::Dpi);
                    }
                    if st
                        .preferences
                        .reconcile_observation(refreshed.preferences, now)
                    {
                        mismatches.push(ProfileResourceKind::Preferences);
                    }
                    if st.buttons.reconcile_observation(refreshed.buttons, now) {
                        mismatches.push(ProfileResourceKind::Buttons);
                    }
                    if st
                        .polling_rate
                        .reconcile_observation(refreshed.polling_rate, now)
                    {
                        mismatches.push(ProfileResourceKind::PollingRate);
                    }
                    if !mismatches.is_empty() {
                        drift.insert(profile, mismatches);
                    }
                }
                Ok(FullProfileRefreshOutcome {
                    original_metadata: capture.original_metadata,
                    restored_metadata: capture.restored_metadata,
                    temporarily_expanded: capture.temporarily_expanded,
                    profiles: capture.profiles,
                    drift,
                    profile_metadata_drift,
                })
            })
            .await
            .map_err(ManagerError::State)?
    }
}

/// Captures the complete live profile configuration of a USB session without
/// any durable side effect.
///
/// Every profile slot is temporarily enabled and activated in turn and each
/// complete live image is read. The original profile metadata is always
/// restored exactly, even when capture fails. This is not a passive read:
/// profile metadata is written while expanding and activating slots, so
/// persistence evidence predating the capture is invalidated.
pub(crate) async fn capture_all_profiles(
    session: &dyn DeviceSession,
) -> Result<ProfileCapture, ManagerError> {
    let transport = session.transport();
    if transport == TransportKind::Ble {
        return Err(ManagerError::UnsupportedOperation {
            operation: "capture-all-profiles",
            transport,
        });
    }

    let original_metadata = session.read_profile_metadata().await?;
    let maximum = ProfileId::MAX_ID;
    let expanded_metadata =
        ProfileMetadata::new(original_metadata.current(), maximum).map_err(|source| {
            ManagerError::Protocol {
                operation: "profile metadata",
                source,
            }
        })?;
    let temporarily_expanded = original_metadata.maximum() != maximum;
    let capture_result = async {
        if temporarily_expanded {
            crate::verification::write_exact_metadata(session, expanded_metadata).await?;
        }
        capture_all_profile_images(session, original_metadata.current(), maximum).await
    }
    .await;

    // Always verify an exact restoration. The backend activates the original
    // current profile before lowering the maximum, preserving its invariant.
    let restore_result =
        crate::verification::write_exact_metadata(session, original_metadata).await;

    let (profiles, restored_metadata) = match (capture_result, restore_result) {
        (Ok(profiles), Ok(restored)) => (profiles, restored),
        (Err(refresh), Ok(_)) => return Err(refresh),
        (Ok(_), Err(restore)) => {
            return Err(ManagerError::RefreshRestorationFailed {
                restore: restore.to_string(),
            });
        }
        (Err(refresh), Err(restore)) => {
            return Err(ManagerError::RefreshRestoreFailed {
                refresh: refresh.to_string(),
                restore: restore.to_string(),
            });
        }
    };

    Ok(ProfileCapture {
        original_metadata,
        restored_metadata,
        temporarily_expanded,
        profiles,
    })
}

/// Walks every profile slot, activating it as needed and reading the complete
/// live image.
async fn capture_all_profile_images(
    session: &dyn DeviceSession,
    original_current: ProfileId,
    maximum: ProfileId,
) -> Result<BTreeMap<ProfileId, RefreshedProfile>, ManagerError> {
    let mut profiles = BTreeMap::new();
    let mut current = original_current;
    let order = std::iter::once(original_current).chain(
        (ProfileId::MIN..=ProfileId::MAX)
            .filter_map(ProfileId::new)
            .filter(|profile| *profile != original_current),
    );

    for target in order {
        if current != target {
            let metadata =
                ProfileMetadata::new(target, maximum).map_err(|source| ManagerError::Protocol {
                    operation: "profile metadata",
                    source,
                })?;
            current = crate::verification::write_exact_metadata(session, metadata)
                .await?
                .current();
        }

        let snapshot = session.read_profile(target).await?;
        if snapshot.persistent_metadata.current() != target
            || snapshot.persistent_metadata.maximum() != maximum
        {
            return Err(ManagerError::VerificationMismatch {
                resource: "complete profile refresh",
                profile: Some(target),
            });
        }
        // Report 0x06 skips the profile loader: the selector byte is an alias
        // with side-effect only, and the returned rate is from the current live
        // image. This must follow an explicit activation and the complete read
        // of the same live profile.
        let polling_rate = session.read_live_polling_rate(target).await?;
        profiles.insert(
            target,
            RefreshedProfile {
                dpi: snapshot.dpi,
                preferences: snapshot.preferences,
                buttons: snapshot.buttons,
                polling_rate,
            },
        );
    }

    Ok(profiles)
}

/// Builds a resource carrying only a fresh observation, with no desired value
/// or verification evidence.
fn observed<T>(value: T, now: Timestamp) -> ResourceState<T> {
    let mut resource = ResourceState::empty();
    resource.observed = Some(ObservedState {
        value,
        source: ObservationSource::UsbReadback,
        observed_at: now,
    });
    resource
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use attack_shark_x3::driver::ProfileSnapshot;
    use attack_shark_x3::{
        ButtonsState, DpiState, PollingRate, PreferencesState, ProfileId, ProfileMetadata,
        TransportKind,
    };

    use super::{DeviceManager, capture_all_profiles};
    use crate::backend::{ScriptedFakeFactory, ScriptedFakeSession, ScriptedWrite};
    use crate::device::DeviceIdentity;
    use crate::error::ManagerError;
    use crate::operation::ProfileResourceKind;
    use crate::state::{
        ApplicationVerification, DesiredSource, DesiredState, ObservationSource, ObservedState,
        PersistenceVerification, StateStore, Timestamp, Verification,
    };

    fn profile(value: u8) -> ProfileId {
        ProfileId::new(value).expect("test profile must be valid")
    }

    fn metadata(current: u8, maximum: u8) -> ProfileMetadata {
        ProfileMetadata::new(profile(current), profile(maximum))
            .expect("test metadata must be valid")
    }

    fn snapshot(target: u8) -> ProfileSnapshot {
        let target = profile(target);
        ProfileSnapshot {
            persistent_metadata: metadata(target.get(), ProfileId::MAX),
            target_profile: target,
            dpi: DpiState::captured_stock_reset(target).expect("stock DPI must be valid"),
            preferences: PreferencesState::captured_stock_reset(target),
            buttons: ButtonsState::default_for_profile(target),
        }
    }

    fn identity() -> DeviceIdentity {
        DeviceIdentity::test_usb(
            TransportKind::Wired,
            0x1d57,
            0xfa61,
            Some("REFRESH-TEST"),
            r"\\?\hid#refresh-test",
            Some("Refresh test mouse"),
        )
        .expect("valid test identity")
    }

    fn manager_with(session: ScriptedFakeSession) -> (DeviceManager, DeviceIdentity) {
        let identity = identity();
        let factory =
            Arc::new(ScriptedFakeFactory::new().with_identity(identity.clone(), true, session));
        let manager = DeviceManager::with_store_and_factory(StateStore::memory(), factory);
        manager
            .register_device(identity.clone())
            .expect("register test identity");
        (manager, identity)
    }

    #[tokio::test]
    async fn refresh_captures_every_profile_reconciles_drift_and_restores_metadata() {
        let original = metadata(2, 3);
        let mut session = ScriptedFakeSession::usb()
            .with_metadata(original)
            .with_polling_rate(PollingRate::Hz1000);
        for target in ProfileId::MIN..=ProfileId::MAX {
            session = session.with_profile(snapshot(target));
        }
        let writes = session.clone();
        let (manager, identity) = manager_with(session);

        let matching_dpi = snapshot(1).dpi;
        let mismatched_preferences = PreferencesState::new(profile(1), 0xfe, 2, 3, [4, 5, 6], 7, 8);
        {
            let mut transaction = manager.store().transaction().expect("state transaction");
            let device = transaction
                .state_mut()
                .devices
                .get_mut(&identity.id)
                .expect("registered device");
            let profile_one = device.profiles.entry(profile(1)).or_default();
            profile_one.dpi.desired = Some(DesiredState {
                value: matching_dpi.clone(),
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::ReadbackVerified,
                    persistence: PersistenceVerification::PowerCycleVerified {
                        verified_at: Timestamp { unix_seconds: 1 },
                    },
                },
                updated_at: Timestamp { unix_seconds: 1 },
            });
            profile_one.dpi.observed = Some(ObservedState {
                value: matching_dpi.clone(),
                source: ObservationSource::UsbReadback,
                observed_at: Timestamp { unix_seconds: 1 },
            });
            profile_one.preferences.desired = Some(DesiredState {
                value: mismatched_preferences,
                source: DesiredSource::UserWrite,
                verification: Verification::not_sent(),
                updated_at: Timestamp { unix_seconds: 1 },
            });
            transaction.commit().expect("seed desired state");
        }

        let outcome = manager
            .refresh_all_profiles(&identity.id)
            .await
            .expect("refresh must succeed");

        assert_eq!(outcome.original_metadata, original);
        assert_eq!(outcome.restored_metadata, original);
        assert!(outcome.temporarily_expanded);
        assert_eq!(outcome.profiles.len(), usize::from(ProfileId::MAX));
        assert_eq!(
            outcome.drift[&profile(1)],
            vec![ProfileResourceKind::Preferences]
        );
        assert!(!outcome.profile_metadata_drift);

        let state = manager.store().load().expect("refreshed state");
        let device = &state.devices[&identity.id];
        assert_eq!(
            device.profile_metadata.observed.as_ref().unwrap().value,
            original
        );
        for target in ProfileId::MIN..=ProfileId::MAX {
            let target = profile(target);
            let stored = &device.profiles[&target];
            assert_eq!(
                stored.dpi.observed.as_ref().unwrap().value,
                outcome.profiles[&target].dpi
            );
            assert_eq!(
                stored.polling_rate.observed.as_ref().unwrap().value,
                PollingRate::Hz1000
            );
        }
        let desired_dpi = device.profiles[&profile(1)].dpi.desired.as_ref().unwrap();
        assert_eq!(desired_dpi.value, matching_dpi);
        assert_eq!(
            desired_dpi.verification.application,
            ApplicationVerification::ReadbackVerified
        );
        assert_eq!(
            desired_dpi.verification.persistence,
            PersistenceVerification::Unknown
        );
        assert_eq!(
            device.profiles[&profile(1)]
                .preferences
                .desired
                .as_ref()
                .unwrap()
                .verification
                .application,
            ApplicationVerification::Mismatch
        );

        let metadata_writes: Vec<_> = writes
            .writes()
            .into_iter()
            .filter_map(|write| match write {
                ScriptedWrite::ProfileMetadata(metadata) => Some(metadata),
                _ => None,
            })
            .collect();
        assert_eq!(metadata_writes.first(), Some(&metadata(2, 5)));
        assert_eq!(metadata_writes.last(), Some(&original));
    }

    #[tokio::test]
    async fn refresh_failure_restores_metadata_without_partial_state() {
        let original = metadata(2, 2);
        let session = ScriptedFakeSession::usb()
            .with_metadata(original)
            .with_polling_rate(PollingRate::Hz500)
            .with_profile(snapshot(1))
            .with_profile(snapshot(2));
        let writes = session.clone();
        let (manager, identity) = manager_with(session);

        let error = manager
            .refresh_all_profiles(&identity.id)
            .await
            .expect_err("missing profile three must fail");
        assert!(matches!(
            error,
            ManagerError::MissingBaseline {
                resource: "profile",
                profile: Some(missing)
            } if missing == profile(3)
        ));
        assert_eq!(
            writes
                .writes()
                .into_iter()
                .filter_map(|write| match write {
                    ScriptedWrite::ProfileMetadata(metadata) => Some(metadata),
                    _ => None,
                })
                .next_back(),
            Some(original)
        );
        assert!(
            manager.store().load().unwrap().devices[&identity.id]
                .profiles
                .is_empty()
        );
    }

    #[tokio::test]
    async fn restoration_failure_is_reported_as_hardware_risk() {
        let original = metadata(2, 3);
        let mut session = ScriptedFakeSession::usb()
            .with_metadata(original)
            .with_polling_rate(PollingRate::Hz500)
            .fail_metadata_write_for(original);
        for target in ProfileId::MIN..=ProfileId::MAX {
            session = session.with_profile(snapshot(target));
        }
        let (manager, identity) = manager_with(session);

        let error = manager
            .refresh_all_profiles(&identity.id)
            .await
            .expect_err("restoration failure must fail the operation");
        assert!(matches!(
            error,
            ManagerError::RefreshRestorationFailed { restore }
                if restore.contains("scripted profile metadata write failure")
        ));
        assert!(
            manager.store().load().unwrap().devices[&identity.id]
                .profiles
                .is_empty()
        );
    }
    #[tokio::test]
    async fn refresh_rejects_ble_without_writes() {
        let session = ScriptedFakeSession::ble();
        let writes = session.clone();
        let (manager, identity) = manager_with(session);

        let error = manager
            .refresh_all_profiles(&identity.id)
            .await
            .expect_err("BLE refresh must be rejected");
        assert!(matches!(
            error,
            ManagerError::UnsupportedOperation {
                operation: "refresh-all-profiles",
                transport: TransportKind::Ble
            }
        ));
        assert!(writes.writes().is_empty());
    }

    #[test]
    fn protocol_error_in_profile_metadata_preserves_typed_source() {
        use attack_shark_x3::ProtocolError;
        use std::error::Error;
        // Simulate the mapping used in refresh: ProfileMetadata::new -> Protocol.
        let source = ProtocolError::InvalidProfileRange {
            current: 5,
            maximum: 1,
        };
        let err = ManagerError::Protocol {
            operation: "profile metadata",
            source,
        };
        assert_eq!(format!("{err}"), "invalid update for profile metadata");
        let chained = err.source().unwrap();
        let typed = chained
            .downcast_ref::<ProtocolError>()
            .expect("typed source");
        assert_eq!(
            *typed,
            ProtocolError::InvalidProfileRange {
                current: 5,
                maximum: 1
            }
        );
        // Debug retains technical detail, Display does not leak it.
        assert!(format!("{err:?}").contains("InvalidProfileRange"));
        assert!(!format!("{err}").contains("InvalidProfileRange"));
    }

    #[test]
    fn profile_metadata_new_error_maps_to_protocol_variant() {
        use attack_shark_x3::ProtocolError;
        // Exercise the actual conversion path: an invalid metadata construction.
        let target = profile(1);
        let invalid_max = profile(5);
        // Valid case should succeed.
        let ok = ProfileMetadata::new(target, invalid_max);
        assert!(ok.is_ok());
        // Invalid current (0) is not constructible via ProfileId::new; use raw ProtocolError directly.
        let source = ProtocolError::InvalidProfile { value: 0 };
        let mapped = Err::<ProfileMetadata, ProtocolError>(source).map_err(|source| {
            ManagerError::Protocol {
                operation: "profile metadata",
                source,
            }
        });
        assert!(matches!(
            mapped,
            Err(ManagerError::Protocol {
                operation: "profile metadata",
                ..
            })
        ));
        if let Err(ManagerError::Protocol { operation, source }) = mapped {
            assert_eq!(operation, "profile metadata");
            assert_eq!(source, ProtocolError::InvalidProfile { value: 0 });
        }
    }

    #[tokio::test]
    async fn capture_all_profiles_is_pure_and_restores_metadata() {
        let original = metadata(2, 3);
        let mut session = ScriptedFakeSession::usb()
            .with_metadata(original)
            .with_polling_rate(PollingRate::Hz1000);
        for target in ProfileId::MIN..=ProfileId::MAX {
            session = session.with_profile(snapshot(target));
        }
        let writes = session.clone();
        let (manager, identity) = manager_with(session.clone());

        let capture = capture_all_profiles(&session)
            .await
            .expect("capture must succeed");

        assert_eq!(capture.original_metadata, original);
        assert_eq!(capture.restored_metadata, original);
        assert!(capture.temporarily_expanded);
        assert_eq!(capture.profiles.len(), usize::from(ProfileId::MAX));

        // A capture mutates no durable resource state.
        let state = manager.store().load().expect("state load");
        let device = &state.devices[&identity.id];
        assert!(device.profile_metadata.observed.is_none());
        assert!(device.profiles.is_empty());

        let metadata_writes: Vec<_> = writes
            .writes()
            .into_iter()
            .filter_map(|write| match write {
                ScriptedWrite::ProfileMetadata(metadata) => Some(metadata),
                _ => None,
            })
            .collect();
        assert_eq!(metadata_writes.first(), Some(&metadata(2, 5)));
        assert_eq!(metadata_writes.last(), Some(&original));
    }
}
