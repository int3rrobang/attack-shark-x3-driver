use attack_shark_x3::protocol::checksum::sum16;
use attack_shark_x3::{
    DebounceMs, DeepSleepMinutes, PreferencesFraming, PreferencesReport, PreferencesState,
    ProfileId, ProtocolError, SleepTimer, TransportKind,
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
            expected: 15,
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

#[test]
fn debounce_boundaries_round_trip_and_reject_invalid() {
    assert_eq!(DebounceMs::MIN_MS, 4);
    assert_eq!(DebounceMs::MAX_MS, 50);
    assert_eq!(DebounceMs::MIN_RAW, 2);
    assert_eq!(DebounceMs::MAX_RAW, 25);

    for ms in (DebounceMs::MIN_MS..=DebounceMs::MAX_MS).step_by(2) {
        let debounce = DebounceMs::new(ms).expect("even in-range debounce must construct");
        assert_eq!(debounce.get(), ms);
        let raw = debounce.raw();
        assert_eq!(raw, ((ms - 4) / 2) + 2);
        assert_eq!(DebounceMs::from_raw(raw), Some(debounce));
    }

    // Canonical raw edge decodes.
    assert_eq!(DebounceMs::from_raw(2).map(DebounceMs::get), Some(4));
    assert_eq!(DebounceMs::from_raw(25).map(DebounceMs::get), Some(50));
    assert_eq!(
        DebounceMs::new(8).map(|value| value.to_string()),
        Some("8 ms".into())
    );

    // Odd, below-range, and above-range user values are rejected, never rounded.
    for ms in [0, 2, 3, 5, 49, 51, 255] {
        assert_eq!(
            DebounceMs::new(ms),
            None,
            "debounce {ms} ms must be rejected"
        );
    }
    // Noncanonical wire bytes are rejected.
    for raw in [0, 1, 26, 255] {
        assert_eq!(
            DebounceMs::from_raw(raw),
            None,
            "raw {raw} must be rejected"
        );
    }
}

#[test]
fn sleep_timer_boundaries_round_trip_and_reject_invalid() {
    assert_eq!(SleepTimer::MIN_HALF_MINUTES, 1);
    assert_eq!(SleepTimer::MAX_HALF_MINUTES, 60);
    assert_eq!(SleepTimer::MIN_RAW, 1);
    assert_eq!(SleepTimer::MAX_RAW, 60);

    for half_minutes in SleepTimer::MIN_HALF_MINUTES..=SleepTimer::MAX_HALF_MINUTES {
        let timer = SleepTimer::new(half_minutes).expect("in-range half-minute sleep timer");
        assert_eq!(timer.get(), half_minutes);
        assert_eq!(timer.raw(), half_minutes);
        assert_eq!(SleepTimer::from_raw(half_minutes), Some(timer));
        assert_eq!(timer.minutes(), f64::from(half_minutes) / 2.0);
    }

    assert_eq!(SleepTimer::new(1).map(SleepTimer::minutes), Some(0.5));
    assert_eq!(SleepTimer::new(60).map(SleepTimer::minutes), Some(30.0));
    assert_eq!(SleepTimer::from_raw(1).map(SleepTimer::minutes), Some(0.5));
    assert_eq!(
        SleepTimer::from_raw(60).map(SleepTimer::minutes),
        Some(30.0)
    );
    assert_eq!(
        SleepTimer::new(2).map(|value| value.to_string()),
        Some("1 min".into())
    );
    assert_eq!(
        SleepTimer::new(1).map(|value| value.to_string()),
        Some("0.5 min".into())
    );

    for value in [0, 61, 255] {
        assert_eq!(
            SleepTimer::new(value),
            None,
            "user {value} must be rejected"
        );
        assert_eq!(
            SleepTimer::from_raw(value),
            None,
            "raw {value} must be rejected"
        );
    }
}

#[test]
fn deep_sleep_bucket_edges_round_trip() {
    // Values on both sides of every bucket boundary, including the exact
    // 16-minute boundaries, round trip through both wire fields.
    for minutes in [1, 15, 16, 17, 31, 32, 33, 47, 48, 49, 59, 60] {
        let deep = DeepSleepMinutes::new(minutes).expect("supported minutes must construct");
        assert_eq!(deep.get(), minutes);
        let decoded = DeepSleepMinutes::from_raw(deep.bucket(), deep.deep_sleep_byte())
            .unwrap_or_else(|| panic!("bucket edge {minutes} minutes must round trip"));
        assert_eq!(decoded, deep);
    }

    // Canonical wire images around the bucket edges. The 15/16/17, 32/33,
    // and 48-minute images are capture-confirmed from the stock app.
    let cases = [
        (1, 0, 0x18),
        (15, 0, 0xf8),
        (16, 1, 0x08),
        (17, 1, 0x18),
        (31, 1, 0xf8),
        (32, 2, 0x08),
        (33, 2, 0x18),
        (47, 2, 0xf8),
        (48, 3, 0x08),
        (49, 3, 0x18),
        (59, 3, 0xb8),
        (60, 3, 0xc8),
    ];
    for (minutes, bucket, byte) in cases {
        let deep = DeepSleepMinutes::new(minutes).expect("supported minutes must construct");
        assert_eq!(deep.bucket(), bucket, "minutes {minutes} bucket");
        assert_eq!(
            deep.deep_sleep_byte(),
            byte,
            "minutes {minutes} deep-sleep byte"
        );
        assert_eq!(DeepSleepMinutes::from_raw(bucket, byte), Some(deep));
    }
}

#[test]
fn deep_sleep_rejects_noncanonical_raw_states() {
    // Bucket zero plus a zero high nibble represents zero minutes, but the
    // capture-confirmed boundary forms in buckets 1..=3 are canonical.
    assert_eq!(DeepSleepMinutes::from_raw(0, 0x08), None);
    for (bucket, minutes) in [(1, 16), (2, 32), (3, 48)] {
        assert_eq!(
            DeepSleepMinutes::from_raw(bucket, 0x08),
            DeepSleepMinutes::new(minutes)
        );
    }
    // The low nibble of the deep-sleep byte must be the fixed 0x08 marker.
    for byte in [0x00, 0x10, 0x19, 0xff] {
        assert_eq!(
            DeepSleepMinutes::from_raw(0, byte),
            None,
            "byte {byte:#04x} has a noncanonical low nibble and must be rejected"
        );
    }
    // Buckets above 3 always exceed the 60-minute ceiling.
    for bucket in [4, 5, 15] {
        for byte in [0x18, 0xf8] {
            assert_eq!(
                DeepSleepMinutes::from_raw(bucket, byte),
                None,
                "bucket {bucket} byte {byte:#04x} must be rejected"
            );
        }
    }
    // Bucket 3 covers 48..=60; higher nibbles overflow the range.
    assert_eq!(DeepSleepMinutes::from_raw(3, 0xd8), None); // 61 minutes
    assert_eq!(
        DeepSleepMinutes::from_raw(3, 0xc8),
        DeepSleepMinutes::new(60)
    );

    for minutes in [0, 61, 255] {
        assert_eq!(
            DeepSleepMinutes::new(minutes),
            None,
            "minutes {minutes} must be rejected"
        );
    }
}

#[test]
fn deep_sleep_preserves_low_configuration_nibble() {
    for minutes in [1, 5, 17, 49, 59, 60] {
        let deep = DeepSleepMinutes::new(minutes).expect("supported minutes must construct");
        for configuration in [0x00, 0x03, 0x0f, 0xa5, 0xff] {
            let low_nibble = configuration & 0x0f;
            let encoded = deep.configuration_with(configuration);
            assert_eq!(
                encoded & 0x0f,
                low_nibble,
                "low configuration nibble must be preserved"
            );
            assert_eq!(
                encoded >> 4,
                deep.bucket(),
                "high configuration nibble is the bucket"
            );
            // A state carrying this configuration plus the deep-sleep byte
            // decodes back to the same minutes.
            assert_eq!(
                DeepSleepMinutes::from_configuration(encoded, deep.deep_sleep_byte()),
                Some(deep)
            );
        }
    }
    // from_configuration derives the bucket from the raw configuration byte.
    assert_eq!(
        DeepSleepMinutes::from_configuration(0x13, 0x18),
        DeepSleepMinutes::from_raw(1, 0x18)
    );
}

#[cfg(feature = "serde")]
#[test]
fn timing_newtypes_serde_round_trip() {
    let debounce = DebounceMs::new(8).unwrap();
    let json = serde_json::to_string(&debounce).unwrap();
    assert_eq!(json, "8");
    assert_eq!(serde_json::from_str::<DebounceMs>(&json).unwrap(), debounce);

    let timer = SleepTimer::new(3).unwrap();
    let json = serde_json::to_string(&timer).unwrap();
    assert_eq!(json, "3");
    assert_eq!(serde_json::from_str::<SleepTimer>(&json).unwrap(), timer);

    let deep = DeepSleepMinutes::new(17).unwrap();
    let json = serde_json::to_string(&deep).unwrap();
    assert_eq!(json, "17");
    assert_eq!(
        serde_json::from_str::<DeepSleepMinutes>(&json).unwrap(),
        deep
    );
}
