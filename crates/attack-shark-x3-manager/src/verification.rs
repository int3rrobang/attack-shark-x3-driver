use attack_shark_x3::driver::ProfileSnapshot;
use attack_shark_x3::{ProfileId, ProfileMetadata, TransportKind};

use crate::device::DeviceId;
use crate::error::ManagerError;
use crate::manager::DeviceManager;
use crate::operation::{PowerCycleVerificationOutcome, ProfileVerificationOutcome, WriteOutcome};
use crate::state::{
    ApplicationVerification, PersistenceVerification, ProfileState, ResourceState, Verification,
};

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
        self.persist_profile_observation(
            device,
            target,
            &reloaded,
            mismatch.is_none(),
            verified_at,
        )?;

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

    /// Physical power cycling is outside the manager's non-interactive
    /// capability boundary. Never report a persistence claim for it.
    pub async fn verify_power_cycle(
        &self,
        device: &DeviceId,
        _target: ProfileId,
    ) -> Result<PowerCycleVerificationOutcome, ManagerError> {
        let (_, session) = self.open_session(device).await?;
        Err(ManagerError::UnsupportedOperation {
            operation: "power-cycle-verification",
            transport: session.transport(),
        })
    }

    fn persist_profile_observation(
        &self,
        device: &DeviceId,
        target: ProfileId,
        snapshot: &ProfileSnapshot,
        complete_match: bool,
        verified_at: crate::state::Timestamp,
    ) -> Result<(), ManagerError> {
        let mut transaction = self.store().transaction()?;
        transaction.state().validate()?;
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

        if complete_match {
            mark_persistence(&mut profile_state.dpi, &snapshot.dpi, verified_at);
            mark_persistence(
                &mut profile_state.preferences,
                &snapshot.preferences,
                verified_at,
            );
            mark_persistence(&mut profile_state.buttons, &snapshot.buttons, verified_at);
        } else {
            invalidate_persistence(&mut profile_state.dpi);
            invalidate_persistence(&mut profile_state.preferences);
            invalidate_persistence(&mut profile_state.buttons);
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

fn first_mismatch(
    initial: &ProfileSnapshot,
    reloaded: &ProfileSnapshot,
    target: ProfileId,
) -> Option<&'static str> {
    if reloaded.target_profile != target || initial.target_profile != target {
        return Some("profile");
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
    verified_at: crate::state::Timestamp,
) {
    if let Some(desired) = resource.desired.as_mut() {
        if desired.value == *readback {
            desired.verification.persistence =
                PersistenceVerification::ProfileReloadVerified { verified_at };
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
