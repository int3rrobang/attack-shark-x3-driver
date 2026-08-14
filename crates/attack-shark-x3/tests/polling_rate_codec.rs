use attack_shark_x3::{
    DecodedPollingRateReport, PollingRate, PollingRateReport, ProfileId, ProtocolError,
    TransportKind,
};

fn profile(value: u8) -> ProfileId {
    ProfileId::try_from(value).expect("test profile must be valid")
}

/// Report `0x06` byte 2 is the one-based target profile consumed by the
/// shared dispatcher prelude. Golden bytes below match the capture-confirmed
/// write shape `06 09 <profile> <rate> <~rate> 00 00 00 00`.
#[test]
fn encodes_profile_one_golden_bytes() {
    assert_eq!(
        PollingRateReport::encode(profile(1), PollingRate::Hz125).as_bytes(),
        b"\x06\x09\x01\x08\xf7\x00\x00\x00\x00"
    );
    assert_eq!(
        PollingRateReport::encode(profile(1), PollingRate::Hz250).as_bytes(),
        b"\x06\x09\x01\x04\xfb\x00\x00\x00\x00"
    );
    assert_eq!(
        PollingRateReport::encode(profile(1), PollingRate::Hz500).as_bytes(),
        b"\x06\x09\x01\x02\xfd\x00\x00\x00\x00"
    );
    assert_eq!(
        PollingRateReport::encode(profile(1), PollingRate::Hz1000).as_bytes(),
        b"\x06\x09\x01\x01\xfe\x00\x00\x00\x00"
    );
}

#[test]
fn encodes_profile_two_with_byte_two_zero_x02() {
    assert_eq!(
        PollingRateReport::encode(profile(2), PollingRate::Hz1000).as_bytes(),
        b"\x06\x09\x02\x01\xfe\x00\x00\x00\x00"
    );
    assert_eq!(
        PollingRateReport::encode(profile(2), PollingRate::Hz500).as_bytes(),
        b"\x06\x09\x02\x02\xfd\x00\x00\x00\x00"
    );
}

#[test]
fn decodes_profile_scoped_write_and_readback_shapes() {
    let packet = b"\x06\x09\x02\x01\xfe\x00\x00\x00\x00";
    let decoded: DecodedPollingRateReport =
        PollingRateReport::decode(packet, profile(2)).expect("valid profile-2 report");
    assert_eq!(decoded.profile, profile(2));
    assert_eq!(decoded.rate, PollingRate::Hz1000);

    // FA60 receiver readback uses declared length `0x0b`.
    let receiver = b"\x06\x0b\x02\x02\xfd\x00\x00\x00\x00";
    let decoded =
        PollingRateReport::decode_for_transport(receiver, TransportKind::Receiver, profile(2))
            .expect("valid receiver readback");
    assert_eq!(decoded.profile, profile(2));
    assert_eq!(decoded.rate, PollingRate::Hz500);
}

#[test]
fn rejects_wrong_profile_byte_and_invalid_profile() {
    let packet = b"\x06\x09\x01\x01\xfe\x00\x00\x00\x00";
    assert!(matches!(
        PollingRateReport::decode(packet, profile(2)),
        Err(ProtocolError::ProfileMismatch {
            expected: 2,
            actual: 1,
        })
    ));

    let mut invalid = *packet;
    invalid[2] = 0;
    assert!(matches!(
        PollingRateReport::decode(&invalid, profile(1)),
        Err(ProtocolError::InvalidProfile { value: 0 })
    ));
}

#[test]
fn rejects_malformed_rate_bytes() {
    let mut packet = *b"\x06\x09\x02\x02\xfd\x00\x00\x00\x00";
    packet[4] = 0;
    assert!(matches!(
        PollingRateReport::decode(&packet, profile(2)),
        Err(ProtocolError::InvalidComplement {
            field: "polling rate",
            ..
        })
    ));

    let mut invalid_code = *b"\x06\x09\x02\x02\xfd\x00\x00\x00\x00";
    invalid_code[3] = 0x10;
    invalid_code[4] = 0xef;
    assert!(matches!(
        PollingRateReport::decode(&invalid_code, profile(2)),
        Err(ProtocolError::InvalidPollingRateCode { value: 0x10 })
    ));
}
