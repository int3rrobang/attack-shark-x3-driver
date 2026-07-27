use attack_shark_x3::{ButtonAssignment, ButtonsState, ProfileId, TransportKind};
use serde::{Deserialize, Serialize};

use crate::backend::{DeviceSession, SessionWrite};
use crate::device::DeviceId;
use crate::error::ManagerError;
use crate::manager::DeviceManager;
use crate::operation::{ResourceSnapshot, UpdatePolicy, WriteOutcome};
use crate::state::{
    ApplicationVerification, DesiredSource, DesiredState, ObservationSource, ObservedState,
    PersistenceVerification, ResourceState, Verification,
};

pub use attack_shark_x3::protocol::buttons::BUTTON_SLOT_COUNT;

/// A bounded, typed update to one button slot.
///
/// A slot delta is only accepted by [`DeviceManager::update_button_slot`]. The
/// operation resolves a complete baseline before writing, so all slots not
/// named by the delta are preserved exactly.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ButtonSlotDelta {
    pub slot_index: usize,
    pub assignment: ButtonAssignment,
}

impl ButtonSlotDelta {
    /// Creates a slot delta after checking the protocol's fixed slot bound.
    pub fn new(slot_index: usize, assignment: ButtonAssignment) -> Result<Self, ManagerError> {
        if slot_index >= BUTTON_SLOT_COUNT {
            return Err(invalid_slot(slot_index));
        }
        Ok(Self {
            slot_index,
            assignment,
        })
    }
}

impl DeviceManager {
    /// Reads the complete button image for one profile.
    ///
    /// BLE has no button read path. USB reads update only `observed`; an
    /// existing desired value and its verification evidence are preserved.
    pub async fn read_buttons(
        &self,
        device: &DeviceId,
        profile: ProfileId,
    ) -> Result<ResourceSnapshot<ButtonsState>, ManagerError> {
        let (_identity, session) = self.open_session(device).await?;
        let transport = session.transport();
        if transport == TransportKind::Ble {
            return Err(unsupported_read(transport));
        }

        let buttons = session.read_buttons(profile).await?;
        if buttons.profile != profile {
            return Err(ManagerError::InvalidUpdate(format!(
                "button readback targets profile {} instead of requested profile {}",
                buttons.profile, profile
            )));
        }

        let now = self.now();
        let mut transaction = self.store().transaction()?;
        let device_state = transaction
            .state_mut()
            .devices
            .get_mut(device)
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
        let profile_state = device_state.profiles.entry(profile).or_default();
        profile_state.buttons.observed = Some(ObservedState {
            value: buttons,
            source: ObservationSource::UsbReadback,
            observed_at: now,
        });
        let resource = profile_state.buttons.clone();
        transaction.state().validate()?;
        transaction.commit()?;

        Ok(ResourceSnapshot { resource })
    }

    /// Writes a complete button image supplied by the caller.
    ///
    /// The complete image is passed through unchanged. USB writes are accepted
    /// only when the worker's complete readback exactly matches it. BLE writes
    /// persist only an acknowledged desired image and intentionally retain no
    /// observed evidence.
    pub async fn update_buttons(
        &self,
        device: &DeviceId,
        requested: ButtonsState,
        _policy: UpdatePolicy,
    ) -> Result<WriteOutcome<ButtonsState>, ManagerError> {
        let (_identity, session) = self.open_session(device).await?;
        self.write_buttons_with_session(device, session.as_ref(), requested)
            .await
    }

    /// Updates one slot while preserving a complete button baseline.
    ///
    /// USB obtains the baseline from a fresh read. BLE uses the latest stored
    /// observed or desired image, rejecting an absent or synthetic-default
    /// baseline unless `policy.allow_explicit_defaults` is enabled.
    pub async fn update_button_slot(
        &self,
        device: &DeviceId,
        profile: ProfileId,
        delta: ButtonSlotDelta,
        policy: UpdatePolicy,
    ) -> Result<WriteOutcome<ButtonsState>, ManagerError> {
        if delta.slot_index >= BUTTON_SLOT_COUNT {
            return Err(invalid_slot(delta.slot_index));
        }

        let (_identity, session) = self.open_session(device).await?;
        let transport = session.transport();
        let mut baseline = if transport == TransportKind::Ble {
            self.load_ble_buttons_baseline(device, profile, policy.allow_explicit_defaults)?
        } else {
            session.read_buttons(profile).await?
        };

        if baseline.profile != profile {
            return Err(ManagerError::InvalidUpdate(format!(
                "button baseline targets profile {} instead of requested profile {}",
                baseline.profile, profile
            )));
        }
        baseline.slots[delta.slot_index] = delta.assignment;
        self.write_buttons_with_session(device, session.as_ref(), baseline)
            .await
    }

