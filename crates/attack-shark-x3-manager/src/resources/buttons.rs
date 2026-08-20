use attack_shark_x3::{ButtonAssignment, ButtonsState, ProfileId, TransportKind};
use serde::{Deserialize, Serialize};
/// binding; those action bytes stay out of the safe set until confirmed.
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
/// Parameterless variants map to an X3/FA61-confirmed firmware byte with
/// zero modifier and key-code fields; shortcut presets map to confirmed
/// `0x11` keyboard-shortcut triplets. Both families are stock-app
/// capture-confirmed 2026-08-14. Easy-aim, custom macro, and arbitrary raw
/// action codes are intentionally excluded.
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
    FireButton,
    ScrollUp,
    ScrollDown,
    DpiCycle,
    DpiPlus,
    DpiMinus,
    ProfileCycle,
    ProfilePlus,
    ProfileMinus,
    MediaPlayer,
    PreviousTrack,
    NextTrack,
    PlayPause,
    Stop,
    Mute,
    VolumeUp,
    VolumeDown,
    Calculator,
    Email,
    BrowserForward,
    BrowserBackward,
    BrowserStop,
    MyComputer,
    BrowserRefresh,
    BrowserHome,
    BrowserSearch,
    BrowserFavorites,
    Cut,
    Copy,
    Paste,
    Open,
    Save,
    Find,
    Redo,
    SelectAll,
    Print,
    CloseWindow,
    SwapWindows,
    ShowDesktop,
    RunCommand,
    LockPc,
    ScreenCapture,
}

impl SafeButtonAction {
    /// The firmware action byte for this action.
    ///
    /// Shortcut presets share the `0x11` keyboard-shortcut action byte;
    /// their modifiers and key code live in the assignment.
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
            Self::FireButton => 0x08,
            Self::ScrollUp => 0x09,
            Self::ScrollDown => 0x0a,
            Self::DpiCycle => 0x0d,
            Self::DpiPlus => 0x0e,
            Self::DpiMinus => 0x0f,
            Self::ProfileCycle => 0x34,
            Self::ProfilePlus => 0x35,
            Self::ProfileMinus => 0x36,
            Self::MediaPlayer => 0x15,
            Self::PreviousTrack => 0x16,
            Self::NextTrack => 0x17,
            Self::PlayPause => 0x18,
            Self::Stop => 0x19,
            Self::Mute => 0x1a,
            Self::VolumeUp => 0x1b,
            Self::VolumeDown => 0x1c,
            Self::Calculator => 0x1d,
            Self::Email => 0x1e,
            Self::BrowserForward => 0x20,
            Self::BrowserBackward => 0x21,
            Self::BrowserStop => 0x22,
            Self::MyComputer => 0x23,
            Self::BrowserRefresh => 0x24,
            Self::BrowserHome => 0x25,
            Self::BrowserSearch => 0x26,
            Self::BrowserFavorites
            | Self::Cut
            | Self::Copy
            | Self::Paste
            | Self::Open
            | Self::Save
            | Self::Find
            | Self::Redo
            | Self::SelectAll
            | Self::Print
            | Self::CloseWindow
            | Self::SwapWindows
            | Self::ShowDesktop
            | Self::RunCommand
            | Self::LockPc
            | Self::ScreenCapture => 0x11,
        }
    }

    /// Converts to a [`ButtonAssignment`].
    ///
    /// Parameterless variants produce zero modifier and key-code; shortcut
    /// presets produce their confirmed `0x11` triplets (stock-app
    /// capture-confirmed 2026-08-14).
    #[must_use]
    pub fn to_assignment(self) -> ButtonAssignment {
        match self {
            Self::Cut => ButtonAssignment::new(0x11, 0x01, 0x1b),
            Self::Copy => ButtonAssignment::new(0x11, 0x01, 0x06),
            Self::Paste => ButtonAssignment::new(0x11, 0x01, 0x19),
            Self::Open => ButtonAssignment::new(0x11, 0x01, 0x12),
            Self::Save => ButtonAssignment::new(0x11, 0x01, 0x16),
            Self::Find => ButtonAssignment::new(0x11, 0x01, 0x09),
            Self::Redo => ButtonAssignment::new(0x11, 0x01, 0x1c),
            Self::SelectAll => ButtonAssignment::new(0x11, 0x01, 0x04),
            Self::Print => ButtonAssignment::new(0x11, 0x01, 0x13),
            Self::CloseWindow => ButtonAssignment::new(0x11, 0x04, 0x3d),
            Self::SwapWindows => ButtonAssignment::new(0x11, 0x04, 0x2b),
            Self::ShowDesktop => ButtonAssignment::new(0x11, 0x08, 0x07),
            Self::RunCommand => ButtonAssignment::new(0x11, 0x08, 0x15),
            Self::LockPc => ButtonAssignment::new(0x11, 0x08, 0x0f),
            Self::ScreenCapture => ButtonAssignment::new(0x11, 0x0a, 0x16),
            Self::BrowserFavorites => ButtonAssignment::new(0x11, 0x03, 0x12),
            _ => ButtonAssignment::new(self.firmware_byte(), 0, 0),
        }
    }
}

