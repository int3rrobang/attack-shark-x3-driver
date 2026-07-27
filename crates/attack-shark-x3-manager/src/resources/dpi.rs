use attack_shark_x3::{DpiState, ProfileId, TransportKind};

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
    /// Reads one profile's complete DPI image and records USB readback evidence.
    pub async fn read_dpi(
        &self,
        device: &DeviceId,
        profile: ProfileId,
    ) -> Result<ResourceSnapshot<DpiState>, ManagerError> {
        let (_identity, session) = self.open_session(device).await?;
        let transport = session.transport();
        if transport == TransportKind::Ble {
            return Err(unsupported("read_dpi", transport));
        }

        let value = session.read_dpi(profile).await?;
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
        profile_state.dpi.observed = Some(ObservedState {
            value,
            source: ObservationSource::UsbReadback,
            observed_at: now,
        });
        let resource = profile_state.dpi.clone();
        transaction.state().validate()?;
        transaction.commit()?;

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

        let (_identity, session) = self.open_session(device).await?;
        let transport = session.transport();

        if transport == TransportKind::Ble && !policy.allow_explicit_defaults {
            let mut transaction = self.store().transaction()?;
            if !has_dpi_baseline(transaction.state(), device, profile) {
                return Err(ManagerError::MissingBaseline {
                    resource: "DPI",
                    profile: Some(profile),
                });
            }

            let write = session.write_dpi(desired.clone()).await?;
            let outcome = persist_dpi_write(
                &mut transaction,
                device,
                profile,
                desired,
                write,
                self.now(),
            )?;
            transaction.state().validate()?;
            transaction.commit()?;
            return finish_dpi_write(outcome, profile);
        }

        let write = session.write_dpi(desired.clone()).await?;
        let mut transaction = self.store().transaction()?;
        let outcome = persist_dpi_write(
            &mut transaction,
            device,
            profile,
            desired,
            write,
            self.now(),
        )?;
        transaction.state().validate()?;
        transaction.commit()?;
        finish_dpi_write(outcome, profile)
    }
}

fn unsupported(operation: &'static str, transport: TransportKind) -> ManagerError {
    ManagerError::UnsupportedOperation {
        operation,
        transport,
    }
}

fn has_dpi_baseline(state: &StateFile, device: &DeviceId, profile: ProfileId) -> bool {
    state
        .devices
        .get(device)
        .and_then(|device_state| device_state.profiles.get(&profile))
        .is_some_and(|profile_state| {
            profile_state.dpi.desired.is_some() || profile_state.dpi.observed.is_some()
        })
}

fn persist_dpi_write(
    transaction: &mut StateTransaction<'_>,
    device: &DeviceId,
    profile: ProfileId,
    desired: DpiState,
    write: SessionWrite<DpiState>,
    now: crate::state::Timestamp,
) -> Result<WriteOutcome<DpiState>, ManagerError> {
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
        .or_insert_with(crate::state::ProfileState::empty);
    profile_state.dpi.desired = Some(DesiredState {
        value: desired.clone(),
        source: DesiredSource::UserWrite,
        verification: Verification {
            application,
            persistence: PersistenceVerification::Unknown,
        },
        updated_at: now,
    });
    if let Some(readback) = observed.as_ref() {
        profile_state.dpi.observed = Some(ObservedState {
            value: readback.clone(),
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

fn finish_dpi_write(
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
    use super::DeviceManager;
    use crate::UpdatePolicy;
    use crate::backend::{ScriptedFakeFactory, ScriptedFakeSession};
    use crate::device::DeviceIdentity;
    use crate::error::ManagerError;
    use crate::state::{ObservationSource, StatePaths, StateStore};
    use attack_shark_x3::{DpiState, DpiValue, ProfileId, StageIndex, TransportKind};
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
            .update_dpi(&device, profile, desired.clone(), UpdatePolicy::default())
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
}
