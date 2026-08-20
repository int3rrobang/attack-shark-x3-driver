use attack_shark_x3::{
    ProfileId, StageIndex,
    protocol::input::{
        BatteryEvent, ConnectionChangedEvent, DpiButtonEvent, DpiIndex, DpiIndexChangedEvent,
        InputEvent, LedModeChangedEvent, ProfileChangedEvent, decode_battery_report,
        decode_dpi_button_report, decode_input_report,
    },
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureCatalog {
    schema_version: u8,
    fixtures: Vec<InputFixture>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InputFixture {
    name: String,
    evidence: String,
    source: String,
    packet_hex: String,
    decoded: Option<DecodedExpectation>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DecodedExpectation {
    kind: String,
    #[serde(default)]
    level: Option<u8>,
    #[serde(default)]
    stage: Option<u8>,
    #[serde(default)]
    profile: Option<u8>,
    #[serde(default)]
    connected: Option<bool>,
    #[serde(default)]
    index: Option<u8>,
    #[serde(default)]
    mode: Option<u8>,
}

fn fixtures() -> FixtureCatalog {
    serde_json::from_str(include_str!("../../../fixtures/protocol/input.json"))
        .expect("the checked-in input fixture catalog must be valid JSON")
}

fn from_hex(value: &str) -> Vec<u8> {
    if value.is_empty() {
        return Vec::new();
    }
    assert_eq!(
        value.len() % 2,
        0,
        "hex fixture must have complete bytes: {value}"
    );
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

fn profile(value: u8) -> ProfileId {
    ProfileId::try_from(value).expect("test profile must be valid")
}

fn stage(value: u8) -> StageIndex {
    StageIndex::try_from(value).expect("test stage must be valid")
}

#[test]
fn fixture_catalog_is_evidence_labeled_and_versioned() {
    let catalog = fixtures();
    assert_eq!(catalog.schema_version, 1);
    assert!(!catalog.fixtures.is_empty());
    for fixture in &catalog.fixtures {
        assert!(!fixture.name.is_empty(), "fixture name must be set");
        assert!(
            !fixture.evidence.is_empty(),
            "fixture {} must be evidence-labeled",
            fixture.name
        );
        assert!(
            !fixture.source.is_empty(),
            "fixture {} must cite source",
            fixture.name
        );
        // packetHex may be empty for empty-packet edge case
        if !fixture.packet_hex.is_empty() {
            assert_eq!(
                fixture.packet_hex.len() % 2,
                0,
                "fixture {} has odd hex length",
                fixture.name
            );
        }
    }
}

#[test]
fn golden_fixtures_decode_as_labeled() {
    let catalog = fixtures();
    for fixture in catalog.fixtures {
        let packet = from_hex(&fixture.packet_hex);
        let decoded = decode_input_report(&packet);
        let battery_decoded = if packet.is_empty() {
            None
        } else {
            decode_battery_report(&packet)
        };
        let dpi_button_decoded = decode_dpi_button_report(&packet);

        match fixture.decoded {
            Some(expected) => {
                let event = decoded.unwrap_or_else(|| {
                    panic!(
                        "fixture {} (evidence: {}, source: {}) with packetHex {} should decode to {} but got None",
                        fixture.name, fixture.evidence, fixture.source, fixture.packet_hex, expected.kind
                    )
                });
                match expected.kind.as_str() {
                    "BatteryChanged" => {
                        let level = expected
                            .level
                            .expect("BatteryChanged fixture must have level");
                        match event {
                            InputEvent::BatteryChanged(BatteryEvent {
                                level: actual,
                                raw_report,
                            }) => {
                                assert_eq!(
                                    actual, level,
                                    "fixture {} level mismatch",
                                    fixture.name
                                );
                                // raw_report is first 5 bytes preserved
                                let expected_raw: [u8; 5] = packet[..5]
                                    .try_into()
                                    .expect("battery packet must be 5 bytes");
                                assert_eq!(
                                    raw_report, expected_raw,
                                    "fixture {} raw_report mismatch",
                                    fixture.name
                                );
                                // battery decoder must agree
                                let via_battery = battery_decoded
                                    .expect("decode_battery_report must agree for BatteryChanged");
                                assert_eq!(
                                    via_battery.level, level,
                                    "fixture {} battery path level mismatch",
                                    fixture.name
                                );
                            }
                            other => panic!(
                                "fixture {} expected BatteryChanged but got {:?}",
                                fixture.name, other
                            ),
                        }
                    }
                    "ActiveDpiStageChanged" => {
                        let stage_val = expected
                            .stage
                            .expect("ActiveDpiStageChanged fixture must have stage");
                        match event {
                            InputEvent::ActiveDpiStageChanged(DpiButtonEvent {
                                active_stage,
                                raw_report,
                            }) => {
                                assert_eq!(
                                    active_stage,
                                    stage(stage_val),
                                    "fixture {} stage mismatch",
                                    fixture.name
                                );
                                let expected_raw: [u8; 5] = packet[..5].try_into().unwrap();
                                assert_eq!(raw_report, expected_raw);
                                let via_dpi = dpi_button_decoded
                                    .expect("decode_dpi_button_report must agree");
                                assert_eq!(via_dpi.active_stage, stage(stage_val));
                            }
                            other => panic!(
                                "fixture {} expected ActiveDpiStageChanged but got {:?}",
                                fixture.name, other
                            ),
                        }
                    }
                    "SecondaryProfileChanged" => {
                        let prof = expected
                            .profile
                            .expect("SecondaryProfileChanged must have profile");
                        match event {
                            InputEvent::SecondaryProfileChanged(ProfileChangedEvent {
                                profile: actual,
                                raw_report,
                            }) => {
                                assert_eq!(
                                    actual,
                                    profile(prof),
                                    "fixture {} profile mismatch",
                                    fixture.name
                                );
                                let expected_raw: [u8; 5] = packet[..5].try_into().unwrap();
                                assert_eq!(raw_report, expected_raw);
                            }
                            other => panic!(
                                "fixture {} expected SecondaryProfileChanged but got {:?}",
                                fixture.name, other
                            ),
                        }
                    }
                    "ConnectionChanged" => {
                        let connected = expected
                            .connected
                            .expect("ConnectionChanged must have connected");
                        match event {
                            InputEvent::ConnectionChanged(ConnectionChangedEvent {
                                connected: actual,
                                raw_report,
                            }) => {
                                assert_eq!(
                                    actual, connected,
                                    "fixture {} connected mismatch",
                                    fixture.name
                                );
                                let expected_raw: [u8; 5] = packet[..5].try_into().unwrap();
                                assert_eq!(raw_report, expected_raw);
                            }
                            other => panic!(
                                "fixture {} expected ConnectionChanged but got {:?}",
                                fixture.name, other
                            ),
                        }
                    }
                    "DpiIndexChanged" => {
                        let idx = expected.index.expect("DpiIndexChanged must have index");
                        match event {
                            InputEvent::DpiIndexChanged(DpiIndexChangedEvent {
                                index,
                                raw_report,
                            }) => {
                                assert_eq!(
                                    index,
                                    DpiIndex::new(idx).unwrap(),
                                    "fixture {} index mismatch",
                                    fixture.name
                                );
                                let expected_raw: [u8; 5] = packet[..5].try_into().unwrap();
                                assert_eq!(raw_report, expected_raw);
                            }
                            other => panic!(
                                "fixture {} expected DpiIndexChanged but got {:?}",
                                fixture.name, other
                            ),
                        }
                    }
                    "LedModeChanged" => {
                        let mode = expected.mode.expect("LedModeChanged must have mode");
                        match event {
                            InputEvent::LedModeChanged(LedModeChangedEvent {
                                mode: actual,
                                raw_report,
                            }) => {
                                assert_eq!(actual, mode, "fixture {} mode mismatch", fixture.name);
                                let expected_raw: [u8; 5] = packet[..5].try_into().unwrap();
                                assert_eq!(raw_report, expected_raw);
                            }
                            other => panic!(
                                "fixture {} expected LedModeChanged but got {:?}",
                                fixture.name, other
                            ),
                        }
                    }
                    "ProfileSync" => {
                        let prof = expected.profile.expect("ProfileSync must have profile");
                        match event {
                            InputEvent::ProfileSync(ProfileChangedEvent {
                                profile: actual,
                                raw_report,
                            }) => {
                                assert_eq!(
                                    actual,
                                    profile(prof),
                                    "fixture {} profile sync mismatch",
                                    fixture.name
                                );
                                let expected_raw: [u8; 5] = packet[..5].try_into().unwrap();
                                assert_eq!(raw_report, expected_raw);
                            }
                            other => panic!(
                                "fixture {} expected ProfileSync but got {:?}",
                                fixture.name, other
                            ),
                        }
                    }
                    other => panic!(
                        "fixture {} has unknown expected kind {}",
                        fixture.name, other
                    ),
                }
                // negative cross-checks: battery and dpi button decoders must not disagree
                if expected.kind != "BatteryChanged" {
                    assert_eq!(
                        battery_decoded, None,
                        "fixture {} non-battery should not decode via battery path",
                        fixture.name
                    );
                }
                if expected.kind != "ActiveDpiStageChanged" {
                    assert_eq!(
                        dpi_button_decoded, None,
                        "fixture {} non-dpi-button should not decode via dpi button path",
                        fixture.name
                    );
                }
            }
            None => {
                assert_eq!(
                    decoded, None,
                    "fixture {} (evidence: {}, source: {}) with packetHex {} should be rejected but got {:?}",
                    fixture.name, fixture.evidence, fixture.source, fixture.packet_hex, decoded
                );
                // also ensure narrow decoders agree on rejection where applicable
                assert_eq!(
                    battery_decoded, None,
                    "fixture {} battery path should also reject",
                    fixture.name
                );
                assert_eq!(
                    dpi_button_decoded, None,
                    "fixture {} dpi button path should also reject",
                    fixture.name
                );
            }
        }
    }
}

#[test]
fn x3_battery_golden_range_one_through_ten_is_contiguous() {
    for level in 1u8..=10 {
        let packet = [0x03, 0x10, 0x40, 0x01, level];
        let event = decode_input_report(&packet).expect("X3 battery level must decode");
        match event {
            InputEvent::BatteryChanged(ev) => assert_eq!(ev.level, level * 10),
            other => panic!("level {level} expected BatteryChanged got {other:?}"),
        }
        let via_battery = decode_battery_report(&packet).expect("battery path must decode");
        assert_eq!(via_battery.level, level * 10);
        // trailing padding is ignored
        let mut padded = packet.to_vec();
        padded.extend_from_slice(&[0x00, 0xff]);
        assert_eq!(decode_input_report(&padded), Some(event));
    }
}

#[test]
fn x3_battery_rejects_zero_and_eleven_and_malformed_prefixes() {
    // 0 and 11 are outside 1..=10
    assert_eq!(decode_input_report(&[0x03, 0x10, 0x40, 0x01, 0x00]), None);
    assert_eq!(decode_battery_report(&[0x03, 0x10, 0x40, 0x01, 0x00]), None);
    assert_eq!(decode_input_report(&[0x03, 0x10, 0x40, 0x01, 0x0b]), None);
    assert_eq!(decode_battery_report(&[0x03, 0x10, 0x40, 0x01, 0x0b]), None);
    assert_eq!(decode_input_report(&[0x03, 0x10, 0x40, 0x01, 0xff]), None);

    // malformed prefixes
    assert_eq!(decode_input_report(&[0x03, 0x11, 0x40, 0x01, 0x05]), None);
    assert_eq!(decode_input_report(&[0x03, 0x10, 0x41, 0x01, 0x05]), None);
    assert_eq!(decode_input_report(&[0x03, 0x10, 0x40, 0x02, 0x05]), None);
    assert_eq!(decode_input_report(&[0x02, 0x10, 0x40, 0x01, 0x05]), None);
}

#[test]
fn malformed_prefixes_and_lengths_are_rejected() {
    // too short
    assert_eq!(decode_input_report(&[]), None);
    assert_eq!(decode_input_report(&[0x03]), None);
    assert_eq!(decode_input_report(&[0x03, 0x10, 0x40, 0x01]), None);
    assert_eq!(decode_battery_report(&[]), None);
    assert_eq!(decode_battery_report(&[0x03, 0x10, 0x40, 0x01]), None);
    assert_eq!(decode_dpi_button_report(&[]), None);
    assert_eq!(decode_dpi_button_report(&[0x03, 0x00, 0x10]), None);

    // wrong report id
    assert_eq!(decode_input_report(&[0x02, 0x00, 0x10, 0x02, 0x00]), None);
    assert_eq!(decode_input_report(&[0x04, 0x00, 0x10, 0x02, 0x00]), None);

    // dpi button trailing byte must be 0
    assert_eq!(decode_input_report(&[0x03, 0x00, 0x10, 0x02, 0x01]), None);
    assert_eq!(
        decode_dpi_button_report(&[0x03, 0x00, 0x10, 0x02, 0x01]),
        None
    );
    // dpi button stage 0 and 9 are out of range
    assert_eq!(decode_input_report(&[0x03, 0x00, 0x10, 0x00, 0x00]), None);
    assert_eq!(decode_input_report(&[0x03, 0x00, 0x10, 0x09, 0x00]), None);

    // connection state out of range
    assert_eq!(decode_input_report(&[0x03, 0x10, 0x50, 0x02, 0x00]), None);
    assert_eq!(decode_input_report(&[0x03, 0x10, 0x50, 0x00, 0x01]), None);

    // dpi index out of range
    assert_eq!(decode_input_report(&[0x03, 0x00, 0x60, 0x00, 0x00]), None);
    assert_eq!(decode_input_report(&[0x03, 0x00, 0x60, 0x0b, 0x00]), None);

    // led mode out of range
    assert_eq!(decode_input_report(&[0x03, 0x00, 0x70, 0x08, 0x00]), None);
    assert_eq!(decode_input_report(&[0x03, 0x00, 0x70, 0x00, 0x01]), None);

    // profile sync out of range (zero-indexed 5 maps to profile 6)
    assert_eq!(decode_input_report(&[0x03, 0x00, 0x80, 0x05, 0x00]), None);
}

#[test]
fn representative_input_events_round_trip() {
    // ActiveDpiStageChanged
    let dpi = [0x03, 0x00, 0x10, 0x03, 0x00];
    assert!(matches!(
        decode_input_report(&dpi),
        Some(InputEvent::ActiveDpiStageChanged(_))
    ));
    assert!(decode_dpi_button_report(&dpi).is_some());
    assert!(decode_battery_report(&dpi).is_none());

    // SecondaryProfileChanged
    let sec = [0x03, 0x00, 0x20, 0x02, 0x00];
    assert!(matches!(
        decode_input_report(&sec),
        Some(InputEvent::SecondaryProfileChanged(_))
    ));

    // BatteryChanged X3 and X11
    assert!(matches!(
        decode_input_report(&[0x03, 0x10, 0x40, 0x01, 0x0a]),
        Some(InputEvent::BatteryChanged(_))
    ));
    assert!(matches!(
        decode_input_report(&[0x03, 0x55, 0x40, 0x01, 0x64]),
        Some(InputEvent::BatteryChanged(_))
    ));
    assert!(decode_battery_report(&[0x03, 0x55, 0x40, 0x01, 0x64]).is_some());

    // ConnectionChanged
    assert!(matches!(
        decode_input_report(&[0x03, 0x10, 0x50, 0x00, 0x00]),
        Some(InputEvent::ConnectionChanged(_))
    ));
    assert!(matches!(
        decode_input_report(&[0x03, 0x10, 0x50, 0x01, 0x00]),
        Some(InputEvent::ConnectionChanged(_))
    ));

    // DpiIndexChanged
    assert!(matches!(
        decode_input_report(&[0x03, 0x00, 0x60, 0x05, 0x00]),
        Some(InputEvent::DpiIndexChanged(_))
    ));

    // LedModeChanged
    assert!(matches!(
        decode_input_report(&[0x03, 0x00, 0x70, 0x03, 0x00]),
        Some(InputEvent::LedModeChanged(_))
    ));

    // ProfileSync
    assert!(matches!(
        decode_input_report(&[0x03, 0x00, 0x80, 0x02, 0x00]),
        Some(InputEvent::ProfileSync(_))
    ));
}

#[test]
fn unknown_and_unsupported_reports_remain_unsupported() {
    // unknown event types
    assert_eq!(decode_input_report(&[0x03, 0x99, 0x99, 0x00, 0x00]), None);
    assert_eq!(decode_input_report(&[0x03, 0x34, 0x12, 0x00, 0x00]), None);
    assert_eq!(decode_input_report(&[0x03, 0x11, 0x11, 0x00, 0x00]), None);
    assert_eq!(decode_input_report(&[0x03, 0xff, 0xff, 0x00, 0x00]), None);
    // 0x07 wakeup mode and 0x09 macro report ids must not be interpreted as input events
    // As input event types they would be 0x0708 and 0x0900 – both unknown
    assert_eq!(decode_input_report(&[0x03, 0x08, 0x07, 0x01, 0x00]), None);
    assert_eq!(decode_input_report(&[0x03, 0x00, 0x09, 0x00, 0x00]), None);
    assert_eq!(decode_input_report(&[0x03, 0x08, 0x07, 0x00, 0x00]), None);
    assert_eq!(decode_input_report(&[0x03, 0x34, 0x12, 0x01, 0x00]), None);
    // narrow decoders also reject unknown
    assert_eq!(decode_battery_report(&[0x03, 0x99, 0x99, 0x00, 0x00]), None);
    assert_eq!(
        decode_dpi_button_report(&[0x03, 0x99, 0x99, 0x00, 0x00]),
        None
    );
    assert_eq!(
        decode_dpi_button_report(&[0x03, 0x00, 0x20, 0x02, 0x00]),
        None
    );
}

#[test]
fn x11_battery_boundaries() {
    // 0 is valid for X11 legacy (direct 0..=100)
    assert_eq!(
        decode_battery_report(&[0x03, 0x55, 0x40, 0x01, 0x00])
            .unwrap()
            .level,
        0
    );
    assert_eq!(
        decode_battery_report(&[0x03, 0x55, 0x40, 0x01, 0x64])
            .unwrap()
            .level,
        100
    );
    assert_eq!(
        decode_input_report(&[0x03, 0x55, 0x40, 0x01, 0x00]).unwrap(),
        InputEvent::BatteryChanged(BatteryEvent {
            raw_report: [0x03, 0x55, 0x40, 0x01, 0x00],
            level: 0
        })
    );
    // 101 is rejected
    assert_eq!(decode_input_report(&[0x03, 0x55, 0x40, 0x01, 0x65]), None);
    assert_eq!(decode_battery_report(&[0x03, 0x55, 0x40, 0x01, 0x65]), None);
    assert_eq!(decode_battery_report(&[0x03, 0x55, 0x40, 0x01, 0xff]), None);
}