use crate::backend::DeviceSession;
use crate::device::DeviceId;
use crate::error::ManagerError;
use crate::manager::DeviceManager;
use crate::operation::{
    BaselineSource, ResourceSnapshot, UpdatePolicy, VerificationMethod, WriteOutcome,
};
use crate::resources::state::reconcile_observed;
use crate::state::DesiredSource;

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
        let (_identity, session, _guard) = self.open_locked(device, "read_buttons").await?;
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
        let device_id = device.clone();
        let resource = self
            .store()
            .mutate_async(move |state| {
                let device_state = state
                    .devices
                    .get_mut(&device_id)
                    .ok_or_else(|| ManagerError::DeviceNotFound(device_id.clone()))?;
                let profile_state = device_state.profiles.entry(profile).or_default();
                reconcile_observed(&mut profile_state.buttons, buttons, now);
                Ok::<_, ManagerError>(profile_state.buttons.clone())
            })
            .await??;

        Ok(ResourceSnapshot { resource })
    }

    /// Updates one slot while preserving a complete button baseline.
    ///
    /// With the default [`BaselineSource::Live`] policy, USB and receiver
    /// obtain the baseline from a fresh read; BLE cannot read and always
    /// merges against the stored image. With [`BaselineSource::Stored`],
    /// every transport merges against the latest stored desired or observed
    /// image, rejecting an absent or synthetic-default baseline unless
    /// `policy.allow_explicit_defaults` is enabled.
    pub async fn update_button_slot(
        &self,
        device: &DeviceId,
        profile: ProfileId,
        delta: ButtonSlotDelta,
        policy: UpdatePolicy,
    ) -> Result<WriteOutcome<ButtonsState>, ManagerError> {
        let slot_index = delta.slot_index();
        let assignment = delta.assignment();

        let (_identity, session, _guard) = self.open_locked(device, "update_button_slot").await?;
        let transport = session.transport();
        let mut baseline = match transport {
            TransportKind::Ble => {
                self.load_stored_buttons_baseline(device, profile, policy.allow_explicit_defaults)
                    .await?
            }
            TransportKind::Wired | TransportKind::Receiver => match policy.baseline {
                BaselineSource::Live => session.read_buttons(profile).await?,
                BaselineSource::Stored => {
                    self.load_stored_buttons_baseline(device, profile, false)
                        .await?
                }
            },
        };
        baseline.slots[slot_index] = assignment;
        self.write_buttons_with_session(device, session.as_ref(), baseline, policy.verification)
            .await
    }

    pub(crate) async fn write_buttons_with_session(
        &self,
        device: &DeviceId,
        session: &dyn DeviceSession,
        requested: ButtonsState,
        verification: VerificationMethod,
    ) -> Result<WriteOutcome<ButtonsState>, ManagerError> {
        let result = session.write_buttons(requested, verification).await?;

        let now = self.now();
        let device_id = device.clone();
        let requested_for_store = requested;
        let profile = requested_for_store.profile;
        let outcome = self
            .store()
            .mutate_async(move |state| {
                crate::resources::state::persist_write(
                    state,
                    &device_id,
                    profile,
                    |ps| &mut ps.buttons,
                    requested_for_store,
                    result,
                    now,
                )
            })
            .await??;

        crate::resources::state::finish_write(outcome, "buttons", profile)
    }

    /// image, then [`ButtonsState::default_for_profile`] only when
    /// `allow_explicit_defaults` is enabled, and finally
    /// [`ManagerError::MissingBaseline`] when nothing is stored.
    pub(crate) async fn load_stored_buttons_baseline(
        &self,
        device: &DeviceId,
        profile: ProfileId,
        allow_explicit_defaults: bool,
    ) -> Result<ButtonsState, ManagerError> {
        let state = self.store().load_async().await?;
        let profile_state = state
            .devices
            .get(device)
            .and_then(|device_state| device_state.profiles.get(&profile));

        if let Some(resource) = profile_state.map(|profile| &profile.buttons) {
            if let Some(desired) = resource.desired.as_ref()
                && (allow_explicit_defaults || desired.source != DesiredSource::ExplicitDefaults)
            {
                return Ok(desired.value);
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
    use super::{ButtonSlotDelta, SafeButtonAction, SafeButtonSlot};
    use crate::backend::{ScriptedFakeFactory, ScriptedFakeSession};
    use crate::device::DeviceIdentity;
    use crate::manager::DeviceManager;
    use crate::operation::{BaselineSource, UpdatePolicy, VerificationMethod};
    use crate::state::{StatePaths, StateStore};
    use attack_shark_x3::protocol::buttons::BUTTON_SLOT_COUNT;
    use attack_shark_x3::{
        ButtonAssignment, ButtonsState, DpiValue, PreferencesState, ProfileId, ProfileMetadata,
        StageIndex, TransportKind,
    };
    use std::sync::Arc;

    fn store(dir: &tempfile::TempDir) -> StateStore {
        StateStore::open(StatePaths::new(dir.path().join("state.json")))
    }

    fn identity(transport: TransportKind) -> DeviceIdentity {
        match transport {
            TransportKind::Ble => {
                DeviceIdentity::test_ble("test-device", Some("Test")).expect("valid BLE identity")
            }
            transport => DeviceIdentity::test_usb(
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

    #[test]
    fn captured_stock_actions_encode_to_their_wire_triplets() {
        let cases = [
            (SafeButtonAction::FireButton, [0x08, 0x00, 0x00]),
            (SafeButtonAction::ScrollUp, [0x09, 0x00, 0x00]),
            (SafeButtonAction::ScrollDown, [0x0a, 0x00, 0x00]),
            (SafeButtonAction::VolumeUp, [0x1b, 0x00, 0x00]),
            (SafeButtonAction::BrowserHome, [0x25, 0x00, 0x00]),
            (SafeButtonAction::BrowserFavorites, [0x11, 0x03, 0x12]),
            (SafeButtonAction::Cut, [0x11, 0x01, 0x1b]),
            (SafeButtonAction::CloseWindow, [0x11, 0x04, 0x3d]),
            (SafeButtonAction::ScreenCapture, [0x11, 0x0a, 0x16]),
        ];
        for (action, expected) in cases {
            let assignment = action.to_assignment();
            assert_eq!(assignment.as_bytes(), expected, "{action:?}");
        }
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
    async fn usb_transport_slot_write_is_acknowledged_without_observed() {
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
        let factory =
            Arc::new(ScriptedFakeFactory::new().with_identity(device.clone(), true, session));
        let manager = DeviceManager::with_store_and_factory(store(&dir), factory);
        manager.register_device(device.clone()).expect("register");

        let outcome = manager
            .update_button_slot(
                &device.id,
                profile,
                ButtonSlotDelta::new(SafeButtonSlot::Backward, SafeButtonAction::ProfileCycle),
                UpdatePolicy::default(),
            )
            .await
            .expect("slot update");
        assert!(outcome.observed.is_none());
        assert_eq!(
            outcome.verification.application,
            crate::ApplicationVerification::Acknowledged
        );
        let stored = manager.store().load().expect("state");
        let resource = &stored.devices.values().next().expect("device").profiles[&profile].buttons;
        assert!(resource.observed.is_none());
        assert_eq!(
            resource
                .desired
                .as_ref()
                .expect("desired")
                .verification
                .application,
            crate::ApplicationVerification::Acknowledged
        );
    }

    #[tokio::test]
    async fn usb_readback_slot_write_produces_observed_evidence() {
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
        let factory =
            Arc::new(ScriptedFakeFactory::new().with_identity(device.clone(), true, session));
        let manager = DeviceManager::with_store_and_factory(store(&dir), factory);
        manager.register_device(device.clone()).expect("register");

        let outcome = manager
            .update_button_slot(
                &device.id,
                profile,
                ButtonSlotDelta::new(SafeButtonSlot::Backward, SafeButtonAction::ProfileCycle),
                UpdatePolicy {
                    verification: VerificationMethod::Readback,
                    ..UpdatePolicy::default()
                },
            )
            .await
            .expect("slot update");
        assert_eq!(
            outcome.verification.application,
            crate::ApplicationVerification::ReadbackVerified
        );
        let observed = outcome.observed.expect("readback must produce observed");
        assert_eq!(
            observed.slots[7],
            SafeButtonAction::ProfileCycle.to_assignment()
        );
        for index in 0..BUTTON_SLOT_COUNT {
            if index != 7 {
                assert_eq!(observed.slots[index], source.slots[index]);
            }
        }
        let stored = manager.store().load().expect("state");
        let resource = &stored.devices.values().next().expect("device").profiles[&profile].buttons;
        assert_eq!(
            resource.observed.as_ref().expect("observed").value.slots[7],
            SafeButtonAction::ProfileCycle.to_assignment()
        );
        assert_eq!(
            resource
                .desired
                .as_ref()
                .expect("desired")
                .verification
                .application,
            crate::ApplicationVerification::ReadbackVerified
        );
    }

    #[tokio::test]
    async fn ble_readback_slot_write_is_unsupported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let profile = ProfileId::new(1).expect("profile");
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
                profile,
                ButtonSlotDelta::new(SafeButtonSlot::Left, SafeButtonAction::Disable),
                UpdatePolicy {
                    allow_explicit_defaults: true,
                    verification: VerificationMethod::Readback,
                    baseline: BaselineSource::Live,
                },
            )
            .await
            .expect_err("BLE readback must fail");
        assert!(matches!(
            error,
            crate::ManagerError::UnsupportedOperation { .. }
        ));
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

    #[tokio::test]
    async fn wired_stored_baseline_slot_update_skips_live_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let profile = ProfileId::new(1).expect("profile");
        let device = identity(TransportKind::Wired);
        let source = buttons(profile);
        // No scripted buttons: any live read would fail with MissingBaseline,
        // proving the Stored baseline policy never touches the session.
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            device.clone(),
            true,
            ScriptedFakeSession::usb(),
        ));
        let manager = DeviceManager::with_store_and_factory(store(&dir), factory);
        manager.register_device(device.clone()).expect("register");
        // Seed stored baseline via the central reconciliation path (record_ack)
        // rather than the removed `persist_button_write` helper.
        {
            let mut txn = manager.store().transaction().expect("txn");
            let device_state = txn.state_mut().devices.get_mut(&device.id).expect("device");
            let profile_state = device_state.profiles.entry(profile).or_default();
            crate::resources::state::record_ack(
                &mut profile_state.buttons,
                source,
                crate::state::Timestamp::default(),
            );
            txn.commit().expect("commit");
        }
        let outcome = manager
            .update_button_slot(
                &device.id,
                profile,
                ButtonSlotDelta::new(SafeButtonSlot::Backward, SafeButtonAction::ProfileCycle),
                UpdatePolicy {
                    baseline: BaselineSource::Stored,
                    ..UpdatePolicy::default()
                },
            )
            .await
            .expect("stored-baseline slot update must succeed without a live read");
        assert_eq!(
            outcome.desired.slots[7],
            SafeButtonAction::ProfileCycle.to_assignment()
        );
        for index in 0..BUTTON_SLOT_COUNT {
            if index != 7 {
                assert_eq!(outcome.desired.slots[index], source.slots[index]);
            }
        }
    }

    #[tokio::test]
    async fn wired_stored_baseline_without_stored_image_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let profile = ProfileId::new(1).expect("profile");
        let device = identity(TransportKind::Wired);
        let id = device.id.clone();
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            device.clone(),
            true,
            ScriptedFakeSession::usb(),
        ));
        let manager = DeviceManager::with_store_and_factory(store(&dir), factory);
        manager.register_device(device).expect("register");

        let error = manager
            .update_button_slot(
                &id,
                profile,
                ButtonSlotDelta::new(SafeButtonSlot::Left, SafeButtonAction::Disable),
                UpdatePolicy {
                    baseline: BaselineSource::Stored,
                    ..UpdatePolicy::default()
                },
            )
            .await
            .expect_err("stored policy without a stored baseline must fail");
        assert!(matches!(
            error,
            crate::ManagerError::MissingBaseline {
                resource: "buttons",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn ack_preserves_historical_observed_and_resets_persistence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let profile = ProfileId::new(1).expect("profile");
        let device = identity(TransportKind::Wired);
        let id = device.id.clone();
        let initial = buttons(profile);
        let factory = Arc::new(
            ScriptedFakeFactory::new().with_identity(
                device.clone(),
                true,
                ScriptedFakeSession::usb().with_profile(attack_shark_x3::driver::ProfileSnapshot {
                    target_profile: profile,
                    persistent_metadata: ProfileMetadata::new(profile, profile).expect("metadata"),
                    dpi: attack_shark_x3::DpiState::captured_empty_profile_one(
                        vec![DpiValue::new(800).expect("dpi")],
                        StageIndex::new(1).expect("stage"),
                    )
                    .expect("dpi"),
                    preferences: PreferencesState::new(profile, 0, 0, 0, [0; 3], 0, 0),
                    buttons: initial,
                }),
            ),
        );
        let manager = DeviceManager::with_store_and_factory(store(&dir), factory);
        manager.register_device(device.clone()).expect("register");
        let first_outcome = manager
            .update_button_slot(
                &id,
                profile,
                ButtonSlotDelta::new(SafeButtonSlot::Backward, SafeButtonAction::ProfileCycle),
                UpdatePolicy {
                    verification: VerificationMethod::Readback,
                    ..UpdatePolicy::default()
                },
            )
            .await
            .expect("first write");
        let first_observed = first_outcome.observed.expect("readback");
        let second_outcome = manager
            .update_button_slot(
                &id,
                profile,
                ButtonSlotDelta::new(SafeButtonSlot::Forward, SafeButtonAction::Disable),
                UpdatePolicy::default(),
            )
            .await
            .expect("second write ack");
        assert!(second_outcome.observed.is_none());
        assert_eq!(
            second_outcome.verification.application,
            crate::ApplicationVerification::Acknowledged
        );
        let state = manager.store().load().expect("load");
        let resource = &state.devices[&id].profiles[&profile].buttons;
        assert_eq!(resource.observed.as_ref().unwrap().value, first_observed);
        assert_eq!(
            resource.desired.as_ref().unwrap().verification.application,
            crate::ApplicationVerification::Acknowledged
        );
        assert!(
            resource
                .desired
                .as_ref()
                .unwrap()
                .verification
                .persistence
                .is_unknown()
        );
        assert!(
            resource.observed.as_ref().unwrap().observed_at.unix_seconds
                <= resource.desired.as_ref().unwrap().updated_at.unix_seconds
        );
        state.validate().expect("validate");
        let mut cloned = resource.clone();
        assert!(
            !cloned
                .try_mark_profile_reload_verified(crate::state::Timestamp { unix_seconds: 9999 })
        );
    }

    #[tokio::test]
    async fn read_mismatch_marks_mismatch_and_resets_persistence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let profile = ProfileId::new(1).expect("profile");
        let device = identity(TransportKind::Wired);
        let id = device.id.clone();
        let initial = buttons(profile);
        let factory1 = Arc::new(
            ScriptedFakeFactory::new().with_identity(
                device.clone(),
                true,
                ScriptedFakeSession::usb().with_profile(attack_shark_x3::driver::ProfileSnapshot {
                    target_profile: profile,
                    persistent_metadata: ProfileMetadata::new(profile, profile).expect("metadata"),
                    dpi: attack_shark_x3::DpiState::captured_empty_profile_one(
                        vec![DpiValue::new(800).expect("dpi")],
                        StageIndex::new(1).expect("stage"),
                    )
                    .expect("dpi"),
                    preferences: PreferencesState::new(profile, 0, 0, 0, [0; 3], 0, 0),
                    buttons: initial,
                }),
            ),
        );
        let store_dir = store(&dir);
        let manager1 = DeviceManager::with_store_and_factory(store_dir.clone(), factory1);
        manager1.register_device(device.clone()).expect("register");
        manager1
            .update_button_slot(
                &id,
                profile,
                ButtonSlotDelta::new(SafeButtonSlot::Backward, SafeButtonAction::ProfileCycle),
                UpdatePolicy {
                    verification: VerificationMethod::Readback,
                    ..UpdatePolicy::default()
                },
            )
            .await
            .expect("seed");
        let mut different = initial;
        different.slots[0] = SafeButtonAction::Disable.to_assignment();
        let factory2 = Arc::new(
            ScriptedFakeFactory::new().with_identity(
                device.clone(),
                true,
                ScriptedFakeSession::usb().with_profile(attack_shark_x3::driver::ProfileSnapshot {
                    target_profile: profile,
                    persistent_metadata: ProfileMetadata::new(profile, profile).expect("metadata"),
                    dpi: attack_shark_x3::DpiState::captured_empty_profile_one(
                        vec![DpiValue::new(800).expect("dpi")],
                        StageIndex::new(1).expect("stage"),
                    )
                    .expect("dpi"),
                    preferences: PreferencesState::new(profile, 0, 0, 0, [0; 3], 0, 0),
                    buttons: different,
                }),
            ),
        );
        let manager2 = DeviceManager::with_store_and_factory(store_dir.clone(), factory2);
        let snap = manager2.read_buttons(&id, profile).await.expect("read");
        assert_eq!(snap.resource.observed.as_ref().unwrap().value, different);
        assert_eq!(
            snap.resource
                .desired
                .as_ref()
                .unwrap()
                .verification
                .application,
            crate::ApplicationVerification::Mismatch
        );
        assert!(
            snap.resource
                .desired
                .as_ref()
                .unwrap()
                .verification
                .persistence
                .is_unknown()
        );
        store_dir.load().unwrap().validate().unwrap();
    }

    #[tokio::test]
    async fn button_slot_live_baseline_uses_one_session() {
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
            ) -> Result<Vec<DiscoveredEndpoint>, crate::error::ManagerError> {
                self.inner.list(selection).await
            }

            async fn open(
                &self,
                endpoint: &DeviceEndpoint,
            ) -> Result<Box<dyn crate::backend::DeviceSession>, crate::error::ManagerError>
            {
                self.opens.fetch_add(1, Ordering::SeqCst);
                self.inner.open(endpoint).await
            }
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let profile = ProfileId::new(1).expect("profile");
        let device = identity(TransportKind::Wired);
        let id = device.id.clone();
        let source = buttons(profile);
        let session =
            ScriptedFakeSession::usb().with_profile(attack_shark_x3::driver::ProfileSnapshot {
                target_profile: profile,
                persistent_metadata: ProfileMetadata::new(profile, profile).expect("metadata"),
                dpi: attack_shark_x3::DpiState::captured_empty_profile_one(
                    vec![DpiValue::new(800).expect("dpi")],
                    StageIndex::new(1).expect("stage"),
                )
                .expect("dpi"),
                preferences: PreferencesState::new(profile, 0, 0, 0, [0; 3], 0, 0),
                buttons: source,
            });
        let opens = Arc::new(AtomicUsize::new(0));
        let inner = ScriptedFakeFactory::new().with_identity(device.clone(), true, session);
        let factory = Arc::new(CountingFactory {
            inner,
            opens: opens.clone(),
        });
        let manager = DeviceManager::with_store_and_factory(store(&dir), factory);
        manager.register_device(device).expect("register");

        let result = manager
            .update_button_slot(
                &id,
                profile,
                ButtonSlotDelta::new(SafeButtonSlot::Backward, SafeButtonAction::ProfileCycle),
                UpdatePolicy::default(),
            )
            .await;
        assert!(
            !matches!(
                result.as_ref().err(),
                Some(crate::error::ManagerError::DeviceOperationBusy { .. })
            ),
            "live baseline must not trigger nested lock, got {result:?}"
        );
        result.unwrap();
        assert_eq!(
            opens.load(Ordering::SeqCst),
            1,
            "live button slot update must open exactly one session"
        );
    }

    #[tokio::test]
    async fn button_slot_stored_baseline_uses_one_session() {
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
            ) -> Result<Vec<DiscoveredEndpoint>, crate::error::ManagerError> {
                self.inner.list(selection).await
            }

            async fn open(
                &self,
                endpoint: &DeviceEndpoint,
            ) -> Result<Box<dyn crate::backend::DeviceSession>, crate::error::ManagerError>
            {
                self.opens.fetch_add(1, Ordering::SeqCst);
                self.inner.open(endpoint).await
            }
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let profile = ProfileId::new(1).expect("profile");
        let device = identity(TransportKind::Wired);
        let id = device.id.clone();
        let source = buttons(profile);
        let opens = Arc::new(AtomicUsize::new(0));
        let inner = ScriptedFakeFactory::new().with_identity(
            device.clone(),
            true,
            ScriptedFakeSession::usb(),
        );
        let factory = Arc::new(CountingFactory {
            inner,
            opens: opens.clone(),
        });
        let manager = DeviceManager::with_store_and_factory(store(&dir), factory);
        manager.register_device(device.clone()).expect("register");
        {
            let mut txn = manager.store().transaction().expect("txn");
            let device_state = txn.state_mut().devices.get_mut(&id).expect("device");
            let profile_state = device_state.profiles.entry(profile).or_default();
            crate::resources::state::record_ack(
                &mut profile_state.buttons,
                source,
                crate::state::Timestamp::default(),
            );
            txn.commit().expect("commit");
        }
        let result = manager
            .update_button_slot(
                &id,
                profile,
                ButtonSlotDelta::new(SafeButtonSlot::Backward, SafeButtonAction::ProfileCycle),
                UpdatePolicy {
                    baseline: BaselineSource::Stored,
                    ..UpdatePolicy::default()
                },
            )
            .await;
        assert!(
            !matches!(
                result.as_ref().err(),
                Some(crate::error::ManagerError::DeviceOperationBusy { .. })
            ),
            "stored baseline must not trigger nested lock, got {result:?}"
        );
        let outcome = result.unwrap();
        assert_eq!(
            outcome.desired.slots[7],
            SafeButtonAction::ProfileCycle.to_assignment()
        );
        assert_eq!(
            opens.load(Ordering::SeqCst),
            1,
            "stored button slot update must open exactly one session"
        );
    }
}
