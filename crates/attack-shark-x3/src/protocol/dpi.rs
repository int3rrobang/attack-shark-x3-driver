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
/// Length of the driver-owned physical identity watermark, which occupies the
/// whole opaque DPI tail (report bytes 25..=49).
pub const WATERMARK_LENGTH: usize = FIXED_TAIL_LENGTH;
/// Magic prefix of the watermark: `X3ID` at bytes 0..=3.
const WATERMARK_MAGIC: [u8; 4] = *b"X3ID";
/// Current watermark format version, stored at byte 4.
const WATERMARK_VERSION: u8 = 1;
const WATERMARK_VERSION_OFFSET: usize = 4;
/// Random 128-bit device token stored at bytes 5..=20.
const WATERMARK_TOKEN_LENGTH: usize = 16;
const WATERMARK_TOKEN_START: usize = WATERMARK_VERSION_OFFSET + 1;
const WATERMARK_TOKEN_END: usize = WATERMARK_TOKEN_START + WATERMARK_TOKEN_LENGTH;
/// CRC-32 (IEEE, as computed by `crc32fast`) of bytes 0..=20, stored
/// big-endian at bytes 21..=24. A corruption check, not authentication.
const WATERMARK_CRC_START: usize = WATERMARK_TOKEN_END;
const WATERMARK_CRC_LENGTH: usize = 4;
const _: () = assert!(WATERMARK_CRC_START + WATERMARK_CRC_LENGTH == WATERMARK_LENGTH);

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
/// Opaque physical-mouse identity carried by the DPI watermark.
///
/// The type is opaque: callers construct it from the raw 128-bit token bytes
/// ([`PhysicalId::from_token_bytes`]) and read them back
/// ([`PhysicalId::as_bytes`] / [`PhysicalId::token_bytes`]), but never reach
/// into the wire encoding. The 25-byte watermark image is produced by
/// [`PhysicalId::to_watermark_bytes`]. Under the `serde` feature the type
/// serializes as a lowercase hexadecimal string of the token bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PhysicalId {
    token: [u8; WATERMARK_TOKEN_LENGTH],
}

impl PhysicalId {
    /// Wraps a caller-supplied 128-bit random token.
    #[must_use]
    pub const fn from_token_bytes(token: [u8; WATERMARK_TOKEN_LENGTH]) -> Self {
        Self { token }
    }

    /// Returns the raw 128-bit token bytes.
    #[must_use]
    pub const fn token_bytes(self) -> [u8; WATERMARK_TOKEN_LENGTH] {
        self.token
    }

    /// Borrows the raw 128-bit token bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; WATERMARK_TOKEN_LENGTH] {
        &self.token
    }

    /// Encodes the full 25-byte watermark image: magic `X3ID`, format
    /// version `1`, the 128-bit token, and the CRC-32 of the preceding
    /// 21 bytes (big-endian).
    #[must_use]
    pub fn to_watermark_bytes(self) -> [u8; WATERMARK_LENGTH] {
        let mut bytes = [0_u8; WATERMARK_LENGTH];
        bytes[..WATERMARK_MAGIC.len()].copy_from_slice(&WATERMARK_MAGIC);
        bytes[WATERMARK_VERSION_OFFSET] = WATERMARK_VERSION;
        bytes[WATERMARK_TOKEN_START..WATERMARK_TOKEN_END].copy_from_slice(&self.token);
        let crc = crc32fast::hash(&bytes[..WATERMARK_CRC_START]);
        bytes[WATERMARK_CRC_START..].copy_from_slice(&crc.to_be_bytes());
        bytes
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for PhysicalId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        physical_id_hex::serialize(&self.token, serializer)
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for PhysicalId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let token = physical_id_hex::deserialize(deserializer)?;
        Ok(Self { token })
    }
}

