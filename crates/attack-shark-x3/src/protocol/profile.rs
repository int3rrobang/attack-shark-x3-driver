use crate::{ProfileId, ProtocolError};

pub const PROFILE_REPORT_ID: u8 = 0x0c;
pub const PROFILE_DECLARED_LENGTH: u8 = 0x0a;
const PROFILE_RECEIVER_DECLARED_LENGTH: u8 = 0x0c;
pub const PROFILE_CONTROL_COMPACT_LENGTH: usize = 6;
pub const PROFILE_REPORT_LENGTH: usize = 10;
pub const READ_SELECTOR_REPORT_ID: u8 = 0xa0;
pub const READ_SELECTOR_LENGTH: usize = 8;

const PROFILE_METADATA_SUBTYPE: u8 = 0x01;
const OBSERVED_UNTARGETED_PARAMETER: u8 = 0x01;

/// Persistent profile metadata reported by report `0x0c`.
///
/// This state is independent of whichever profile-backed section was most
/// recently loaded into the device's working buffers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ProfileMetadata {
    current: ProfileId,
    maximum: ProfileId,
}

impl ProfileMetadata {
    /// Creates metadata satisfying `current <= maximum`.
    ///
    /// # Errors
    ///
    /// Returns an error when the current profile is greater than the maximum
    /// enabled profile.
    pub fn new(current: ProfileId, maximum: ProfileId) -> Result<Self, ProtocolError> {
        if current.get() > maximum.get() {
            return Err(ProtocolError::InvalidProfileRange {
                current: current.get(),
                maximum: maximum.get(),
            });
        }
        Ok(Self { current, maximum })
    }

    #[must_use]
    pub const fn current(self) -> ProfileId {
        self.current
    }

    #[must_use]
    pub const fn maximum(self) -> ProfileId {
        self.maximum
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for ProfileMetadata {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct Helper {
            current: ProfileId,
            maximum: ProfileId,
        }

        let helper = Helper::deserialize(deserializer)?;
        ProfileMetadata::new(helper.current, helper.maximum).map_err(serde::de::Error::custom)
    }
}

/// Wire framing for a profile-control write.
///
/// `Compact` (6 bytes) is the FA61 wired write image. `Full` (10 bytes) is
/// the normalized ten-byte report image with four trailing zero bytes observed
/// on USB; BLE writes must use `Compact`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileControlFraming {
    /// Six-byte FA61 write observed on the wired transport.
    Compact,
    /// Ten-byte report image with four trailing zero bytes.
    Full,
}

impl ProfileControlFraming {
    #[must_use]
    pub const fn transmitted_length(self) -> usize {
        match self {
            Self::Compact => PROFILE_CONTROL_COMPACT_LENGTH,
            Self::Full => PROFILE_REPORT_LENGTH,
        }
    }

    /// Returns whether this framing is valid for `transport`.
    ///
    /// `Compact` is valid for every transport. `Full` is only valid for USB
    /// transports (`Wired` and `Receiver`); BLE must use `Compact`.
    #[must_use]
    pub const fn supports_transport(self, transport: crate::TransportKind) -> bool {
        match self {
            Self::Compact => true,
            Self::Full => match transport {
                crate::TransportKind::Wired | crate::TransportKind::Receiver => true,
                crate::TransportKind::Ble => false,
            },
        }
    }

    /// Valid wire lengths for `transport`.
    #[must_use]
    pub const fn valid_lengths_for_transport(transport: crate::TransportKind) -> &'static [usize] {
        match transport {
            crate::TransportKind::Ble => &[PROFILE_CONTROL_COMPACT_LENGTH],
            crate::TransportKind::Wired | crate::TransportKind::Receiver => {
                &[PROFILE_CONTROL_COMPACT_LENGTH, PROFILE_REPORT_LENGTH]
            }
        }
    }
}

/// Edge-triggered profile current/maximum control report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileControlReport {
    bytes: [u8; PROFILE_REPORT_LENGTH],
    transmitted_length: usize,
}

