use attack_shark_x3::{ButtonAssignment, ButtonsState, ProfileId, TransportKind};
use serde::{Deserialize, Serialize};

/// Physical button slot exposed by the safe public API.
///
/// Scroll slots (indices 4, 5) are intentionally excluded because direct
/// scroll remaps may repeat until unplug or reboot on some firmware.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SafeButtonSlot {
    Left,
    Right,
    Middle,
    Dpi,
    Forward,
    Backward,
}

impl SafeButtonSlot {
    /// The zero-based slot index within the 18-slot button table.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
            Self::Middle => 2,
            Self::Dpi => 3,
            Self::Forward => 6,
            Self::Backward => 7,
        }
    }
}

/// Firmware action code exposed by the safe public API.
///
/// Every variant maps to an X3/FA61-confirmed firmware byte with zero
/// modifier and key-code fields. Scroll-up, scroll-down, custom macro,
/// and arbitrary raw action codes are intentionally excluded.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SafeButtonAction {
    Disable,
    LeftClick,
    RightClick,
    MiddleClick,
    Backward,
    Forward,
    DoubleClick,
    DpiCycle,
    DpiPlus,
    DpiMinus,
    ProfileCycle,
    ProfilePlus,
    ProfileMinus,
}

impl SafeButtonAction {
    /// The firmware action byte for this action.
    #[must_use]
    pub const fn firmware_byte(self) -> u8 {
        match self {
            Self::Disable => 0x01,
            Self::LeftClick => 0x02,
            Self::RightClick => 0x03,
            Self::MiddleClick => 0x04,
            Self::Backward => 0x05,
            Self::Forward => 0x06,
            Self::DoubleClick => 0x07,
            Self::DpiCycle => 0x0d,
            Self::DpiPlus => 0x0e,
            Self::DpiMinus => 0x0f,
            Self::ProfileCycle => 0x34,
            Self::ProfilePlus => 0x35,
            Self::ProfileMinus => 0x36,
        }
    }

    /// Converts to a [`ButtonAssignment`] with zero modifier and key-code.
    #[must_use]
    pub fn to_assignment(self) -> ButtonAssignment {
        ButtonAssignment::new(self.firmware_byte(), 0, 0)
    }
}

use crate::backend::{DeviceSession, SessionWrite};
use crate::device::DeviceId;
use crate::error::ManagerError;
use crate::manager::DeviceManager;
use crate::operation::{ResourceSnapshot, UpdatePolicy, WriteOutcome};
use crate::state::{
    ApplicationVerification, DesiredSource, DesiredState, ObservationSource, ObservedState,
    PersistenceVerification, ResourceState, Verification,
};
#[allow(unused_imports)]
pub use attack_shark_x3::protocol::buttons::BUTTON_SLOT_COUNT;

/// A bounded, typed update to one safe button slot.
///
/// A slot delta is only accepted by [`DeviceManager::update_button_slot`]. The
/// operation resolves a complete baseline before writing, so all slots not
/// named by the delta are preserved exactly.
///
/// Only safe slot/action combinations are accepted. The raw slot index and
/// firmware assignment are available through read-only accessors for
/// downstream protocol code.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ButtonSlotDelta {
    slot: SafeButtonSlot,
    action: SafeButtonAction,
}

impl ButtonSlotDelta {
    /// Creates a safe slot delta.
    #[must_use]
    pub fn new(slot: SafeButtonSlot, action: SafeButtonAction) -> Self {
        Self { slot, action }
    }

    /// The safe button slot.
    #[must_use]
    pub fn slot(self) -> SafeButtonSlot {
        self.slot
    }

    /// The safe button action.
    #[must_use]
    pub fn action(self) -> SafeButtonAction {
        self.action
    }

    /// The zero-based slot index within the 18-slot table.
    #[must_use]
    pub fn slot_index(self) -> usize {
        self.slot.index()
    }

    /// The firmware-level [`ButtonAssignment`] (action byte + zero modifier/key-code).
    #[must_use]
    pub fn assignment(self) -> ButtonAssignment {
        self.action.to_assignment()
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
        let slot_index = delta.slot_index();
        let assignment = delta.assignment();

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
        baseline.slots[slot_index] = assignment;
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
            return Ok(ButtonsState::default_for_profile(profile));
        }

        Err(ManagerError::MissingBaseline {
            resource: "buttons",
            profile: Some(profile),
        })
    }
}

fn unsupported_read(transport: TransportKind) -> ManagerError {
    ManagerError::UnsupportedOperation {
        operation: "read_buttons",
        transport,
    }
}

#[cfg(test)]
mod tests {
    use super::{BUTTON_SLOT_COUNT, ButtonSlotDelta, SafeButtonAction, SafeButtonSlot};
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

        manager
            .update_button_slot(
                &device.id,
                profile,
                ButtonSlotDelta::new(SafeButtonSlot::Backward, SafeButtonAction::ProfileCycle),
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
        let expected_assignment = SafeButtonAction::ProfileCycle.to_assignment();
        assert_eq!(actual.slots[7], expected_assignment);
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
                ButtonSlotDelta::new(SafeButtonSlot::Left, SafeButtonAction::Disable),
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