/// Result of strictly decoding a 25-byte DPI-tail watermark.
///
/// Every possible 25-byte tail maps to exactly one variant: a tail that does
/// not begin with the `X3ID` magic is [`Absent`](Self::Absent); a recognized
/// magic with an unsupported format version is
/// [`UnsupportedVersion`](Self::UnsupportedVersion); a version-1 tail with a
/// failed integrity check is [`Malformed`](Self::Malformed); a fully
/// validated tail is [`Valid`](Self::Valid). A token is only ever produced
/// by [`Valid`](Self::Valid) — malformed or unknown input never yields
/// identity bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WatermarkDecode {
    /// The tail does not begin with the `X3ID` magic: no watermark present.
    Absent,
    /// The tail begins with the magic but fails validation (bad integrity
    /// check or truncated token).
    Malformed,
    /// The tail carries a recognized magic with a format version this driver
    /// does not support.
    UnsupportedVersion { version: u8 },
    /// A valid physical identity.
    Valid(PhysicalId),
}

impl WatermarkDecode {
    /// Returns the decoded identity when the status is [`Valid`](Self::Valid).
    #[must_use]
    pub const fn physical_id(self) -> Option<PhysicalId> {
        match self {
            Self::Valid(id) => Some(id),
            Self::Absent | Self::Malformed | Self::UnsupportedVersion { .. } => None,
        }
    }
}

/// Strictly decodes a 25-byte DPI-tail watermark.
///
/// `tail` is the opaque tail of a DPI `0x04` report (report bytes 25..=49).
/// Wrong-length input is treated as absent. See [`WatermarkDecode`] for the
/// status semantics.
#[must_use]
pub fn decode_watermark(tail: &[u8]) -> WatermarkDecode {
    if tail.len() != WATERMARK_LENGTH {
        return WatermarkDecode::Absent;
    }
    if tail[..WATERMARK_MAGIC.len()] != WATERMARK_MAGIC {
        return WatermarkDecode::Absent;
    }
    if tail[WATERMARK_VERSION_OFFSET] != WATERMARK_VERSION {
        return WatermarkDecode::UnsupportedVersion {
            version: tail[WATERMARK_VERSION_OFFSET],
        };
    }
    let expected = crc32fast::hash(&tail[..WATERMARK_CRC_START]);
    let actual = u32::from_be_bytes([
        tail[WATERMARK_CRC_START],
        tail[WATERMARK_CRC_START + 1],
        tail[WATERMARK_CRC_START + 2],
        tail[WATERMARK_CRC_START + 3],
    ]);
    if actual != expected {
        return WatermarkDecode::Malformed;
    }
    let mut token = [0_u8; WATERMARK_TOKEN_LENGTH];
    token.copy_from_slice(&tail[WATERMARK_TOKEN_START..WATERMARK_TOKEN_END]);
    WatermarkDecode::Valid(PhysicalId { token })
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

    /// Strictly decodes the physical identity watermark carried by the opaque
    /// tail.
    ///
    /// Legacy single-mouse tails (for example the captured stock bytes) decode
    /// as [`WatermarkDecode::Absent`]; see [`WatermarkDecode`] for the full
    /// status semantics.
    #[must_use]
    pub fn physical_id(&self) -> WatermarkDecode {
        decode_watermark(&self.preserved_tail)
    }

    /// Overlays `id`'s watermark onto the opaque tail, replacing any previous
    /// tail bytes with the strict DPI-tail watermark encoding.
    ///
    /// Identity metadata is not transferable DPI configuration: only overlay a
    /// watermark that belongs to the physical mouse this state will be written
    /// to. The outer report checksum is recomputed by [`DpiReport::encode`] on
    /// the next encode.
    pub fn overlay_physical_id(&mut self, id: PhysicalId) {
        self.preserved_tail = id.to_watermark_bytes();
    }

    /// Builder-style variant of [`DpiState::overlay_physical_id`].
    #[must_use]
    pub fn with_physical_id(mut self, id: PhysicalId) -> Self {
        self.overlay_physical_id(id);
        self
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
        bytes[5] = enabled_stage_mask_unchecked(state.stages.len());
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
        TransportKind::Wired | TransportKind::Receiver => match actual {
            0..=DPI_WIRED_LENGTH => DPI_WIRED_LENGTH,
            _ => DPI_RECEIVER_LENGTH,
        },
    }
}

fn enabled_stage_mask_unchecked(stage_count: usize) -> u8 {
    debug_assert!((1..=8).contains(&stage_count));
    if stage_count == 8 {
        u8::MAX
    } else {
        (1_u8 << stage_count) - 1
    }
}

