use crate::ProtocolError;

pub const POLLING_RATE_REPORT_ID: u8 = 0x06;
pub const POLLING_RATE_DECLARED_LENGTH: u8 = 0x09;
pub const POLLING_RATE_REPORT_LENGTH: usize = 9;

const POLLING_RATE_SUBCOMMAND: u8 = 0x01;
const PADDING_START: usize = 5;

/// A polling rate supported by the X3/M600 USB configuration protocol.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub enum PollingRate {
    Hz125,
    Hz250,
    Hz500,
    Hz1000,
}

impl PollingRate {
    /// Creates a polling rate from its host-visible frequency.
    #[must_use]
    pub const fn new(hz: u16) -> Option<Self> {
        match hz {
            125 => Some(Self::Hz125),
            250 => Some(Self::Hz250),
            500 => Some(Self::Hz500),
            1000 => Some(Self::Hz1000),
            _ => None,
        }
    }

    /// Returns the host-visible frequency in hertz.
    #[must_use]
    pub const fn hz(self) -> u16 {
        match self {
            Self::Hz125 => 125,
            Self::Hz250 => 250,
            Self::Hz500 => 500,
            Self::Hz1000 => 1000,
        }
    }

    /// Returns the protocol's encoded rate value.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Hz125 => 0x08,
            Self::Hz250 => 0x04,
            Self::Hz500 => 0x02,
            Self::Hz1000 => 0x01,
        }
    }

    const fn from_code(code: u8) -> Result<Self, ProtocolError> {
        match code {
            0x08 => Ok(Self::Hz125),
            0x04 => Ok(Self::Hz250),
            0x02 => Ok(Self::Hz500),
            0x01 => Ok(Self::Hz1000),
            value => Err(ProtocolError::InvalidPollingRateCode { value }),
        }
    }
}

impl TryFrom<u16> for PollingRate {
    type Error = ProtocolError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::new(value).ok_or(ProtocolError::InvalidPollingRate { value })
    }
}

impl std::fmt::Display for PollingRate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} Hz", self.hz())
    }
}

/// A validated report-`0x06` decode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedPollingRateReport {
    pub rate: PollingRate,
}

/// Encoded report-`0x06` bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PollingRateReport {
    bytes: [u8; POLLING_RATE_REPORT_LENGTH],
}

impl PollingRateReport {
    /// Encodes a USB wired or receiver polling-rate report.
    #[must_use]
    pub fn encode(rate: PollingRate) -> Self {
        let code = rate.code();
        Self {
            bytes: [
                POLLING_RATE_REPORT_ID,
                POLLING_RATE_DECLARED_LENGTH,
                POLLING_RATE_SUBCOMMAND,
                code,
                !code,
                0,
                0,
                0,
                0,
            ],
        }
    }

    /// Decodes the canonical wired/receiver write-shaped report-`0x06`.
    ///
    /// # Errors
    ///
    /// Rejects malformed length, identity, sub-command, rate code,
    /// complement, or padding bytes.
    pub fn decode(packet: &[u8]) -> Result<DecodedPollingRateReport, ProtocolError> {
        Self::decode_with_declared_length(packet, POLLING_RATE_DECLARED_LENGTH)
    }

    /// Decodes a report-`0x06` readback for a selected USB transport.
    ///
    /// The FA60 receiver returns the same nine-byte functional image with
    /// declared length `0x0b`, while X3 writes use the canonical `0x09`.
    ///
    /// # Errors
    ///
    /// Rejects malformed length, identity, sub-command, rate code,
    /// complement, or padding bytes.
    pub fn decode_for_transport(
        packet: &[u8],
        transport: crate::TransportKind,
    ) -> Result<DecodedPollingRateReport, ProtocolError> {
        let declared_length = match transport {
            crate::TransportKind::Receiver => 0x0b,
            crate::TransportKind::Wired | crate::TransportKind::Ble => POLLING_RATE_DECLARED_LENGTH,
        };
        Self::decode_with_declared_length(packet, declared_length)
    }

