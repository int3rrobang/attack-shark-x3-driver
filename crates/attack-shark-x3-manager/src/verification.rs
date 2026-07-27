use std::time::Duration;

use attack_shark_x3::driver::ProfileSnapshot;
use attack_shark_x3::{ProfileId, ProfileMetadata, TransportKind};
use tokio::time::{Instant, sleep};

use crate::device::{DeviceId, DeviceIdentity, TransportSelection};
use crate::error::ManagerError;
use crate::manager::DeviceManager;
use crate::operation::{PowerCycleVerificationOutcome, ProfileVerificationOutcome, WriteOutcome};
use crate::state::{
    ApplicationVerification, PersistenceVerification, ProfileState, ResourceState, Verification,
};

#[cfg(not(test))]
const POWER_CYCLE_POLL_INTERVAL: Duration = Duration::from_millis(250);
#[cfg(test)]
const POWER_CYCLE_POLL_INTERVAL: Duration = Duration::from_millis(1);

#[cfg(not(test))]
const POWER_CYCLE_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(test)]
const POWER_CYCLE_TIMEOUT: Duration = Duration::from_millis(20);

impl DeviceManager {
    /// Verifies that a complete profile image survives a profile reload.
    ///
    /// This workflow is intentionally limited to transports with USB-style
    /// readback. It reads the target image before changing the active profile,
    /// loads it again after switching away and back, and restores the original
    /// active profile before returning any result.
    pub async fn verify_profile_reload(
        &self,
        device: &DeviceId,
        target: ProfileId,
    ) -> Result<ProfileVerificationOutcome, ManagerError> {
        let (_, session) = self.open_session(device).await?;
        let transport = session.transport();
        if transport == TransportKind::Ble {
            return Err(ManagerError::UnsupportedOperation {
                operation: "profile-reload-verification",
                transport,
            });
        }

        let original = session.read_profile_metadata().await?;
        if target.get() > original.maximum().get() {
            return Err(ManagerError::NoAlternateProfile {
                target,
                maximum: original.maximum(),
            });
        }

        let away = alternate_profile(original, target).ok_or(ManagerError::NoAlternateProfile {
            target,
            maximum: original.maximum(),
        })?;

        // Capture the complete target image before any profile transition.
        let initial = session.read_profile(target).await?;
        let away_metadata = ProfileMetadata::new(away, original.maximum())
            .map_err(|error| ManagerError::InvalidUpdate(error.to_string()))?;

        // If a transport reports an error after partially applying the change,
        // still make the best effort to restore the original profile. A
        // restoration error is always more important than the triggering error.
        if let Err(error) = session.write_profile_metadata(away_metadata).await {
            session.write_profile_metadata(original).await?;
            return Err(error);
        }

        // Readback is kept separate from restoration so every path after the
        // first transition attempts to restore the original active profile.
        let reloaded = async {
            let target_metadata = ProfileMetadata::new(target, original.maximum())
                .map_err(|error| ManagerError::InvalidUpdate(error.to_string()))?;
            session.write_profile_metadata(target_metadata).await?;
            session.read_profile(target).await
        }
        .await;

        let restore_result = session.write_profile_metadata(original).await;
        restore_result?;
        let reloaded = reloaded?;

        let mismatch = first_mismatch(&initial, &reloaded, target);
        let verified_at = self.now();
        let persistence = mismatch
            .is_none()
            .then_some(PersistenceVerification::ProfileReloadVerified { verified_at });
        self.persist_profile_observation(device, target, &reloaded, persistence, verified_at)?;

        if let Some(resource) = mismatch {
            return Err(ManagerError::VerificationMismatch {
                resource,
                profile: Some(target),
            });
        }

        let verification = Verification {
            application: ApplicationVerification::ReadbackVerified,
            persistence: PersistenceVerification::ProfileReloadVerified { verified_at },
        };

        Ok(ProfileVerificationOutcome {
            profile: target,
            dpi: WriteOutcome {
                desired: initial.dpi,
                observed: Some(reloaded.dpi),
                verification: verification.clone(),
            },
            preferences: WriteOutcome {
                desired: initial.preferences,
                observed: Some(reloaded.preferences),
                verification: verification.clone(),
            },
            buttons: WriteOutcome {
                desired: initial.buttons,
                observed: Some(reloaded.buttons),
                verification,
            },
        })
    }

