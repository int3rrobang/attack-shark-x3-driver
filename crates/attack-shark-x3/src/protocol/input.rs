use crate::{ProfileId, StageIndex};

/// Length of the FA61 auxiliary HID input report.
pub const INPUT_REPORT_LENGTH: usize = 5;
/// The only input report ID consumed by the stock X3 host driver.
pub const INPUT_REPORT_ID: u8 = 0x03;

const PROFILE_EVENT_TYPE: u16 = 0x1000;
const SECONDARY_PROFILE_EVENT_TYPE: u16 = 0x2000;
const BATTERY_EVENT_TYPE_X3: u16 = 0x4010;
const BATTERY_EVENT_TYPE_X11: u16 = 0x4055;
const CONNECTION_EVENT_TYPE: u16 = 0x5010;
const DPI_INDEX_EVENT_TYPE: u16 = 0x6000;
const LED_MODE_EVENT_TYPE: u16 = 0x7000;
const PROFILE_SYNC_EVENT_TYPE: u16 = 0x8000;

/// Length of the FA61 auxiliary DPI-button input report.
pub const DPI_BUTTON_REPORT_LENGTH: usize = INPUT_REPORT_LENGTH;

/// Fixed prefix and trailing byte of the DPI-button report.
///
/// Byte 3 is the resulting one-based active DPI-stage index.
pub const DPI_BUTTON_REPORT_PREFIX: [u8; 3] = [INPUT_REPORT_ID, 0x00, 0x10];
pub const DPI_BUTTON_REPORT_TRAILING_BYTE: u8 = 0x00;

/// A physical DPI-button press reported on the auxiliary HID input path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct DpiButtonEvent {
    pub raw_report: [u8; DPI_BUTTON_REPORT_LENGTH],
    pub active_stage: StageIndex,
}

/// A profile index carried by the report-`0x03` DPI/profile event.
///
/// The firmware and stock host use the same event type for profile changes and
/// the FA61 DPI-button notification. The live-confirmed Rust interpretation is
/// retained as [`DpiButtonEvent`] because the payload is an active stage on
/// that path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct ProfileChangedEvent {
    pub raw_report: [u8; INPUT_REPORT_LENGTH],
    pub profile: ProfileId,
}

/// A one-based DPI index from the stock host's report-`0x03` event family.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct DpiIndex(u8);

impl DpiIndex {
    pub const MIN: u8 = 1;
    pub const MAX: u8 = 10;

    /// Creates an index when it is in the firmware's reported range.
    #[must_use]
    pub const fn new(value: u8) -> Option<Self> {
        if value >= Self::MIN && value <= Self::MAX {
            Some(Self(value))
        } else {
            None
        }
    }

    /// Returns the one-based firmware index.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// A profile-indexed input event emitted by the receiver/USB auxiliary path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct DpiIndexChangedEvent {
    pub raw_report: [u8; INPUT_REPORT_LENGTH],
    pub index: DpiIndex,
}

/// A connection-state event from report `0x03`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct ConnectionChangedEvent {
    pub raw_report: [u8; INPUT_REPORT_LENGTH],
    pub connected: bool,
}

/// An LED-mode event from report `0x03`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct LedModeChangedEvent {
    pub raw_report: [u8; INPUT_REPORT_LENGTH],
    pub mode: u8,
}

/// A decoded report-`0x03` input event.
///
/// The five-byte report is parsed once by the USB worker. Unknown event types,
/// malformed payloads, and values outside the firmware-confirmed ranges are
/// ignored rather than exposed as guessed semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub enum InputEvent {
    ActiveDpiStageChanged(DpiButtonEvent),
    ProfileChanged(ProfileChangedEvent),
    SecondaryProfileChanged(ProfileChangedEvent),
    BatteryChanged(BatteryEvent),
    ConnectionChanged(ConnectionChangedEvent),
    DpiIndexChanged(DpiIndexChangedEvent),
    LedModeChanged(LedModeChangedEvent),
    ProfileSync(ProfileChangedEvent),
}

