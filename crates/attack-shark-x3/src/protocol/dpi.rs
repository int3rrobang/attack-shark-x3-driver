use crate::{DpiValue, ProfileId, ProtocolError, StageIndex, TransportKind};

use super::checksum::sum16;

pub const DPI_REPORT_ID: u8 = 0x04;
pub const DPI_DECLARED_LENGTH: u8 = 0x38;
const DPI_RECEIVER_DECLARED_LENGTH: u8 = 0x3a;
pub const DPI_WIRED_LENGTH: usize = 52;
pub const DPI_RECEIVER_LENGTH: usize = 56;

const CHECKSUM_OFFSET: usize = 50;
const FIXED_TAIL_START: usize = 25;
const FIXED_TAIL_LENGTH: usize = 25;
const CAPTURED_EMPTY_PROFILE_TAIL: [u8; FIXED_TAIL_LENGTH] = [
    0xff, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0x00, 0x00, 0xff, 0xff, 0xff,
    0x00, 0xff, 0xff, 0x40, 0x00, 0xff, 0xff, 0xff, 0x01,
];
const CAPTURED_STOCK_RESET_STAGES: [u16; 6] = [800, 1600, 2400, 3200, 5000, 26000];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub enum LiftOffDistance {
    #[default]
    OneMillimeter,
    TwoMillimeters,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct SensorOptions {
    pub lift_off_distance: LiftOffDistance,
    pub ripple_control: bool,
    pub angle_snap: bool,
    pub motion_sync: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct DpiState {
    pub profile: ProfileId,
    pub stages: Vec<DpiValue>,
    pub active_stage: StageIndex,
    pub sensor: SensorOptions,
    /// Bytes 25..=49 have unresolved semantics and must survive read-modify-write.
    #[cfg_attr(feature = "serde", serde(with = "preserved_tail_hex"))]
    pub preserved_tail: [u8; FIXED_TAIL_LENGTH],
}

impl DpiState {
    /// Creates a DPI state while preserving the caller-supplied unresolved tail.
    ///
    /// # Errors
    ///
    /// Returns an error when the stage count is outside 1..=8 or `active_stage`
    /// does not refer to one of the configured stages.
    pub fn new(
        profile: ProfileId,
        stages: Vec<DpiValue>,
        active_stage: StageIndex,
        preserved_tail: [u8; FIXED_TAIL_LENGTH],
    ) -> Result<Self, ProtocolError> {
        validate_stage_selection(stages.len(), active_stage)?;
        Ok(Self {
            profile,
            stages,
            active_stage,
            sensor: SensorOptions::default(),
            preserved_tail,
        })
    }

    #[must_use]
    pub fn with_sensor(mut self, sensor: SensorOptions) -> Self {
        self.sensor = sensor;
        self
    }

    /// Reproduces the profile-1 DPI tail observed after the stock app applied
    /// a new empty profile.
    ///
    /// This is not established as a factory or firmware default and must not
    /// be used as the basis for updating an existing profile.
    ///
    /// # Errors
    ///
    /// Returns an error when the stage count is outside 1..=8 or `active_stage`
    /// does not refer to one of the configured stages.
    pub fn captured_empty_profile_one(
        stages: Vec<DpiValue>,
        active_stage: StageIndex,
    ) -> Result<Self, ProtocolError> {
        Self::new(
            ProfileId::try_from(ProfileId::MIN)?,
            stages,
            active_stage,
            CAPTURED_EMPTY_PROFILE_TAIL,
        )
    }

    /// Reproduces the complete DPI state written by the stock X3 reset flow,
    /// retargeted to `profile`.
    ///
    /// Source: `docs/evidence/x3-fa61/reset-packets.json` (`dpi.profile1`).
    ///
    /// # Errors
    ///
    /// Returns an error if the capture-qualified stage values or active stage
    /// cannot be represented by the protocol model.
    pub fn captured_stock_reset(profile: ProfileId) -> Result<Self, ProtocolError> {
        let stages = CAPTURED_STOCK_RESET_STAGES
            .into_iter()
            .map(DpiValue::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let active_stage = StageIndex::try_from(2)?;
        Self::new(profile, stages, active_stage, CAPTURED_EMPTY_PROFILE_TAIL)
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for DpiState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Helper {
            profile: ProfileId,
            stages: Vec<DpiValue>,
            active_stage: StageIndex,
            sensor: SensorOptions,
            #[serde(with = "preserved_tail_hex")]
            preserved_tail: [u8; FIXED_TAIL_LENGTH],
        }

        let helper = Helper::deserialize(deserializer)?;
        DpiState::new(
            helper.profile,
            helper.stages,
            helper.active_stage,
            helper.preserved_tail,
        )
        .map(|state| state.with_sensor(helper.sensor))
        .map_err(serde::de::Error::custom)
    }
}

/// Wire framing for a DPI report.
///
/// `Compact` (52 bytes) is the canonical write image for every transport
/// (FA61 wired, FA60 receiver, and BLE FEE3). `Full` (56 bytes) is the
/// normalized feature-report readback observed only on USB
/// (`hid_get_feature_report` on FA61 and the FA60 prepared-read path); it
/// appends four trailing zero bytes to the compact image. BLE writes must use
/// `Compact`; `Full` is a USB readback framing and is not advertised for BLE.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DpiFraming {
    /// 52-byte compact write image used for every transport.
    Compact,
    /// 56-byte USB readback image with four trailing zero bytes.
    Full,
}

impl DpiFraming {
    #[must_use]
    pub const fn transmitted_length(self) -> usize {
        match self {
            Self::Compact => DPI_WIRED_LENGTH,
            Self::Full => DPI_RECEIVER_LENGTH,
        }
    }

    /// Returns whether this framing is valid for `transport`.
    ///
    /// `Compact` is valid for every transport. `Full` is only valid for the
    /// USB transports (`Wired` and `Receiver`); BLE must use `Compact`.
    #[must_use]
    pub const fn supports_transport(self, transport: TransportKind) -> bool {
        match self {
            Self::Compact => true,
            Self::Full => match transport {
                TransportKind::Wired | TransportKind::Receiver => true,
                TransportKind::Ble => false,
            },
        }
    }

    /// Valid wire lengths for `transport` in preference order.
    #[must_use]
    pub const fn valid_lengths_for_transport(transport: TransportKind) -> &'static [usize] {
        match transport {
            TransportKind::Ble => &[DPI_WIRED_LENGTH],
            TransportKind::Wired | TransportKind::Receiver => {
                &[DPI_WIRED_LENGTH, DPI_RECEIVER_LENGTH]
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedDpiReport {
    pub state: DpiState,
    pub transport: TransportKind,
    pub framing: DpiFraming,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DpiReport {
    bytes: [u8; DPI_RECEIVER_LENGTH],
    transmitted_length: usize,
}

impl DpiReport {
    /// Encodes a model state with the write framing for `transport`.
    ///
    /// Writes are always `Compact` (52 bytes) regardless of transport; `Full`
    /// (56 bytes) is the USB readback framing and is not used for BLE writes.
    ///
    /// # Errors
    ///
    /// Returns an error when the stage count or active stage is invalid.
    pub fn encode(state: &DpiState, transport: TransportKind) -> Result<Self, ProtocolError> {
        let _ = transport;
        // Compact is valid for every transport; Full is USB-only and must not
        // be advertised for BLE. Keep writes compact for all transports.
        Self::encode_framed(state, DpiFraming::Compact)
    }

    /// Encodes a model state with an explicit compact or full framing.
    ///
    /// `Compact` is valid for every transport. `Full` is the USB readback
    /// framing (`Wired` and `Receiver`) and must not be used for BLE writes;
    /// callers that need transport-aware validation should check
    /// `framing.supports_transport(transport)` before encoding.
    ///
    /// # Errors
    ///
    /// Returns an error when the stage count or active stage is invalid.
    pub fn encode_framed(state: &DpiState, framing: DpiFraming) -> Result<Self, ProtocolError> {
        validate_stage_selection(state.stages.len(), state.active_stage)?;

        let mut bytes = [0_u8; DPI_RECEIVER_LENGTH];
        bytes[0] = DPI_REPORT_ID;
        bytes[1] = DPI_DECLARED_LENGTH;
        bytes[2] = state.profile.get();
        bytes[3] = match state.sensor.lift_off_distance {
            LiftOffDistance::OneMillimeter => 0,
            LiftOffDistance::TwoMillimeters => 1,
        };
        bytes[4] = u8::from(state.sensor.ripple_control);
        bytes[5] = enabled_stage_mask(state.stages.len())?;
        bytes[6] = u8::from(state.sensor.angle_snap);
        bytes[7] = u8::from(state.sensor.motion_sync);

        for (index, dpi) in state.stages.iter().copied().enumerate() {
            let raw = dpi.get() / 50 - 1;
            let [low, high] = raw.to_le_bytes();
            bytes[8 + index] = low;
            bytes[16 + index] = high;
        }

        bytes[24] = state.active_stage.get();
        bytes[FIXED_TAIL_START..CHECKSUM_OFFSET].copy_from_slice(&state.preserved_tail);
        let checksum = sum16(&bytes[3..CHECKSUM_OFFSET]);
        bytes[CHECKSUM_OFFSET..DPI_WIRED_LENGTH].copy_from_slice(&checksum.to_be_bytes());

        Ok(Self {
            bytes,
            transmitted_length: framing.transmitted_length(),
        })
    }
    /// Encodes with transport-aware framing validation.
    ///
    /// `Compact` is valid for every transport; `Full` is only valid for
    /// `Wired` and `Receiver`. Returns `InvalidReportLength` when the framing
    /// is not supported for `transport`, preventing a BLE caller from
    /// advertising an unsupported full readback framing.
    pub fn encode_framed_for_transport(
        state: &DpiState,
        transport: TransportKind,
        framing: DpiFraming,
    ) -> Result<Self, ProtocolError> {
        if !framing.supports_transport(transport) {
            return Err(ProtocolError::InvalidReportLength {
                expected: DpiFraming::Compact.transmitted_length(),
                actual: framing.transmitted_length(),
            });
        }
        Self::encode_framed(state, framing)
    }

    /// Decodes and validates a DPI report for an explicit target profile.
    ///
    /// FA61 writes are compact, while its feature-report readback is full.
    ///
    /// # Errors
    ///
    /// Returns a structured error for malformed framing, identity, profile,
    /// checksum, sensor fields, stages, or padding.
    pub fn decode(
        packet: &[u8],
        transport: TransportKind,
        expected_profile: ProfileId,
    ) -> Result<DecodedDpiReport, ProtocolError> {
        let framing = decode_framing(transport, packet.len())?;
        if packet[0] != DPI_REPORT_ID {
            return Err(ProtocolError::UnexpectedReportId {
                expected: DPI_REPORT_ID,
                actual: packet[0],
            });
        }
        let expected_declared_length = match transport {
            TransportKind::Receiver => DPI_RECEIVER_DECLARED_LENGTH,
            TransportKind::Wired | TransportKind::Ble => DPI_DECLARED_LENGTH,
        };
        let declared_length_is_valid = packet[1] == expected_declared_length
            || (transport == TransportKind::Receiver && packet[1] == DPI_DECLARED_LENGTH);
        if !declared_length_is_valid {
            return Err(ProtocolError::UnexpectedDeclaredLength {
                expected: expected_declared_length,
                actual: packet[1],
            });
        }
        if packet[2] != expected_profile.get() {
            return Err(ProtocolError::ProfileMismatch {
                expected: expected_profile.get(),
                actual: packet[2],
            });
        }
        if framing == DpiFraming::Full {
            validate_fixed_range(packet, DPI_WIRED_LENGTH, DPI_RECEIVER_LENGTH, 0)?;
        }

        let actual_checksum =
            u16::from_be_bytes([packet[CHECKSUM_OFFSET], packet[CHECKSUM_OFFSET + 1]]);
        let expected_checksum = sum16(&packet[3..CHECKSUM_OFFSET]);
        if actual_checksum != expected_checksum {
            return Err(ProtocolError::ChecksumMismatch {
                expected: expected_checksum,
                actual: actual_checksum,
            });
        }

        let sensor = SensorOptions {
            lift_off_distance: match packet[3] {
                0 => LiftOffDistance::OneMillimeter,
                1 => LiftOffDistance::TwoMillimeters,
                value => {
                    return Err(ProtocolError::InvalidSensorValue {
                        field: "lift_off_distance",
                        value,
                    });
                }
            },
            ripple_control: decode_toggle("ripple_control", packet[4])?,
            angle_snap: decode_toggle("angle_snap", packet[6])?,
            motion_sync: decode_toggle("motion_sync", packet[7])?,
        };

        let stage_count = decode_stage_count(packet[5])?;
        let mut stages = Vec::with_capacity(stage_count);
        for index in 0..8 {
            let raw = u16::from(packet[8 + index]) | (u16::from(packet[16 + index]) << 8);
            if index >= stage_count {
                if raw != 0 {
                    return Err(ProtocolError::InvalidStageValue {
                        stage: index + 1,
                        value: raw,
                    });
                }
                continue;
            }
            if raw > 519 {
                return Err(ProtocolError::InvalidStageValue {
                    stage: index + 1,
                    value: raw,
                });
            }
            stages.push(DpiValue::try_from((raw + 1) * 50)?);
        }

        let active_stage = StageIndex::try_from(packet[24])?;
        validate_stage_selection(stages.len(), active_stage)?;
        let mut preserved_tail = [0_u8; FIXED_TAIL_LENGTH];
        preserved_tail.copy_from_slice(&packet[FIXED_TAIL_START..CHECKSUM_OFFSET]);

        Ok(DecodedDpiReport {
            state: DpiState {
                profile: expected_profile,
                stages,
                active_stage,
                sensor,
                preserved_tail,
            },
            transport,
            framing,
        })
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.transmitted_length]
    }

    #[must_use]
    pub const fn as_full_bytes(&self) -> &[u8; DPI_RECEIVER_LENGTH] {
        &self.bytes
    }
}

fn decode_framing(transport: TransportKind, actual: usize) -> Result<DpiFraming, ProtocolError> {
    match (transport, actual) {
        (TransportKind::Wired | TransportKind::Ble | TransportKind::Receiver, DPI_WIRED_LENGTH) => {
            Ok(DpiFraming::Compact)
        }
        (TransportKind::Wired | TransportKind::Receiver, DPI_RECEIVER_LENGTH) => {
            Ok(DpiFraming::Full)
        }
        _ => Err(ProtocolError::InvalidReportLength {
            expected: expected_length_for_error(transport, actual),
            actual,
        }),
    }
}

const fn expected_length_for_error(transport: TransportKind, actual: usize) -> usize {
    match transport {
        TransportKind::Ble => DPI_WIRED_LENGTH,
        TransportKind::Wired | TransportKind::Receiver => {
            if actual == DPI_WIRED_LENGTH || actual == DPI_RECEIVER_LENGTH {
                DPI_WIRED_LENGTH
            } else if actual < DPI_WIRED_LENGTH {
                DPI_WIRED_LENGTH
            } else if actual < DPI_RECEIVER_LENGTH {
                DPI_RECEIVER_LENGTH
            } else {
                DPI_RECEIVER_LENGTH
            }
        }
    }
}

fn enabled_stage_mask(stage_count: usize) -> Result<u8, ProtocolError> {
    if !(1..=8).contains(&stage_count) {
        return Err(ProtocolError::InvalidStageCount { count: stage_count });
    }
    Ok(if stage_count == 8 {
        u8::MAX
    } else {
        (1_u8 << stage_count) - 1
    })
}

fn decode_stage_count(mask: u8) -> Result<usize, ProtocolError> {
    let count = mask.count_ones() as usize;
    if count == 0 || enabled_stage_mask(count)? != mask {
        return Err(ProtocolError::InvalidStageMask { mask });
    }
    Ok(count)
}

fn validate_stage_selection(
    stage_count: usize,
    active_stage: StageIndex,
) -> Result<(), ProtocolError> {
    if !(1..=8).contains(&stage_count) {
        return Err(ProtocolError::InvalidStageCount { count: stage_count });
    }
    if usize::from(active_stage.get()) > stage_count {
        return Err(ProtocolError::ActiveStageOutOfRange {
            active: active_stage.get(),
            count: stage_count,
        });
    }
    Ok(())
}

fn decode_toggle(field: &'static str, value: u8) -> Result<bool, ProtocolError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        value => Err(ProtocolError::InvalidSensorValue { field, value }),
    }
}

fn validate_fixed_range(
    packet: &[u8],
    start: usize,
    end: usize,
    expected: u8,
) -> Result<(), ProtocolError> {
    crate::protocol::profile::validate_fixed_range(packet, start, end, expected)
}

#[cfg(all(test, feature = "serde"))]
mod dpi_state_serde_tests {
    use super::DpiState;
    use crate::{DpiValue, ProfileId, StageIndex};

    fn sample_tail() -> [u8; 25] {
        [
            0xff, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0x00, 0x00, 0xff,
            0xff, 0xff, 0x00, 0xff, 0xff, 0x40, 0x00, 0xff, 0xff, 0xff, 0x01,
        ]
    }

    fn valid_state() -> DpiState {
        let stages = vec![
            DpiValue::try_from(800).unwrap(),
            DpiValue::try_from(1600).unwrap(),
            DpiValue::try_from(2400).unwrap(),
        ];
        DpiState::new(
            ProfileId::try_from(1).unwrap(),
            stages,
            StageIndex::try_from(2).unwrap(),
            sample_tail(),
        )
        .unwrap()
    }

    #[test]
    fn dpi_state_round_trip_preserves_shape() {
        let state = valid_state();
        let json = serde_json::to_string(&state).unwrap();
        // ensure shape contains expected keys and integer values
        assert!(json.contains("\"profile\":1"));
        assert!(json.contains("\"stages\":[800,1600,2400]"));
        assert!(json.contains("\"activeStage\":2"));
        assert!(json.contains("\"preservedTail\":\""));
        let restored: DpiState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, restored);
    }

    #[test]
    fn dpi_state_rejects_invalid_dpi_in_stages() {
        // misaligned DPI 51 should be rejected via DpiValue deserialization
        let json = r#"{"profile":1,"stages":[51],"activeStage":1,"sensor":{"liftOffDistance":"oneMillimeter","rippleControl":false,"angleSnap":false,"motionSync":false},"preservedTail":"ff000000ff000000ffffff0000ffffff00ffff4000ffffff01"}"#;
        assert!(serde_json::from_str::<DpiState>(json).is_err());
        let json2 = r#"{"profile":1,"stages":[800,26001],"activeStage":1,"sensor":{"liftOffDistance":"oneMillimeter","rippleControl":false,"angleSnap":false,"motionSync":false},"preservedTail":"ff000000ff000000ffffff0000ffffff00ffff4000ffffff01"}"#;
        assert!(serde_json::from_str::<DpiState>(json2).is_err());
    }

    #[test]
    fn dpi_state_rejects_invalid_profile() {
        let json = r#"{"profile":0,"stages":[800],"activeStage":1,"sensor":{"liftOffDistance":"oneMillimeter","rippleControl":false,"angleSnap":false,"motionSync":false},"preservedTail":"ff000000ff000000ffffff0000ffffff00ffff4000ffffff01"}"#;
        assert!(serde_json::from_str::<DpiState>(json).is_err());
        let json2 = r#"{"profile":6,"stages":[800],"activeStage":1,"sensor":{"liftOffDistance":"oneMillimeter","rippleControl":false,"angleSnap":false,"motionSync":false},"preservedTail":"ff000000ff000000ffffff0000ffffff00ffff4000ffffff01"}"#;
        assert!(serde_json::from_str::<DpiState>(json2).is_err());
    }

    #[test]
    fn dpi_state_rejects_active_stage_out_of_range() {
        let json = r#"{"profile":1,"stages":[800,1600],"activeStage":3,"sensor":{"liftOffDistance":"oneMillimeter","rippleControl":false,"angleSnap":false,"motionSync":false},"preservedTail":"ff000000ff000000ffffff0000ffffff00ffff4000ffffff01"}"#;
        assert!(serde_json::from_str::<DpiState>(json).is_err());
    }

    #[test]
    fn dpi_state_rejects_empty_stages() {
        let json = r#"{"profile":1,"stages":[],"activeStage":1,"sensor":{"liftOffDistance":"oneMillimeter","rippleControl":false,"angleSnap":false,"motionSync":false},"preservedTail":"ff000000ff000000ffffff0000ffffff00ffff4000ffffff01"}"#;
        assert!(serde_json::from_str::<DpiState>(json).is_err());
    }

    #[test]
    fn dpi_state_rejects_too_many_stages() {
        let json = r#"{"profile":1,"stages":[800,800,800,800,800,800,800,800,800],"activeStage":1,"sensor":{"liftOffDistance":"oneMillimeter","rippleControl":false,"angleSnap":false,"motionSync":false},"preservedTail":"ff000000ff000000ffffff0000ffffff00ffff4000ffffff01"}"#;
        assert!(serde_json::from_str::<DpiState>(json).is_err());
    }

    #[test]
    fn dpi_state_rejects_invalid_stage_index() {
        let json = r#"{"profile":1,"stages":[800],"activeStage":0,"sensor":{"liftOffDistance":"oneMillimeter","rippleControl":false,"angleSnap":false,"motionSync":false},"preservedTail":"ff000000ff000000ffffff0000ffffff00ffff4000ffffff01"}"#;
        assert!(serde_json::from_str::<DpiState>(json).is_err());
        let json2 = r#"{"profile":1,"stages":[800],"activeStage":9,"sensor":{"liftOffDistance":"oneMillimeter","rippleControl":false,"angleSnap":false,"motionSync":false},"preservedTail":"ff000000ff000000ffffff0000ffffff00ffff4000ffffff01"}"#;
        assert!(serde_json::from_str::<DpiState>(json2).is_err());
    }
}

#[cfg(feature = "serde")]
mod preserved_tail_hex {
    use serde::{Deserializer, Serializer};

    const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";

    pub fn serialize<S: Serializer>(bytes: &[u8; 25], serializer: S) -> Result<S::Ok, S::Error> {
        let mut buf = [0_u8; 50];
        for (i, &byte) in bytes.iter().enumerate() {
            buf[i * 2] = HEX_CHARS[(byte >> 4) as usize];
            buf[i * 2 + 1] = HEX_CHARS[(byte & 0x0f) as usize];
        }
        let s = std::str::from_utf8(&buf).expect("hex output is always valid UTF-8");
        serializer.serialize_str(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 25], D::Error> {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = [u8; 25];

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a string of exactly 50 hexadecimal characters")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                if v.len() != 50 {
                    return Err(E::invalid_length(v.len(), &"50 hexadecimal characters"));
                }
                let mut out = [0_u8; 25];
                for (i, byte) in out.iter_mut().enumerate() {
                    let hi = hex_nibble(v.as_bytes()[i * 2]).map_err(E::custom)?;
                    let lo = hex_nibble(v.as_bytes()[i * 2 + 1]).map_err(E::custom)?;
                    *byte = (hi << 4) | lo;
                }
                Ok(out)
            }
        }

        deserializer.deserialize_str(Visitor)
    }

    fn hex_nibble(c: u8) -> Result<u8, String> {
        match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            b'A'..=b'F' => Ok(c - b'A' + 10),
            _ => Err(format!("invalid hex character: {}", c as char)),
        }
    }
}

