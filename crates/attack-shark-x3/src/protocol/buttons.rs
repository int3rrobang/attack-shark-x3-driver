use crate::{ProfileId, ProtocolError, TransportKind};

use super::checksum::sum16;
use thiserror::Error;

pub const BUTTON_REPORT_ID: u8 = 0x08;
pub const BUTTON_DECLARED_LENGTH: u8 = 0x3b;
const BUTTON_RECEIVER_DECLARED_LENGTH: u8 = 0x3d;
pub const BUTTON_REPORT_LENGTH: usize = 59;
pub const BUTTON_SLOT_COUNT: usize = 18;
/// Canonical factory-default button table confirmed on X3/FA61.
///
/// Source: `fixtures/protocol/buttons.json` x3-profile-1-compact-reset.
/// Capture-confirmed across wired and receiver transports.
#[rustfmt::skip]
pub const DEFAULT_BUTTON_SLOTS: [ButtonAssignment; BUTTON_SLOT_COUNT] = [
    ButtonAssignment::new(0x02, 0x00, 0x00), //  0: left-click
    ButtonAssignment::new(0x03, 0x00, 0x00), //  1: right-click
    ButtonAssignment::new(0x04, 0x00, 0x00), //  2: middle-click
    ButtonAssignment::new(0x0d, 0x00, 0x00), //  3: dpi-cycle
    ButtonAssignment::new(0x3c, 0x00, 0x00), //  4: scroll-up
    ButtonAssignment::new(0x0f, 0x00, 0x00), //  5: dpi-minus (scroll-down placeholder)
    ButtonAssignment::new(0x06, 0x00, 0x00), //  6: forward
    ButtonAssignment::new(0x05, 0x00, 0x00), //  7: backward
    ButtonAssignment::new(0x3c, 0x00, 0x00), //  8: scroll-up (second instance)
    ButtonAssignment::new(0x01, 0x00, 0x00), //  9: disabled
    ButtonAssignment::new(0x01, 0x00, 0x00), // 10: disabled
    ButtonAssignment::new(0x01, 0x00, 0x00), // 11: disabled
    ButtonAssignment::new(0x01, 0x00, 0x00), // 12: disabled
    ButtonAssignment::new(0x01, 0x00, 0x00), // 13: disabled
    ButtonAssignment::new(0x01, 0x00, 0x00), // 14: disabled
    ButtonAssignment::new(0x01, 0x00, 0x00), // 15: disabled
    ButtonAssignment::new(0x0a, 0x00, 0x00), // 16: scroll-down (0x0a, capture-confirmed)
    ButtonAssignment::new(0x09, 0x00, 0x00), // 17: scroll-up (0x09, capture-confirmed)
];

const SLOTS_START: usize = 3;
const SLOTS_END: usize = SLOTS_START + BUTTON_SLOT_COUNT * 3;
const CHECKSUM_OFFSET: usize = SLOTS_END;

/// One raw FA61 button-assignment slot.
///
/// The fields deliberately remain raw firmware values: action, modifier, and
/// key/action value semantics are not complete for every firmware revision.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct ButtonAssignment {
    pub action: u8,
    pub modifier: u8,
    pub key_code: u8,
}

impl ButtonAssignment {
    #[must_use]
    pub const fn new(action: u8, modifier: u8, key_code: u8) -> Self {
        Self {
            action,
            modifier,
            key_code,
        }
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 3] {
        [self.action, self.modifier, self.key_code]
    }

    /// Interprets this raw slot using the verified X3 action dialect.
    ///
    /// Unknown actions remain valid raw assignments and are rejected here
    /// rather than silently assigned an incorrect semantic name.
    pub fn decode_x3_action(self) -> Result<X3ButtonAction, ButtonActionError> {
        X3ButtonAction::try_from(self)
    }
}

/// Validation failures for the typed X3 button-action dialect.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ButtonActionError {
    #[error(
        "unsupported X3 button action 0x{action:02x} with parameters 0x{modifier:02x} 0x{key_code:02x}"
    )]
    Unsupported {
        action: u8,
        modifier: u8,
        key_code: u8,
    },
    #[error("invalid keyboard modifier mask 0x{value:02x}; only bits 0..=3 are supported")]
    InvalidKeyboardModifiers { value: u8 },
    #[error("invalid HID keyboard usage 0x{value:02x}; expected 0x04..=0xe7")]
    InvalidKeyboardUsage { value: u8 },
    #[error(
        "X3 action 0x{action:02x} does not accept parameters 0x{modifier:02x} 0x{key_code:02x}"
    )]
    UnexpectedParameters {
        action: u8,
        modifier: u8,
        key_code: u8,
    },
}

