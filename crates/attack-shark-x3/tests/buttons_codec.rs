use attack_shark_x3::protocol::buttons::{
    BUTTON_REPORT_LENGTH, BUTTON_SLOT_COUNT, ButtonAssignment, ButtonsReport, ButtonsState,
};
use attack_shark_x3::{ProfileId, ProtocolError, TransportKind};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureCatalog {
    schema_version: u8,
    fixtures: Vec<ButtonFixture>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ButtonFixture {
    name: String,
    evidence: String,
    source: String,
    transport: String,
    framing: String,
    profile: u8,
    packet_hex: String,
    known_state: KnownState,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnownState {
    slots: Vec<[u8; 3]>,
    checksum: u16,
}

fn fixtures() -> FixtureCatalog {
    serde_json::from_str(include_str!("../../../fixtures/protocol/buttons.json"))
        .expect("button fixture catalog must be valid JSON")
}

fn profile(value: u8) -> ProfileId {
    ProfileId::try_from(value).expect("fixture profile must be valid")
}

fn from_hex(value: &str) -> Vec<u8> {
    assert_eq!(
        value.len() % 2,
        0,
        "fixture hex must contain complete bytes"
    );
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = nibble(pair[0]).expect("fixture must use hexadecimal digits");
            let low = nibble(pair[1]).expect("fixture must use hexadecimal digits");
            (high << 4) | low
        })
        .collect()
}

const fn nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[test]
fn live_profile_fixtures_round_trip_without_normalizing_slots() {
    let catalog = fixtures();
    assert_eq!(catalog.schema_version, 1);
    assert!(catalog.fixtures.iter().any(|fixture| fixture.profile == 1));
    assert!(catalog.fixtures.iter().any(|fixture| fixture.profile == 2));

    for fixture in catalog.fixtures {
        assert!(!fixture.name.is_empty());
        assert!(!fixture.evidence.is_empty());
        assert!(!fixture.source.is_empty());
        assert!(!fixture.transport.is_empty());
        assert!(!fixture.framing.is_empty());

        let packet = from_hex(&fixture.packet_hex);
        assert_eq!(packet.len(), BUTTON_REPORT_LENGTH);
        assert_eq!(packet[57..].len(), 2);
        assert_eq!(
            u16::from_be_bytes([packet[57], packet[58]]),
            fixture.known_state.checksum
        );
        assert_eq!(fixture.known_state.slots.len(), BUTTON_SLOT_COUNT);

        let decoded = ButtonsReport::decode(&packet, profile(fixture.profile))
            .unwrap_or_else(|error| panic!("fixture {} did not decode: {error}", fixture.name));
        let expected_slots: [ButtonAssignment; BUTTON_SLOT_COUNT] = fixture
            .known_state
            .slots
            .iter()
            .map(|slot| ButtonAssignment::new(slot[0], slot[1], slot[2]))
            .collect::<Vec<_>>()
            .try_into()
            .expect("fixture must contain exactly 18 slots");
        assert_eq!(decoded.state.slots, expected_slots);

        let encoded = ButtonsReport::encode(&decoded.state);
        assert_eq!(
            encoded.as_bytes(),
            packet.as_slice(),
            "{} changed",
            fixture.name
        );
    }
}

#[test]
fn receiver_readback_accepts_webdriver_envelope_declaration() {
    let mut packet = from_hex(&fixtures().fixtures[0].packet_hex);
    packet[1] = 0x3d;
    let decoded = ButtonsReport::decode_for_transport(&packet, TransportKind::Receiver, profile(1))
        .expect("FA60 prepared button readback must decode");
    assert_eq!(decoded.state.profile, profile(1));
    assert!(ButtonsReport::decode(&packet, profile(1)).is_err());
}

#[test]
fn every_slot_and_unresolved_values_survive_round_trip() {
    let mut slots = [ButtonAssignment::default(); BUTTON_SLOT_COUNT];
    for (index, slot) in slots.iter_mut().enumerate() {
        let index = u8::try_from(index).expect("slot index fits in a byte");
        *slot = ButtonAssignment::new(0x80 | index, 0xa0 | (index * 3), 0xf0 ^ index);
    }
    let state = ButtonsState::new(profile(5), slots);
    let packet = ButtonsReport::encode(&state);
    let decoded =
        ButtonsReport::decode(packet.as_bytes(), profile(5)).expect("raw slots are valid");
    assert_eq!(decoded.state, state);
    assert_eq!(
        ButtonsReport::encode(&decoded.state).as_bytes(),
        packet.as_bytes()
    );
}

#[test]
fn checksum_high_and_low_byte_corruption_is_rejected() {
    let fixture = from_hex(&fixtures().fixtures[0].packet_hex);
    for offset in [57, 58] {
        let mut corrupted = fixture.clone();
        corrupted[offset] ^= 0x01;
        assert!(matches!(
            ButtonsReport::decode(&corrupted, profile(1)),
            Err(ProtocolError::ChecksumMismatch { .. })
        ));
    }
}

#[test]
fn target_profile_mismatch_is_rejected() {
    let packet = from_hex(&fixtures().fixtures[0].packet_hex);
    assert_eq!(
        ButtonsReport::decode(&packet, profile(2)),
        Err(ProtocolError::ProfileMismatch {
            expected: 2,
            actual: 1,
        })
    );
}

#[test]
fn malformed_header_declared_length_and_padding_are_rejected() {
    let packet = from_hex(&fixtures().fixtures[0].packet_hex);

    let mut bad_id = packet.clone();
    bad_id[0] = 0x04;
    assert!(matches!(
        ButtonsReport::decode(&bad_id, profile(1)),
        Err(ProtocolError::UnexpectedReportId { .. })
    ));

    let mut bad_declared_length = packet.clone();
    bad_declared_length[1] = 0x38;
    assert!(matches!(
        ButtonsReport::decode(&bad_declared_length, profile(1)),
        Err(ProtocolError::UnexpectedDeclaredLength { .. })
    ));

    let mut trailing_padding = packet.clone();
    trailing_padding.extend_from_slice(&[0, 0, 0, 0, 0]);
    assert!(matches!(
        ButtonsReport::decode(&trailing_padding, profile(1)),
        Err(ProtocolError::InvalidReportLength {
            expected: BUTTON_REPORT_LENGTH,
            actual: 64
        })
    ));

    assert!(matches!(
        ButtonsReport::decode(&packet[..58], profile(1)),
        Err(ProtocolError::InvalidReportLength {
            expected: BUTTON_REPORT_LENGTH,
            actual: 58
        })
    ));
}
