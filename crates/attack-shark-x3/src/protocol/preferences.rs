use crate::{ProfileId, ProtocolError, TransportKind};

use super::checksum::sum16;

pub const PREFERENCES_REPORT_ID: u8 = 0x05;
pub const PREFERENCES_DECLARED_LENGTH: u8 = 0x0f;
const PREFERENCES_RECEIVER_DECLARED_LENGTH: u8 = 0x11;
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
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
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

    /// Reproduces the preferences state written by the stock X3 reset flow,
    /// retargeted to `profile`.
    ///
    /// Source: `docs/evidence/x3-fa61/reset-packets.json`
    /// (`preferences.profile1`).
    #[must_use]
    pub const fn captured_stock_reset(profile: ProfileId) -> Self {
        Self::new(profile, 0x00, 0x03, 0xa8, [0x00, 0x00, 0xff], 0x01, 0x04)
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

/// A debounce (key-response) time, in milliseconds.
///
/// User-facing values are even milliseconds in `4..=50`. The wire byte is
/// `((ms - 4) / 2) + 2` (`2..=25`), documented in
/// `docs/protocols/05-preferences.md` field 6. Odd, sub-range, or
/// out-of-range values are rejected rather than rounded or clamped.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct DebounceMs(u8);

impl DebounceMs {
    /// Smallest supported debounce, in milliseconds.
    pub const MIN_MS: u8 = 4;
    /// Largest supported debounce, in milliseconds.
    pub const MAX_MS: u8 = 50;
    /// Wire byte for `MIN_MS`: `((4 - 4) / 2) + 2`.
    pub const MIN_RAW: u8 = 2;
    /// Wire byte for `MAX_MS`: `((50 - 4) / 2) + 2`.
    pub const MAX_RAW: u8 = 25;

    /// Creates a debounce from user-facing milliseconds; requires an even
    /// value in `4..=50`.
    #[must_use]
    pub const fn new(ms: u8) -> Option<Self> {
        if ms >= Self::MIN_MS && ms <= Self::MAX_MS && ms.is_multiple_of(2) {
            Some(Self(ms))
        } else {
            None
        }
    }

    /// Returns the debounce in milliseconds.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Encodes the canonical wire byte: `((ms - 4) / 2) + 2` (`2..=25`).
    #[must_use]
    pub const fn raw(self) -> u8 {
        ((self.0 - Self::MIN_MS) / 2) + Self::MIN_RAW
    }

    /// Decodes a canonical wire byte in `2..=25`.
    ///
    /// Every canonical byte maps to a distinct even millisecond value in
    /// `4..=50`; bytes outside the range are rejected, never rounded.
    #[must_use]
    pub const fn from_raw(raw: u8) -> Option<Self> {
        if raw >= Self::MIN_RAW && raw <= Self::MAX_RAW {
            Some(Self(((raw - Self::MIN_RAW) * 2) + Self::MIN_MS))
        } else {
            None
        }
    }
}

impl std::fmt::Display for DebounceMs {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} ms", self.0)
    }
}

/// A normal (standby) sleep timer, in half-minute units.
///
/// User-facing values are `0.5..=30` minutes in `0.5` steps. The wire byte is
/// `minutes * 2` — exactly the half-minute count (`1..=60`), documented in
/// `docs/protocols/05-preferences.md` field 5. Out-of-range values are
/// rejected rather than rounded or clamped.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct SleepTimer(u8);

impl SleepTimer {
    /// Smallest supported sleep timer, `0.5` minutes, in half-minute units.
    pub const MIN_HALF_MINUTES: u8 = 1;
    /// Largest supported sleep timer, `30` minutes, in half-minute units.
    pub const MAX_HALF_MINUTES: u8 = 60;
    /// Wire byte for `MIN_HALF_MINUTES` (`0.5` minutes).
    pub const MIN_RAW: u8 = 1;
    /// Wire byte for `MAX_HALF_MINUTES` (`30` minutes).
    pub const MAX_RAW: u8 = 60;

    /// Creates a sleep timer from half-minute units in `1..=60`
    /// (`0.5..=30` minutes in `0.5` steps).
    #[must_use]
    pub const fn new(half_minutes: u8) -> Option<Self> {
        if half_minutes >= Self::MIN_HALF_MINUTES && half_minutes <= Self::MAX_HALF_MINUTES {
            Some(Self(half_minutes))
        } else {
            None
        }
    }

    /// Returns the sleep timer in half-minute units.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Returns the sleep timer in minutes (half-minute granularity).
    #[must_use]
    pub const fn minutes(self) -> f64 {
        self.0 as f64 / 2.0
    }

    /// Encodes the canonical wire byte: `minutes * 2`, identical to the
    /// half-minute count (`1..=60`).
    #[must_use]
    pub const fn raw(self) -> u8 {
        self.0
    }