/// Four-bit X3 keyboard-shortcut modifier mask.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct KeyboardModifiers(u8);

impl KeyboardModifiers {
    pub const CTRL: Self = Self(0x01);
    pub const SHIFT: Self = Self(0x02);
    pub const ALT: Self = Self(0x04);
    pub const WIN: Self = Self(0x08);

    /// Creates a modifier mask when no unsupported bits are set.
    #[must_use]
    pub const fn new(value: u8) -> Option<Self> {
        if value & !0x0f == 0 {
            Some(Self(value))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }
}

/// A keyboard-page HID usage accepted by the X3 shortcut encoding.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct HidKeyboardUsage(u8);

impl HidKeyboardUsage {
    pub const MIN: u8 = 0x04;
    pub const MAX: u8 = 0xe7;

    /// Creates a keyboard usage from the USB HID usage-page value.
    #[must_use]
    pub const fn new(value: u8) -> Option<Self> {
        if value >= Self::MIN && value <= Self::MAX {
            Some(Self(value))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// A typed X3 button action.
///
/// Parameterless variants cover the currently confirmed safe X3 actions:
/// mouse buttons, DPI controls, profile controls (capture/live-confirmed),
/// and the stock-app media/browser/fire/scroll families (capture-confirmed
/// 2026-08-14). Keyboard shortcuts and macro references retain their
/// distinct wire encodings. Any other raw assignment must remain a
/// [`ButtonAssignment`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub enum X3ButtonAction {
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
    KeyboardShortcut {
        modifiers: KeyboardModifiers,
        key: HidKeyboardUsage,
    },
    Macro {
        reference: u8,
    },
}

impl X3ButtonAction {
    /// Encodes this action as one complete three-byte slot.
    #[must_use]
    pub const fn to_assignment(self) -> ButtonAssignment {
        let (action, modifier, key_code) = match self {
            Self::Disable => (0x01, 0, 0),
            Self::LeftClick => (0x02, 0, 0),
            Self::RightClick => (0x03, 0, 0),
            Self::MiddleClick => (0x04, 0, 0),
            Self::Backward => (0x05, 0, 0),
            Self::Forward => (0x06, 0, 0),
            Self::DoubleClick => (0x07, 0, 0),
            Self::FireButton => (0x08, 0, 0),
            Self::ScrollUp => (0x09, 0, 0),
            Self::ScrollDown => (0x0a, 0, 0),
            Self::DpiCycle => (0x0d, 0, 0),
            Self::DpiPlus => (0x0e, 0, 0),
            Self::DpiMinus => (0x0f, 0, 0),
            Self::ProfileCycle => (0x34, 0, 0),
            Self::ProfilePlus => (0x35, 0, 0),
            Self::ProfileMinus => (0x36, 0, 0),
            Self::MediaPlayer => (0x15, 0, 0),
            Self::PreviousTrack => (0x16, 0, 0),
            Self::NextTrack => (0x17, 0, 0),
            Self::PlayPause => (0x18, 0, 0),
            Self::Stop => (0x19, 0, 0),
            Self::Mute => (0x1a, 0, 0),
            Self::VolumeUp => (0x1b, 0, 0),
            Self::VolumeDown => (0x1c, 0, 0),
            Self::Calculator => (0x1d, 0, 0),
            Self::Email => (0x1e, 0, 0),
            Self::BrowserForward => (0x20, 0, 0),
            Self::BrowserBackward => (0x21, 0, 0),
            Self::BrowserStop => (0x22, 0, 0),
            Self::MyComputer => (0x23, 0, 0),
            Self::BrowserRefresh => (0x24, 0, 0),
            Self::BrowserHome => (0x25, 0, 0),
            Self::BrowserSearch => (0x26, 0, 0),
            Self::KeyboardShortcut { modifiers, key } => (0x11, modifiers.bits(), key.get()),
            Self::Macro { reference } => (0x12, 0, reference),
        };
        ButtonAssignment::new(action, modifier, key_code)
    }
}

impl TryFrom<ButtonAssignment> for X3ButtonAction {
    type Error = ButtonActionError;

    fn try_from(assignment: ButtonAssignment) -> Result<Self, Self::Error> {
        let parameterless = match assignment.action {
            0x01 => Some(Self::Disable),
            0x02 => Some(Self::LeftClick),
            0x03 => Some(Self::RightClick),
            0x04 => Some(Self::MiddleClick),
            0x05 => Some(Self::Backward),
            0x06 => Some(Self::Forward),
            0x07 => Some(Self::DoubleClick),
            0x08 => Some(Self::FireButton),
            0x09 => Some(Self::ScrollUp),
            0x0a => Some(Self::ScrollDown),
            0x0d => Some(Self::DpiCycle),
            0x0e => Some(Self::DpiPlus),
            0x0f => Some(Self::DpiMinus),
            0x15 => Some(Self::MediaPlayer),
            0x16 => Some(Self::PreviousTrack),
            0x17 => Some(Self::NextTrack),
            0x18 => Some(Self::PlayPause),
            0x19 => Some(Self::Stop),
            0x1a => Some(Self::Mute),
            0x1b => Some(Self::VolumeUp),
            0x1c => Some(Self::VolumeDown),
            0x1d => Some(Self::Calculator),
            0x1e => Some(Self::Email),
            0x20 => Some(Self::BrowserForward),
            0x21 => Some(Self::BrowserBackward),
            0x22 => Some(Self::BrowserStop),
            0x23 => Some(Self::MyComputer),
            0x24 => Some(Self::BrowserRefresh),
            0x25 => Some(Self::BrowserHome),
            0x26 => Some(Self::BrowserSearch),
            0x34 => Some(Self::ProfileCycle),
            0x35 => Some(Self::ProfilePlus),
            0x36 => Some(Self::ProfileMinus),
            _ => None,
        };
        if let Some(action) = parameterless {
            if assignment.modifier != 0 || assignment.key_code != 0 {
                return Err(ButtonActionError::UnexpectedParameters {
                    action: assignment.action,
                    modifier: assignment.modifier,
                    key_code: assignment.key_code,
                });
            }
            return Ok(action);
        }

        match assignment.action {
            0x11 => {
                let modifiers = KeyboardModifiers::new(assignment.modifier).ok_or(
                    ButtonActionError::InvalidKeyboardModifiers {
                        value: assignment.modifier,
                    },
                )?;
                let key = HidKeyboardUsage::new(assignment.key_code).ok_or(
                    ButtonActionError::InvalidKeyboardUsage {
                        value: assignment.key_code,
                    },
                )?;
                Ok(Self::KeyboardShortcut { modifiers, key })
            }
            0x12 if assignment.modifier == 0 => Ok(Self::Macro {
                reference: assignment.key_code,
            }),
            _ => Err(ButtonActionError::Unsupported {
                action: assignment.action,
                modifier: assignment.modifier,
                key_code: assignment.key_code,
            }),
        }
    }
}

/// The complete raw table carried by an FA61 report-`0x08` packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct ButtonsState {
    pub profile: ProfileId,
    pub slots: [ButtonAssignment; BUTTON_SLOT_COUNT],
}

impl ButtonsState {
    #[must_use]
    pub const fn new(profile: ProfileId, slots: [ButtonAssignment; BUTTON_SLOT_COUNT]) -> Self {
        Self { profile, slots }
    }