    async fn write_buttons_with_session(
        &self,
        device: &DeviceId,
        session: &dyn DeviceSession,
        requested: ButtonsState,
    ) -> Result<WriteOutcome<ButtonsState>, ManagerError> {
        let transport = session.transport();
        let result = session.write_buttons(requested).await?;
        let (application, observed) = match (transport, result) {
            (
                TransportKind::Wired | TransportKind::Receiver,
                SessionWrite::ReadbackVerified(actual),
            ) => {
                if actual != requested || actual.profile != requested.profile {
                    return Err(ManagerError::VerificationMismatch {
                        resource: "buttons",
                        profile: Some(requested.profile),
                    });
                }
                (ApplicationVerification::ReadbackVerified, Some(actual))
            }
            (TransportKind::Wired | TransportKind::Receiver, SessionWrite::Acknowledged) => {
                (ApplicationVerification::Acknowledged, None)
            }
            (TransportKind::Ble, SessionWrite::Acknowledged) => {
                (ApplicationVerification::Acknowledged, None)
            }
            (TransportKind::Ble, SessionWrite::ReadbackVerified(_)) => {
                return Err(ManagerError::InvalidUpdate(
                    "BLE button writes cannot produce readback evidence".to_owned(),
                ));
            }
        };

        let verification = Verification {
            application,
            persistence: PersistenceVerification::Unknown,
        };
        self.persist_button_write(device, requested, observed, verification.clone())?;

        Ok(WriteOutcome {
            desired: requested,
            observed,
            verification,
        })
    }

    fn persist_button_write(
        &self,
        device: &DeviceId,
        requested: ButtonsState,
        observed: Option<ButtonsState>,
        verification: Verification,
    ) -> Result<(), ManagerError> {
        let profile = requested.profile;
        let now = self.now();
        let mut transaction = self.store().transaction()?;
        let device_state = transaction
            .state_mut()
            .devices
            .get_mut(device)
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
        let profile_state = device_state.profiles.entry(profile).or_default();
        profile_state.buttons = ResourceState {
            desired: Some(DesiredState {
                value: requested,
                source: DesiredSource::UserWrite,
                verification,
                updated_at: now,
            }),
            observed: observed.map(|value| ObservedState {
                value,
                source: ObservationSource::UsbReadback,
                observed_at: now,
            }),
        };
        transaction.state().validate()?;
        transaction.commit()?;
        Ok(())
    }

    fn load_ble_buttons_baseline(
        &self,
        device: &DeviceId,
        profile: ProfileId,
        allow_explicit_defaults: bool,
    ) -> Result<ButtonsState, ManagerError> {
        let state = self.store().load()?;
        let profile_state = state
            .devices
            .get(device)
            .and_then(|device_state| device_state.profiles.get(&profile));

        if let Some(resource) = profile_state.map(|profile| &profile.buttons) {
            if let Some(observed) = resource.observed.as_ref() {
                return Ok(observed.value);
            }
            if let Some(desired) = resource.desired.as_ref() {
                if allow_explicit_defaults || desired.source != DesiredSource::ExplicitDefaults {
                    return Ok(desired.value);
                }
            }
        }

        if allow_explicit_defaults {
            return Ok(ButtonsState::new(
                profile,
                [ButtonAssignment::default(); BUTTON_SLOT_COUNT],
            ));
        }

        Err(ManagerError::MissingBaseline {
            resource: "buttons",
            profile: Some(profile),
        })
    }
}

fn invalid_slot(slot_index: usize) -> ManagerError {
    ManagerError::InvalidUpdate(format!(
        "button slot index {slot_index} is outside 0..{BUTTON_SLOT_COUNT}"
    ))
}

fn unsupported_read(transport: TransportKind) -> ManagerError {
    ManagerError::UnsupportedOperation {
        operation: "read_buttons",
        transport,
    }
}