    /// Decodes a canonical wire byte in `1..=60`.
    #[must_use]
    pub const fn from_raw(raw: u8) -> Option<Self> {
        if raw >= Self::MIN_RAW && raw <= Self::MAX_RAW {
            Some(Self(raw))
        } else {
            None
        }
    }
}

impl std::fmt::Display for SleepTimer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.is_multiple_of(2) {
            write!(formatter, "{} min", self.0 / 2)
        } else {
            write!(formatter, "{}.5 min", self.0 / 2)
        }
    }
}

/// A deep-sleep timer, in whole minutes.
///
/// The wire representation is split across two fields, documented in
/// `docs/protocols/05-preferences.md` field 2:
///
/// - the high nibble of the configuration byte holds `minutes / 16`
///   (`0..=3`);
/// - the high nibble of the deep-sleep byte holds `minutes % 16`, while its
///   low nibble is the fixed `0x08` marker.
///
/// Controlled stock-app captures at 15/16/17, 32/33, and 48 minutes confirm
/// that exact multiples of 16 advance the configuration bucket and use a zero
/// high nibble in the deep-sleep byte. All values in `1..=60` therefore encode
/// and decode exactly. `PreferencesState` still preserves the raw bytes of any
/// noncanonical state; the typed decode is an optional, checked view.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct DeepSleepMinutes(u8);

impl DeepSleepMinutes {
    /// Smallest supported deep sleep, in minutes.
    pub const MIN_MINUTES: u8 = 1;
    /// Largest supported deep sleep, in minutes.
    pub const MAX_MINUTES: u8 = 60;
    /// Highest canonical configuration bucket: `floor(60 / 16)`.
    pub const MAX_BUCKET: u8 = 3;

    const DEEP_SLEEP_LOW_NIBBLE: u8 = 0x08;

    /// Creates a deep-sleep timer from whole minutes.
    #[must_use]
    pub const fn new(minutes: u8) -> Option<Self> {
        if minutes >= Self::MIN_MINUTES && minutes <= Self::MAX_MINUTES {
            Some(Self(minutes))
        } else {
            None
        }
    }

    /// Returns the deep sleep in minutes.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Returns the configuration high-nibble bucket: `minutes / 16`.
    #[must_use]
    pub const fn bucket(self) -> u8 {
        self.0 / 16
    }

    /// Returns the canonical deep-sleep wire byte:
    /// `0x08 | ((minutes % 16) << 4)`.
    #[must_use]
    pub const fn deep_sleep_byte(self) -> u8 {
        Self::DEEP_SLEEP_LOW_NIBBLE | ((self.0 % 16) << 4)
    }

    /// Returns the configuration byte for this timer, replacing only the high
    /// nibble with the bucket and preserving `configuration`'s low nibble.
    #[must_use]
    pub const fn configuration_with(self, configuration: u8) -> u8 {
        (self.bucket() << 4) | (configuration & 0x0f)
    }

    /// Decodes from the configuration bucket and the deep-sleep byte.
    ///
    /// Rejects noncanonical inputs: deep-sleep low nibbles other than `0x08`,
    /// buckets above `3`, and combined minute values outside `1..=60`.
    #[must_use]
    pub const fn from_raw(bucket: u8, deep_sleep: u8) -> Option<Self> {
        if deep_sleep & 0x0f != Self::DEEP_SLEEP_LOW_NIBBLE {
            return None;
        }
        if bucket > Self::MAX_BUCKET {
            return None;
        }
        Self::new((bucket << 4) | (deep_sleep >> 4))
    }

    /// Decodes from a raw configuration byte and deep-sleep byte, using the
    /// configuration high nibble as the bucket.
    #[must_use]
    pub const fn from_configuration(configuration: u8, deep_sleep: u8) -> Option<Self> {
        Self::from_raw(configuration >> 4, deep_sleep)
    }
}

impl std::fmt::Display for DeepSleepMinutes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} min", self.0)
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

    /// Decodes a canonical compact write or full readback for the explicit
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
        Self::decode_with_declared_length(packet, expected_profile, PREFERENCES_DECLARED_LENGTH)
    }

    /// Decodes a preferences readback using the selected transport dialect.
    ///
    /// # Errors
    ///
    /// Rejects malformed framing, report identity, declared length, target
    /// profile, checksum, or full-readback padding.
    pub fn decode_for_transport(
        packet: &[u8],
        transport: TransportKind,
        expected_profile: ProfileId,
    ) -> Result<DecodedPreferencesReport, ProtocolError> {
        let declared_length = match transport {
            TransportKind::Receiver => PREFERENCES_RECEIVER_DECLARED_LENGTH,
            TransportKind::Wired | TransportKind::Ble => PREFERENCES_DECLARED_LENGTH,
        };
        Self::decode_with_declared_length(packet, expected_profile, declared_length)
    }

    fn decode_with_declared_length(
        packet: &[u8],
        expected_profile: ProfileId,
        declared_length: u8,
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
        if packet[1] != declared_length {
            return Err(ProtocolError::UnexpectedDeclaredLength {
                expected: declared_length,
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
