use std::time::Duration;

use attack_shark_x3::driver::ProfileSnapshot;
use attack_shark_x3::{
    PhysicalId, PollingRate, ProfileId, ProfileMetadata, TransportKind, WatermarkDecode,
    decode_watermark,
};
use tokio::time::{Instant, sleep};

use crate::backend::SessionWrite;
use crate::device::{DeviceEndpoint, DeviceId, TransportSelection};
use crate::error::ManagerError;
use crate::manager::DeviceManager;
use crate::operation::{
    DiscoveredEndpoint, PowerCycleVerificationOutcome, ProfileVerificationOutcome, WriteOutcome,
};
use crate::state::{ApplicationVerification, PersistenceVerification, ProfileState, Verification};
const POWER_CYCLE_POLL_INTERVAL: Duration = if cfg!(test) {
    Duration::from_millis(1)
} else {
    Duration::from_millis(250)
};

const POWER_CYCLE_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_millis(20)
} else {
    Duration::from_secs(30)
};

impl DeviceManager {
    /// Verifies that a complete profile image survives a profile reload.
    ///
    /// This workflow is intentionally limited to transports with USB-style
    /// readback. It reads the target image before changing the active profile,
    /// loads it again after switching away and back, and restores the original
    /// active profile before returning any result.
    ///
    /// The target profile is captured as a complete non-rate read followed
    /// immediately by `read_live_polling_rate(target)` in the same locked
    /// session, both before transitions and after reload. Every profile
    /// metadata write requires exact `ReadbackVerified(expected)`; ACK or
    /// mismatch aborts.
    pub async fn verify_profile_reload(
        &self,
        device: &DeviceId,
        target: ProfileId,
    ) -> Result<ProfileVerificationOutcome, ManagerError> {
        let (_identity, session, _guard) =
            self.open_locked(device, "verify_profile_reload").await?;
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
        // Safe capture is read_profile(target) then read_live_polling_rate(target) without interleaving.
        let (initial_snapshot, initial_rate) =
            read_complete_profile(session.as_ref(), target).await?;
        let away_metadata = ProfileMetadata::new(away, original.maximum()).map_err(|source| {
            ManagerError::Protocol {
                operation: "profile metadata",
                source,
            }
        })?;

        // If a transport reports an error after partially applying the change,
        // still make the best effort to restore the original profile. A
        // restoration error is always more important than the triggering error.
        if let Err(error) = write_exact_metadata(session.as_ref(), away_metadata).await {
            write_exact_metadata(session.as_ref(), original).await?;
            return Err(error);
        }

        // Readback is kept separate from restoration so every path after the
        // first transition attempts to restore the original active profile.
        let target_metadata =
            ProfileMetadata::new(target, original.maximum()).map_err(|source| {
                ManagerError::Protocol {
                    operation: "profile metadata",
                    source,
                }
            })?;

        let reloaded_result = async {
            write_exact_metadata(session.as_ref(), target_metadata).await?;
            read_complete_profile(session.as_ref(), target).await
        }
        .await;
        let restore_result = write_exact_metadata(session.as_ref(), original).await;
        // Restoration error remains higher priority.
        restore_result?;
        let (reloaded_snapshot, reloaded_rate) = reloaded_result?;

        let mismatch = first_mismatch(
            &initial_snapshot,
            initial_rate,
            &reloaded_snapshot,
            reloaded_rate,
        );
        let verified_at = self.now();
        let persistence = mismatch
            .is_none()
            .then_some(PersistenceVerification::ProfileReloadVerified { verified_at });
        self.persist_profile_observation(
            device,
            target,
            &reloaded_snapshot,
            reloaded_rate,
            persistence,
            verified_at,
        )
        .await?;

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
                desired: initial_snapshot.dpi,
                observed: Some(reloaded_snapshot.dpi),
                verification: verification.clone(),
            },
            preferences: WriteOutcome {
                desired: initial_snapshot.preferences,
                observed: Some(reloaded_snapshot.preferences),
                verification: verification.clone(),
            },
            buttons: WriteOutcome {
                desired: initial_snapshot.buttons,
                observed: Some(reloaded_snapshot.buttons),
                verification: verification.clone(),
            },
            polling_rate: WriteOutcome {
                desired: initial_rate,
                observed: Some(reloaded_rate),
                verification,
            },
        })
    }

    /// Verifies that a complete USB profile image survives an interactive
    /// physical power cycle.
    ///
    /// The open session is released before discovery polling begins. A
    /// persistence claim is made only after the device disappears and the
    /// physical mouse returns. In persistent identity mode the reappearing
    /// candidate is authenticated by its current-profile watermark: exactly
    /// the candidate matching the saved physical id is rebound, and zero,
    /// multiple, malformed, unsupported, or foreign candidates are refused —
    /// the workflow never guesses by VID/PID. In legacy mode (no physical
    /// id) the fuzzy model-level behavior is retained: a unique same-VID/PID
    /// USB device is accepted after a port change, because legacy makes no
    /// per-unit identity claim.
    ///
    /// Capture uses read_profile then read_live_polling_rate in same session
    /// before disconnect and after reconnect.
    pub async fn verify_power_cycle(
        &self,
        device: &DeviceId,
        target: ProfileId,
    ) -> Result<PowerCycleVerificationOutcome, ManagerError> {
        let (_identity, session, guard) = self.open_locked(device, "verify_power_cycle").await?;
        let transport = session.transport();
        if transport == TransportKind::Ble {
            return Err(ManagerError::UnsupportedOperation {
                operation: "power-cycle-verification",
                transport,
            });
        }

        // Capture the complete target image while the original session is
        // still open, then release the handle before waiting for the user.
        let (initial_snapshot, initial_rate) =
            read_complete_profile(session.as_ref(), target).await?;
        drop(session);
        drop(guard);

        let state = self
            .store()
            .load_async()
            .await
            .map_err(ManagerError::State)?;
        let device_state = state
            .devices
            .get(device)
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
        let old_endpoint = device_state
            .identity
            .endpoint(transport)
            .cloned()
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
        let physical_id = device_state.identity.physical_id;

        self.wait_for_disappearance(device, transport, &old_endpoint)
            .await?;
        self.wait_for_reappearance(device, transport, &old_endpoint, physical_id)
            .await?;

        let (_identity, reopened_session, _guard) = self
            .open_locked(device, "verify_power_cycle_reopen")
            .await?;
        if reopened_session.transport() != transport {
            return Err(ManagerError::VerificationMismatch {
                resource: "device identity",
                profile: Some(target),
            });
        }
        let (reloaded_snapshot, reloaded_rate) =
            read_complete_profile(reopened_session.as_ref(), target).await?;
        drop(reopened_session);

        let mismatch = first_mismatch(
            &initial_snapshot,
            initial_rate,
            &reloaded_snapshot,
            reloaded_rate,
        );
        let verified_at = self.now();
        let persistence = mismatch
            .is_none()
            .then_some(PersistenceVerification::PowerCycleVerified { verified_at });
        self.persist_profile_observation(
            device,
            target,
            &reloaded_snapshot,
            reloaded_rate,
            persistence,
            verified_at,
        )
        .await?;

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
                desired: initial_snapshot.dpi,
                observed: Some(reloaded_snapshot.dpi),
                verification: verification.clone(),
            },
            preferences: WriteOutcome {
                desired: initial_snapshot.preferences,
                observed: Some(reloaded_snapshot.preferences),
                verification: verification.clone(),
            },
            buttons: WriteOutcome {
                desired: initial_snapshot.buttons,
                observed: Some(reloaded_snapshot.buttons),
                verification: verification.clone(),
            },
            polling_rate: WriteOutcome {
                desired: initial_rate,
                observed: Some(reloaded_rate),
                verification,
            },
        })
    }

    async fn persist_profile_observation(
        &self,
        device: &DeviceId,
        target: ProfileId,
        snapshot: &ProfileSnapshot,
        polling_rate: PollingRate,
        persistence: Option<PersistenceVerification>,
        verified_at: crate::state::Timestamp,
    ) -> Result<(), ManagerError> {
        let device = device.clone();
        let snapshot = snapshot.clone();
        self.store()
            .mutate_async(move |state| {
                let profile_state = state
                    .devices
                    .get_mut(&device)
                    .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?
                    .profiles
                    .entry(target)
                    .or_insert_with(ProfileState::empty);
                profile_state
                    .dpi
                    .reconcile_observation(snapshot.dpi.clone(), verified_at);
                profile_state
                    .preferences
                    .reconcile_observation(snapshot.preferences, verified_at);
                profile_state
                    .buttons
                    .reconcile_observation(snapshot.buttons, verified_at);
                profile_state
                    .polling_rate
                    .reconcile_observation(polling_rate, verified_at);
                if let Some(persistence) = persistence {
                    match persistence {
                        PersistenceVerification::ProfileReloadVerified { verified_at } => {
                            profile_state.try_mark_profile_reload_verified(verified_at);
                        }
                        PersistenceVerification::PowerCycleVerified { verified_at } => {
                            profile_state.try_mark_power_cycle_verified(verified_at);
                        }
                        PersistenceVerification::Unknown => {}
                    }
                }
                Ok::<_, ManagerError>(())
            })
            .await
            .map_err(ManagerError::State)??;
        Ok(())
    }
    async fn wait_for_disappearance(
        &self,
        device: &DeviceId,
        transport: TransportKind,
        old_endpoint: &DeviceEndpoint,
    ) -> Result<(), ManagerError> {
        let started = Instant::now();
        loop {
            let discovered = self
                .factory
                .list(TransportSelection::Exact(transport))
                .await?;
            let still_present = discovered.iter().any(|candidate| {
                candidate.connected && candidate.endpoint.locator == old_endpoint.locator
            });
            if !still_present {
                return Ok(());
            }
            if started.elapsed() >= POWER_CYCLE_TIMEOUT {
                return Err(ManagerError::PowerCycleDisappearanceTimeout {
                    device: device.clone(),
                    timeout: POWER_CYCLE_TIMEOUT,
                });
            }
            sleep(POWER_CYCLE_POLL_INTERVAL).await;
        }
    }

    /// Waits for the verified device to return after a confirmed power cycle.
    ///
    /// In persistent identity mode (`Some(token)`) every reappearing candidate
    /// is authenticated by reading its current-profile watermark: exactly one
    /// candidate carrying the saved token is accepted and its locator stored.
    /// Zero, multiple, malformed, unsupported, and foreign candidates are
    /// refused — the loop never guesses by VID/PID — and polling continues
    /// until [`POWER_CYCLE_TIMEOUT`]. In legacy mode (`None`, which claims no
    /// physical identity) the model-level behavior is retained: a candidate on
    /// the old locator returns immediately, otherwise a unique same-VID/PID
    /// candidate is accepted after a port change.
    async fn wait_for_reappearance(
        &self,
        device: &DeviceId,
        transport: TransportKind,
        old_endpoint: &DeviceEndpoint,
        physical_id: Option<PhysicalId>,
    ) -> Result<DeviceEndpoint, ManagerError> {
        let started = Instant::now();
        loop {
            let discovered = self
                .factory
                .list(TransportSelection::Exact(transport))
                .await?;
            if let Some(token) = physical_id {
                // Persistent identity: physical-token authentication only.
                if let Some(found) = self.authenticated_candidate(&discovered, token).await {
                    self.update_locator(device, found.clone()).await?;
                    return Ok(found);
                }
            } else if let Some(found) = discovered.iter().find(|candidate| {
                candidate.connected && candidate.endpoint.locator == old_endpoint.locator
            }) {
                // Legacy: the same locator returning needs no rebind.
                return Ok(found.endpoint.clone());
            } else {
                // Legacy: fuzzy unique same-VID/PID rebind after a port change.
                let mut candidates =
                    crate::device::legacy_rebind_candidates(discovered, transport, old_endpoint);
                match candidates.len() {
                    0 => {}
                    1 => {
                        let new_endpoint = candidates.pop().expect("length checked above");
                        self.update_locator(device, new_endpoint.clone()).await?;
                        return Ok(new_endpoint);
                    }
                    _ => {
                        // Ambiguous: more than one same VID/PID candidate — do not guess.
                        // Keep polling until timeout.
                    }
                }
            }
            if started.elapsed() >= POWER_CYCLE_TIMEOUT {
                return Err(ManagerError::PowerCycleReappearanceTimeout {
                    device: device.clone(),
                    timeout: POWER_CYCLE_TIMEOUT,
                });
            }
            sleep(POWER_CYCLE_POLL_INTERVAL).await;
        }
    }

    /// Returns the single reappearing endpoint whose current-profile watermark
    /// decodes to `token`, or `None` when zero or multiple candidates match.
    ///
    /// Every candidate is opened and read individually. A candidate that
    /// cannot be opened or read is refused for that round (a transient
    /// enumeration failure must not abort the wait), unsupported transports
    /// decode as `Absent`, and malformed or foreign watermarks never match —
    /// so the saved mouse is only ever rebound to a physically authenticated
    /// unit, never to an identical-model device with a different token.
    async fn authenticated_candidate(
        &self,
        discovered: &[DiscoveredEndpoint],
        token: PhysicalId,
    ) -> Option<DeviceEndpoint> {
        let mut matches: Vec<DeviceEndpoint> = Vec::new();
        for candidate in discovered {
            if !candidate.connected {
                continue;
            }
            let Some(decode) = self.candidate_watermark(&candidate.endpoint).await else {
                continue;
            };
            if decode == WatermarkDecode::Valid(token) {
                matches.push(candidate.endpoint.clone());
            }
        }
        matches.sort_by(|a, b| a.locator.cmp(&b.locator));
        matches.dedup_by(|a, b| a.locator == b.locator);
        // Refuse multiple: one unique token must not authenticate two units.
        if matches.len() != 1 {
            return None;
        }
        matches.pop()
    }

    /// Decodes the current-profile watermark of one discovered endpoint.
    ///
    /// Mirrors the manager's per-attachment authentication read: open the
    /// endpoint, then read the current profile's DPI watermark. Transports
    /// that cannot produce one decode as `Absent`; any other open/read
    /// failure yields `None` so the polling loop refuses the candidate for
    /// that round instead of aborting on a transient error.
    async fn candidate_watermark(&self, endpoint: &DeviceEndpoint) -> Option<WatermarkDecode> {
        let session = self.factory.open(endpoint).await.ok()?;
        match read_current_watermark(session.as_ref()).await {
            Ok(decode) => Some(decode),
            Err(ManagerError::UnsupportedOperation { .. }) => Some(WatermarkDecode::Absent),
            Err(_) => None,
        }
    }

    /// Stores the reappeared endpoint as the device's locator for its transport.
    async fn update_locator(
        &self,
        device: &DeviceId,
        endpoint: DeviceEndpoint,
    ) -> Result<(), ManagerError> {
        let device = device.clone();
        self.store()
            .mutate_async(move |state| {
                state
                    .devices
                    .get_mut(&device)
                    .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?
                    .identity
                    .upsert_endpoint(endpoint);
                Ok::<_, ManagerError>(())
            })
            .await
            .map_err(ManagerError::State)??;
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

async fn read_complete_profile(
    session: &dyn crate::backend::DeviceSession,
    target: ProfileId,
) -> Result<(ProfileSnapshot, PollingRate), ManagerError> {
    // Safe sequence: complete profile read immediately followed by live polling rate read
    // in same locked session; report 0x06 alias is side-effect only.
    let snapshot = session.read_profile(target).await?;
    let rate = session.read_live_polling_rate(target).await?;
    Ok((snapshot, rate))
}

/// Reads the current profile's watermark through an open session: the current
/// profile from metadata, then that profile's DPI tail decoded. Mirrors the
/// manager's per-attachment authentication read.
async fn read_current_watermark(
    session: &dyn crate::backend::DeviceSession,
) -> Result<WatermarkDecode, ManagerError> {
    let metadata = session.read_profile_metadata().await?;
    let current = metadata.current();
    let dpi = session.read_dpi(current).await?;
    Ok(decode_watermark(&dpi.preserved_tail))
}

pub(crate) async fn write_exact_metadata(
    session: &dyn crate::backend::DeviceSession,
    expected: ProfileMetadata,
) -> Result<ProfileMetadata, ManagerError> {
    match session.write_profile_metadata(expected).await? {
        SessionWrite::ReadbackVerified(actual) if actual == expected => Ok(actual),
        SessionWrite::ReadbackVerified(_) | SessionWrite::Acknowledged => {
            Err(ManagerError::VerificationMismatch {
                resource: "profile metadata",
                profile: Some(expected.current()),
            })
        }
    }
}

fn first_mismatch(
    initial: &ProfileSnapshot,
    initial_rate: PollingRate,
    reloaded: &ProfileSnapshot,
    reloaded_rate: PollingRate,
) -> Option<&'static str> {
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
    if initial_rate != reloaded_rate {
        return Some("polling rate");
    }
    None
}
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use attack_shark_x3::driver::ProfileSnapshot;
    use attack_shark_x3::{
        ButtonAssignment, ButtonsState, DpiState, DpiValue, PhysicalId, PollingRate,
        PreferencesState, ProfileId, ProfileMetadata, StageIndex, TransportKind,
    };

    use super::DeviceManager;
    use crate::backend::{DeviceSession, ScriptedFakeFactory, ScriptedFakeSession, SessionWrite};
    use crate::device::DeviceIdentity;
    use crate::error::ManagerError;
    use crate::operation::DiscoveredEndpoint;
    use crate::state::{
        ApplicationVerification, DesiredSource, DesiredState, IdentityMode,
        PersistenceVerification, ProfileState, StateStore, Timestamp, Verification,
    };
    use async_trait::async_trait;

    fn profile() -> ProfileId {
        ProfileId::new(1).expect("profile one is valid")
    }

    fn identity(path: &str) -> DeviceIdentity {
        DeviceIdentity::test_usb(
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
        discovery_sequence: Vec<Vec<DiscoveredEndpoint>>,
    ) -> (DeviceManager, Arc<ScriptedFakeFactory>) {
        let endpoint = initial_identity
            .endpoints
            .values()
            .next()
            .cloned()
            .expect("initial must have endpoint");
        let factory =
            ScriptedFakeFactory::new().with_discovery_sequence(discovery_sequence.clone());
        factory.add_endpoint(
            DiscoveredEndpoint {
                endpoint,
                connected: true,
            },
            session.clone(),
        );
        for seq in &discovery_sequence {
            for disc in seq {
                factory.add_endpoint(disc.clone(), session.clone());
            }
        }
        let factory = Arc::new(factory);
        let manager = DeviceManager::with_store_and_factory(StateStore::memory(), factory.clone());
        manager
            .register_device(initial_identity.clone())
            .expect("register test identity");
        (manager, factory)
    }

    fn seed_desired(manager: &DeviceManager, device: &DeviceIdentity, expected: &ProfileSnapshot) {
        seed_desired_with_rate(manager, device, expected, PollingRate::Hz1000);
    }

    fn seed_desired_with_rate(
        manager: &DeviceManager,
        device: &DeviceIdentity,
        expected: &ProfileSnapshot,
        rate: PollingRate,
    ) {
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
        let observed_at = Timestamp { unix_seconds: 2 };
        profile_state.dpi.desired = Some(DesiredState {
            value: expected.dpi.clone(),
            source: DesiredSource::UserWrite,
            verification: verification.clone(),
            updated_at,
        });
        profile_state.dpi.observed = Some(crate::state::ObservedState {
            value: expected.dpi.clone(),
            source: crate::state::ObservationSource::UsbReadback,
            observed_at,
        });
        profile_state.preferences.desired = Some(DesiredState {
            value: expected.preferences,
            source: DesiredSource::UserWrite,
            verification: verification.clone(),
            updated_at,
        });
        profile_state.preferences.observed = Some(crate::state::ObservedState {
            value: expected.preferences,
            source: crate::state::ObservationSource::UsbReadback,
            observed_at,
        });
        profile_state.buttons.desired = Some(DesiredState {
            value: expected.buttons,
            source: DesiredSource::UserWrite,
            verification: verification.clone(),
            updated_at,
        });
        profile_state.buttons.observed = Some(crate::state::ObservedState {
            value: expected.buttons,
            source: crate::state::ObservationSource::UsbReadback,
            observed_at,
        });
        profile_state.polling_rate.desired = Some(DesiredState {
            value: rate,
            source: DesiredSource::UserWrite,
            verification: verification.clone(),
            updated_at,
        });
        profile_state.polling_rate.observed = Some(crate::state::ObservedState {
            value: rate,
            source: crate::state::ObservationSource::UsbReadback,
            observed_at,
        });
        transaction.commit().expect("commit desired state");
    }

    #[tokio::test]
    async fn power_cycle_success_requires_exact_usb_transition_and_refreshes_locator() {
        let initial_identity = identity(r"\\?\hid#power-cycle-old");
        let returned_identity = identity(r"\\?\hid#power-cycle-new");
        let expected = snapshot(800);
        let returned_endpoint = returned_identity
            .endpoints
            .values()
            .next()
            .cloned()
            .unwrap();
        let returned = DiscoveredEndpoint {
            endpoint: returned_endpoint.clone(),
            connected: true,
        };
        let session = ScriptedFakeSession::usb()
            .with_metadata(expected.persistent_metadata)
            .with_profile(expected.clone())
            .with_polling_rate(PollingRate::Hz1000);
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
        assert!(matches!(
            outcome.polling_rate.verification.persistence,
            PersistenceVerification::PowerCycleVerified { .. }
        ));
        assert_eq!(outcome.polling_rate.desired, PollingRate::Hz1000);
        assert_eq!(outcome.polling_rate.observed, Some(PollingRate::Hz1000));
        let refreshed = manager
            .device_identity(&initial_identity.id)
            .expect("refreshed identity");
        let refreshed_locator = refreshed
            .endpoint(TransportKind::Wired)
            .unwrap()
            .locator
            .clone();
        let expected_locator = returned_endpoint.locator.clone();
        assert_eq!(refreshed_locator, expected_locator);
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
        assert_eq!(
            profile_state
                .polling_rate
                .observed
                .as_ref()
                .map(|o| o.value),
            Some(PollingRate::Hz1000)
        );
        assert!(matches!(
            profile_state
                .dpi
                .desired
                .as_ref()
                .map(|desired| &desired.verification.persistence),
            Some(PersistenceVerification::PowerCycleVerified { .. })
        ));
        assert!(matches!(
            profile_state
                .polling_rate
                .desired
                .as_ref()
                .map(|d| &d.verification.persistence),
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
    async fn persistent_power_cycle_never_rebinds_identical_model_foreign_token() {
        let token = PhysicalId::from_token_bytes([0x11; 16]);
        let foreign_token = PhysicalId::from_token_bytes([0x22; 16]);
        let mut saved = identity(r"\\?\hid#persistent-saved");
        saved.physical_id = Some(token);
        let saved_endpoint = saved.endpoints.values().next().cloned().unwrap();
        let mut foreign = identity(r"\\?\hid#persistent-foreign");
        foreign.physical_id = Some(foreign_token);
        let foreign_endpoint = foreign.endpoints.values().next().cloned().unwrap();

        let expected = snapshot(800);
        let mut saved_snapshot = expected.clone();
        saved_snapshot.dpi.preserved_tail = token.to_watermark_bytes();
        let mut foreign_snapshot = expected.clone();
        foreign_snapshot.dpi.preserved_tail = foreign_token.to_watermark_bytes();

        let saved_session = ScriptedFakeSession::usb()
            .with_metadata(expected.persistent_metadata)
            .with_profile(saved_snapshot)
            .with_polling_rate(PollingRate::Hz1000);
        let foreign_session = ScriptedFakeSession::usb()
            .with_metadata(expected.persistent_metadata)
            .with_profile(foreign_snapshot)
            .with_polling_rate(PollingRate::Hz1000);

        let foreign_disc = DiscoveredEndpoint {
            endpoint: foreign_endpoint.clone(),
            connected: true,
        };
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_discovery_sequence(vec![vec![], vec![foreign_disc.clone()]]),
        );
        factory.add_endpoint(
            DiscoveredEndpoint {
                endpoint: saved_endpoint.clone(),
                connected: true,
            },
            saved_session,
        );
        factory.add_endpoint(foreign_disc, foreign_session);
        let manager = DeviceManager::with_store_and_factory(StateStore::memory(), factory.clone());
        {
            let mut txn = manager.store().transaction().expect("state transaction");
            txn.state_mut().identity_mode = IdentityMode::Persistent;
            txn.commit().expect("commit persistent identity mode");
        }
        manager
            .register_device(saved.clone())
            .expect("register persistent identity");

        let error = manager
            .verify_power_cycle(&saved.id, expected.target_profile)
            .await
            .expect_err("identical-model foreign token must never rebind the saved mouse");

        assert!(matches!(
            error,
            ManagerError::PowerCycleReappearanceTimeout { device: id, .. } if id == saved.id
        ));
        let refreshed = manager
            .device_identity(&saved.id)
            .expect("saved identity still present");
        assert_eq!(
            refreshed.endpoint(TransportKind::Wired).unwrap().locator,
            saved_endpoint.locator,
            "a foreign identical-model device must never replace the saved locator"
        );
    }

    #[tokio::test]
    async fn persistent_power_cycle_selects_matching_token_after_port_change() {
        let token = PhysicalId::from_token_bytes([0x11; 16]);
        let mut saved = identity(r"\\?\hid#persistent-old");
        saved.physical_id = Some(token);
        let saved_endpoint = saved.endpoints.values().next().cloned().unwrap();
        let mut returned_identity = identity(r"\\?\hid#persistent-new");
        returned_identity.physical_id = Some(token);
        let returned_endpoint = returned_identity
            .endpoints
            .values()
            .next()
            .cloned()
            .unwrap();

        let expected = snapshot(800);
        let mut stamped = expected.clone();
        stamped.dpi.preserved_tail = token.to_watermark_bytes();
        let session = ScriptedFakeSession::usb()
            .with_metadata(expected.persistent_metadata)
            .with_profile(stamped.clone())
            .with_polling_rate(PollingRate::Hz1000);

        let returned_disc = DiscoveredEndpoint {
            endpoint: returned_endpoint.clone(),
            connected: true,
        };
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_discovery_sequence(vec![vec![], vec![returned_disc.clone()]]),
        );
        factory.add_endpoint(
            DiscoveredEndpoint {
                endpoint: saved_endpoint.clone(),
                connected: true,
            },
            session.clone(),
        );
        factory.add_endpoint(returned_disc, session);
        let manager = DeviceManager::with_store_and_factory(StateStore::memory(), factory.clone());
        {
            let mut txn = manager.store().transaction().expect("state transaction");
            txn.state_mut().identity_mode = IdentityMode::Persistent;
            txn.commit().expect("commit persistent identity mode");
        }
        manager
            .register_device(saved.clone())
            .expect("register persistent identity");
        seed_desired(&manager, &saved, &stamped);

        let outcome = manager
            .verify_power_cycle(&saved.id, expected.target_profile)
            .await
            .expect("matching watermark token must rebind after a port change");

        assert!(matches!(
            outcome.dpi.verification.persistence,
            PersistenceVerification::PowerCycleVerified { .. }
        ));
        let refreshed = manager
            .device_identity(&saved.id)
            .expect("refreshed identity");
        assert_eq!(
            refreshed.endpoint(TransportKind::Wired).unwrap().locator,
            returned_endpoint.locator,
            "locator must move to the token-authenticated candidate"
        );
        let state = manager.store().load().expect("load state");
        assert!(matches!(
            state.devices[&saved.id].profiles[&expected.target_profile]
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
        let ble_identity = DeviceIdentity::test_ble("power-cycle-ble-test", Some("BLE test mouse"))
            .expect("BLE ID");
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
        let endpoint = device.endpoints.values().next().cloned().unwrap();
        let present = DiscoveredEndpoint {
            endpoint,
            connected: true,
        };
        let session = ScriptedFakeSession::usb()
            .with_metadata(expected.persistent_metadata)
            .with_profile(expected.clone())
            .with_polling_rate(PollingRate::Hz1000);
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
            .with_profile(expected.clone())
            .with_polling_rate(PollingRate::Hz1000);
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
        let endpoint = device.endpoints.values().next().cloned().unwrap();
        let returned = DiscoveredEndpoint {
            endpoint,
            connected: true,
        };
        let session = ScriptedFakeSession::usb()
            .with_metadata(expected.persistent_metadata)
            .with_profile_sequence(vec![expected.clone(), mismatched.clone()])
            .with_polling_rate(PollingRate::Hz1000);
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

    #[tokio::test]
    async fn power_cycle_polling_only_mismatch_is_detected() {
        let device = identity(r"\\?\hid#power-cycle-polling-mismatch");
        let expected = snapshot(800);
        let old_endpoint = device.endpoints.values().next().cloned().unwrap();
        let new_identity = identity(r"\\?\hid#power-cycle-polling-mismatch-new");
        let new_endpoint = new_identity.endpoints.values().next().cloned().unwrap();
        let returned = DiscoveredEndpoint {
            endpoint: new_endpoint.clone(),
            connected: true,
        };
        let session_old = ScriptedFakeSession::usb()
            .with_metadata(expected.persistent_metadata)
            .with_profile(expected.clone())
            .with_polling_rate_for(profile(), PollingRate::Hz1000);
        let session_new = ScriptedFakeSession::usb()
            .with_metadata(expected.persistent_metadata)
            .with_profile(expected.clone())
            .with_polling_rate_for(profile(), PollingRate::Hz500);
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_discovery_sequence(vec![vec![], vec![returned.clone()]]),
        );
        factory.add_endpoint(
            DiscoveredEndpoint {
                endpoint: old_endpoint.clone(),
                connected: true,
            },
            session_old.clone(),
        );
        factory.add_endpoint(returned.clone(), session_new.clone());
        let manager = DeviceManager::with_store_and_factory(StateStore::memory(), factory.clone());
        manager.register_device(device.clone()).expect("register");
        seed_desired(&manager, &device, &expected);
        let err = manager
            .verify_power_cycle(&device.id, profile())
            .await
            .expect_err("polling mismatch");
        assert!(
            matches!(err, ManagerError::VerificationMismatch { resource: "polling rate", profile: Some(p) } if p == profile())
        );
        let state = manager.store().load().expect("load state");
        let ps = &state.devices[&device.id].profiles[&profile()];
        assert_eq!(
            ps.polling_rate.observed.as_ref().map(|o| o.value),
            Some(PollingRate::Hz500)
        );
        assert!(matches!(
            ps.polling_rate
                .desired
                .as_ref()
                .map(|d| &d.verification.persistence),
            Some(PersistenceVerification::Unknown)
        ));
    }

    #[tokio::test]
    async fn profile_reload_polling_mismatch_persists_and_invalidates() {
        let initial = snapshot(800);
        let reloaded = snapshot(800);
        let mismatch =
            super::first_mismatch(&initial, PollingRate::Hz1000, &reloaded, PollingRate::Hz500);
        assert_eq!(mismatch, Some("polling rate"));

        use std::sync::{Arc, Mutex};
        struct AlternatingSession {
            snapshot: ProfileSnapshot,
            metadata: ProfileMetadata,
            rates: Vec<PollingRate>,
            idx: Arc<Mutex<usize>>,
        }
        #[async_trait(?Send)]
        impl DeviceSession for AlternatingSession {
            fn transport(&self) -> TransportKind {
                TransportKind::Wired
            }
            async fn read_profile_metadata(&self) -> Result<ProfileMetadata, ManagerError> {
                Ok(self.metadata)
            }
            async fn read_profile(
                &self,
                _profile: ProfileId,
            ) -> Result<ProfileSnapshot, ManagerError> {
                Ok(self.snapshot.clone())
            }
            async fn read_dpi(&self, _p: ProfileId) -> Result<DpiState, ManagerError> {
                Ok(self.snapshot.dpi.clone())
            }
            async fn read_preferences(
                &self,
                _p: ProfileId,
            ) -> Result<PreferencesState, ManagerError> {
                Ok(self.snapshot.preferences)
            }
            async fn read_buttons(&self, _p: ProfileId) -> Result<ButtonsState, ManagerError> {
                Ok(self.snapshot.buttons)
            }
            async fn read_live_polling_rate(
                &self,
                _alias: ProfileId,
            ) -> Result<PollingRate, ManagerError> {
                let mut i = self.idx.lock().unwrap();
                let rate = self.rates[*i % self.rates.len()];
                *i += 1;
                Ok(rate)
            }
            async fn write_dpi(
                &self,
                s: DpiState,
                _v: crate::operation::VerificationMethod,
            ) -> Result<SessionWrite<DpiState>, ManagerError> {
                Ok(SessionWrite::ReadbackVerified(s))
            }
            async fn write_preferences(
                &self,
                s: PreferencesState,
                _v: crate::operation::VerificationMethod,
            ) -> Result<SessionWrite<PreferencesState>, ManagerError> {
                Ok(SessionWrite::ReadbackVerified(s))
            }
            async fn write_buttons(
                &self,
                s: ButtonsState,
                _v: crate::operation::VerificationMethod,
            ) -> Result<SessionWrite<ButtonsState>, ManagerError> {
                Ok(SessionWrite::ReadbackVerified(s))
            }
            async fn write_polling_rate_unchecked(
                &self,
                _p: ProfileId,
                r: PollingRate,
                _v: crate::operation::VerificationMethod,
            ) -> Result<SessionWrite<PollingRate>, ManagerError> {
                Ok(SessionWrite::ReadbackVerified(r))
            }
            async fn write_profile_metadata(
                &self,
                m: ProfileMetadata,
            ) -> Result<SessionWrite<ProfileMetadata>, ManagerError> {
                Ok(SessionWrite::ReadbackVerified(m))
            }
            async fn read_battery(&self, _t: std::time::Duration) -> Result<u8, ManagerError> {
                Ok(100)
            }
            fn subscribe_events(&self) -> crate::backend::SessionEvents {
                crate::backend::SessionEvents { input: None }
            }
        }
        struct AltFactory {
            snapshot: ProfileSnapshot,
            metadata: ProfileMetadata,
            rates: Vec<PollingRate>,
            idx: Arc<Mutex<usize>>,
        }
        #[async_trait(?Send)]
        impl crate::backend::SessionFactory for AltFactory {
            async fn list(
                &self,
                _s: crate::device::TransportSelection,
            ) -> Result<Vec<DiscoveredEndpoint>, ManagerError> {
                Ok(vec![])
            }
            async fn open(
                &self,
                _endpoint: &crate::device::DeviceEndpoint,
            ) -> Result<Box<dyn DeviceSession>, ManagerError> {
                Ok(Box::new(AlternatingSession {
                    snapshot: self.snapshot.clone(),
                    metadata: self.metadata,
                    rates: self.rates.clone(),
                    idx: self.idx.clone(),
                }))
            }
        }
        let device = identity(r"\\?\hid#reload-polling-mismatch-alt");
        let expected = snapshot(800);
        let metadata = ProfileMetadata::new(profile(), ProfileId::new(5).unwrap()).unwrap();
        let factory = Arc::new(AltFactory {
            snapshot: expected.clone(),
            metadata,
            rates: vec![
                PollingRate::Hz1000,
                PollingRate::Hz500,
                PollingRate::Hz500,
                PollingRate::Hz1000,
            ],
            idx: Arc::new(Mutex::new(0)),
        });
        let manager = DeviceManager::with_store_and_factory(StateStore::memory(), factory);
        manager.register_device(device.clone()).expect("register");
        seed_desired_with_rate(&manager, &device, &expected, PollingRate::Hz1000);
        let err = manager
            .verify_profile_reload(&device.id, profile())
            .await
            .expect_err("polling mismatch should be detected");
        assert!(
            matches!(err, ManagerError::VerificationMismatch { resource: "polling rate", profile: Some(p) } if p == profile())
        );
        let state = manager.store().load().expect("load state");
        let ps = &state.devices[&device.id].profiles[&profile()];
        assert_eq!(
            ps.polling_rate.observed.as_ref().map(|o| o.value),
            Some(PollingRate::Hz500)
        );
        assert!(matches!(
            ps.polling_rate
                .desired
                .as_ref()
                .map(|d| &d.verification.persistence),
            Some(PersistenceVerification::Unknown)
        ));
    }

    #[tokio::test]
    async fn profile_reload_success_marks_all_resources() {
        let device = identity(r"\\?\hid#reload-success-all");
        let expected = snapshot(800);
        let session = ScriptedFakeSession::usb()
            .with_metadata(ProfileMetadata::new(profile(), ProfileId::new(5).unwrap()).unwrap())
            .with_profile(expected.clone())
            .with_polling_rate(PollingRate::Hz1000);
        let (manager, _) = manager_for(&device, session, vec![]);
        seed_desired_with_rate(&manager, &device, &expected, PollingRate::Hz1000);
        let outcome = manager
            .verify_profile_reload(&device.id, profile())
            .await
            .expect("should succeed");
        assert!(matches!(
            outcome.polling_rate.verification.persistence,
            PersistenceVerification::ProfileReloadVerified { .. }
        ));
        let state = manager.store().load().expect("state");
        let ps = &state.devices[&device.id].profiles[&profile()];
        assert!(matches!(
            ps.dpi.desired.as_ref().unwrap().verification.persistence,
            PersistenceVerification::ProfileReloadVerified { .. }
        ));
        assert!(matches!(
            ps.preferences
                .desired
                .as_ref()
                .unwrap()
                .verification
                .persistence,
            PersistenceVerification::ProfileReloadVerified { .. }
        ));
        assert!(matches!(
            ps.buttons
                .desired
                .as_ref()
                .unwrap()
                .verification
                .persistence,
            PersistenceVerification::ProfileReloadVerified { .. }
        ));
        assert!(matches!(
            ps.polling_rate
                .desired
                .as_ref()
                .unwrap()
                .verification
                .persistence,
            PersistenceVerification::ProfileReloadVerified { .. }
        ));
    }
}