/// Decodes one stock report-`0x03` notification.
///
/// The two 16-bit values are little-endian:
///
/// ```text
/// event_type = byte[1] | byte[2] << 8
/// event_data = byte[3] | byte[4] << 8
/// ```
///
/// HID reads may include trailing transport padding, so only the first five
/// bytes are consumed.
#[must_use]
pub fn decode_input_report(packet: &[u8]) -> Option<InputEvent> {
    let raw_report = raw_report(packet)?;
    let event_type = u16::from_le_bytes([raw_report[1], raw_report[2]]);

    match event_type {
        PROFILE_EVENT_TYPE => {
            if raw_report[4] != 0 {
                return None;
            }
            let active_stage = StageIndex::try_from(raw_report[3]).ok()?;
            Some(InputEvent::ActiveDpiStageChanged(DpiButtonEvent {
                raw_report,
                active_stage,
            }))
        }
        SECONDARY_PROFILE_EVENT_TYPE => Some(InputEvent::SecondaryProfileChanged(profile_event(
            raw_report,
        )?)),
        BATTERY_EVENT_TYPE_X3 => Some(InputEvent::BatteryChanged(decode_battery_raw(raw_report)?)),
        BATTERY_EVENT_TYPE_X11 => Some(InputEvent::BatteryChanged(decode_battery_raw(raw_report)?)),
        CONNECTION_EVENT_TYPE => {
            if raw_report[4] != 0 || raw_report[3] > 1 {
                return None;
            }
            Some(InputEvent::ConnectionChanged(ConnectionChangedEvent {
                raw_report,
                connected: raw_report[3] == 0,
            }))
        }
        DPI_INDEX_EVENT_TYPE => {
            if raw_report[4] != 0 {
                return None;
            }
            Some(InputEvent::DpiIndexChanged(DpiIndexChangedEvent {
                raw_report,
                index: DpiIndex::new(raw_report[3])?,
            }))
        }
        LED_MODE_EVENT_TYPE => {
            if raw_report[4] != 0 || raw_report[3] > 7 {
                return None;
            }
            Some(InputEvent::LedModeChanged(LedModeChangedEvent {
                raw_report,
                mode: raw_report[3],
            }))
        }
        PROFILE_SYNC_EVENT_TYPE => Some(InputEvent::ProfileSync(profile_event(raw_report)?)),
        _ => None,
    }
}

fn raw_report(packet: &[u8]) -> Option<[u8; INPUT_REPORT_LENGTH]> {
    if packet.len() < INPUT_REPORT_LENGTH || packet[0] != INPUT_REPORT_ID {
        return None;
    }
    packet[..INPUT_REPORT_LENGTH].try_into().ok()
}

fn profile_event(raw_report: [u8; INPUT_REPORT_LENGTH]) -> Option<ProfileChangedEvent> {
    if raw_report[4] != 0 {
        return None;
    }
    Some(ProfileChangedEvent {
        raw_report,
        profile: ProfileId::try_from(raw_report[3]).ok()?,
    })
}

fn decode_battery_raw(raw_report: [u8; INPUT_REPORT_LENGTH]) -> Option<BatteryEvent> {
    let level = if raw_report[..4] == BATTERY_REPORT_PREFIX_X3 {
        if !(1..=10).contains(&raw_report[4]) {
            return None;
        }
        raw_report[4] * 10
    } else if raw_report[..4] == BATTERY_REPORT_PREFIX {
        if raw_report[4] > 100 {
            return None;
        }
        raw_report[4]
    } else {
        return None;
    };
    Some(BatteryEvent { raw_report, level })
}

/// Decodes the FA61 auxiliary DPI-button report.
///
/// This convenience decoder delegates to [`decode_input_report`], so the USB
/// worker and direct callers share exactly one report parser.
#[must_use]
pub fn decode_dpi_button_report(packet: &[u8]) -> Option<DpiButtonEvent> {
    match decode_input_report(packet) {
        Some(InputEvent::ActiveDpiStageChanged(event)) => Some(event),
        _ => None,
    }
}

pub const BATTERY_REPORT_LENGTH: usize = INPUT_REPORT_LENGTH;

/// X3/M600 FA60 receiver battery prefix. Level is 0–10 scale; multiply by 10
/// for percentage. Confirmed by X3.exe disassembly and FA60 capture.
pub const BATTERY_REPORT_PREFIX_X3: [u8; 4] = [INPUT_REPORT_ID, 0x10, 0x40, 0x01];

/// X11 legacy receiver battery prefix. Level is 0–100 directly.
pub const BATTERY_REPORT_PREFIX: [u8; 4] = [INPUT_REPORT_ID, 0x55, 0x40, 0x01];