#[cfg(all(test, feature = "serde"))]
mod preserved_tail_hex_tests {
    use super::preserved_tail_hex;

    const TAIL: [u8; 25] = [
        0xff, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0x00, 0x00, 0xff, 0xff,
        0xff, 0x00, 0xff, 0xff, 0x40, 0x00, 0xff, 0xff, 0xff, 0x01,
    ];

    #[test]
    fn serializes_as_50_lowercase_hex_chars() {
        let json = serde_json::to_string(&Tail(TAIL)).unwrap();
        let inner: String = serde_json::from_str(&json).unwrap();
        assert_eq!(inner.len(), 50);
        assert_eq!(inner, "ff000000ff000000ffffff0000ffffff00ffff4000ffffff01");
        assert_eq!(inner, inner.to_lowercase());
    }

    #[test]
    fn deserializes_uppercase_input() {
        let json = "\"FF000000FF000000FFFFFF0000FFFFFF00FFFF4000FFFFFF01\"";
        let Tail(out) = serde_json::from_str(json).unwrap();
        assert_eq!(out, TAIL);
    }

    #[test]
    fn rejects_wrong_length() {
        let result = serde_json::from_str::<Tail>("\"ff00\"");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_invalid_hex_character() {
        let bad = "zz000000ff000000ffffff0000ffffff00ffff4000ffffff01";
        let result = serde_json::from_str::<Tail>(&format!("\"{bad}\""));
        assert!(result.is_err());
    }

    #[test]
    fn rejects_non_string() {
        let result = serde_json::from_str::<Tail>("[1,2,3]");
        assert!(result.is_err());
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(transparent)]
    struct Tail(#[serde(with = "preserved_tail_hex")] [u8; 25]);
}

#[cfg(test)]
mod framing_tests {
    use super::*;
    use crate::{DpiValue, ProfileId, StageIndex, TransportKind};

    fn state() -> DpiState {
        let stages = vec![DpiValue::try_from(800).unwrap()];
        DpiState::new(
            ProfileId::try_from(1).unwrap(),
            stages,
            StageIndex::try_from(1).unwrap(),
            [
                0xff, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0x00, 0x00, 0xff,
                0xff, 0xff, 0x00, 0xff, 0xff, 0x40, 0x00, 0xff, 0xff, 0xff, 0x01,
            ],
        )
        .unwrap()
    }

    #[test]
    fn compact_is_valid_for_every_transport() {
        for transport in [
            TransportKind::Wired,
            TransportKind::Ble,
            TransportKind::Receiver,
        ] {
            assert!(DpiFraming::Compact.supports_transport(transport));
            assert_eq!(DpiFraming::Compact.transmitted_length(), DPI_WIRED_LENGTH);
            let report = DpiReport::encode_framed(&state(), DpiFraming::Compact).unwrap();
            assert_eq!(report.as_bytes().len(), DPI_WIRED_LENGTH);
            let decoded = DpiReport::decode(
                report.as_bytes(),
                transport,
                ProfileId::try_from(1).unwrap(),
            )
            .unwrap();
            assert_eq!(decoded.framing, DpiFraming::Compact);
            assert_eq!(decoded.transport, transport);
        }
    }

    #[test]
    fn full_is_only_valid_for_usb_transports() {
        assert!(DpiFraming::Full.supports_transport(TransportKind::Wired));
        assert!(DpiFraming::Full.supports_transport(TransportKind::Receiver));
        assert!(!DpiFraming::Full.supports_transport(TransportKind::Ble));
        assert_eq!(DpiFraming::Full.transmitted_length(), DPI_RECEIVER_LENGTH);

        let full = DpiReport::encode_framed(&state(), DpiFraming::Full).unwrap();
        assert_eq!(full.as_bytes().len(), DPI_RECEIVER_LENGTH);
        // Full decodes for Wired and Receiver but not for Ble.
        assert!(
            DpiReport::decode(
                full.as_bytes(),
                TransportKind::Wired,
                ProfileId::try_from(1).unwrap()
            )
            .is_ok()
        );
        assert!(
            DpiReport::decode(
                full.as_bytes(),
                TransportKind::Receiver,
                ProfileId::try_from(1).unwrap()
            )
            .is_ok()
        );
        assert!(matches!(
            DpiReport::decode(
                full.as_bytes(),
                TransportKind::Ble,
                ProfileId::try_from(1).unwrap()
            ),
            Err(ProtocolError::InvalidReportLength { .. })
        ));
    }

    #[test]
    fn encode_for_transport_prevents_ble_full() {
        assert!(matches!(
            DpiReport::encode_framed_for_transport(&state(), TransportKind::Ble, DpiFraming::Full),
            Err(ProtocolError::InvalidReportLength { .. })
        ));
        assert!(
            DpiReport::encode_framed_for_transport(
                &state(),
                TransportKind::Ble,
                DpiFraming::Compact
            )
            .is_ok()
        );
        assert!(
            DpiReport::encode_framed_for_transport(
                &state(),
                TransportKind::Wired,
                DpiFraming::Full
            )
            .is_ok()
        );
    }

    #[test]
    fn malformed_lengths_report_truthful_expected() {
        // Ble only supports 52, so any other length reports 52.
        let short = vec![0u8; 51];
        assert_eq!(
            DpiReport::decode(&short, TransportKind::Ble, ProfileId::try_from(1).unwrap()),
            Err(ProtocolError::InvalidReportLength {
                expected: DPI_WIRED_LENGTH,
                actual: 51
            })
        );
        // Wired: 53 should hint at Full (56) rather than always Compact.
        let mid = vec![0u8; 53];
        assert_eq!(
            DpiReport::decode(&mid, TransportKind::Wired, ProfileId::try_from(1).unwrap()),
            Err(ProtocolError::InvalidReportLength {
                expected: DPI_RECEIVER_LENGTH,
                actual: 53
            })
        );
        let long = vec![0u8; 57];
        assert_eq!(
            DpiReport::decode(&long, TransportKind::Wired, ProfileId::try_from(1).unwrap()),
            Err(ProtocolError::InvalidReportLength {
                expected: DPI_RECEIVER_LENGTH,
                actual: 57
            })
        );
    }

    #[test]
    fn valid_compact_and_full_usb_cases_remain_accepted() {
        let compact = DpiReport::encode(&state(), TransportKind::Wired).unwrap();
        assert_eq!(compact.as_bytes().len(), DPI_WIRED_LENGTH);
        assert!(
            DpiReport::decode(
                compact.as_bytes(),
                TransportKind::Wired,
                ProfileId::try_from(1).unwrap()
            )
            .is_ok()
        );
        assert!(
            DpiReport::decode(
                compact.as_bytes(),
                TransportKind::Receiver,
                ProfileId::try_from(1).unwrap()
            )
            .is_ok()
        );

        let full = DpiReport::encode_framed(&state(), DpiFraming::Full).unwrap();
        assert!(
            DpiReport::decode(
                full.as_bytes(),
                TransportKind::Wired,
                ProfileId::try_from(1).unwrap()
            )
            .is_ok()
        );
        assert!(
            DpiReport::decode(
                full.as_bytes(),
                TransportKind::Receiver,
                ProfileId::try_from(1).unwrap()
            )
            .is_ok()
        );
    }
}
