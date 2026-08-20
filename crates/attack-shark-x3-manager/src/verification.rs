use std::time::Duration;

use attack_shark_x3::driver::ProfileSnapshot;
use attack_shark_x3::{PollingRate, ProfileId, ProfileMetadata, TransportKind};
use tokio::time::{Instant, sleep};

use crate::backend::SessionWrite;
use crate::device::{DeviceEndpoint, DeviceId, DeviceIdentity, TransportSelection};
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
            // Attempt exact restoration; prioritize restoration error.
            if let Err(restore_err) = write_exact_metadata(session.as_ref(), original).await {
                return Err(restore_err);
            }
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
        if let Err(restore_err) = restore_result {
            return Err(restore_err);
        }
        let (reloaded_snapshot, reloaded_rate) = reloaded_result?;

        let mismatch = first_mismatch(
            &initial_snapshot,
            initial_rate,
            &reloaded_snapshot,
            reloaded_rate,
            target,
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
    /// persistence claim is made only after the exact USB identity disappears,
    /// returns, and produces a complete matching profile readback.
    ///
    /// Capture uses read_profile then read_live_polling_rate in same session
    /// before disconnect and after reconnect.
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
        let (initial_snapshot, initial_rate) =
            read_complete_profile(session.as_ref(), target).await?;
        drop(session);

        let state = self
            .store()
            .load_async()
            .await
            .map_err(ManagerError::State)?;
        let old_endpoint = state
            .devices
            .get(device)
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?
            .identity
            .endpoint(transport)
            .cloned()
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;

        self.wait_for_disappearance(device, transport, &old_endpoint)
            .await?;
        let reappeared_endpoint = self
            .wait_for_reappearance_with_rebind(device, transport, &old_endpoint)
            .await?;

        // Ensure reappeared endpoint is now the stored locator before reopening.
        // wait_for_reappearance_with_rebind already performed the unique
        // VID/PID rebind when needed; for the same-path case no change is needed.
        let state_after = self
            .store()
            .load_async()
            .await
            .map_err(ManagerError::State)?;
        let stored_locator = state_after
            .devices
            .get(device)
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?
            .identity
            .endpoint(transport)
            .map(|endpoint| &endpoint.locator)
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
        if stored_locator != &reappeared_endpoint.locator {
            // Defensive async rebind when unique (ignore errors as original did).
            let device_clone = device.clone();
            let reappeared_clone = reappeared_endpoint.clone();
            let _ = self
                .store()
                .mutate_async(move |state| {
                    let device_state = match state.devices.get_mut(&device_clone) {
                        Some(ds) => ds,
                        None => return Err(ManagerError::DeviceNotFound(device_clone.clone())),
                    };
                    let stored = match device_state.identity.endpoint(transport) {
                        Some(ep) => ep.clone(),
                        None => {
                            return Err(ManagerError::InvalidUpdate(format!(
                                "no stored endpoint for {transport:?}"
                            )));
                        }
                    };
                    if reappeared_clone.locator == stored.locator {
                        return Ok(());
                    }
                    if reappeared_clone.transport != transport {
                        return Err(ManagerError::InvalidUpdate(format!(
                            "no candidate for rebind of {device_clone} transport {transport:?}"
                        )));
                    }
                    let is_usb =
                        matches!(transport, TransportKind::Wired | TransportKind::Receiver);
                    if is_usb
                        && (reappeared_clone.vendor_id != stored.vendor_id
                            || reappeared_clone.product_id != stored.product_id)
                    {
                        return Err(ManagerError::InvalidUpdate(format!(
                            "no candidate for rebind of {device_clone} transport {transport:?}"
                        )));
                    }
                    device_state.identity.upsert_endpoint(reappeared_clone);
                    Ok(())
                })
                .await;
        }

        let (reopened_identity, reopened_session) = self.open_session(device).await?;
        if reopened_identity.id != *device || reopened_session.transport() != transport {
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
            target,
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
        let device_owned = device.clone();
        let snapshot_owned = snapshot.clone();
        let persistence_owned = persistence.clone();
        self.store()
            .mutate_async(move |state| {
                let device_state = match state.devices.get_mut(&device_owned) {
                    Some(ds) => ds,
                    None => return Err(ManagerError::DeviceNotFound(device_owned.clone())),
                };
                let profile_state = device_state
                    .profiles
                    .entry(target)
                    .or_insert_with(ProfileState::empty);
                profile_state
                    .dpi
                    .reconcile_observation(snapshot_owned.dpi.clone(), verified_at);
                profile_state
                    .preferences
                    .reconcile_observation(snapshot_owned.preferences, verified_at);
                profile_state
                    .buttons
                    .reconcile_observation(snapshot_owned.buttons, verified_at);
                profile_state
                    .polling_rate
                    .reconcile_observation(polling_rate, verified_at);
                if let Some(persistence_value) = persistence_owned.clone() {
                    match persistence_value {
                        PersistenceVerification::ProfileReloadVerified { verified_at } => {
                            let _ = crate::resources::state::try_mark_profile_reload(
                                &mut profile_state.dpi,
                                verified_at,
                            );
                            let _ = crate::resources::state::try_mark_profile_reload(
                                &mut profile_state.preferences,
                                verified_at,
                            );
                            let _ = crate::resources::state::try_mark_profile_reload(
                                &mut profile_state.buttons,
                                verified_at,
                            );
                            let _ = crate::resources::state::try_mark_profile_reload(
                                &mut profile_state.polling_rate,
                                verified_at,
                            );
                        }
                        PersistenceVerification::PowerCycleVerified { verified_at } => {
                            let _ = crate::resources::state::try_mark_power_cycle(
                                &mut profile_state.dpi,
                                verified_at,
                            );
                            let _ = crate::resources::state::try_mark_power_cycle(
                                &mut profile_state.preferences,
                                verified_at,
                            );
                            let _ = crate::resources::state::try_mark_power_cycle(
                                &mut profile_state.buttons,
                                verified_at,
                            );
                            let _ = crate::resources::state::try_mark_power_cycle(
                                &mut profile_state.polling_rate,
                                verified_at,
                            );
                        }
                        PersistenceVerification::Unknown => {}
                    }
                }
                Ok(())
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

    async fn wait_for_reappearance_with_rebind(
        &self,
        device: &DeviceId,
        transport: TransportKind,
        old_endpoint: &DeviceEndpoint,
    ) -> Result<DeviceEndpoint, ManagerError> {
        let started = Instant::now();
        loop {
            let discovered = self
                .factory
                .list(TransportSelection::Exact(transport))
                .await?;
            if let Some(found) = discovered.iter().find(|candidate| {
                candidate.connected && candidate.endpoint.locator == old_endpoint.locator
            }) {
                return Ok(found.endpoint.clone());
            }
            let is_usb = matches!(transport, TransportKind::Wired | TransportKind::Receiver);
            let mut candidates: Vec<DeviceEndpoint> = discovered
                .into_iter()
                .filter(|candidate| {
                    candidate.connected && candidate.endpoint.transport == transport
                })
                .filter(|candidate| {
                    if is_usb {
                        candidate.endpoint.vendor_id == old_endpoint.vendor_id
                            && candidate.endpoint.product_id == old_endpoint.product_id
                    } else {
                        true
                    }
                })
                .map(|candidate| candidate.endpoint)
                .collect();
            candidates.sort_by(|a, b| format!("{:?}", a.locator).cmp(&format!("{:?}", b.locator)));
            candidates.dedup_by(|a, b| a.locator == b.locator);
            match candidates.len() {
                0 => {}
                1 => {
                    let new_endpoint = candidates.into_iter().next().unwrap();
                    let device_clone = device.clone();
                    let new_endpoint_clone = new_endpoint.clone();
                    self.store()
                        .mutate_async(move |state| {
                            let device_state = match state.devices.get_mut(&device_clone) {
                                Some(ds) => ds,
                                None => {
                                    return Err(ManagerError::DeviceNotFound(device_clone.clone()));
                                }
                            };
                            let stored = match device_state.identity.endpoint(transport) {
                                Some(ep) => ep.clone(),
                                None => {
                                    return Err(ManagerError::InvalidUpdate(format!(
                                        "no stored endpoint for {transport:?}"
                                    )));
                                }
                            };
                            if new_endpoint_clone.locator == stored.locator {
                                return Ok(());
                            }
                            // Already filtered to unique VID/PID candidate, so directly upsert.
                            device_state.identity.upsert_endpoint(new_endpoint_clone);
                            Ok(())
                        })
                        .await
                        .map_err(ManagerError::State)??;
                    return Ok(new_endpoint);
                }
                _ => {
                    // Ambiguous: more than one same VID/PID candidate — do not guess.
                    // Keep polling until timeout.
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

    #[allow(dead_code)]
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
        let mut has_transport = false;
        for t in identity.endpoints.keys() {
            if device_state.identity.has_endpoint(*t) || identity.has_endpoint(*t) {
                has_transport = true;
                break;
            }
        }
        if !has_transport {
            return Err(ManagerError::VerificationMismatch {
                resource: "device identity",
                profile: None,
            });
        }
        for endpoint in identity.endpoints.into_values() {
            device_state.identity.upsert_endpoint(endpoint);
        }
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

async fn write_exact_metadata(
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
    if initial_rate != reloaded_rate {
        return Some("polling rate");
    }
    None
}
#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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
        ButtonAssignment, ButtonsState, DpiState, DpiValue, PollingRate, PreferencesState,
        ProfileId, ProfileMetadata, StageIndex, TransportKind,
    };

    use super::DeviceManager;
    use crate::backend::{DeviceSession, ScriptedFakeFactory, ScriptedFakeSession, SessionWrite};
    use crate::device::DeviceIdentity;
    use crate::error::ManagerError;
    use crate::operation::DiscoveredEndpoint;
    use crate::state::{
        ApplicationVerification, DesiredSource, DesiredState, PersistenceVerification,
        ProfileState, StateStore, Timestamp, Verification,
    };
    use async_trait::async_trait;

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
        let mismatch = super::first_mismatch(
            &initial,
            PollingRate::Hz1000,
            &reloaded,
            PollingRate::Hz500,
            profile(),
        );
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
