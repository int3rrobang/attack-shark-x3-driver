use attack_shark_x3::{
    DpiFraming, DpiReport, DpiState, DpiValue, ProfileId, ProtocolError, SensorOptions, StageIndex,
    TransportKind,
    protocol::{
        checksum::sum16,
        dpi::{DPI_WIRED_LENGTH, LiftOffDistance},
    },
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureCatalog {
    schema_version: u8,
    fixtures: Vec<DpiFixture>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DpiFixture {
    name: String,
    evidence: String,
    source: String,
    transport: FixtureTransport,
    framing: FixtureFraming,
    profile: u8,
    packet_hex: String,
    known_state: KnownState,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum FixtureTransport {
    Wired,
    Receiver,
}

impl From<FixtureTransport> for TransportKind {
    fn from(value: FixtureTransport) -> Self {
        match value {
            FixtureTransport::Wired => Self::Wired,
            FixtureTransport::Receiver => Self::Receiver,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum FixtureFraming {
    Compact,
    Full,
}

impl From<FixtureFraming> for DpiFraming {
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
    lod: String,
    ripple_control: bool,
    stage_mask: u8,
    enabled_stages: usize,
    angle_snap: bool,
    motion_sync: bool,
    dpi_values: Vec<u16>,
    active_stage: u8,
    fixed_tail_last_byte: u8,
    #[serde(default)]
    padding: Vec<u8>,
}

fn fixtures() -> FixtureCatalog {
    serde_json::from_str(include_str!("../../../fixtures/protocol/dpi.json"))
        .expect("the checked-in DPI fixture catalog must be valid JSON")
}

fn profile(value: u8) -> ProfileId {
    ProfileId::try_from(value).expect("test profile must be valid")
}

fn stage(value: u8) -> StageIndex {
    StageIndex::try_from(value).expect("test stage must be valid")
}

fn dpi(value: u16) -> DpiValue {
    DpiValue::try_from(value).expect("test DPI must be valid")
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
    let checksum = sum16(&packet[3..50]).to_be_bytes();
    packet[50..52].copy_from_slice(&checksum);
}

#[test]
fn captured_and_implementation_fixtures_round_trip_byte_for_byte() {
    let catalog = fixtures();
    assert_eq!(catalog.schema_version, 1);
    assert!(!catalog.fixtures.is_empty());

    for fixture in catalog.fixtures {
        assert!(!fixture.name.is_empty());
        assert!(!fixture.evidence.is_empty());
        assert!(!fixture.source.is_empty());

        let packet = from_hex(&fixture.packet_hex);
        let transport = TransportKind::from(fixture.transport);
        let expected_profile = profile(fixture.profile);
        let decoded = DpiReport::decode(&packet, transport, expected_profile)
            .unwrap_or_else(|error| panic!("fixture {} did not decode: {error}", fixture.name));

        assert_eq!(decoded.framing, DpiFraming::from(fixture.framing));
        assert_eq!(decoded.state.profile, expected_profile);
        assert_eq!(
            decoded.state.active_stage.get(),
            fixture.known_state.active_stage
        );
        assert_eq!(
            decoded.state.stages.len(),
            fixture.known_state.enabled_stages
        );
        assert_eq!(
            decoded
                .state
                .stages
                .iter()
                .map(|value| value.get())
                .collect::<Vec<_>>(),
            fixture.known_state.dpi_values
        );
        assert_eq!(
            decoded.state.sensor.ripple_control,
            fixture.known_state.ripple_control
        );
        assert_eq!(
            decoded.state.sensor.angle_snap,
            fixture.known_state.angle_snap
        );
        assert_eq!(
            decoded.state.sensor.motion_sync,
            fixture.known_state.motion_sync
        );
        assert_eq!(
            decoded.state.preserved_tail[24],
            fixture.known_state.fixed_tail_last_byte
        );
        assert_eq!(packet[5], fixture.known_state.stage_mask);
        let expected_lod = match fixture.known_state.lod.as_str() {
            "1mm" => LiftOffDistance::OneMillimeter,
            "2mm" => LiftOffDistance::TwoMillimeters,
            value => panic!("fixture {} has unsupported LOD {value}", fixture.name),
        };
        assert_eq!(decoded.state.sensor.lift_off_distance, expected_lod);
        if decoded.framing == DpiFraming::Full {
            assert_eq!(&packet[DPI_WIRED_LENGTH..], fixture.known_state.padding);
        }

        let encoded = DpiReport::encode_framed(&decoded.state, decoded.framing)
            .unwrap_or_else(|error| panic!("fixture {} did not re-encode: {error}", fixture.name));
        assert_eq!(
            encoded.as_bytes(),
            packet,
            "fixture {} changed during round trip",
            fixture.name
        );
    }
}

#[test]
fn profile_five_round_trips_with_an_explicit_caller_supplied_tail() {
    let preserved_tail = [0xa5; 25];
    let state = DpiState::new(
        profile(5),
        vec![dpi(50), dpi(12_800), dpi(26_000)],
        stage(3),
        preserved_tail,
    )
    .expect("state must be valid");

    for transport in [TransportKind::Wired, TransportKind::Receiver] {
        let encoded = DpiReport::encode(&state, transport).expect("profile 5 must encode");
        assert_eq!(encoded.as_bytes()[2], 5);
        let decoded = DpiReport::decode(encoded.as_bytes(), transport, profile(5))
            .expect("profile 5 must decode");
        assert_eq!(decoded.state, state);
    }
}

#[test]
fn round_trip_preserves_every_unresolved_tail_byte() {
    let fixture = &fixtures().fixtures[0];
    let mut packet = from_hex(&fixture.packet_hex);
    packet[31] ^= 0x5a;
    rewrite_checksum(&mut packet);

    let decoded = DpiReport::decode(&packet, TransportKind::Wired, profile(1))
        .expect("packet with an unresolved-tail change must decode");
    let encoded = DpiReport::encode_framed(&decoded.state, decoded.framing)
        .expect("decoded packet must re-encode");

    assert_eq!(encoded.as_bytes(), packet);
}

#[test]
fn sensor_options_use_their_confirmed_offsets() {
    let stages = vec![dpi(800), dpi(1600)];
    let active = stage(1);
    let cases = [
        (
            3,
            SensorOptions {
                lift_off_distance: LiftOffDistance::TwoMillimeters,
                ..SensorOptions::default()
            },
        ),
        (
            4,
            SensorOptions {
                ripple_control: true,
                ..SensorOptions::default()
            },
        ),
        (
            6,
            SensorOptions {
                angle_snap: true,
                ..SensorOptions::default()
            },
        ),
        (
            7,
            SensorOptions {
                motion_sync: true,
                ..SensorOptions::default()
            },
        ),
    ];

    for (expected_offset, sensor) in cases {
        let mut state = DpiState::captured_empty_profile_one(stages.clone(), active)
            .expect("profile 1 state must be valid");
        state.sensor = sensor;
        let report =
            DpiReport::encode(&state, TransportKind::Wired).expect("sensor state must encode");
        for offset in [3, 4, 6, 7] {
            assert_eq!(
                report.as_bytes()[offset],
                u8::from(offset == expected_offset),
                "unexpected sensor byte at offset {offset}"
            );
        }

        let decoded = DpiReport::decode(report.as_bytes(), TransportKind::Wired, profile(1))
            .expect("sensor state must decode");
        assert_eq!(decoded.state.sensor, sensor);
    }
}

#[test]
fn decoder_rejects_wrong_identity_profile_checksum_and_length() {
    let fixture = &fixtures().fixtures[0];
    let packet = from_hex(&fixture.packet_hex);

    let mut wrong_id = packet.clone();
    wrong_id[0] = 0x05;
    assert_eq!(
        DpiReport::decode(&wrong_id, TransportKind::Wired, profile(1)),
        Err(ProtocolError::UnexpectedReportId {
            expected: 0x04,
            actual: 0x05
        })
    );

    assert_eq!(
        DpiReport::decode(&packet, TransportKind::Wired, profile(2)),
        Err(ProtocolError::ProfileMismatch {
            expected: 2,
            actual: 1
        })
    );

    let mut wrong_checksum = packet.clone();
    wrong_checksum[51] ^= 1;
    assert!(matches!(
        DpiReport::decode(&wrong_checksum, TransportKind::Wired, profile(1)),
        Err(ProtocolError::ChecksumMismatch { .. })
    ));

    assert_eq!(
        DpiReport::decode(&packet[..51], TransportKind::Wired, profile(1)),
        Err(ProtocolError::InvalidReportLength {
            expected: 52,
            actual: 51
        })
    );
}

#[test]
fn decoder_rejects_noncanonical_stage_and_sensor_fields() {
    let fixture = &fixtures().fixtures[0];
    let packet = from_hex(&fixture.packet_hex);

    let mut invalid_mask = packet.clone();
    invalid_mask[5] = 0x05;
    rewrite_checksum(&mut invalid_mask);
    assert_eq!(
        DpiReport::decode(&invalid_mask, TransportKind::Wired, profile(1)),
        Err(ProtocolError::InvalidStageMask { mask: 0x05 })
    );

    let mut invalid_sensor = packet;
    invalid_sensor[4] = 2;
    rewrite_checksum(&mut invalid_sensor);
    assert_eq!(
        DpiReport::decode(&invalid_sensor, TransportKind::Wired, profile(1)),
        Err(ProtocolError::InvalidSensorValue {
            field: "ripple_control",
            value: 2,
        })
    );
}

#[test]
fn full_framing_rejects_nonzero_padding() {
    let fixture = &fixtures().fixtures[1];
    let mut packet = from_hex(&fixture.packet_hex);
    packet[55] = 1;

    assert_eq!(
        DpiReport::decode(&packet, TransportKind::Receiver, profile(1)),
        Err(ProtocolError::InvalidFixedByte {
            offset: 55,
            expected: 0,
            actual: 1,
        })
    );
}
