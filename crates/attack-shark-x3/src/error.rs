use thiserror::Error;

/// Errors produced while validating or decoding an X3 protocol report.
#[derive(Debug, Eq, Error, PartialEq)]
pub enum ProtocolError {
    #[error(
        "invalid profile {value}; expected {}..={}",
        crate::model::ProfileId::MIN,
        crate::model::ProfileId::MAX
    )]
    InvalidProfile { value: u8 },

    #[error(
        "invalid profile metadata: expected 1 <= current <= maximum <= {}, got {current}/{maximum}",
        crate::model::ProfileId::MAX
    )]
    InvalidProfileRange { current: u8, maximum: u8 },

    #[error("invalid complement for {field}: value 0x{value:02x}, complement 0x{complement:02x}")]
    InvalidComplement {
        field: &'static str,
        value: u8,
        complement: u8,
    },

    #[error("invalid readiness status 0x{value:02x}; expected 0x00 or 0x01")]
    InvalidReadinessStatus { value: u8 },

    #[error(
        "invalid stage {value}; expected {}..={}",
        crate::model::StageIndex::MIN,
        crate::model::StageIndex::MAX
    )]
    InvalidStage { value: u8 },

    #[error(
        "invalid DPI {value}; expected {}..={} in steps of {}",
        crate::model::DpiValue::MIN,
        crate::model::DpiValue::MAX,
        crate::model::DpiValue::STEP
    )]
    InvalidDpi { value: u16 },

    #[error("invalid report length: expected {expected} bytes, got {actual}")]
    InvalidReportLength { expected: usize, actual: usize },

    #[error("unexpected report ID: expected 0x{expected:02x}, got 0x{actual:02x}")]
    UnexpectedReportId { expected: u8, actual: u8 },

    #[error("unexpected declared length: expected {expected} bytes, got {actual}")]
    UnexpectedDeclaredLength { expected: u8, actual: u8 },

    #[error("profile mismatch: expected profile {expected}, got {actual}")]
    ProfileMismatch { expected: u8, actual: u8 },

    #[error("checksum mismatch: expected 0x{expected:04x}, got 0x{actual:04x}")]
    ChecksumMismatch { expected: u16, actual: u16 },

    #[error("invalid fixed byte at offset {offset}: expected 0x{expected:02x}, got 0x{actual:02x}")]
    InvalidFixedByte {
        offset: usize,
        expected: u8,
        actual: u8,
    },

    #[error("invalid stage mask: 0x{mask:02x}")]
    InvalidStageMask { mask: u8 },

    #[error("invalid raw value for stage {stage}: {value}")]
    InvalidStageValue { stage: usize, value: u16 },

    #[error("active stage {active} is out of range for {count} configured stages")]
    ActiveStageOutOfRange { active: u8, count: usize },

    #[error("invalid stage count {count}; expected 1..=8")]
    InvalidStageCount { count: usize },

    #[error("invalid sensor value for {field}: 0x{value:02x}")]
    InvalidSensorValue { field: &'static str, value: u8 },
}

#[cfg(test)]
mod tests {
    use super::ProtocolError;

    #[test]
    fn formats_validation_errors_with_values() {
        assert_eq!(
            ProtocolError::InvalidProfile { value: 0 }.to_string(),
            "invalid profile 0; expected 1..=5"
        );
        assert_eq!(
            ProtocolError::InvalidDpi { value: 51 }.to_string(),
            "invalid DPI 51; expected 50..=26000 in steps of 50"
        );
    }

    #[test]
    fn formats_report_validation_errors_with_context() {
        assert_eq!(
            ProtocolError::InvalidFixedByte {
                offset: 25,
                expected: 0x01,
                actual: 0x02,
            }
            .to_string(),
            "invalid fixed byte at offset 25: expected 0x01, got 0x02"
        );
        assert_eq!(
            ProtocolError::ChecksumMismatch {
                expected: 0x1234,
                actual: 0xabcd,
            }
            .to_string(),
            "checksum mismatch: expected 0x1234, got 0xabcd"
        );
        assert_eq!(
            ProtocolError::InvalidSensorValue {
                field: "lod",
                value: 0x02,
            }
            .to_string(),
            "invalid sensor value for lod: 0x02"
        );
    }
}
