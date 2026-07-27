//! Presentation-neutral packet construction for manager-owned debug commands.
//!
//! These helpers deliberately perform no device or state-store access. They accept
//! the serializable domain models and delegate wire encoding to the low-level
//! protocol crate, returning exact transmitted bytes plus framing metadata.

use attack_shark_x3::{
    ButtonsReport, ButtonsState, DpiFraming, DpiReport, DpiState, PreferencesFraming,
    PreferencesReport, PreferencesState, ProtocolError, TransportKind,
};
use serde::{Deserialize, Serialize};

/// The report family represented by an [`OfflinePacket`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OfflinePacketKind {
    Dpi,
    Preferences,
    Buttons,
}

/// Wire framing used for an offline packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OfflinePacketFraming {
    /// The write-shaped image without readback padding.
    Compact,
    /// The full feature-report image including required trailing padding.
    Full,
    /// A fixed-size image with no compact/full distinction.
    Fixed,
}

impl From<DpiFraming> for OfflinePacketFraming {
    fn from(framing: DpiFraming) -> Self {
        match framing {
            DpiFraming::Compact => Self::Compact,
            DpiFraming::Full => Self::Full,
        }
    }
}

impl From<PreferencesFraming> for OfflinePacketFraming {
    fn from(framing: PreferencesFraming) -> Self {
        match framing {
            PreferencesFraming::Compact => Self::Compact,
            PreferencesFraming::Full => Self::Full,
        }
    }
}

/// Exact bytes and protocol metadata produced by an offline debug encoder.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OfflinePacket {
    pub kind: OfflinePacketKind,
    pub report_id: u8,
    pub declared_length: u8,
    pub framing: OfflinePacketFraming,
    /// Transport is populated for DPI, whose write framing is transport-aware.
    pub transport: Option<TransportKind>,
    /// Bytes in the exact transmitted image; no formatted hex is involved.
    pub bytes: Vec<u8>,
}

