use crate::{ProfileId, ProtocolError, TransportKind};

use super::checksum::sum16;

pub const BUTTON_REPORT_ID: u8 = 0x08;
pub const BUTTON_DECLARED_LENGTH: u8 = 0x3b;
const BUTTON_RECEIVER_DECLARED_LENGTH: u8 = 0x3d;
pub const BUTTON_REPORT_LENGTH: usize = 59;
pub const BUTTON_SLOT_COUNT: usize = 18;

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
