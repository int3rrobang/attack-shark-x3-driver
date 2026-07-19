use crate::{ProfileId, ProtocolError};

pub const PROFILE_REPORT_ID: u8 = 0x0c;
pub const PROFILE_DECLARED_LENGTH: u8 = 0x0a;
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

/// Wire framing for a profile-control write.
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
}

/// Edge-triggered profile current/maximum control report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileControlReport {
    bytes: [u8; PROFILE_REPORT_LENGTH],
    transmitted_length: usize,
}

impl ProfileControlReport {
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
    /// Decodes and validates a ten-byte report-`0x0c` metadata readback.
    ///
    /// # Errors
    ///
    /// Rejects wrong lengths, headers, subtype, complement pairs, profile
    /// ranges, and nonzero reserved bytes.
    pub fn decode(packet: &[u8]) -> Result<Self, ProtocolError> {
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
        if packet[1] != PROFILE_DECLARED_LENGTH {
            return Err(ProtocolError::UnexpectedDeclaredLength {
                expected: PROFILE_DECLARED_LENGTH,
                actual: packet[1],
            });
        }
        validate_fixed_byte(packet, 2, PROFILE_METADATA_SUBTYPE)?;
        validate_complement("current profile", packet[3], packet[4])?;
        validate_complement("maximum profile", packet[5], packet[6])?;
        for offset in 7..PROFILE_REPORT_LENGTH {
            validate_fixed_byte(packet, offset, 0)?;
        }

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

/// Profile-aware reports supported by the one-shot FA61 read mailbox.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadbackRequest {
    Version,
    ProfileMetadata,
    Dpi(ProfileId),
    Preferences(ProfileId),
    Buttons(ProfileId),
}

impl ReadbackRequest {
    #[must_use]
    pub const fn report_id(self) -> u8 {
        match self {
            Self::Version => 0x0b,
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
            Self::Dpi(profile) | Self::Preferences(profile) | Self::Buttons(profile) => {
                Some(profile)
            }
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
        for offset in 2..READ_SELECTOR_LENGTH {
            validate_fixed_byte(packet, offset, 0)?;
        }
        match packet[1] {
            0 => Ok(Self::NotReady),
            1 => Ok(Self::Ready),
            value => Err(ProtocolError::InvalidReadinessStatus { value }),
        }
    }
}

fn validate_complement(
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

fn validate_fixed_byte(packet: &[u8], offset: usize, expected: u8) -> Result<(), ProtocolError> {
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