impl ProfileControlReport {
    /// Encodes metadata with an explicit framing.
    ///
    /// `Compact` is valid for every transport. `Full` is the USB ten-byte
    /// image and must not be used for BLE writes; callers needing
    /// transport-aware validation should check
    /// `framing.supports_transport(transport)` or use
    /// `encode_for_transport`.
    #[must_use]
    pub fn encode(metadata: ProfileMetadata, framing: ProfileControlFraming) -> Self {
        let current = metadata.current.get();
        let maximum = metadata.maximum.get();
        let mut bytes = [0_u8; PROFILE_REPORT_LENGTH];
        bytes[..PROFILE_CONTROL_COMPACT_LENGTH].copy_from_slice(&[
            PROFILE_REPORT_ID,
            PROFILE_DECLARED_LENGTH,
            current,
            !current,
            maximum,
            !maximum,
        ]);
        Self {
            bytes,
            transmitted_length: framing.transmitted_length(),
        }
    }

    /// Encodes with transport-aware framing validation.
    ///
    /// Returns an error when the framing is not supported for `transport`
    /// (e.g. `Full` for `Ble`).
    pub fn encode_for_transport(
        metadata: ProfileMetadata,
        transport: crate::TransportKind,
        framing: ProfileControlFraming,
    ) -> Result<Self, ProtocolError> {
        if !framing.supports_transport(transport) {
            return Err(ProtocolError::InvalidReportLength {
                expected: ProfileControlFraming::Compact.transmitted_length(),
                actual: framing.transmitted_length(),
            });
        }
        Ok(Self::encode(metadata, framing))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.transmitted_length]
    }
}

/// Validated normalized profile-metadata readback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileMetadataReport {
    pub metadata: ProfileMetadata,
    bytes: [u8; PROFILE_REPORT_LENGTH],
}

impl ProfileMetadataReport {
    /// Decodes and validates a canonical ten-byte report-`0x0c` metadata readback.
    ///
    /// # Errors
    ///
    /// Rejects wrong lengths, headers, subtype, complement pairs, profile
    /// ranges, and nonzero reserved bytes.
    pub fn decode(packet: &[u8]) -> Result<Self, ProtocolError> {
        Self::decode_with_declared_length(packet, PROFILE_DECLARED_LENGTH)
    }

    /// Decodes a profile-metadata readback using the selected transport dialect.
    ///
    /// FA60 receiver readbacks declare `0x0c`; wired and BLE packets use the
    /// canonical `0x0a` declaration. Both report images remain ten bytes long.
    ///
    /// # Errors
    ///
    /// Rejects wrong lengths, headers, subtype, complement pairs, profile
    /// ranges, and nonzero reserved bytes.
    pub fn decode_for_transport(
        packet: &[u8],
        transport: crate::TransportKind,
    ) -> Result<Self, ProtocolError> {
        let declared_length = match transport {
            crate::TransportKind::Receiver => PROFILE_RECEIVER_DECLARED_LENGTH,
            crate::TransportKind::Wired | crate::TransportKind::Ble => PROFILE_DECLARED_LENGTH,
        };
        Self::decode_with_declared_length(packet, declared_length)
    }

    fn decode_with_declared_length(
        packet: &[u8],
        declared_length: u8,
    ) -> Result<Self, ProtocolError> {
        if packet.len() != PROFILE_REPORT_LENGTH {
            return Err(ProtocolError::InvalidReportLength {
                expected: PROFILE_REPORT_LENGTH,
                actual: packet.len(),
            });
        }
        if packet[0] != PROFILE_REPORT_ID {
            return Err(ProtocolError::UnexpectedReportId {
                expected: PROFILE_REPORT_ID,
                actual: packet[0],
            });
        }
        if packet[1] != declared_length {
            return Err(ProtocolError::UnexpectedDeclaredLength {
                expected: declared_length,
                actual: packet[1],
            });
        }
        validate_fixed_byte(packet, 2, PROFILE_METADATA_SUBTYPE)?;
        validate_complement("current profile", packet[3], packet[4])?;
        validate_complement("maximum profile", packet[5], packet[6])?;
        validate_fixed_range(packet, 7, PROFILE_REPORT_LENGTH, 0)?;

        let current = ProfileId::try_from(packet[3])?;
        let maximum = ProfileId::try_from(packet[5])?;
        let metadata = ProfileMetadata::new(current, maximum)?;
        let mut bytes = [0_u8; PROFILE_REPORT_LENGTH];
        bytes.copy_from_slice(packet);
        Ok(Self { metadata, bytes })
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8; PROFILE_REPORT_LENGTH] {
        &self.bytes
    }
}

/// Reports supported by the one-shot FA61 read mailbox.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadbackRequest {
    Version,
    ProfileMetadata,
    PollingRate(ProfileId),
    Dpi(ProfileId),
    Preferences(ProfileId),
    Buttons(ProfileId),
}