impl OfflinePacket {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

fn packet(
    kind: OfflinePacketKind,
    bytes: &[u8],
    framing: OfflinePacketFraming,
    transport: Option<TransportKind>,
) -> OfflinePacket {
    assert!(
        bytes.len() >= 2,
        "protocol encoder produced a {kind:?} packet shorter than 2 bytes"
    );
    OfflinePacket {
        kind,
        report_id: bytes[0],
        declared_length: bytes[1],
        framing,
        transport,
        bytes: bytes.to_vec(),
    }
}

/// Encodes the complete DPI state for the selected write transport.
///
/// The low-level encoder validates stage selection and preserves every byte in
/// `DpiState::preserved_tail`; this function adds only presentation-neutral
/// packet metadata.
pub fn encode_debug_dpi(
    state: &DpiState,
    transport: TransportKind,
) -> Result<OfflinePacket, ProtocolError> {
    let report = DpiReport::encode(state, transport)?;
    let framing = match transport {
        TransportKind::Wired | TransportKind::Ble | TransportKind::Receiver => DpiFraming::Compact,
    };
    Ok(packet(
        OfflinePacketKind::Dpi,
        report.as_bytes(),
        framing.into(),
        Some(transport),
    ))
}

/// Alias named after the corresponding `debug dpi` command.
pub fn debug_dpi(
    state: &DpiState,
    transport: TransportKind,
) -> Result<OfflinePacket, ProtocolError> {
    encode_debug_dpi(state, transport)
}

/// Encodes preferences using compact or full report framing.
pub fn encode_debug_prefs(state: &PreferencesState, framing: PreferencesFraming) -> OfflinePacket {
    let report = PreferencesReport::encode_framed(state, framing);
    packet(
        OfflinePacketKind::Preferences,
        report.as_bytes(),
        framing.into(),
        None,
    )
}

/// Descriptive alias for [`encode_debug_prefs`].
pub fn encode_debug_preferences(
    state: &PreferencesState,
    framing: PreferencesFraming,
) -> OfflinePacket {
    encode_debug_prefs(state, framing)
}

/// Alias named after the corresponding `debug prefs` command.
pub fn debug_prefs(state: &PreferencesState, framing: PreferencesFraming) -> OfflinePacket {
    encode_debug_prefs(state, framing)
}

/// Encodes the complete button table, including its checksum.
pub fn encode_debug_buttons(state: &ButtonsState) -> OfflinePacket {
    let report = ButtonsReport::encode(state);
    packet(
        OfflinePacketKind::Buttons,
        report.as_bytes(),
        OfflinePacketFraming::Fixed,
        None,
    )
}

/// Alias named after the corresponding `debug buttons` command.
pub fn debug_buttons(state: &ButtonsState) -> OfflinePacket {
    encode_debug_buttons(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use attack_shark_x3::{
        ButtonAssignment, DpiValue, LiftOffDistance, ProfileId, SensorOptions, StageIndex,
    };

    #[test]
    fn dpi_packet_preserves_tail_and_exact_checksum() {
        let profile = ProfileId::new(2).expect("profile");
        let stages = vec![
            DpiValue::new(800).expect("DPI"),
            DpiValue::new(1_600).expect("DPI"),
        ];
        let active = StageIndex::new(2).expect("stage");
        let mut state = DpiState::new(profile, stages, active, [0; 25]).expect("state");
        state.sensor = SensorOptions {
            lift_off_distance: LiftOffDistance::TwoMillimeters,
            ripple_control: true,
            angle_snap: true,
            motion_sync: false,
        };
        state.preserved_tail = core::array::from_fn(|index| index as u8 + 1);

        let packet = encode_debug_dpi(&state, TransportKind::Wired).expect("packet");
        assert_eq!(packet.kind, OfflinePacketKind::Dpi);
        assert_eq!(packet.framing, OfflinePacketFraming::Compact);
        assert_eq!(packet.transport, Some(TransportKind::Wired));
        assert_eq!(packet.len(), 52);
        assert_eq!(
            packet.as_bytes()[..25],
            [
                0x04, 0x38, 0x02, 0x01, 0x01, 0x03, 0x01, 0x00, 0x0f, 0x1f, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0x02,
            ]
        );
        assert_eq!(
            &packet.as_bytes()[25..50],
            &[
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
                24, 25
            ]
        );
        assert_eq!(&packet.as_bytes()[50..], &[0x01, 0x7b]);
    }

    #[test]
    fn preferences_packet_has_exact_compact_checksum_and_full_length() {
        let state = PreferencesState::new(
            ProfileId::new(3).expect("profile"),
            0x10,
            0x21,
            0xa8,
            [0xaa, 0xbb, 0xcc],
            4,
            2,
        );
        let compact = encode_debug_prefs(&state, PreferencesFraming::Compact);
        assert_eq!(compact.len(), 13);
        assert_eq!(
            compact.as_bytes(),
            &[
                0x05, 0x0f, 0x03, 0x10, 0x21, 0xa8, 0xaa, 0xbb, 0xcc, 4, 2, 0x03, 0x10
            ]
        );
        let full = encode_debug_prefs(&state, PreferencesFraming::Full);
        assert_eq!(full.len(), 15);
        assert_eq!(&full.as_bytes()[13..], &[0, 0]);
        assert_eq!(full.as_bytes()[..13], compact.as_bytes()[..]);
    }

    #[test]
    fn buttons_packet_contains_complete_table_and_checksum() {
        let profile = ProfileId::new(1).expect("profile");
        let mut slots = [ButtonAssignment::default(); 18];
        slots[0] = ButtonAssignment::new(1, 2, 3);
        slots[17] = ButtonAssignment::new(4, 5, 6);
        let packet = encode_debug_buttons(&ButtonsState::new(profile, slots));
        assert_eq!(packet.kind, OfflinePacketKind::Buttons);
        assert_eq!(packet.framing, OfflinePacketFraming::Fixed);
        assert_eq!(packet.len(), 59);
        assert_eq!(&packet.as_bytes()[..6], &[0x08, 0x3b, 0x01, 1, 2, 3]);
        assert_eq!(&packet.as_bytes()[54..], &[4, 5, 6, 0, 0x15]);
    }
}