    /// Factory-default button mapping for a given profile.
    #[must_use]
    pub fn default_for_profile(profile: ProfileId) -> Self {
        Self {
            profile,
            slots: DEFAULT_BUTTON_SLOTS,
        }
    }
}

/// A validated report-`0x08` state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedButtonsReport {
    pub state: ButtonsState,
}

/// FA61 report-`0x08` bytes.
///
/// X3 writes and normalized full readbacks are both exactly 59 bytes. They
/// therefore intentionally share one framing rather than exposing a false
/// compact/full distinction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ButtonsReport {
    bytes: [u8; BUTTON_REPORT_LENGTH],
}

impl ButtonsReport {
    /// Encodes the complete table and its 16-bit big-endian checksum.
    #[must_use]
    pub fn encode(state: &ButtonsState) -> Self {
        let mut bytes = [0_u8; BUTTON_REPORT_LENGTH];
        bytes[0] = BUTTON_REPORT_ID;
        bytes[1] = BUTTON_DECLARED_LENGTH;
        bytes[2] = state.profile.get();
        for (index, slot) in state.slots.iter().copied().enumerate() {
            let offset = SLOTS_START + index * 3;
            bytes[offset..offset + 3].copy_from_slice(&slot.as_bytes());
        }
        let checksum = sum16(&bytes[SLOTS_START..SLOTS_END]);
        bytes[CHECKSUM_OFFSET..].copy_from_slice(&checksum.to_be_bytes());
        Self { bytes }
    }