impl ReadbackRequest {
    #[must_use]
    pub const fn report_id(self) -> u8 {
        match self {
            Self::Version => 0x0b,
            Self::PollingRate(_) => 0x06,
            Self::ProfileMetadata => PROFILE_REPORT_ID,
            Self::Dpi(_) => 0x04,
            Self::Preferences(_) => 0x05,
            Self::Buttons(_) => 0x08,
        }
    }

    #[must_use]
    pub const fn report_length(self) -> u8 {
        match self {
            Self::Version => 0x08,
            Self::ProfileMetadata => PROFILE_DECLARED_LENGTH,
            Self::PollingRate(_) => 0x09,
            Self::Dpi(_) => 0x38,
            Self::Preferences(_) => 0x0f,
            Self::Buttons(_) => 0x3b,
        }
    }

    /// Returns the working-profile target for profile-backed sections.
    ///
    /// `None` means byte 4 of the A0 selector is merely the observed `0x01`
    /// parameter; it must not be interpreted as persistent profile state.
    #[must_use]
    pub const fn target_profile(self) -> Option<ProfileId> {
        match self {
            Self::Dpi(profile)
            | Self::Preferences(profile)
            | Self::Buttons(profile)
            | Self::PollingRate(profile) => Some(profile),
            Self::Version | Self::ProfileMetadata => None,
        }
    }

    const fn selector_parameter(self) -> u8 {
        match self.target_profile() {
            Some(profile) => profile.get(),
            None => OBSERVED_UNTARGETED_PARAMETER,
        }
    }
}

/// Eight-byte selector that arms exactly one FA61 feature-report read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadSelector {
    bytes: [u8; READ_SELECTOR_LENGTH],
}

impl ReadSelector {
    #[must_use]
    pub const fn encode(request: ReadbackRequest) -> Self {
        Self {
            bytes: [
                READ_SELECTOR_REPORT_ID,
                request.report_id(),
                request.report_length(),
                0,
                request.selector_parameter(),
                0,
                0,
                0,
            ],
        }
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; READ_SELECTOR_LENGTH] {
        &self.bytes
    }
}

/// State published by the one-shot FA61 readiness mailbox.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadinessStatus {
    NotReady,
    Ready,
}

impl ReadinessStatus {
    /// Decodes and validates an eight-byte feature report `0xa0`.
    ///
    /// # Errors
    ///
    /// Rejects wrong lengths, report IDs, unknown status values, or nonzero
    /// reserved bytes.
    pub fn decode(packet: &[u8]) -> Result<Self, ProtocolError> {
        if packet.len() != READ_SELECTOR_LENGTH {
            return Err(ProtocolError::InvalidReportLength {
                expected: READ_SELECTOR_LENGTH,
                actual: packet.len(),
            });
        }
        if packet[0] != READ_SELECTOR_REPORT_ID {
            return Err(ProtocolError::UnexpectedReportId {
                expected: READ_SELECTOR_REPORT_ID,
                actual: packet[0],
            });
        }
        validate_fixed_range(packet, 2, READ_SELECTOR_LENGTH, 0)?;
        match packet[1] {
            0 => Ok(Self::NotReady),
            1 => Ok(Self::Ready),
            value => Err(ProtocolError::InvalidReadinessStatus { value }),
        }
    }
}

pub(crate) fn validate_complement(
    field: &'static str,
    value: u8,
    complement: u8,
) -> Result<(), ProtocolError> {
    if value ^ complement == u8::MAX {
        Ok(())
    } else {
        Err(ProtocolError::InvalidComplement {
            field,
            value,
            complement,
        })
    }
}

pub(crate) fn validate_fixed_byte(
    packet: &[u8],
    offset: usize,
    expected: u8,
) -> Result<(), ProtocolError> {
    let actual = packet[offset];
    if actual == expected {
        Ok(())
    } else {
        Err(ProtocolError::InvalidFixedByte {
            offset,
            expected,
            actual,
        })
    }
}

pub(crate) fn validate_fixed_range(
    packet: &[u8],
    start: usize,
    end: usize,
    expected: u8,
) -> Result<(), ProtocolError> {
    for offset in start..end {
        validate_fixed_byte(packet, offset, expected)?;
    }
    Ok(())
}

#[cfg(all(test, feature = "serde"))]
mod serde_tests {
    use super::ProfileMetadata;
    use crate::ProfileId;