    fn decode_with_declared_length(
        packet: &[u8],
        declared_length: u8,
    ) -> Result<DecodedPollingRateReport, ProtocolError> {
        if packet.len() != POLLING_RATE_REPORT_LENGTH {
            return Err(ProtocolError::InvalidReportLength {
                expected: POLLING_RATE_REPORT_LENGTH,
                actual: packet.len(),
            });
        }
        if packet[0] != POLLING_RATE_REPORT_ID {
            return Err(ProtocolError::UnexpectedReportId {
                expected: POLLING_RATE_REPORT_ID,
                actual: packet[0],
            });
        }
        if packet[1] != declared_length {
            return Err(ProtocolError::UnexpectedDeclaredLength {
                expected: declared_length,
                actual: packet[1],
            });
        }
        validate_fixed_byte(packet, 2, POLLING_RATE_SUBCOMMAND)?;
        let rate = PollingRate::from_code(packet[3])?;
        validate_complement(packet[3], packet[4])?;
        for offset in PADDING_START..POLLING_RATE_REPORT_LENGTH {
            validate_fixed_byte(packet, offset, 0)?;
        }
        Ok(DecodedPollingRateReport { rate })
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; POLLING_RATE_REPORT_LENGTH] {
        &self.bytes
    }
}

fn validate_complement(value: u8, complement: u8) -> Result<(), ProtocolError> {
    if value ^ complement == u8::MAX {
        Ok(())
    } else {
        Err(ProtocolError::InvalidComplement {
            field: "polling rate",
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

#[cfg(test)]
mod tests {
    use super::{PollingRate, PollingRateReport};
    use crate::{ProtocolError, TransportKind};

    #[test]
    fn encodes_all_supported_rates() {
        assert_eq!(
            PollingRateReport::encode(PollingRate::Hz125).as_bytes(),
            b"\x06\x09\x01\x08\xf7\x00\x00\x00\x00"
        );
        assert_eq!(
            PollingRateReport::encode(PollingRate::Hz250).as_bytes(),
            b"\x06\x09\x01\x04\xfb\x00\x00\x00\x00"
        );
        assert_eq!(
            PollingRateReport::encode(PollingRate::Hz500).as_bytes(),
            b"\x06\x09\x01\x02\xfd\x00\x00\x00\x00"
        );
        assert_eq!(
            PollingRateReport::encode(PollingRate::Hz1000).as_bytes(),
            b"\x06\x09\x01\x01\xfe\x00\x00\x00\x00"
        );
    }

    #[test]
    fn decodes_and_rejects_malformed_reports() {
        let packet = PollingRateReport::encode(PollingRate::Hz1000);
        let decoded = PollingRateReport::decode(packet.as_bytes()).expect("valid rate report");
        assert_eq!(decoded.rate, PollingRate::Hz1000);

        let mut wrong_complement = *packet.as_bytes();
        wrong_complement[4] = 0;
        assert!(matches!(
            PollingRateReport::decode(&wrong_complement),
            Err(ProtocolError::InvalidComplement {
                field: "polling rate",
                ..
            })
        ));

        let mut wrong_padding = *packet.as_bytes();
        wrong_padding[8] = 1;
        assert!(matches!(
            PollingRateReport::decode(&wrong_padding),
            Err(ProtocolError::InvalidFixedByte {
                offset: 8,
                expected: 0,
                actual: 1,
            })
        ));
    }

    #[test]
    fn decodes_receiver_readback_declared_length() {
        let packet = b"\x06\x0b\x01\x02\xfd\x00\x00\x00\x00";
        let decoded = PollingRateReport::decode_for_transport(packet, TransportKind::Receiver)
            .expect("FA60 receiver readback must decode");
        assert_eq!(decoded.rate, PollingRate::Hz500);
        assert!(PollingRateReport::decode(packet).is_err());
    }

    #[test]
    fn validates_host_rates() {
        assert_eq!(PollingRate::try_from(125), Ok(PollingRate::Hz125));
        assert_eq!(PollingRate::try_from(1000), Ok(PollingRate::Hz1000));
        assert!(matches!(
            PollingRate::try_from(333),
            Err(ProtocolError::InvalidPollingRate { value: 333 })
        ));
    }
}