    /// Verifies that a complete USB profile image survives an interactive
    /// physical power cycle.
    ///
    /// The open session is released before discovery polling begins. A
    /// persistence claim is made only after the exact USB identity disappears,
    /// returns, and produces a complete matching profile readback.
    pub async fn verify_power_cycle(
        &self,
        device: &DeviceId,
        target: ProfileId,
    ) -> Result<PowerCycleVerificationOutcome, ManagerError> {
        let (_identity, session) = self.open_session(device).await?;
        let transport = session.transport();
        if transport == TransportKind::Ble {
            return Err(ManagerError::UnsupportedOperation {
                operation: "power-cycle-verification",
                transport,
            });
        }

        // Capture the complete target image while the original session is
        // still open, then release the handle before waiting for the user.
        let initial = session.read_profile(target).await?;
        drop(session);

        self.wait_for_power_cycle_state(device, transport, false)
            .await?;
        let returned_identity = self
            .wait_for_power_cycle_state(device, transport, true)
            .await?
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
        self.refresh_device_identity(device, returned_identity)?;

        let (reopened_identity, reopened_session) = self.open_session(device).await?;
        if reopened_identity.id != *device || reopened_session.transport() != transport {
            return Err(ManagerError::VerificationMismatch {
                resource: "device identity",
                profile: Some(target),
            });
        }
        let reloaded = reopened_session.read_profile(target).await?;
        drop(reopened_session);

        let mismatch = first_mismatch(&initial, &reloaded, target);
        let verified_at = self.now();
        let persistence = mismatch
            .is_none()
            .then_some(PersistenceVerification::PowerCycleVerified { verified_at });
        self.persist_profile_observation(device, target, &reloaded, persistence, verified_at)?;

        if let Some(resource) = mismatch {
            return Err(ManagerError::VerificationMismatch {
                resource,
                profile: Some(target),
            });
        }

        let verification = Verification {
            application: ApplicationVerification::ReadbackVerified,
            persistence: PersistenceVerification::PowerCycleVerified { verified_at },
        };

        Ok(PowerCycleVerificationOutcome {
            profile: target,
            dpi: WriteOutcome {
                desired: initial.dpi,
                observed: Some(reloaded.dpi),
                verification: verification.clone(),
            },
            preferences: WriteOutcome {
                desired: initial.preferences,
                observed: Some(reloaded.preferences),
                verification: verification.clone(),
            },
            buttons: WriteOutcome {
                desired: initial.buttons,
                observed: Some(reloaded.buttons),
                verification,
            },
        })
    }

    fn persist_profile_observation(
        &self,
        device: &DeviceId,
        target: ProfileId,
        snapshot: &ProfileSnapshot,
        persistence: Option<PersistenceVerification>,
        verified_at: crate::state::Timestamp,
    ) -> Result<(), ManagerError> {
        let mut transaction = self.store().transaction()?;
        let device_state = transaction
            .state_mut()
            .devices
            .get_mut(device)
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
        let profile_state = device_state
            .profiles
            .entry(target)
            .or_insert_with(ProfileState::empty);

        set_observed(&mut profile_state.dpi, snapshot.dpi.clone(), verified_at);
        set_observed(
            &mut profile_state.preferences,
            snapshot.preferences,
            verified_at,
        );
        set_observed(&mut profile_state.buttons, snapshot.buttons, verified_at);

        if let Some(persistence) = persistence.as_ref() {
            mark_persistence(&mut profile_state.dpi, &snapshot.dpi, persistence);
            mark_persistence(
                &mut profile_state.preferences,
                &snapshot.preferences,
                persistence,
            );
            mark_persistence(&mut profile_state.buttons, &snapshot.buttons, persistence);
        } else {
            invalidate_persistence(&mut profile_state.dpi);
            invalidate_persistence(&mut profile_state.preferences);
            invalidate_persistence(&mut profile_state.buttons);
        }
        transaction.state().validate()?;
        transaction.commit()?;
        Ok(())
    }
    async fn wait_for_power_cycle_state(
        &self,
        device: &DeviceId,
        transport: TransportKind,
        expect_present: bool,
    ) -> Result<Option<DeviceIdentity>, ManagerError> {
        let started = Instant::now();
        loop {
            let discovered = self
                .list_devices(TransportSelection::Exact(transport))
                .await?;
            let exact = discovered.into_iter().find(|candidate| {
                candidate.connected
                    && candidate.identity.id == *device
                    && candidate.identity.transport == transport
            });
            let reached_state = if expect_present {
                exact.is_some()
            } else {
                exact.is_none()
            };
            if reached_state {
                return Ok(exact.map(|candidate| candidate.identity));
            }

            if started.elapsed() >= POWER_CYCLE_TIMEOUT {
                return Err(if expect_present {
                    ManagerError::PowerCycleReappearanceTimeout {
                        device: device.clone(),
                        timeout: POWER_CYCLE_TIMEOUT,
                    }
                } else {
                    ManagerError::PowerCycleDisappearanceTimeout {
                        device: device.clone(),
                        timeout: POWER_CYCLE_TIMEOUT,
                    }
                });
            }
            sleep(POWER_CYCLE_POLL_INTERVAL).await;
        }
    }