    #[test]
    fn profile_metadata_round_trip_preserves_invariants() {
        let metadata = ProfileMetadata::new(
            ProfileId::try_from(2).unwrap(),
            ProfileId::try_from(4).unwrap(),
        )
        .unwrap();
        let json = serde_json::to_string(&metadata).unwrap();
        let restored: ProfileMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(metadata, restored);
        assert_eq!(restored.current().get(), 2);
        assert_eq!(restored.maximum().get(), 4);
        assert!(restored.current().get() <= restored.maximum().get());
    }

    #[test]
    fn profile_metadata_rejects_current_greater_than_maximum() {
        let json = r#"{"current":4,"maximum":2}"#;
        assert!(serde_json::from_str::<ProfileMetadata>(json).is_err());
    }

    #[test]
    fn profile_metadata_rejects_invalid_profile() {
        let json = r#"{"current":0,"maximum":1}"#;
        assert!(serde_json::from_str::<ProfileMetadata>(json).is_err());
        let json2 = r#"{"current":1,"maximum":6}"#;
        assert!(serde_json::from_str::<ProfileMetadata>(json2).is_err());
    }

    #[test]
    fn profile_metadata_rejects_equal_is_allowed_but_greater_not() {
        // equal is allowed (single profile enabled)
        let json = r#"{"current":3,"maximum":3}"#;
        assert!(serde_json::from_str::<ProfileMetadata>(json).is_ok());
        // but current > maximum is rejected via new()
        let json2 = r#"{"current":5,"maximum":1}"#;
        assert!(serde_json::from_str::<ProfileMetadata>(json2).is_err());
    }
}

#[cfg(test)]
mod framing_tests {
    use super::*;
    use crate::TransportKind;

    fn metadata() -> ProfileMetadata {
        ProfileMetadata::new(
            crate::ProfileId::try_from(1).unwrap(),
            crate::ProfileId::try_from(5).unwrap(),
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
            assert!(ProfileControlFraming::Compact.supports_transport(transport));
            assert_eq!(
                ProfileControlFraming::Compact.transmitted_length(),
                PROFILE_CONTROL_COMPACT_LENGTH
            );
            let report = ProfileControlReport::encode(metadata(), ProfileControlFraming::Compact);
            assert_eq!(report.as_bytes().len(), PROFILE_CONTROL_COMPACT_LENGTH);
            assert!(ProfileControlFraming::Compact.supports_transport(transport));
        }
    }

    #[test]
    fn full_is_only_valid_for_usb_transports() {
        assert!(ProfileControlFraming::Full.supports_transport(TransportKind::Wired));
        assert!(ProfileControlFraming::Full.supports_transport(TransportKind::Receiver));
        assert!(!ProfileControlFraming::Full.supports_transport(TransportKind::Ble));
        assert_eq!(
            ProfileControlFraming::Full.transmitted_length(),
            PROFILE_REPORT_LENGTH
        );
        let full = ProfileControlReport::encode(metadata(), ProfileControlFraming::Full);
        assert_eq!(full.as_bytes().len(), PROFILE_REPORT_LENGTH);
    }

    #[test]
    fn encode_for_transport_prevents_ble_full() {
        assert!(
            ProfileControlReport::encode_for_transport(
                metadata(),
                TransportKind::Ble,
                ProfileControlFraming::Full
            )
            .is_err()
        );
        assert!(
            ProfileControlReport::encode_for_transport(
                metadata(),
                TransportKind::Ble,
                ProfileControlFraming::Compact
            )
            .is_ok()
        );
        assert!(
            ProfileControlReport::encode_for_transport(
                metadata(),
                TransportKind::Wired,
                ProfileControlFraming::Full
            )
            .is_ok()
        );
    }

    #[test]
    fn metadata_decode_accepts_both_declared_lengths() {
        let mut packet = [0u8; PROFILE_REPORT_LENGTH];
        packet[0] = PROFILE_REPORT_ID;
        packet[1] = PROFILE_DECLARED_LENGTH;
        packet[2] = 0x01;
        packet[3] = 0x01;
        packet[4] = 0xfe;
        packet[5] = 0x05;
        packet[6] = 0xfa;
        assert!(ProfileMetadataReport::decode(&packet).is_ok());
        packet[1] = PROFILE_RECEIVER_DECLARED_LENGTH;
        assert!(
            ProfileMetadataReport::decode_for_transport(&packet, TransportKind::Receiver).is_ok()
        );
        packet[1] = 0x0d;
        assert!(ProfileMetadataReport::decode(&packet).is_err());
    }
}
