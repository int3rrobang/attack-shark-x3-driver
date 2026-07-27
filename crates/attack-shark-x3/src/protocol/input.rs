use crate::StageIndex;

/// Length of the FA61 auxiliary DPI-button input report.
pub const DPI_BUTTON_REPORT_LENGTH: usize = 5;

/// Fixed prefix and trailing byte of the DPI-button report.
///
/// Byte 3 is the resulting one-based active DPI-stage index.
pub const DPI_BUTTON_REPORT_PREFIX: [u8; 3] = [0x03, 0x00, 0x10];
pub const DPI_BUTTON_REPORT_TRAILING_BYTE: u8 = 0x00;

/// A physical DPI-button press reported on the auxiliary HID input path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DpiButtonEvent {
    pub raw_report: [u8; DPI_BUTTON_REPORT_LENGTH],
    pub active_stage: StageIndex,
}

/// Decodes the FA61 auxiliary DPI-button report.
///
/// The report carries the resulting one-based active DPI-stage index in byte 3.
/// This interpretation is live-confirmed by profile-1 captures containing
/// stages `2` and `3`, followed by a targeted DPI read reporting active stage 3.
#[must_use]
pub fn decode_dpi_button_report(packet: &[u8]) -> Option<DpiButtonEvent> {
    if packet.len() != DPI_BUTTON_REPORT_LENGTH
        || packet[..DPI_BUTTON_REPORT_PREFIX.len()] != DPI_BUTTON_REPORT_PREFIX
        || packet[DPI_BUTTON_REPORT_LENGTH - 1] != DPI_BUTTON_REPORT_TRAILING_BYTE
    {
        return None;
    }
    let active_stage = StageIndex::try_from(packet[3]).ok()?;
    Some(DpiButtonEvent {
        raw_report: packet.try_into().ok()?,
        active_stage,
    })
}
pub const BATTERY_REPORT_LENGTH: usize = 5;

/// X3/M600 FA60 receiver battery prefix. Level is 0–10 scale; multiply by 10
/// for percentage. Confirmed by X3.exe disassembly (0x41346d–0x413470) and
/// 2026-07-24 FA60 capture.
pub const BATTERY_REPORT_PREFIX_X3: [u8; 4] = [0x03, 0x10, 0x40, 0x01];

/// X11 legacy receiver battery prefix. Level is 0–100 directly.
pub const BATTERY_REPORT_PREFIX: [u8; 4] = [0x03, 0x55, 0x40, 0x01];

/// A battery percentage decoded from the receiver interrupt IN report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BatteryEvent {
    pub raw_report: [u8; BATTERY_REPORT_LENGTH],
    pub level: u8,
}

/// Decodes a receiver battery report (endpoint 0x83, autonomous push).
///
/// Supports two signatures:
/// - X3/M600 FA60: `03 10 40 01 <level>` where level is 1–10 (×10 = percentage)
/// - X11 legacy: `03 55 40 01 <pct>` where pct is 0–100 directly
#[must_use]
pub fn decode_battery_report(packet: &[u8]) -> Option<BatteryEvent> {
    if packet.len() < BATTERY_REPORT_LENGTH {
        return None;
    }
    let raw_report: [u8; BATTERY_REPORT_LENGTH] =
        packet[..BATTERY_REPORT_LENGTH].try_into().ok()?;
    let level = if packet[..4] == BATTERY_REPORT_PREFIX_X3 {
        let raw = packet[4];
        if !(1..=10).contains(&raw) {
            return None;
        }
        raw * 10
    } else if packet[..4] == BATTERY_REPORT_PREFIX {
        let raw = packet[4];
        if raw > 100 {
            return None;
        }
        raw
    } else {
        return None;
    };
    Some(BatteryEvent { raw_report, level })
}

#[cfg(test)]
mod battery_tests {
    use super::{
        BATTERY_REPORT_PREFIX, BATTERY_REPORT_PREFIX_X3, BatteryEvent, decode_battery_report,
    };

    #[test]
    fn decodes_x11_legacy_report() {
        let packet = [0x03, 0x55, 0x40, 0x01, 87, 0x00];
        assert_eq!(
            decode_battery_report(&packet),
            Some(BatteryEvent {
                raw_report: [0x03, 0x55, 0x40, 0x01, 87],
                level: 87,
            })
        );
    }

    #[test]
    fn decodes_x3_report_with_times_ten_conversion() {
        let packet = [0x03, 0x10, 0x40, 0x01, 0x0a];
        assert_eq!(
            decode_battery_report(&packet),
            Some(BatteryEvent {
                raw_report: [0x03, 0x10, 0x40, 0x01, 0x0a],
                level: 100,
            })
        );

        let half = [0x03, 0x10, 0x40, 0x01, 5];
        assert_eq!(
            decode_battery_report(&half),
            Some(BatteryEvent {
                raw_report: [0x03, 0x10, 0x40, 0x01, 5],
                level: 50,
            })
        );
    }

    #[test]
    fn rejects_x3_out_of_range() {
        let zero = [0x03, 0x10, 0x40, 0x01, 0];
        assert_eq!(decode_battery_report(&zero), None);

        let eleven = [0x03, 0x10, 0x40, 0x01, 11];
        assert_eq!(decode_battery_report(&eleven), None);
    }

    #[test]
    fn rejects_wrong_prefix_and_out_of_range_level() {
        let mut wrong_prefix = [0x03, 0x55, 0x40, 0x01, 50];
        wrong_prefix[2] = 0x41;
        assert_eq!(decode_battery_report(&wrong_prefix), None);

        let invalid_level = [
            BATTERY_REPORT_PREFIX[0],
            BATTERY_REPORT_PREFIX[1],
            BATTERY_REPORT_PREFIX[2],
            BATTERY_REPORT_PREFIX[3],
            101,
        ];
        assert_eq!(decode_battery_report(&invalid_level), None);
    }

    #[test]
    fn rejects_unrelated_report() {
        let dpi = [0x03, 0x00, 0x10, 0x02, 0x00];
        assert_eq!(decode_battery_report(&dpi), None);
        assert_ne!(BATTERY_REPORT_PREFIX_X3, BATTERY_REPORT_PREFIX);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DPI_BUTTON_REPORT_PREFIX, DPI_BUTTON_REPORT_TRAILING_BYTE, DpiButtonEvent,
        decode_dpi_button_report,
    };

    #[test]
    fn decodes_stage_two_and_preserves_the_raw_report() {
        let packet = [0x03, 0x00, 0x10, 0x02, DPI_BUTTON_REPORT_TRAILING_BYTE];
        assert_eq!(
            decode_dpi_button_report(&packet),
            Some(DpiButtonEvent {
                raw_report: packet,
                active_stage: super::StageIndex::try_from(2).expect("stage 2 is valid"),
            })
        );
    }

    #[test]
    fn rejects_invalid_stage_and_malformed_reports() {
        let invalid_stage = [0x03, 0x00, 0x10, 0x00, 0x00];
        assert_eq!(decode_dpi_button_report(&invalid_stage), None);

        let mut nearby = [0x03, 0x00, 0x10, 0x03, 0x00];
        nearby[2] = 0x11;
        assert_eq!(decode_dpi_button_report(&nearby), None);
        assert_eq!(decode_dpi_button_report(&DPI_BUTTON_REPORT_PREFIX), None);
    }
}