    fn refresh_device_identity(
        &self,
        device: &DeviceId,
        identity: DeviceIdentity,
    ) -> Result<(), ManagerError> {
        if identity.id != *device {
            return Err(ManagerError::DeviceNotFound(device.clone()));
        }
        let mut transaction = self.store().transaction()?;
        let device_state = transaction
            .state_mut()
            .devices
            .get_mut(device)
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
        if device_state.identity.transport != identity.transport {
            return Err(ManagerError::VerificationMismatch {
                resource: "device identity",
                profile: None,
            });
        }
        device_state.identity = identity;
        transaction.commit()?;
        Ok(())
    }
}

fn alternate_profile(original: ProfileMetadata, target: ProfileId) -> Option<ProfileId> {
    if original.current() != target {
        return Some(original.current());
    }

    (ProfileId::MIN..=original.maximum().get())
        .filter_map(ProfileId::new)
        .find(|candidate| *candidate != target)
}

fn first_mismatch(
    initial: &ProfileSnapshot,
    reloaded: &ProfileSnapshot,
    target: ProfileId,
) -> Option<&'static str> {
    if reloaded.target_profile != target || initial.target_profile != target {
        return Some("profile");
    }
    if initial.persistent_metadata.maximum() != reloaded.persistent_metadata.maximum() {
        return Some("profile metadata");
    }
    if initial.dpi != reloaded.dpi {
        return Some("dpi");
    }
    if initial.preferences != reloaded.preferences {
        return Some("preferences");
    }
    if initial.buttons != reloaded.buttons {
        return Some("buttons");
    }
    None
}
fn set_observed<T>(
    resource: &mut ResourceState<T>,
    value: T,
    observed_at: crate::state::Timestamp,
) {
    resource.observed = Some(crate::state::ObservedState {
        value,
        source: crate::state::ObservationSource::UsbReadback,
        observed_at,
    });
}

fn mark_persistence<T: Eq>(
    resource: &mut ResourceState<T>,
    readback: &T,
    persistence: &PersistenceVerification,
) {
    if let Some(desired) = resource.desired.as_mut() {
        if desired.value == *readback {
            desired.verification.persistence = persistence.clone();
        } else {
            desired.verification.invalidate_persistence();
        }
    }
}