    /// Decodes and validates a canonical 59-byte FA61 report-`0x08` packet.
    ///
    /// The caller supplies the expected target profile explicitly; this
    /// prevents a read of one profile from being mistaken for another.
    ///
    /// # Errors
    ///
    /// Rejects malformed lengths, report identity, declared length, target
    /// profile, or checksum.
    pub fn decode(
        packet: &[u8],
        expected_profile: ProfileId,
    ) -> Result<DecodedButtonsReport, ProtocolError> {
        Self::decode_with_declared_length(packet, expected_profile, BUTTON_DECLARED_LENGTH)
    }

    /// Decodes a button-table readback using the selected transport dialect.
    ///
    /// # Errors
    ///
    /// Rejects malformed lengths, report identity, declared length, target
    /// profile, or checksum.
    pub fn decode_for_transport(
        packet: &[u8],
        transport: TransportKind,
        expected_profile: ProfileId,
    ) -> Result<DecodedButtonsReport, ProtocolError> {
        let declared_length = match transport {
            TransportKind::Receiver => BUTTON_RECEIVER_DECLARED_LENGTH,
            TransportKind::Wired | TransportKind::Ble => BUTTON_DECLARED_LENGTH,
        };
        Self::decode_with_declared_length(packet, expected_profile, declared_length)
    }

    fn decode_with_declared_length(
        packet: &[u8],
        expected_profile: ProfileId,
        declared_length: u8,
    ) -> Result<DecodedButtonsReport, ProtocolError> {
        if packet.len() != BUTTON_REPORT_LENGTH {
            return Err(ProtocolError::InvalidReportLength {
                expected: BUTTON_REPORT_LENGTH,
                actual: packet.len(),
            });
        }
        if packet[0] != BUTTON_REPORT_ID {
            return Err(ProtocolError::UnexpectedReportId {
                expected: BUTTON_REPORT_ID,
                actual: packet[0],
            });
        }
        if packet[1] != declared_length {
            return Err(ProtocolError::UnexpectedDeclaredLength {
                expected: declared_length,
                actual: packet[1],
            });
        }
        if packet[2] != expected_profile.get() {
            return Err(ProtocolError::ProfileMismatch {
                expected: expected_profile.get(),
                actual: packet[2],
            });
        }

        let actual_checksum =
            u16::from_be_bytes([packet[CHECKSUM_OFFSET], packet[CHECKSUM_OFFSET + 1]]);
        let expected_checksum = sum16(&packet[SLOTS_START..SLOTS_END]);
        if actual_checksum != expected_checksum {
            return Err(ProtocolError::ChecksumMismatch {
                expected: expected_checksum,
                actual: actual_checksum,
            });
        }

        let mut slots = [ButtonAssignment::default(); BUTTON_SLOT_COUNT];
        for (index, slot) in slots.iter_mut().enumerate() {
            let offset = SLOTS_START + index * 3;
            *slot = ButtonAssignment::new(packet[offset], packet[offset + 1], packet[offset + 2]);
        }
        Ok(DecodedButtonsReport {
            state: ButtonsState::new(expected_profile, slots),
        })
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; BUTTON_REPORT_LENGTH] {
        &self.bytes
    }
}

#[cfg(test)]
mod action_tests {
    use super::{
        ButtonActionError, ButtonAssignment, HidKeyboardUsage, KeyboardModifiers, X3ButtonAction,
    };

