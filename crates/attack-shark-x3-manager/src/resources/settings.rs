use attack_shark_x3::{PollingRate, PreferencesState, ProfileId, TransportKind};

use crate::backend::SessionWrite;
use crate::device::DeviceId;
use crate::error::ManagerError;
use crate::manager::DeviceManager;
use crate::operation::{ResourceSnapshot, UpdatePolicy, WriteOutcome};
use crate::state::{
    ApplicationVerification, DesiredSource, DesiredState, ObservationSource, ObservedState,
    PersistenceVerification, ProfileState, StateFile, StateTransaction, Verification,
};

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
            let mut transaction = self.store().transaction()?;
            if !has_preferences_baseline(transaction.state(), device, profile) {
                return Err(ManagerError::MissingBaseline {
                    resource: "preferences",
                    profile: Some(profile),
                });
            }

            let write = session.write_preferences(desired).await?;
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
    use super::DeviceManager;
    use crate::UpdatePolicy;
    use crate::backend::{ScriptedFakeFactory, ScriptedFakeSession};
    use crate::device::DeviceIdentity;
    use crate::state::{ObservationSource, StatePaths, StateStore};
    use attack_shark_x3::{PollingRate, PreferencesState, ProfileId, TransportKind};
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
