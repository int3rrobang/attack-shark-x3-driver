use attack_shark_x3::protocol::checksum::sum16;
use attack_shark_x3::{
    PreferencesFraming, PreferencesReport, PreferencesState, ProfileId, ProtocolError,
    TransportKind,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureCatalog {
    schema_version: u8,
    fixtures: Vec<PreferencesFixture>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreferencesFixture {
    name: String,
    evidence: String,
    source: String,
    transport: String,
    framing: FixtureFraming,
    profile: u8,
    packet_hex: String,
    known_state: KnownState,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum FixtureFraming {
    Compact,
    Full,
}

impl From<FixtureFraming> for PreferencesFraming {
    fn from(value: FixtureFraming) -> Self {
        match value {
            FixtureFraming::Compact => Self::Compact,
            FixtureFraming::Full => Self::Full,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnownState {
    light_mode: u8,
    configuration: u8,
    deep_sleep: u8,
    host_color_bytes: [u8; 3],
    sleep_timer: u8,
    debounce: u8,
    #[serde(default)]
    padding: Vec<u8>,
}

#[test]
fn captured_stock_reset_constructor_matches_x3_exe_packet() {
    let expected = from_hex("050f010003a80000ff010401af");
    let state = PreferencesState::captured_stock_reset(profile(1));
    let encoded = PreferencesReport::encode_framed(&state, PreferencesFraming::Compact);
    assert_eq!(encoded.as_bytes(), expected);
}

fn fixtures() -> FixtureCatalog {
    serde_json::from_str(include_str!("../../../fixtures/protocol/preferences.json"))
        .expect("the checked-in preferences fixture catalog must be valid JSON")
}

fn profile(value: u8) -> ProfileId {
    ProfileId::try_from(value).expect("test profile must be valid")
}

fn from_hex(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0, "hex fixture must have complete bytes");
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_nibble(pair[0]).expect("fixture must contain hexadecimal characters");
            let low = hex_nibble(pair[1]).expect("fixture must contain hexadecimal characters");
            (high << 4) | low
        })
        .collect()
}

const fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn rewrite_checksum(packet: &mut [u8]) {
    let checksum = sum16(&packet[3..11]).to_be_bytes();
    packet[11..13].copy_from_slice(&checksum);
}

#[test]
fn captured_and_live_fixtures_round_trip_byte_for_byte() {
    let catalog = fixtures();
    assert_eq!(catalog.schema_version, 1);
    assert_eq!(catalog.fixtures.len(), 3);

    for fixture in catalog.fixtures {
        assert!(!fixture.name.is_empty());
        assert!(!fixture.evidence.is_empty());
        assert!(!fixture.source.is_empty());
        assert!(!fixture.transport.is_empty());

        let packet = from_hex(&fixture.packet_hex);
        let expected_profile = profile(fixture.profile);
        let decoded = PreferencesReport::decode(&packet, expected_profile)
            .unwrap_or_else(|error| panic!("fixture {} did not decode: {error}", fixture.name));
        assert_eq!(decoded.framing, PreferencesFraming::from(fixture.framing));
        assert_eq!(decoded.state.profile, expected_profile);
        assert_eq!(decoded.state.light_mode, fixture.known_state.light_mode);
        assert_eq!(
            decoded.state.configuration,
            fixture.known_state.configuration
        );
        assert_eq!(decoded.state.deep_sleep, fixture.known_state.deep_sleep);
        assert_eq!(
            decoded.state.host_color,
            fixture.known_state.host_color_bytes
        );
        assert_eq!(decoded.state.sleep_timer, fixture.known_state.sleep_timer);
        assert_eq!(decoded.state.debounce, fixture.known_state.debounce);
        if decoded.framing == PreferencesFraming::Full {
            assert_eq!(&packet[13..], fixture.known_state.padding.as_slice());
        }

        let encoded = PreferencesReport::encode_framed(&decoded.state, decoded.framing);
        assert_eq!(
            encoded.as_bytes(),
            packet,
            "fixture {} changed during round trip",
            fixture.name
        );
    }
}

#[test]
fn receiver_readback_accepts_captured_webdriver_declaration() {
    let packet = from_hex("0511010003a80000ff010401af0000");
    let decoded =
        PreferencesReport::decode_for_transport(&packet, TransportKind::Receiver, profile(1))
            .expect("captured FA60 preferences readback must decode");
    assert_eq!(decoded.state.profile, profile(1));
    assert!(PreferencesReport::decode(&packet, profile(1)).is_err());
}

#[test]
fn compact_and_full_framings_have_exact_lengths_and_parity() {
    let state = PreferencesState::new(profile(5), 0x60, 0xa5, 0xf1, [0x12, 0x34, 0x56], 0x3c, 0x19);
    let compact = PreferencesReport::encode_framed(&state, PreferencesFraming::Compact);
    let full = PreferencesReport::encode_framed(&state, PreferencesFraming::Full);

    assert_eq!(compact.as_bytes().len(), 13);
    assert_eq!(full.as_bytes().len(), 15);
    assert_eq!(&full.as_bytes()[..13], compact.as_bytes());
    assert_eq!(&full.as_bytes()[13..], &[0, 0]);
    assert_eq!(
        PreferencesReport::decode(compact.as_bytes(), profile(5))
            .unwrap()
            .framing,
        PreferencesFraming::Compact
    );
    assert_eq!(
        PreferencesReport::decode(full.as_bytes(), profile(5))
            .unwrap()
            .framing,
        PreferencesFraming::Full
    );
}

#[test]
fn every_profile_id_is_explicitly_encoded_and_validated() {
    for value in 1..=5 {
        let state =
            PreferencesState::new(profile(value), 0x10, 0x21, 0xa8, [0xaa, 0xbb, 0xcc], 4, 2);
        let packet = PreferencesReport::encode_framed(&state, PreferencesFraming::Compact);
        let decoded = PreferencesReport::decode(packet.as_bytes(), profile(value)).unwrap();
        assert_eq!(decoded.state, state);
    }
}

#[test]
fn unresolved_host_bytes_are_preserved_exactly() {
    let mut packet = from_hex(&fixtures().fixtures[0].packet_hex);
    packet[3..11].copy_from_slice(&[0x7f, 0xd2, 0x09, 0x01, 0xfe, 0x80, 0x2d, 0xe7]);
    rewrite_checksum(&mut packet);

    let decoded = PreferencesReport::decode(&packet, profile(1)).unwrap();
    let encoded = PreferencesReport::encode_framed(&decoded.state, decoded.framing);
    assert_eq!(encoded.as_bytes(), packet);
    assert_eq!(decoded.state.host_color, [0x01, 0xfe, 0x80]);
}

#[test]
fn decoder_rejects_checksum_high_and_low_byte_corruption() {
    let packet = from_hex(&fixtures().fixtures[1].packet_hex);

    let mut high = packet.clone();
    high[11] ^= 1;
    assert!(matches!(
        PreferencesReport::decode(&high, profile(1)),
        Err(ProtocolError::ChecksumMismatch { .. })
    ));

    let mut low = packet;
    low[12] ^= 1;
    assert!(matches!(
        PreferencesReport::decode(&low, profile(1)),
        Err(ProtocolError::ChecksumMismatch { .. })
    ));
}

#[test]
fn decoder_rejects_target_mismatch_identity_length_and_padding() {
    let packet = from_hex(&fixtures().fixtures[1].packet_hex);
    assert_eq!(
        PreferencesReport::decode(&packet, profile(2)),
        Err(ProtocolError::ProfileMismatch {
            expected: 2,
            actual: 1
        })
    );

    let mut wrong_id = packet.clone();
    wrong_id[0] = 0x04;
    assert_eq!(
        PreferencesReport::decode(&wrong_id, profile(1)),
        Err(ProtocolError::UnexpectedReportId {
            expected: 0x05,
            actual: 0x04
        })
    );

    let mut wrong_declared_length = packet.clone();
    wrong_declared_length[1] = 0x0d;
    assert_eq!(
        PreferencesReport::decode(&wrong_declared_length, profile(1)),
        Err(ProtocolError::UnexpectedDeclaredLength {
            expected: 0x0f,
            actual: 0x0d
        })
    );

    assert_eq!(
        PreferencesReport::decode(&packet[..14], profile(1)),
        Err(ProtocolError::InvalidReportLength {
            expected: 13,
            actual: 14
        })
    );

    let mut bad_padding = packet;
    bad_padding[14] = 1;
    assert_eq!(
        PreferencesReport::decode(&bad_padding, profile(1)),
        Err(ProtocolError::InvalidFixedByte {
            offset: 14,
            expected: 0,
            actual: 1
        })
    );
}