    #[test]
    fn confirmed_actions_encode_and_decode_without_raw_semantic_loss() {
        let actions = [
            X3ButtonAction::Disable,
            X3ButtonAction::LeftClick,
            X3ButtonAction::RightClick,
            X3ButtonAction::MiddleClick,
            X3ButtonAction::Backward,
            X3ButtonAction::Forward,
            X3ButtonAction::DoubleClick,
            X3ButtonAction::FireButton,
            X3ButtonAction::ScrollUp,
            X3ButtonAction::ScrollDown,
            X3ButtonAction::DpiCycle,
            X3ButtonAction::DpiPlus,
            X3ButtonAction::DpiMinus,
            X3ButtonAction::ProfileCycle,
            X3ButtonAction::ProfilePlus,
            X3ButtonAction::ProfileMinus,
            X3ButtonAction::MediaPlayer,
            X3ButtonAction::PreviousTrack,
            X3ButtonAction::NextTrack,
            X3ButtonAction::PlayPause,
            X3ButtonAction::Stop,
            X3ButtonAction::Mute,
            X3ButtonAction::VolumeUp,
            X3ButtonAction::VolumeDown,
            X3ButtonAction::Calculator,
            X3ButtonAction::Email,
            X3ButtonAction::BrowserForward,
            X3ButtonAction::BrowserBackward,
            X3ButtonAction::BrowserStop,
            X3ButtonAction::MyComputer,
            X3ButtonAction::BrowserRefresh,
            X3ButtonAction::BrowserHome,
            X3ButtonAction::BrowserSearch,
        ];
        for action in actions {
            let assignment = action.to_assignment();
            assert_eq!(X3ButtonAction::try_from(assignment), Ok(action));
            assert_eq!(assignment.decode_x3_action(), Ok(action));
        }
    }

    #[test]
    fn keyboard_shortcuts_validate_modifier_and_usage_ranges() {
        let action = X3ButtonAction::KeyboardShortcut {
            modifiers: KeyboardModifiers::new(0x03).expect("Ctrl+Shift is valid"),
            key: HidKeyboardUsage::new(0x16).expect("S usage is valid"),
        };
        assert_eq!(action.to_assignment().as_bytes(), [0x11, 0x03, 0x16]);
        assert_eq!(X3ButtonAction::try_from(action.to_assignment()), Ok(action));
        assert_eq!(KeyboardModifiers::new(0x10), None);
        assert_eq!(HidKeyboardUsage::new(0x03), None);
    }

    #[test]
    fn macro_reference_is_distinct_from_keyboard_shortcut() {
        let action = X3ButtonAction::Macro { reference: 7 };
        assert_eq!(action.to_assignment().as_bytes(), [0x12, 0x00, 0x07]);
        assert_eq!(X3ButtonAction::try_from(action.to_assignment()), Ok(action));
    }

    #[test]
    fn unknown_or_malformed_assignments_stay_raw() {
        // 0x3c is the factory wheel-slot encoding; it is not a typed action.
        assert_eq!(
            X3ButtonAction::try_from(ButtonAssignment::new(0x3c, 0, 0)),
            Err(ButtonActionError::Unsupported {
                action: 0x3c,
                modifier: 0,
                key_code: 0,
            })
        );
        assert!(matches!(
            X3ButtonAction::try_from(ButtonAssignment::new(0x02, 1, 0)),
            Err(ButtonActionError::UnexpectedParameters { .. })
        ));
        assert!(matches!(
            X3ButtonAction::try_from(ButtonAssignment::new(0x11, 0x10, 0x04)),
            Err(ButtonActionError::InvalidKeyboardModifiers { .. })
        ));
    }
}

#[cfg(all(test, feature = "serde"))]
mod serde_tests {
    use super::{BUTTON_SLOT_COUNT, ButtonAssignment, ButtonsState};
    use crate::ProfileId;

    #[test]
    fn button_assignment_round_trip() {
        let slot = ButtonAssignment::new(0x01, 0x02, 0x03);
        let json = serde_json::to_string(&slot).unwrap();
        let restored: ButtonAssignment = serde_json::from_str(&json).unwrap();
        assert_eq!(slot, restored);
    }

    #[test]
    fn buttons_state_round_trip() {
        let mut slots = [ButtonAssignment::default(); BUTTON_SLOT_COUNT];
        slots[0] = ButtonAssignment::new(0x01, 0x00, 0x04);
        slots[17] = ButtonAssignment::new(0x02, 0x01, 0x05);
        let state = ButtonsState::new(ProfileId::try_from(2).unwrap(), slots);
        let json = serde_json::to_string(&state).unwrap();
        let restored: ButtonsState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, restored);
    }
}