#[cfg(test)]
mod tests {
    use super::{BUTTON_SLOT_COUNT, ButtonSlotDelta};
    use crate::backend::{ScriptedFakeFactory, ScriptedFakeSession};
    use crate::device::DeviceIdentity;
    use crate::manager::DeviceManager;
    use crate::operation::UpdatePolicy;
    use crate::state::{StatePaths, StateStore};
    use attack_shark_x3::{
        ButtonAssignment, ButtonsState, DpiValue, PreferencesState, ProfileId, ProfileMetadata,
        StageIndex, TransportKind,
    };
    use std::sync::Arc;

    fn store(dir: &tempfile::TempDir) -> StateStore {
        StateStore::open(StatePaths {
            state_file: dir.path().join("state.json"),
            lock_file: dir.path().join("state.lock"),
        })
    }

    fn identity(transport: TransportKind) -> DeviceIdentity {
        match transport {
            TransportKind::Ble => {
                DeviceIdentity::ble("test-device", Some("Test")).expect("valid BLE identity")
            }
            transport => DeviceIdentity::usb(
                transport,
                0x1d57,
                0xfa61,
                Some("TEST001"),
                r"\\?\hid#test",
                Some("Test"),
            )
            .expect("valid USB identity"),
        }
    }

    fn buttons(profile: ProfileId) -> ButtonsState {
        let mut slots = [ButtonAssignment::default(); BUTTON_SLOT_COUNT];
        for (index, slot) in slots.iter_mut().enumerate() {
            *slot = ButtonAssignment::new(index as u8, 0xa0 | index as u8, 0xf0 ^ index as u8);
        }
        ButtonsState::new(profile, slots)
    }

    #[tokio::test]
    async fn slot_update_preserves_all_other_slots() {
        let dir = tempfile::tempdir().expect("tempdir");
        let profile = ProfileId::new(1).expect("profile");
        let device = identity(TransportKind::Wired);
        let source = buttons(profile);
        let session =
            ScriptedFakeSession::usb().with_profile(attack_shark_x3::driver::ProfileSnapshot {
                target_profile: profile,
                persistent_metadata: ProfileMetadata::new(profile, profile).expect("metadata"),
                dpi: attack_shark_x3::DpiState::captured_empty_profile_one(
                    vec![DpiValue::new(800).expect("dpi")],
                    StageIndex::new(1).expect("stage"),
                )
                .expect("dpi state"),
                preferences: PreferencesState::new(profile, 0, 0, 0, [0; 3], 0, 0),
                buttons: source,
            });
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            device.clone(),
            true,
            session.clone(),
        ));
        let manager = DeviceManager::with_store_and_factory(store(&dir), factory);
        manager.register_device(device.clone()).expect("register");

        let replacement = ButtonAssignment::new(0xee, 0xdd, 0xcc);
        manager
            .update_button_slot(
                &device.id,
                profile,
                ButtonSlotDelta::new(7, replacement).expect("valid delta"),
                UpdatePolicy::default(),
            )
            .await
            .expect("slot update");
        let stored = manager.store().load().expect("state");
        let actual = stored.devices.values().next().expect("device").profiles[&profile]
            .buttons
            .desired
            .as_ref()
            .expect("desired")
            .value;
        assert_eq!(actual.slots[7], replacement);
        for index in 0..BUTTON_SLOT_COUNT {
            if index != 7 {
                assert_eq!(actual.slots[index], source.slots[index]);
            }
        }
    }

    #[tokio::test]
    async fn ble_slot_update_requires_a_baseline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let device = identity(TransportKind::Ble);
        let id = device.id.clone();
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            device.clone(),
            true,
            ScriptedFakeSession::ble(),
        ));
        let manager = DeviceManager::with_store_and_factory(store(&dir), factory);
        manager.register_device(device).expect("register");

        let error = manager
            .update_button_slot(
                &id,
                ProfileId::new(1).expect("profile"),
                ButtonSlotDelta::new(0, ButtonAssignment::default()).expect("delta"),
                UpdatePolicy::default(),
            )
            .await
            .expect_err("missing baseline must fail");
        assert!(matches!(
            error,
            crate::ManagerError::MissingBaseline {
                resource: "buttons",
                ..
            }
        ));
    }
}