fn invalidate_persistence<T>(resource: &mut ResourceState<T>) {
    if let Some(desired) = resource.desired.as_mut() {
        desired.verification.invalidate_persistence();
    }
}
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use attack_shark_x3::driver::ProfileSnapshot;
    use attack_shark_x3::{
        ButtonAssignment, ButtonsState, DpiState, DpiValue, PreferencesState, ProfileId,
        ProfileMetadata, StageIndex, TransportKind,
    };

    use super::DeviceManager;
    use crate::backend::{ScriptedFakeFactory, ScriptedFakeSession};
    use crate::device::DeviceIdentity;
    use crate::error::ManagerError;
    use crate::operation::DiscoveredDevice;
    use crate::state::{
        ApplicationVerification, DesiredSource, DesiredState, PersistenceVerification,
        ProfileState, StateStore, Timestamp, Verification,
    };

    fn profile() -> ProfileId {
        ProfileId::new(1).expect("profile one is valid")
    }

    fn identity(path: &str) -> DeviceIdentity {
        DeviceIdentity::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa61,
            Some("POWER-CYCLE-TEST"),
            path,
            Some("Power-cycle test mouse"),
        )
        .expect("valid test USB identity")
    }

    fn snapshot(dpi: u16) -> ProfileSnapshot {
        let profile = profile();
        let dpi = DpiState::captured_empty_profile_one(
            vec![DpiValue::new(dpi).expect("valid DPI")],
            StageIndex::new(1).expect("valid stage"),
        )
        .expect("valid DPI state");
        let preferences = PreferencesState::new(profile, 1, 2, 3, [4, 5, 6], 7, 8);
        let mut slots = [ButtonAssignment::default(); 18];
        slots[17] = ButtonAssignment::new(0x10, 0x20, 0x30);
        let buttons = ButtonsState::new(profile, slots);
        let metadata = ProfileMetadata::new(profile, profile).expect("valid metadata");

        ProfileSnapshot {
            persistent_metadata: metadata,
            target_profile: profile,
            dpi,
            preferences,
            buttons,
        }
    }

    fn manager_for(
        initial_identity: &DeviceIdentity,
        session: ScriptedFakeSession,
        discovery_sequence: Vec<Vec<DiscoveredDevice>>,
    ) -> (DeviceManager, Arc<ScriptedFakeFactory>) {
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_identity(initial_identity.clone(), true, session)
                .with_discovery_sequence(discovery_sequence),
        );
        let manager = DeviceManager::with_store_and_factory(StateStore::memory(), factory.clone());
        manager
            .register_device(initial_identity.clone())
            .expect("register test identity");
        (manager, factory)
    }

    fn seed_desired(manager: &DeviceManager, device: &DeviceIdentity, expected: &ProfileSnapshot) {
        let mut transaction = manager.store().transaction().expect("state transaction");
        let profile_state = transaction
            .state_mut()
            .devices
            .get_mut(&device.id)
            .expect("registered device")
            .profiles
            .entry(expected.target_profile)
            .or_insert_with(ProfileState::empty);
        let verification = Verification {
            application: ApplicationVerification::ReadbackVerified,
            persistence: PersistenceVerification::Unknown,
        };
        let updated_at = Timestamp { unix_seconds: 1 };
        profile_state.dpi.desired = Some(DesiredState {
            value: expected.dpi.clone(),
            source: DesiredSource::UserWrite,
            verification: verification.clone(),
            updated_at,
        });
        profile_state.preferences.desired = Some(DesiredState {
            value: expected.preferences,
            source: DesiredSource::UserWrite,
            verification: verification.clone(),
            updated_at,
        });
        profile_state.buttons.desired = Some(DesiredState {
            value: expected.buttons,
            source: DesiredSource::UserWrite,
            verification,
            updated_at,
        });
        transaction.commit().expect("commit desired state");
    }

    #[tokio::test]
    async fn power_cycle_success_requires_exact_usb_transition_and_refreshes_locator() {
        let initial_identity = identity(r"\\?\hid#power-cycle-old");
        let returned_identity = identity(r"\\?\hid#power-cycle-new");
        let expected = snapshot(800);
        let returned = DiscoveredDevice {
            identity: returned_identity.clone(),
            connected: true,
        };
        let session = ScriptedFakeSession::usb()
            .with_metadata(expected.persistent_metadata)
            .with_profile(expected.clone());
        let (manager, factory) =
            manager_for(&initial_identity, session, vec![vec![], vec![returned]]);
        seed_desired(&manager, &initial_identity, &expected);

        let outcome = manager
            .verify_power_cycle(&initial_identity.id, expected.target_profile)
            .await
            .expect("matching USB power-cycle readback");

        assert!(matches!(
            outcome.dpi.verification.persistence,
            PersistenceVerification::PowerCycleVerified { .. }
        ));
        assert_eq!(
            manager
                .device_identity(&initial_identity.id)
                .expect("refreshed identity")
                .locator,
            returned_identity.locator
        );
        let state = manager.store().load().expect("load state");
        let profile_state = &state.devices[&initial_identity.id].profiles[&expected.target_profile];
        assert_eq!(
            profile_state
                .dpi
                .observed
                .as_ref()
                .map(|observed| &observed.value),
            Some(&expected.dpi)
        );
        assert!(matches!(
            profile_state
                .dpi
                .desired
                .as_ref()
                .map(|desired| &desired.verification.persistence),
            Some(PersistenceVerification::PowerCycleVerified { .. })
        ));
        assert_eq!(
            factory.list_calls(),
            vec![
                crate::device::TransportSelection::Exact(TransportKind::Wired),
                crate::device::TransportSelection::Exact(TransportKind::Wired)
            ]
        );
    }

    #[tokio::test]
    async fn power_cycle_rejects_ble_without_discovery() {
        let ble_identity =
            DeviceIdentity::ble("power-cycle-ble-test", Some("BLE test mouse")).expect("BLE ID");
        let (manager, factory) = manager_for(&ble_identity, ScriptedFakeSession::ble(), Vec::new());

        let error = manager
            .verify_power_cycle(&ble_identity.id, profile())
            .await
            .expect_err("BLE power-cycle verification must be unsupported");

        assert!(matches!(
            error,
            ManagerError::UnsupportedOperation {
                operation: "power-cycle-verification",
                transport: TransportKind::Ble
            }
        ));
        assert!(factory.list_calls().is_empty());
    }

    #[tokio::test]
    async fn power_cycle_times_out_when_exact_device_never_disappears() {
        let device = identity(r"\\?\hid#power-cycle-never-away");
        let expected = snapshot(800);
        let present = DiscoveredDevice {
            identity: device.clone(),
            connected: true,
        };
        let session = ScriptedFakeSession::usb()
            .with_metadata(expected.persistent_metadata)
            .with_profile(expected.clone());
        let (manager, _) = manager_for(&device, session, vec![vec![present]]);

        let error = manager
            .verify_power_cycle(&device.id, expected.target_profile)
            .await
            .expect_err("persistent discovery must time out");

        assert!(matches!(
            error,
            ManagerError::PowerCycleDisappearanceTimeout { device: id, .. } if id == device.id
        ));
    }

    #[tokio::test]
    async fn power_cycle_times_out_when_exact_device_never_returns() {
        let device = identity(r"\\?\hid#power-cycle-never-back");
        let expected = snapshot(800);
        let session = ScriptedFakeSession::usb()
            .with_metadata(expected.persistent_metadata)
            .with_profile(expected.clone());
        let (manager, _) = manager_for(&device, session, vec![vec![], vec![]]);

        let error = manager
            .verify_power_cycle(&device.id, expected.target_profile)
            .await
            .expect_err("missing discovery must time out");

        assert!(matches!(
            error,
            ManagerError::PowerCycleReappearanceTimeout { device: id, .. } if id == device.id
        ));
    }

    #[tokio::test]
    async fn power_cycle_mismatch_persists_readback_but_invalidates_persistence() {
        let device = identity(r"\\?\hid#power-cycle-mismatch");
        let expected = snapshot(800);
        let mismatched = snapshot(1600);
        let returned = DiscoveredDevice {
            identity: device.clone(),
            connected: true,
        };
        let session = ScriptedFakeSession::usb()
            .with_metadata(expected.persistent_metadata)
            .with_profile_sequence(vec![expected.clone(), mismatched.clone()]);
        let (manager, _) = manager_for(&device, session, vec![vec![], vec![returned]]);
        seed_desired(&manager, &device, &expected);

        let error = manager
            .verify_power_cycle(&device.id, expected.target_profile)
            .await
            .expect_err("changed DPI must fail complete verification");

        assert!(matches!(
            error,
            ManagerError::VerificationMismatch {
                resource: "dpi",
                profile: Some(profile_id)
            } if profile_id == expected.target_profile
        ));
        let state = manager.store().load().expect("load state");
        let profile_state = &state.devices[&device.id].profiles[&expected.target_profile];
        assert_eq!(
            profile_state
                .dpi
                .observed
                .as_ref()
                .map(|observed| &observed.value),
            Some(&mismatched.dpi)
        );
        assert!(matches!(
            profile_state
                .dpi
                .desired
                .as_ref()
                .map(|desired| &desired.verification.persistence),
            Some(PersistenceVerification::Unknown)
        ));
    }
}