/// A battery percentage decoded from the receiver interrupt IN report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub struct BatteryEvent {
    pub raw_report: [u8; BATTERY_REPORT_LENGTH],
    pub level: u8,
}

/// Decodes a receiver battery report (endpoint 0x83, autonomous push).
///
/// Supports X3/M600 FA60 (`03 10 40 01 <level>`) and the X11 legacy
/// (`03 55 40 01 <pct>`) signature.
#[must_use]
pub fn decode_battery_report(packet: &[u8]) -> Option<BatteryEvent> {
    match decode_input_report(packet) {
        Some(InputEvent::BatteryChanged(event)) => Some(event),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BATTERY_REPORT_PREFIX, BATTERY_REPORT_PREFIX_X3, BatteryEvent, ConnectionChangedEvent,
        DPI_BUTTON_REPORT_PREFIX, DPI_BUTTON_REPORT_TRAILING_BYTE, DpiButtonEvent, DpiIndex,
        DpiIndexChangedEvent, InputEvent, decode_battery_report, decode_dpi_button_report,
        decode_input_report,
    };
    use crate::{ProfileId, StageIndex};

    #[test]
    fn decodes_stage_two_and_preserves_the_raw_event() {
        let packet = [0x03, 0x00, 0x10, 0x02, DPI_BUTTON_REPORT_TRAILING_BYTE];
        let expected = DpiButtonEvent {
            raw_report: packet,
            active_stage: StageIndex::try_from(2).expect("stage 2 is valid"),
        };
        assert_eq!(decode_dpi_button_report(&packet), Some(expected));
        assert_eq!(
            decode_input_report(&packet),
            Some(InputEvent::ActiveDpiStageChanged(expected))
        );
    }

    #[test]
    fn decodes_additional_little_endian_event_types() {
        let profile = [0x03, 0x00, 0x20, 0x03, 0x00];
        assert_eq!(
            decode_input_report(&profile),
            Some(InputEvent::SecondaryProfileChanged(
                super::ProfileChangedEvent {
                    raw_report: profile,
                    profile: ProfileId::try_from(3).expect("profile 3 is valid"),
                }
            ))
        );

        let connection = [0x03, 0x10, 0x50, 0x01, 0x00];
        assert_eq!(
            decode_input_report(&connection),
            Some(InputEvent::ConnectionChanged(ConnectionChangedEvent {
                raw_report: connection,
                connected: false,
            }))
        );

        let dpi = [0x03, 0x00, 0x60, 0x0a, 0x00];
        assert_eq!(
            decode_input_report(&dpi),
            Some(InputEvent::DpiIndexChanged(DpiIndexChangedEvent {
                raw_report: dpi,
                index: DpiIndex::new(10).expect("DPI index 10 is valid"),
            }))
        );
    }

    #[test]
    fn decodes_x3_and_legacy_battery_reports_once() {
        let x3 = [0x03, 0x10, 0x40, 0x01, 0x0a, 0x00];
        assert_eq!(
            decode_battery_report(&x3),
            Some(BatteryEvent {
                raw_report: [0x03, 0x10, 0x40, 0x01, 0x0a],
                level: 100,
            })
        );
        let legacy = [0x03, 0x55, 0x40, 0x01, 87];
        assert_eq!(
            decode_battery_report(&legacy),
            Some(BatteryEvent {
                raw_report: legacy,
                level: 87,
            })
        );
        assert_ne!(BATTERY_REPORT_PREFIX_X3, BATTERY_REPORT_PREFIX);
    }

    #[test]
    fn rejects_unknown_and_out_of_range_events() {
        assert_eq!(decode_input_report(&[0x03, 0x99, 0x99, 0, 0]), None);
        assert_eq!(decode_input_report(&[0x03, 0x00, 0x10, 0, 0]), None);
        assert_eq!(decode_input_report(&[0x03, 0x00, 0x60, 11, 0]), None);
        assert_eq!(decode_input_report(&[0x03, 0x00, 0x70, 8, 0]), None);
        assert_eq!(decode_input_report(&[0x03, 0x00, 0x10, 2]), None);
        assert_eq!(decode_dpi_button_report(&DPI_BUTTON_REPORT_PREFIX), None);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn input_events_round_trip_through_json() {
        let event = decode_input_report(&[0x03, 0x10, 0x50, 0x00, 0x00]).unwrap();
        let json = serde_json::to_string(&event).unwrap();
        let restored: InputEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, restored);
    }
}
