use crate::{ProfileId, ProtocolError};

use super::checksum::sum16;

pub const PREFERENCES_REPORT_ID: u8 = 0x05;
pub const PREFERENCES_DECLARED_LENGTH: u8 = 0x0f;
pub const PREFERENCES_COMPACT_LENGTH: usize = 13;
pub const PREFERENCES_FULL_LENGTH: usize = 15;

const CHECKSUM_OFFSET: usize = 11;
const PAYLOAD_END: usize = CHECKSUM_OFFSET;
const FULL_PADDING_START: usize = PREFERENCES_COMPACT_LENGTH;

/// The two FA61 wire images used by report `0x05`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreferencesFraming {
    /// The thirteen-byte compact image used for a write.
    Compact,
    /// The fifteen-byte normalized feature-report readback, including two
    /// required trailing zero bytes.
    Full,
}

impl PreferencesFraming {
    #[must_use]
    pub const fn transmitted_length(self) -> usize {
        match self {
            Self::Compact => PREFERENCES_COMPACT_LENGTH,
            Self::Full => PREFERENCES_FULL_LENGTH,
        }
    }
}

/// Raw report-`0x05` profile state.
///
/// The three host-labeled color bytes are intentionally exposed as an opaque
/// byte array: their presence in an X3 image is established, but an X3
/// hardware effect is not.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct PreferencesState {
    pub profile: ProfileId,
    pub light_mode: u8,
    pub configuration: u8,
    pub deep_sleep: u8,
    pub host_color: [u8; 3],
    pub sleep_timer: u8,
    pub debounce: u8,
}

impl PreferencesState {
    #[must_use]
    pub const fn new(
        profile: ProfileId,
        light_mode: u8,
        configuration: u8,
        deep_sleep: u8,
        host_color: [u8; 3],
        sleep_timer: u8,
        debounce: u8,
    ) -> Self {
        Self {
            profile,
            light_mode,
            configuration,
            deep_sleep,
            host_color,
            sleep_timer,
            debounce,
        }
    }

    #[must_use]
    pub const fn host_color_bytes(self) -> [u8; 3] {
        self.host_color
    }

    #[must_use]
    const fn payload(self) -> [u8; 8] {
        [
            self.light_mode,
            self.configuration,
            self.deep_sleep,
            self.host_color[0],
            self.host_color[1],
            self.host_color[2],
            self.sleep_timer,
            self.debounce,
        ]
    }
}

/// A validated report-`0x05` decode and its exact wire framing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedPreferencesReport {
    pub state: PreferencesState,
    pub framing: PreferencesFraming,
}

/// Encoded FA61 report-`0x05` bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreferencesReport {
    bytes: [u8; PREFERENCES_FULL_LENGTH],
    transmitted_length: usize,
}

impl PreferencesReport {
    /// Encodes a state with an explicit compact or full framing.
    #[must_use]
    pub fn encode_framed(state: &PreferencesState, framing: PreferencesFraming) -> Self {
        let mut bytes = [0_u8; PREFERENCES_FULL_LENGTH];
        bytes[0] = PREFERENCES_REPORT_ID;
        bytes[1] = PREFERENCES_DECLARED_LENGTH;
        bytes[2] = state.profile.get();
        bytes[3..PAYLOAD_END].copy_from_slice(&state.payload());
        let checksum = sum16(&bytes[3..PAYLOAD_END]);
        bytes[CHECKSUM_OFFSET..PREFERENCES_COMPACT_LENGTH].copy_from_slice(&checksum.to_be_bytes());
        Self {
            bytes,
            transmitted_length: framing.transmitted_length(),
        }
    }

    /// Decodes a compact write or full readback for the explicit target
    /// profile. The packet length is the framing discriminator.
    ///
    /// # Errors
    ///
    /// Rejects malformed framing, report identity, declared length, target
    /// profile, checksum, or full-readback padding.
    pub fn decode(
        packet: &[u8],
        expected_profile: ProfileId,
    ) -> Result<DecodedPreferencesReport, ProtocolError> {
        let framing = match packet.len() {
            PREFERENCES_COMPACT_LENGTH => PreferencesFraming::Compact,
            PREFERENCES_FULL_LENGTH => PreferencesFraming::Full,
            actual => {
                return Err(ProtocolError::InvalidReportLength {
                    expected: PREFERENCES_COMPACT_LENGTH,
                    actual,
                });
            }
        };
        if packet[0] != PREFERENCES_REPORT_ID {
            return Err(ProtocolError::UnexpectedReportId {
                expected: PREFERENCES_REPORT_ID,
                actual: packet[0],
            });
        }
        if packet[1] != PREFERENCES_DECLARED_LENGTH {
            return Err(ProtocolError::UnexpectedDeclaredLength {
                expected: PREFERENCES_DECLARED_LENGTH,
                actual: packet[1],
            });
        }
        if packet[2] != expected_profile.get() {
            return Err(ProtocolError::ProfileMismatch {
                expected: expected_profile.get(),
                actual: packet[2],
            });
        }
        if framing == PreferencesFraming::Full {
            for (offset, value) in packet[FULL_PADDING_START..].iter().copied().enumerate() {
                if value != 0 {
                    return Err(ProtocolError::InvalidFixedByte {
                        offset: FULL_PADDING_START + offset,
                        expected: 0,
                        actual: value,
                    });
                }
            }
        }
        let actual_checksum =
            u16::from_be_bytes([packet[CHECKSUM_OFFSET], packet[CHECKSUM_OFFSET + 1]]);
        let expected_checksum = sum16(&packet[3..PAYLOAD_END]);
        if actual_checksum != expected_checksum {
            return Err(ProtocolError::ChecksumMismatch {
                expected: expected_checksum,
                actual: actual_checksum,
            });
        }
        let state = PreferencesState {
            profile: expected_profile,
            light_mode: packet[3],
            configuration: packet[4],
            deep_sleep: packet[5],
            host_color: [packet[6], packet[7], packet[8]],
            sleep_timer: packet[9],
            debounce: packet[10],
        };
        Ok(DecodedPreferencesReport { state, framing })
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.transmitted_length]
    }

    #[must_use]
    pub const fn as_full_bytes(&self) -> &[u8; PREFERENCES_FULL_LENGTH] {
        &self.bytes
    }
}