fn decode_stage_count(mask: u8) -> Result<usize, ProtocolError> {
    let count = mask.count_ones() as usize;
    if count == 0 || enabled_stage_mask_unchecked(count) != mask {
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
fn hex_nibble(c: u8) -> Result<u8, String> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(format!("invalid hex character: {}", c as char)),
    }
}

#[cfg(feature = "serde")]
mod preserved_tail_hex {
    use super::hex_nibble;
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
#[cfg(feature = "serde")]
mod physical_id_hex {
    use super::{WATERMARK_TOKEN_LENGTH, hex_nibble};
    use serde::{Deserializer, Serializer};

    const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";

    pub fn serialize<S: Serializer>(
        bytes: &[u8; WATERMARK_TOKEN_LENGTH],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut buf = [0_u8; WATERMARK_TOKEN_LENGTH * 2];
        for (i, &byte) in bytes.iter().enumerate() {
            buf[i * 2] = HEX_CHARS[(byte >> 4) as usize];
            buf[i * 2 + 1] = HEX_CHARS[(byte & 0x0f) as usize];
        }
        let s = std::str::from_utf8(&buf).expect("hex output is always valid UTF-8");
        serializer.serialize_str(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<[u8; WATERMARK_TOKEN_LENGTH], D::Error> {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = [u8; WATERMARK_TOKEN_LENGTH];

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(
                    f,
                    "a string of exactly {} hexadecimal characters",
                    WATERMARK_TOKEN_LENGTH * 2
                )
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                if v.len() != WATERMARK_TOKEN_LENGTH * 2 {
                    return Err(E::invalid_length(v.len(), &"32 hexadecimal characters"));
                }
                let mut out = [0_u8; WATERMARK_TOKEN_LENGTH];
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
}

#[cfg(all(test, feature = "serde"))]
mod physical_id_hex_tests {
    use super::PhysicalId;

    const TOKEN: [u8; 16] = [
        0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x0f, 0x1e, 0x2d, 0x3c, 0x4b, 0x5a, 0x69,
        0x78,
    ];

    #[test]
    fn serializes_as_32_lowercase_hex_chars() {
        let json = serde_json::to_string(&PhysicalId::from_token_bytes(TOKEN)).unwrap();
        let inner: String = serde_json::from_str(&json).unwrap();
        assert_eq!(inner.len(), 32);
        assert_eq!(inner, "123456789abcdef00f1e2d3c4b5a6978");
        assert_eq!(inner, inner.to_lowercase());
    }

    #[test]
    fn deserializes_uppercase_input() {
        let id: PhysicalId = serde_json::from_str("\"123456789ABCDEF00F1E2D3C4B5A6978\"").unwrap();
        assert_eq!(id, PhysicalId::from_token_bytes(TOKEN));
    }

    #[test]
    fn round_trips_through_json() {
        let id = PhysicalId::from_token_bytes(TOKEN);
        let json = serde_json::to_string(&id).unwrap();
        let restored: PhysicalId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, restored);
    }

    #[test]
    fn rejects_wrong_length() {
        let result = serde_json::from_str::<PhysicalId>("\"ff00\"");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_invalid_hex_character() {
        let bad = "zz3456789abcdef00f1e2d3c4b5a6978";
        let result = serde_json::from_str::<PhysicalId>(&format!("\"{bad}\""));
        assert!(result.is_err());
    }

    #[test]
    fn rejects_non_string() {
        let result = serde_json::from_str::<PhysicalId>("[1,2,3]");
        assert!(result.is_err());
    }
}
#[cfg(test)]
mod physical_id_tests {
    use super::PhysicalId;

    #[test]
    fn ordering_follows_token_bytes() {
        let low = PhysicalId::from_token_bytes([0x00; 16]);
        let high = PhysicalId::from_token_bytes([0x01; 16]);
        assert!(low < high);
        assert_eq!(low, low.clone());

        // Lexicographic over the whole 16-byte token, last byte included.
        let mut a = [0x00; 16];
        let mut b = [0x00; 16];
        a[15] = 0x00;
        b[15] = 0x01;
        assert!(PhysicalId::from_token_bytes(a) < PhysicalId::from_token_bytes(b));
    }
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
