//! Versioned serde JSON durable state model for `x3ctl`.
//!
//! Every stored section carries a [`StateSource`] (where the value originated),
//! a [`StateVerification`] (how confident we are the device accepted it), and an
//! ISO-8601 update timestamp.  The rule "do not claim cached data is observed"
//! means [`load`] preserves stored verification verbatim and never upgrades an
//! entry to [`StateVerification::Observed`] merely because it was read from disk.
//!
//! # Dependencies
//!
//! Requires the crate features `serde` and a `serde_json` dependency on the
//! binary target.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── schema version ──────────────────────────────────────────────────────────

/// Bump this constant when the on-disk format changes in an incompatible way.
/// [`load`] rejects any file whose `schema_version` differs.
const SCHEMA_VERSION: u32 = 1;
/// Number of button-assignment slots in the FA61 button report.
pub const BUTTON_SLOT_COUNT: usize = 18;

// ── provenance enums ────────────────────────────────────────────────────────

/// Where a stored value originated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StateSource {
    /// Value was read back from the device over USB.
    UsbReadback,
    /// Value was imported from an external profile or file.
    Imported,
    /// Value was populated from explicit built-in defaults.
    ExplicitDefaults,
    /// Value was written by a local command (e.g. `dpi`, `rate`, `bind`).
    LocallyWritten,
}

/// How confident we are that the device accepted the stored value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StateVerification {
    /// Value was read and observed directly from the device.
    Observed,
    /// Value was written and immediately read back, matching the write.
    VerifiedImmediate,
    /// The transport ACK'd the write (BLE `fee4` ACK received).
    AckAccepted,
    /// Value was written but the application did not verify acceptance.
    ApplicationUnknown,
    /// Value is stored on disk but has never been verified on the device.
    PersistenceUnknown,
}

// ── versioned state wrapper ─────────────────────────────────────────────────

/// A value together with its provenance, verification, and update time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionedState<T> {
    /// The stored value.
    pub value: T,
    /// Where the value came from.
    pub source: StateSource,
    /// How confident we are the device accepted it.
    pub verification: StateVerification,
    /// ISO-8601 UTC timestamp of the last update.
    pub updated_at: String,
}

impl<T> VersionedState<T> {
    /// Create a new versioned state entry with the current UTC timestamp.
    pub fn now(value: T, source: StateSource, verification: StateVerification) -> Self {
        Self {
            value,
            source,
            verification,
            updated_at: utc_now_iso8601(),
        }
    }
}

// ── wire-safe stored types (primitives only) ────────────────────────────────

/// Platform-independent transport discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StoredTransport {
    /// USB wired device (PID `0xfa61`).
    Wired,
    /// USB 2.4 GHz receiver (PID `0xfa60`).
    Receiver,
    /// BLE connection.
    Ble,
}

/// A device selector stored in platform-independent form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub enum StoredSelector {
    /// Open the sole matching HID collection (USB).
    Unique,
    /// Open a specific HID path (USB).
    Path(String),
    /// Select the sole connected BLE FEE0 device.
    UniqueConnected,
    /// Reopen a platform-specific BLE device ID.
    BleDevice(String),
    /// Select a connected BLE device by its advertised name.
    BleName(String),
    /// Select a connected BLE device by its address.
    BleAddr(String),
}

// ── DPI ─────────────────────────────────────────────────────────────────────

/// A single DPI stage in wire-safe form.
///
/// Only the DPI value is stored — per-stage colours do not exist in the
/// FA61 protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredDpiStage {
    /// DPI value (50..=26000 in steps of 50).
    pub dpi: u16,
}

/// Sensor options in wire-safe form.
///
/// Matches the FA61 DPI report's sensor field: lift-off distance, ripple
/// control, angle snap, and motion sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSensorOptions {
    /// Lift-off distance: 1 = Low (1 mm), 2 = High (2 mm).
    pub lift_off_distance: u8,
    /// Ripple control enabled.
    pub ripple_control: bool,
    /// Angle snapping enabled.
    pub angle_snap: bool,
    /// Motion Sync enabled.
    pub motion_sync: bool,
}

/// Complete DPI configuration in wire-safe form.
///
/// Matches the FA61 [`DpiState`] decode exactly: profile, stages (as DPI
/// values), active stage, sensor options, and the unresolved 25-byte
/// `preserved_tail` required for safe read-modify-write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredDpiState {
    /// Profile id (1-based).
    pub profile: u8,
    /// Ordered DPI stages (index 0 is stage 1).
    pub stages: Vec<StoredDpiStage>,
    /// Currently active stage (1-based).
    pub active_stage: u8,
    /// Sensor-level options.
    pub sensor_options: StoredSensorOptions,
    /// Bytes 25..=49 of the report — unresolved semantics.  Hex-encoded
    /// (50-char lowercase).
    #[serde(with = "hex_25")]
    pub preserved_tail: [u8; 25],
}

impl Default for StoredDpiState {
    fn default() -> Self {
        Self {
            profile: 1,
            stages: vec![
                StoredDpiStage { dpi: 800 },
                StoredDpiStage { dpi: 1600 },
                StoredDpiStage { dpi: 3200 },
                StoredDpiStage { dpi: 6400 },
            ],
            active_stage: 1,
            sensor_options: StoredSensorOptions {
                lift_off_distance: 1,
                ripple_control: false,
                angle_snap: false,
                motion_sync: false,
            },
            preserved_tail: [0u8; 25],
        }
    }
}

/// DPI defaults using the captured empty-profile-1 tail.
///
/// This is NOT a general-purpose default — it MUST only be used where the
/// protocol API explicitly permits it (i.e. `captured_empty_profile_one`).
impl StoredDpiState {
    /// Captured empty-profile-1 tail bytes from stock-app observation.
    pub const CAPTURED_EMPTY_PROFILE_TAIL: [u8; 25] = [
        0xff, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0x00, 0x00, 0xff, 0xff,
        0xff, 0x00, 0xff, 0xff, 0x40, 0x00, 0xff, 0xff, 0xff, 0x01,
    ];

    /// Create a DPI state using the captured empty-profile-1 tail.
    ///
    /// Sourced as [`StateSource::ExplicitDefaults`] and MUST never be
    /// labelled [`StateVerification::Observed`].
    #[must_use]
    pub fn with_captured_tail(
        profile: u8,
        stages: Vec<StoredDpiStage>,
        active_stage: u8,
        sensor_options: StoredSensorOptions,
    ) -> Self {
        Self {
            profile,
            stages,
            active_stage,
            sensor_options,
            preserved_tail: Self::CAPTURED_EMPTY_PROFILE_TAIL,
        }
    }
}

// ── preferences ─────────────────────────────────────────────────────────────

/// Preferences (report `0x05`) in wire-safe form.
///
/// Matches the actual [`PreferencesState`] protocol type exactly.  There is
/// no `brightness`, `speed`, `motion_sync`, or `performance_mode` in the
/// FA61 preferences report — those were invented and are removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredPreferencesState {
    /// Lighting mode (raw firmware byte, 0–7).
    pub light_mode: u8,
    /// Configuration byte (raw firmware semantics).
    pub configuration: u8,
    /// Deep-sleep toggle byte (raw firmware semantics).
    pub deep_sleep: u8,
    /// Host-labeled RGB colour bytes (presence established; hardware
    /// effect is not).
    pub host_color: [u8; 3],
    /// Sleep timer in minutes.
    pub sleep_timer: u8,
    /// Debounce setting (raw firmware byte).
    pub debounce: u8,
}

impl Default for StoredPreferencesState {
    fn default() -> Self {
        Self {
            light_mode: 0x02,
            configuration: 0x01,
            deep_sleep: 0x00,
            host_color: [0xff, 0x00, 0x00],
            sleep_timer: 5,
            debounce: 0x00,
        }
    }
}

// ── buttons ─────────────────────────────────────────────────────────────────

/// A single button-assignment slot in wire-safe form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredButtonSlot {
    /// Raw action byte.
    pub action: u8,
    /// Raw modifier byte.
    pub modifier: u8,
    /// Raw key / action-value byte (internal name: `key_code` in protocol).
    pub key_code: u8,
}

/// Complete button-assignment table (report `0x08`) in wire-safe form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredButtonsState {
    /// Profile id (1-based).
    pub profile: u8,
    /// Exactly [`BUTTON_SLOT_COUNT`] slots in firmware order.
    pub slots: [StoredButtonSlot; BUTTON_SLOT_COUNT],
}

impl Default for StoredButtonsState {
    fn default() -> Self {
        Self {
            profile: 1,
            slots: [StoredButtonSlot::default(); BUTTON_SLOT_COUNT],
        }
    }
}

impl Default for StoredButtonSlot {
    fn default() -> Self {
        Self {
            action: 0x00,
            modifier: 0x00,
            key_code: 0x00,
        }
    }
}

// ── profile metadata ────────────────────────────────────────────────────────

/// Profile metadata (report `0x0c`) in wire-safe form.
///
/// Matches the [`ProfileMetadata`] protocol struct: `current` and `maximum`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredProfileMetadata {
    /// Currently active (1-based) profile.
    pub current: u8,
    /// Maximum enabled profile (1-based).
    pub maximum: u8,
}

// ── per-profile aggregate ───────────────────────────────────────────────────

/// All desired/per-profile state sections for a single profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredProfileState {
    /// DPI stage configuration, if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpi: Option<VersionedState<StoredDpiState>>,
    /// Preferences, if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferences: Option<VersionedState<StoredPreferencesState>>,
    /// Button assignments, if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buttons: Option<VersionedState<StoredButtonsState>>,
}

// ── device entry ────────────────────────────────────────────────────────────

/// All stored state relating to a single device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceEntry {
    /// Transport used to reach this device.
    pub transport: StoredTransport,
    /// How to select this device on the transport.
    pub selector: StoredSelector,
    /// Desired global polling rate in Hz (125, 250, 500, or 1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polling_rate: Option<VersionedState<u16>>,
    /// Per-profile desired state keyed by 1-based profile id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<u8, VersionedState<StoredProfileState>>,
    /// Cached profile metadata readback, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_metadata: Option<VersionedState<StoredProfileMetadata>>,
}

impl DeviceEntry {
    /// Create a minimal device entry for the given transport and selector.
    pub fn new(transport: StoredTransport, selector: StoredSelector) -> Self {
        Self {
            transport,
            selector,
            polling_rate: None,
            profiles: BTreeMap::new(),
            profile_metadata: None,
        }
    }
}

// ── root state file ─────────────────────────────────────────────────────────

/// Top-level durable state serialised as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateFile {
    /// Schema version — must equal [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Key of the currently-selected device, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_device: Option<String>,
    /// All known devices keyed by a stable user-chosen name.
    #[serde(default)]
    pub devices: BTreeMap<String, DeviceEntry>,
}

impl Default for StateFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            selected_device: None,
            devices: BTreeMap::new(),
        }
    }
}

// ── errors ──────────────────────────────────────────────────────────────────

/// Errors produced by state load, save, or validation.
#[derive(Debug, Error)]
pub enum StateError {
    /// The on-disk schema version is incompatible.
    #[error(
        "unsupported state schema version {found} (expected {expected}); \
         the state file was written by a different version of x3ctl"
    )]
    SchemaVersionMismatch { found: u32, expected: u32 },

    /// A lower-level I/O error.
    #[error("state I/O error: {0}")]
    Io(#[from] io::Error),

    /// A JSON serialisation or deserialisation error.
    #[error("state JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Errors produced by merge helpers when a partial update cannot be applied.
#[derive(Debug, Error)]
pub enum MergeError {
    #[cfg(any(feature = "ble", test))]
    /// A BLE partial update was requested without a stored baseline, and
    /// explicit defaults were not permitted.
    #[error(
        "no stored baseline for `{resource}` on device `{device_key}` profile {profile_id}; \
         use explicit-defaults to allow built-in defaults"
    )]
    MissingBaseline {
        device_key: String,
        profile_id: u8,
        resource: &'static str,
    },

    /// The requested device does not exist in state.
    #[error("no device entry for `{0}`")]
    NoDevice(String),
}

// ── merge delta types (state-local, not dependent on wire) ──────────────────

#[cfg(any(feature = "ble", test))]
/// Partial DPI update for merging into stored state.
#[derive(Debug, Clone)]
pub struct DpiMergeDelta {
    /// New DPI values (replaces all stages when `Some`).
    pub stages: Option<Vec<u16>>,
    /// New active stage (1-based).
    pub active_stage: Option<u8>,
    /// Sensor tweaks — omitted fields are unchanged.
    pub sensor: Option<SensorMergeDelta>,
}

#[cfg(any(feature = "ble", test))]
/// Partial sensor-options update.
#[derive(Debug, Clone, Copy)]
pub struct SensorMergeDelta {
    pub lift_off_distance: Option<u8>,
    pub ripple_control: Option<bool>,
    pub angle_snap: Option<bool>,
    pub motion_sync: Option<bool>,
}

#[cfg(any(feature = "ble", test))]
/// Partial preferences update for merging into stored state.
#[derive(Debug, Clone, Copy)]
pub struct PrefsMergeDelta {
    pub light_mode: Option<u8>,
    pub configuration: Option<u8>,
    pub deep_sleep: Option<u8>,
    pub host_color: Option<[u8; 3]>,
    pub sleep_timer: Option<u8>,
    pub debounce: Option<u8>,
}

// ── path resolution ─────────────────────────────────────────────────────────

/// Return the canonical path to the state file.
///
/// Resolution order:
/// 1. `X3CTL_STATE_PATH` environment variable (absolute path).
/// 2. `%LOCALAPPDATA%/x3ctl/state.json` on Windows.
/// 3. `$XDG_STATE_HOME/x3ctl/state.json`.
/// 4. `$HOME/.local/state/x3ctl/state.json`.
pub fn state_path() -> PathBuf {
    if let Ok(path) = std::env::var("X3CTL_STATE_PATH") {
        let p = PathBuf::from(path);
        if p.is_absolute() {
            return p;
        }
    }

    #[cfg(windows)]
    {
        if let Ok(local_appdata) = std::env::var("LOCALAPPDATA") {
            return PathBuf::from(local_appdata)
                .join("x3ctl")
                .join("state.json");
        }
    }

    if let Ok(xdg_state_home) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(xdg_state_home)
            .join("x3ctl")
            .join("state.json");
    }

    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("x3ctl")
            .join("state.json");
    }

    // Ultimate fallback — unlikely to be useful, but avoids panic.
    PathBuf::from("x3ctl-state.json")
}

// ── load / save ─────────────────────────────────────────────────────────────

/// Load state from the canonical path.
///
/// Returns a default [`StateFile`] when the file does not exist.  Rejects files
/// whose `schema_version` differs from [`SCHEMA_VERSION`].
///
/// # Errors
///
/// Returns [`StateError`] on I/O failure, invalid JSON, or schema mismatch.
pub fn load() -> Result<StateFile, StateError> {
    load_from(state_path())
}

/// Load state from an explicit `path`.
///
/// See [`load`] for semantics.
pub fn load_from(path: impl AsRef<Path>) -> Result<StateFile, StateError> {
    let path = path.as_ref();
    match std::fs::read_to_string(path) {
        Ok(json) => {
            let state: StateFile = serde_json::from_str(&json)?;
            if state.schema_version != SCHEMA_VERSION {
                return Err(StateError::SchemaVersionMismatch {
                    found: state.schema_version,
                    expected: SCHEMA_VERSION,
                });
            }
            Ok(state)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(StateFile::default()),
        Err(e) => Err(StateError::Io(e)),
    }
}

/// Atomically save `state` to the canonical path.
///
/// Serialises to a temporary file in the same directory, then renames it over
/// the target.  The parent directory is created if it does not exist.
///
/// # Errors
///
/// Returns [`StateError`] on serialisation, I/O, or directory-creation failure.
pub fn save(state: &StateFile) -> Result<(), StateError> {
    save_to(state, state_path())
}

/// Atomically save `state` to an explicit `path`.
///
/// See [`save`] for semantics.
pub fn save_to(state: &StateFile, path: impl AsRef<Path>) -> Result<(), StateError> {
    let path = path.as_ref();
    let json = serde_json::to_string_pretty(state)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Write to a sibling temp file, then atomically rename.
    let tmp = temp_sibling(path)?;
    std::fs::write(&tmp, json.as_bytes())?;
    std::fs::rename(&tmp, path)?;

    Ok(())
}

/// Generate a `.tmp` path in the same directory as `target`.
fn temp_sibling(target: &Path) -> io::Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let pid = std::process::id();
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let fname = target.file_name().unwrap_or_default().to_string_lossy();
    // Insert the tmp token before the final extension, or append when there is
    // no extension.  E.g. "state.json" → "state.1234.5678.tmp.json".
    let tmp_name = if let Some(dot) = fname.rfind('.') {
        format!("{}.{pid}.{nanos}.tmp.{}", &fname[..dot], &fname[dot + 1..])
    } else {
        format!("{fname}.{pid}.{nanos}.tmp")
    };
    Ok(parent.join(tmp_name))
}

// ── helpers ─────────────────────────────────────────────────────────────────

/// Return the current UTC time as an ISO-8601 string (e.g. `"2026-07-24T12:34:56Z"`).
///
/// Falls back to `"1970-01-01T00:00:00Z"` when the system clock is before the
/// Unix epoch (unlikely but handled for safety).
pub fn utc_now_iso8601() -> String {
    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(dur) => {
            let secs = dur.as_secs();
            // Decompose into calendar fields manually to avoid chrono dep.
            unix_to_iso8601(secs)
        }
        Err(_) => String::from("1970-01-01T00:00:00Z"),
    }
}

/// Convert a Unix timestamp (seconds) to an ISO-8601 UTC string.
fn unix_to_iso8601(unix: u64) -> String {
    // Days since epoch, accounting for leap-year rules through 2100.
    let total_days = unix / 86400;
    let secs_of_day = unix % 86400;

    let hours = secs_of_day / 3600;
    let minutes = (secs_of_day % 3600) / 60;
    let seconds = secs_of_day % 60;

    // Civil date from days since 1970-01-01 (Gregorian).
    let (year, month, day) = civil_from_days(total_days as i64);

    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

/// Convert days since 1970-01-01 to (year, month, day).
fn civil_from_days(mut days: i64) -> (i64, u32, u32) {
    days += 719468; // shift epoch to 0000-03-01 (start of Gregorian cycle)
    let era = if days >= 0 { days } else { days - 146096 } / 146097;
    let doe = days - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month as u32, day as u32)
}

// ── serde helpers ───────────────────────────────────────────────────────────

/// Serde module for hex-encoding `[u8; 25]` as a 50-char lowercase string.
mod hex_25 {
    use serde::de::Error;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 25], s: S) -> Result<S::Ok, S::Error> {
        let mut hex = String::with_capacity(50);
        for b in bytes {
            hex.push(HEX_CHARS[(b >> 4) as usize]);
            hex.push(HEX_CHARS[(b & 0x0f) as usize]);
        }
        s.serialize_str(&hex)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 25], D::Error> {
        let hex_str = String::deserialize(d)?;
        if hex_str.len() != 50 {
            return Err(D::Error::custom(format!(
                "preserved_tail must be 50 hex chars, got {}",
                hex_str.len()
            )));
        }
        let mut out = [0u8; 25];
        for (i, chunk) in hex_str.as_bytes().chunks(2).enumerate() {
            let hi = hex_val(chunk[0]).ok_or_else(|| {
                D::Error::custom(format!("invalid hex char '{}'", chunk[0] as char))
            })?;
            let lo = hex_val(chunk[1]).ok_or_else(|| {
                D::Error::custom(format!("invalid hex char '{}'", chunk[1] as char))
            })?;
            out[i] = hi << 4 | lo;
        }
        Ok(out)
    }

    fn hex_val(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }

    const HEX_CHARS: [char; 16] = [
        '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
    ];
}

// ── selection helpers ───────────────────────────────────────────────────────

/// Return the key of the currently-selected device, if any.
#[must_use]
pub fn selected_device(state: &StateFile) -> Option<&str> {
    state.selected_device.as_deref()
}

/// Set the currently-selected device.
pub fn select_device(state: &mut StateFile, key: &str) {
    state.selected_device = Some(key.to_owned());
}

// ── lookup / patch helpers ──────────────────────────────────────────────────

/// Ensure a device entry exists, creating a minimal one if necessary.
/// Returns a mutable reference to the entry.
pub fn ensure_device<'s>(
    state: &'s mut StateFile,
    key: &str,
    transport: StoredTransport,
    selector: StoredSelector,
) -> &'s mut DeviceEntry {
    state
        .devices
        .entry(key.to_owned())
        .or_insert_with(|| DeviceEntry::new(transport, selector))
}

/// Look up a profile's state within a device (immutable).
///
/// Returns `None` if the device or profile is absent.
#[cfg(any(feature = "ble", test))]
#[must_use]
pub fn profile<'s>(
    state: &'s StateFile,
    device_key: &str,
    profile_id: u8,
) -> Option<&'s VersionedState<StoredProfileState>> {
    state.devices.get(device_key)?.profiles.get(&profile_id)
}

/// Look up a profile's state within a device (mutable).
///
/// Returns `None` if the device is absent.  The profile entry is created on
/// demand with default values and [`StateSource::ExplicitDefaults`]
/// / [`StateVerification::PersistenceUnknown`] provenance.
pub fn profile_mut<'s>(
    state: &'s mut StateFile,
    device_key: &str,
    profile_id: u8,
) -> Option<&'s mut VersionedState<StoredProfileState>> {
    let dev = state.devices.get_mut(device_key)?;
    Some(dev.profiles.entry(profile_id).or_insert_with(|| {
        VersionedState::now(
            StoredProfileState::default(),
            StateSource::ExplicitDefaults,
            StateVerification::PersistenceUnknown,
        )
    }))
}

/// Patch the DPI state for a profile within a device.
///
/// Creates the device and profile entries on demand if they do not exist.
pub fn patch_dpi(
    state: &mut StateFile,
    device_key: &str,
    profile_id: u8,
    dpi: StoredDpiState,
    source: StateSource,
    verification: StateVerification,
) {
    let dev = ensure_device(
        state,
        device_key,
        // These transport/selector values are placeholders; the caller should
        // have already populated a real entry via `ensure_device` or `load`.
        StoredTransport::Wired,
        StoredSelector::Unique,
    );
    let profile = dev.profiles.entry(profile_id).or_insert_with(|| {
        VersionedState::now(
            StoredProfileState::default(),
            StateSource::ExplicitDefaults,
            StateVerification::PersistenceUnknown,
        )
    });
    profile.value.dpi = Some(VersionedState::now(dpi, source, verification));
}

/// Patch the preferences state for a profile within a device.
pub fn patch_preferences(
    state: &mut StateFile,
    device_key: &str,
    profile_id: u8,
    prefs: StoredPreferencesState,
    source: StateSource,
    verification: StateVerification,
) {
    let dev = ensure_device(
        state,
        device_key,
        StoredTransport::Wired,
        StoredSelector::Unique,
    );
    let profile = dev.profiles.entry(profile_id).or_insert_with(|| {
        VersionedState::now(
            StoredProfileState::default(),
            StateSource::ExplicitDefaults,
            StateVerification::PersistenceUnknown,
        )
    });
    profile.value.preferences = Some(VersionedState::now(prefs, source, verification));
}

/// Patch the button-assignment state for a profile within a device.
pub fn patch_buttons(
    state: &mut StateFile,
    device_key: &str,
    profile_id: u8,
    buttons: StoredButtonsState,
    source: StateSource,
    verification: StateVerification,
) {
    let dev = ensure_device(
        state,
        device_key,
        StoredTransport::Wired,
        StoredSelector::Unique,
    );
    let profile = dev.profiles.entry(profile_id).or_insert_with(|| {
        VersionedState::now(
            StoredProfileState::default(),
            StateSource::ExplicitDefaults,
            StateVerification::PersistenceUnknown,
        )
    });
    profile.value.buttons = Some(VersionedState::now(buttons, source, verification));
}

/// Update the global polling rate for a device.
pub fn patch_polling_rate(
    state: &mut StateFile,
    device_key: &str,
    rate_hz: u16,
    source: StateSource,
    verification: StateVerification,
) {
    let dev = ensure_device(
        state,
        device_key,
        StoredTransport::Wired,
        StoredSelector::Unique,
    );
    dev.polling_rate = Some(VersionedState::now(rate_hz, source, verification));
}

/// Update the cached profile metadata for a device.
pub fn patch_profile_metadata(
    state: &mut StateFile,
    device_key: &str,
    metadata: StoredProfileMetadata,
    source: StateSource,
    verification: StateVerification,
) {
    let dev = ensure_device(
        state,
        device_key,
        StoredTransport::Wired,
        StoredSelector::Unique,
    );
    dev.profile_metadata = Some(VersionedState::now(metadata, source, verification));
}

// ── merge helpers (safe partial updates with baseline enforcement) ──────────

/// Resolve the baseline for a BLE-sensitive merge.
///
/// When a stored baseline exists, returns it.  When absent and
/// `explicit_defaults_allowed` is `true`, returns the explicit default.
/// Otherwise returns [`MergeError::MissingBaseline`].
#[cfg(any(feature = "ble", test))]
fn resolve_ble_baseline<T: Clone>(
    stored: Option<&VersionedState<T>>,
    device_key: &str,
    profile_id: u8,
    resource: &'static str,
    explicit_defaults_allowed: bool,
    default_value: T,
) -> Result<T, MergeError> {
    match stored {
        Some(vs) => {
            // When explicit defaults are not permitted, reject baselines
            // that were synthesised from defaults — they have never been
            // confirmed by hardware or import.
            if !explicit_defaults_allowed && vs.source == StateSource::ExplicitDefaults {
                return Err(MergeError::MissingBaseline {
                    device_key: device_key.to_owned(),
                    profile_id,
                    resource,
                });
            }
            Ok(vs.value.clone())
        }
        None if explicit_defaults_allowed => Ok(default_value),
        None => Err(MergeError::MissingBaseline {
            device_key: device_key.to_owned(),
            profile_id,
            resource,
        }),
    }
}

/// Merge a partial DPI update into the stored state for a profile.
///
/// Partial updates preserve the complete baseline: omitted stages/sensor
/// fields leave the existing stored value unchanged.  The `preserved_tail`
/// from the stored baseline is always carried forward.
///
/// When no stored DPI baseline exists for the profile and the transport
/// cannot read it back (BLE), this returns [`MergeError::MissingBaseline`]
/// unless `explicit_defaults_allowed` is `true`, in which case explicit
/// defaults (with the captured empty-profile-1 tail) are used.
///
/// # Errors
///
/// Returns [`MergeError::NoDevice`] when the device key is absent.
/// Returns [`MergeError::MissingBaseline`] when no baseline exists and
/// explicit defaults are not permitted.
#[cfg(any(feature = "ble", test))]
pub fn merge_dpi_delta(
    state: &mut StateFile,
    device_key: &str,
    profile_id: u8,
    delta: &DpiMergeDelta,
    source: StateSource,
    verification: StateVerification,
    explicit_defaults_allowed: bool,
) -> Result<(), MergeError> {
    let dev = state
        .devices
        .get_mut(device_key)
        .ok_or_else(|| MergeError::NoDevice(device_key.to_owned()))?;

    // Resolve existing baseline or explicit default.
    let existing_stored = dev
        .profiles
        .get(&profile_id)
        .and_then(|p| p.value.dpi.as_ref());

    let mut merged = resolve_ble_baseline(
        existing_stored,
        device_key,
        profile_id,
        "dpi",
        explicit_defaults_allowed,
        StoredDpiState::default(),
    )?;

    // Apply delta.
    merged.profile = profile_id;

    if let Some(ref stages) = delta.stages {
        merged.stages = stages.iter().map(|&dpi| StoredDpiStage { dpi }).collect();
    }

    if let Some(active) = delta.active_stage {
        merged.active_stage = active;
    }

    if let Some(sensor_delta) = delta.sensor {
        if let Some(lod) = sensor_delta.lift_off_distance {
            merged.sensor_options.lift_off_distance = lod;
        }
        if let Some(rc) = sensor_delta.ripple_control {
            merged.sensor_options.ripple_control = rc;
        }
        if let Some(asnap) = sensor_delta.angle_snap {
            merged.sensor_options.angle_snap = asnap;
        }
        if let Some(ms) = sensor_delta.motion_sync {
            merged.sensor_options.motion_sync = ms;
        }
    }

    let profile = dev.profiles.entry(profile_id).or_insert_with(|| {
        VersionedState::now(
            StoredProfileState::default(),
            StateSource::ExplicitDefaults,
            StateVerification::PersistenceUnknown,
        )
    });
    profile.value.dpi = Some(VersionedState::now(merged, source, verification));

    Ok(())
}

/// Merge a partial preferences update into the stored state for a profile.
///
/// Partial updates preserve the complete baseline: every field in the delta
/// is `Option`; omitted fields leave the existing stored value unchanged.
///
/// When no stored preferences baseline exists for the profile and the
/// transport cannot read it back (BLE), this returns
/// [`MergeError::MissingBaseline`] unless `explicit_defaults_allowed` is
/// `true`, in which case explicit defaults are used.
///
/// # Errors
///
/// Returns [`MergeError::NoDevice`] when the device key is absent.
/// Returns [`MergeError::MissingBaseline`] when no baseline exists and
/// explicit defaults are not permitted.
#[cfg(any(feature = "ble", test))]
pub fn merge_prefs_delta(
    state: &mut StateFile,
    device_key: &str,
    profile_id: u8,
    delta: &PrefsMergeDelta,
    source: StateSource,
    verification: StateVerification,
    explicit_defaults_allowed: bool,
) -> Result<(), MergeError> {
    let dev = state
        .devices
        .get_mut(device_key)
        .ok_or_else(|| MergeError::NoDevice(device_key.to_owned()))?;

    let existing_stored = dev
        .profiles
        .get(&profile_id)
        .and_then(|p| p.value.preferences.as_ref());

    let mut merged = resolve_ble_baseline(
        existing_stored,
        device_key,
        profile_id,
        "preferences",
        explicit_defaults_allowed,
        StoredPreferencesState::default(),
    )?;

    if let Some(v) = delta.light_mode {
        merged.light_mode = v;
    }
    if let Some(v) = delta.configuration {
        merged.configuration = v;
    }
    if let Some(v) = delta.deep_sleep {
        merged.deep_sleep = v;
    }
    if let Some(v) = delta.host_color {
        merged.host_color = v;
    }
    if let Some(v) = delta.sleep_timer {
        merged.sleep_timer = v;
    }
    if let Some(v) = delta.debounce {
        merged.debounce = v;
    }

    let profile = dev.profiles.entry(profile_id).or_insert_with(|| {
        VersionedState::now(
            StoredProfileState::default(),
            StateSource::ExplicitDefaults,
            StateVerification::PersistenceUnknown,
        )
    });
    profile.value.preferences = Some(VersionedState::now(merged, source, verification));

    Ok(())
}

/// Merge a single button-slot update into the stored state for a profile.
///
/// Partial updates preserve the complete baseline: only the specified slot
/// index is modified; all other slots are unchanged.
///
/// When no stored buttons baseline exists for the profile and the transport
/// cannot read it back (BLE), this returns [`MergeError::MissingBaseline`]
/// unless `explicit_defaults_allowed` is `true`, in which case explicit
/// defaults are used.
///
/// # Errors
///
/// Returns [`MergeError::NoDevice`] when the device key is absent.
/// Returns [`MergeError::MissingBaseline`] when no baseline exists and
/// explicit defaults are not permitted.
#[cfg(any(feature = "ble", test))]
pub fn merge_button_delta(
    state: &mut StateFile,
    device_key: &str,
    profile_id: u8,
    slot_index: usize,
    action: u8,
    modifier: u8,
    key_code: u8,
    source: StateSource,
    verification: StateVerification,
    explicit_defaults_allowed: bool,
) -> Result<(), MergeError> {
    let dev = state
        .devices
        .get_mut(device_key)
        .ok_or_else(|| MergeError::NoDevice(device_key.to_owned()))?;

    let existing_stored = dev
        .profiles
        .get(&profile_id)
        .and_then(|p| p.value.buttons.as_ref());

    let mut merged = resolve_ble_baseline(
        existing_stored,
        device_key,
        profile_id,
        "buttons",
        explicit_defaults_allowed,
        StoredButtonsState::default(),
    )?;

    if slot_index < BUTTON_SLOT_COUNT {
        merged.slots[slot_index] = StoredButtonSlot {
            action,
            modifier,
            key_code,
        };
    }

    let profile = dev.profiles.entry(profile_id).or_insert_with(|| {
        VersionedState::now(
            StoredProfileState::default(),
            StateSource::ExplicitDefaults,
            StateVerification::PersistenceUnknown,
        )
    });
    profile.value.buttons = Some(VersionedState::now(merged, source, verification));

    Ok(())
}

// ── state management operations ─────────────────────────────────────────────

/// Initialise a device profile with explicit defaults.
///
/// DPI defaults use the captured empty-profile-1 tail.  All values are
/// sourced as [`StateSource::ExplicitDefaults`] with
/// [`StateVerification::PersistenceUnknown`].
///
/// Returns `true` if a new profile was created, `false` if it already
/// existed and was left untouched.
pub fn init_profile_defaults(
    state: &mut StateFile,
    device_key: &str,
    profile_id: u8,
) -> Result<bool, MergeError> {
    let dev = state
        .devices
        .get_mut(device_key)
        .ok_or_else(|| MergeError::NoDevice(device_key.to_owned()))?;

    if dev.profiles.contains_key(&profile_id) {
        return Ok(false);
    }

    let default_dpi = StoredDpiState::with_captured_tail(
        profile_id,
        StoredDpiState::default().stages,
        StoredDpiState::default().active_stage,
        StoredDpiState::default().sensor_options,
    );

    let profile = VersionedState::now(
        StoredProfileState {
            dpi: Some(VersionedState::now(
                default_dpi,
                StateSource::ExplicitDefaults,
                StateVerification::PersistenceUnknown,
            )),
            preferences: Some(VersionedState::now(
                StoredPreferencesState::default(),
                StateSource::ExplicitDefaults,
                StateVerification::PersistenceUnknown,
            )),
            buttons: Some(VersionedState::now(
                StoredButtonsState::default(),
                StateSource::ExplicitDefaults,
                StateVerification::PersistenceUnknown,
            )),
        },
        StateSource::ExplicitDefaults,
        StateVerification::PersistenceUnknown,
    );

    dev.profiles.insert(profile_id, profile);
    Ok(true)
}

/// Remove a device and all its state.  If it was the selected device, clear the
/// selection.  Returns `true` when a device was actually removed.
pub fn forget_device(state: &mut StateFile, key: &str) -> bool {
    let removed = state.devices.remove(key).is_some();
    if removed && state.selected_device.as_deref() == Some(key) {
        state.selected_device = None;
    }
    removed
}

/// Remove a single profile from a device.  Returns `true` when the profile
/// existed and was removed.
pub fn forget_profile(state: &mut StateFile, device_key: &str, profile_id: u8) -> bool {
    if let Some(dev) = state.devices.get_mut(device_key) {
        dev.profiles.remove(&profile_id).is_some()
    } else {
        false
    }
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Return a fresh path in the system temp directory that is safe for
    /// concurrent test runs.
    fn temp_state_path(label: &str) -> PathBuf {
        let pid = std::process::id();
        let tid = std::thread::current().id();
        let tid_str = format!("{tid:?}").replace(['(', ')'], "");
        std::env::temp_dir().join(format!("x3ctl-test-{label}-{pid}-{tid_str}.json"))
    }

    /// Remove a test file if it exists.
    fn clean(path: &Path) {
        let _ = fs::remove_file(path);
    }

    // ── round-trip ──────────────────────────────────────────────────────

    #[test]
    fn round_trip_preserves_all_fields() {
        let path = temp_state_path("roundtrip");
        clean(&path);

        let mut state = StateFile::default();
        state.selected_device = Some("my-x3".to_owned());

        let dev = ensure_device(
            &mut state,
            "my-x3",
            StoredTransport::Wired,
            StoredSelector::Path(r"\\?\HID#VID_1D57&PID_FA61#...".to_owned()),
        );
        dev.polling_rate = Some(VersionedState::now(
            1000u16,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
        ));
        dev.profile_metadata = Some(VersionedState::now(
            StoredProfileMetadata {
                current: 2,
                maximum: 3,
            },
            StateSource::UsbReadback,
            StateVerification::Observed,
        ));

        // Add a profile with DPI and preferences.
        let profile = dev.profiles.entry(1).or_insert_with(|| {
            VersionedState::now(
                StoredProfileState::default(),
                StateSource::ExplicitDefaults,
                StateVerification::PersistenceUnknown,
            )
        });
        profile.value.dpi = Some(VersionedState::now(
            StoredDpiState::default(),
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
        ));
        profile.value.preferences = Some(VersionedState::now(
            StoredPreferencesState::default(),
            StateSource::Imported,
            StateVerification::PersistenceUnknown,
        ));

        save_to(&state, &path).expect("save should succeed");

        let loaded = load_from(&path).expect("load should succeed");
        assert_eq!(loaded, state, "round-trip must be exact");

        clean(&path);
    }

    // ── missing file returns default ────────────────────────────────────

    #[test]
    fn missing_file_returns_default() {
        let path = temp_state_path("missing");
        clean(&path);

        let state = load_from(&path).expect("missing file should load as default");
        assert_eq!(state, StateFile::default());
        assert!(state.devices.is_empty());
        assert!(state.selected_device.is_none());
        assert_eq!(state.schema_version, SCHEMA_VERSION);
    }

    // ── schema mismatch ─────────────────────────────────────────────────

    #[test]
    fn schema_mismatch_rejects() {
        let path = temp_state_path("mismatch");
        clean(&path);

        let bad_json = r#"{"schema_version":999,"devices":{},"selected_device":null}"#;
        fs::write(&path, bad_json).expect("write should succeed");

        let err = load_from(&path).expect_err("should reject mismatched version");
        match err {
            StateError::SchemaVersionMismatch { found, expected } => {
                assert_eq!(found, 999);
                assert_eq!(expected, SCHEMA_VERSION);
            }
            other => panic!("expected SchemaVersionMismatch, got {other:?}"),
        }

        clean(&path);
    }

    #[test]
    fn schema_match_accepts() {
        let path = temp_state_path("match");
        clean(&path);

        let state = StateFile::default();
        save_to(&state, &path).expect("save should succeed");
        let loaded = load_from(&path).expect("matching version should load");
        assert_eq!(loaded.schema_version, SCHEMA_VERSION);

        clean(&path);
    }

    // ── selection ───────────────────────────────────────────────────────

    #[test]
    fn selection_helpers_work() {
        let mut state = StateFile::default();
        assert!(selected_device(&state).is_none());

        select_device(&mut state, "my-x3");
        assert_eq!(selected_device(&state), Some("my-x3"));

        state.selected_device = None;
        assert!(selected_device(&state).is_none());
    }

    // ── provenance preservation ─────────────────────────────────────────

    #[test]
    fn provenance_preserved_through_round_trip() {
        let path = temp_state_path("provenance");
        clean(&path);

        let mut state = StateFile::default();
        ensure_device(
            &mut state,
            "dev1",
            StoredTransport::Receiver,
            StoredSelector::Unique,
        );

        patch_dpi(
            &mut state,
            "dev1",
            1,
            StoredDpiState::default(),
            StateSource::UsbReadback,
            StateVerification::Observed,
        );
        patch_preferences(
            &mut state,
            "dev1",
            1,
            StoredPreferencesState::default(),
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
        );
        patch_buttons(
            &mut state,
            "dev1",
            2,
            StoredButtonsState::default(),
            StateSource::ExplicitDefaults,
            StateVerification::PersistenceUnknown,
        );
        patch_polling_rate(
            &mut state,
            "dev1",
            500,
            StateSource::Imported,
            StateVerification::AckAccepted,
        );

        save_to(&state, &path).expect("save");
        let loaded = load_from(&path).expect("load");

        // Verify DPI provenance on profile 1.
        let p1 = profile(&loaded, "dev1", 1).expect("profile 1 exists");
        let dpi = p1.value.dpi.as_ref().expect("dpi present");
        assert_eq!(dpi.source, StateSource::UsbReadback);
        assert_eq!(dpi.verification, StateVerification::Observed);
        assert!(!dpi.updated_at.is_empty());

        // Verify preferences provenance on profile 1.
        let prefs = p1.value.preferences.as_ref().expect("prefs present");
        assert_eq!(prefs.source, StateSource::LocallyWritten);
        assert_eq!(prefs.verification, StateVerification::ApplicationUnknown);

        // Verify buttons on profile 2.
        let p2 = profile(&loaded, "dev1", 2).expect("profile 2 exists");
        let btns = p2.value.buttons.as_ref().expect("buttons present");
        assert_eq!(btns.source, StateSource::ExplicitDefaults);
        assert_eq!(btns.verification, StateVerification::PersistenceUnknown);

        // Verify polling rate.
        let dev = loaded.devices.get("dev1").expect("device exists");
        let rate = dev.polling_rate.as_ref().expect("rate present");
        assert_eq!(rate.value, 500);
        assert_eq!(rate.source, StateSource::Imported);
        assert_eq!(rate.verification, StateVerification::AckAccepted);

        clean(&path);
    }

    // ── atomic replacement ──────────────────────────────────────────────

    #[test]
    fn atomic_save_does_not_leave_temp_file() {
        let path = temp_state_path("atomic");
        clean(&path);

        let state = StateFile::default();
        save_to(&state, &path).expect("save should succeed");

        let raw = fs::read_to_string(&path).expect("file should exist");
        assert!(raw.contains("\"schema_version\""));

        let parent = path.parent().unwrap();
        let tmp_files: Vec<_> = fs::read_dir(parent)
            .expect("read dir")
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name().to_string_lossy().contains("atomic")
                    && e.file_name().to_string_lossy().ends_with(".tmp.json")
            })
            .collect();
        assert!(
            tmp_files.is_empty(),
            "no temp files should remain: {tmp_files:?}"
        );

        clean(&path);
    }

    #[test]
    fn atomic_save_overwrites_existing() {
        let path = temp_state_path("overwrite");
        clean(&path);

        let mut state = StateFile::default();
        select_device(&mut state, "first");
        save_to(&state, &path).expect("first save");

        let mut state2 = StateFile::default();
        select_device(&mut state2, "second");
        save_to(&state2, &path).expect("second save");

        let loaded = load_from(&path).expect("load after overwrite");
        assert_eq!(selected_device(&loaded), Some("second"));

        clean(&path);
    }

    // ── forget ──────────────────────────────────────────────────────────

    #[test]
    fn forget_device_clears_selection() {
        let mut state = StateFile::default();
        ensure_device(
            &mut state,
            "dev-a",
            StoredTransport::Wired,
            StoredSelector::Unique,
        );
        select_device(&mut state, "dev-a");

        let removed = forget_device(&mut state, "dev-a");
        assert!(removed);
        assert!(state.devices.get("dev-a").is_none());
        assert!(selected_device(&state).is_none());
    }

    #[test]
    fn forget_device_returns_false_for_unknown() {
        let mut state = StateFile::default();
        assert!(!forget_device(&mut state, "nobody"));
    }

    #[test]
    fn forget_profile_removes_single_entry() {
        let mut state = StateFile::default();
        let dev = ensure_device(
            &mut state,
            "dev1",
            StoredTransport::Ble,
            StoredSelector::UniqueConnected,
        );
        dev.profiles.insert(
            1,
            VersionedState::now(
                StoredProfileState::default(),
                StateSource::ExplicitDefaults,
                StateVerification::PersistenceUnknown,
            ),
        );
        dev.profiles.insert(
            3,
            VersionedState::now(
                StoredProfileState::default(),
                StateSource::ExplicitDefaults,
                StateVerification::PersistenceUnknown,
            ),
        );

        assert!(forget_profile(&mut state, "dev1", 3));
        assert!(profile(&state, "dev1", 1).is_some());
        assert!(profile(&state, "dev1", 3).is_none());

        assert!(!forget_profile(&mut state, "dev1", 3));
    }

    #[test]
    fn forget_profile_unknown_device_returns_false() {
        let mut state = StateFile::default();
        assert!(!forget_profile(&mut state, "nobody", 1));
    }

    // ── profile_mut lazy creation ───────────────────────────────────────

    #[test]
    fn profile_mut_creates_entry_with_default_provenance() {
        let mut state = StateFile::default();
        ensure_device(
            &mut state,
            "dev1",
            StoredTransport::Receiver,
            StoredSelector::Path("some-path".to_owned()),
        );

        let p = profile_mut(&mut state, "dev1", 2).expect("device exists");
        assert_eq!(p.source, StateSource::ExplicitDefaults);
        assert_eq!(p.verification, StateVerification::PersistenceUnknown);
        assert!(p.value.dpi.is_none());
        assert!(p.value.preferences.is_none());
        assert!(p.value.buttons.is_none());
    }

    #[test]
    fn profile_mut_missing_device() {
        let mut state = StateFile::default();
        assert!(profile_mut(&mut state, "nobody", 1).is_none());
    }

    // ── device lookup ───────────────────────────────────────────────────

    #[test]
    fn device_lookup() {
        let mut state = StateFile::default();
        ensure_device(
            &mut state,
            "dev1",
            StoredTransport::Ble,
            StoredSelector::BleName("Attack Shark X3".to_owned()),
        );

        assert!(state.devices.get("dev1").is_some());
        assert!(state.devices.get("dev2").is_none());
    }

    // ── profile lookup ──────────────────────────────────────────────────

    #[test]
    fn profile_lookup() {
        let mut state = StateFile::default();
        let dev = ensure_device(
            &mut state,
            "dev1",
            StoredTransport::Wired,
            StoredSelector::Unique,
        );
        dev.profiles.insert(
            1,
            VersionedState::now(
                StoredProfileState::default(),
                StateSource::ExplicitDefaults,
                StateVerification::PersistenceUnknown,
            ),
        );

        assert!(profile(&state, "dev1", 1).is_some());
        assert!(profile(&state, "dev1", 5).is_none());
        assert!(profile(&state, "missing", 1).is_none());
    }

    // ── invalid JSON ────────────────────────────────────────────────────

    #[test]
    fn invalid_json_returns_error() {
        let path = temp_state_path("invalid");
        clean(&path);

        fs::write(&path, b"not json").expect("write");
        let err = load_from(&path).expect_err("should fail on invalid JSON");
        assert!(matches!(err, StateError::Json(_)));

        clean(&path);
    }

    // ── patch_profile_metadata ──────────────────────────────────────────

    #[test]
    fn patch_profile_metadata_works() {
        let mut state = StateFile::default();
        let meta = StoredProfileMetadata {
            current: 2,
            maximum: 3,
        };
        patch_profile_metadata(
            &mut state,
            "dev1",
            meta,
            StateSource::UsbReadback,
            StateVerification::Observed,
        );

        let dev = state.devices.get("dev1").expect("device created");
        let stored = dev.profile_metadata.as_ref().expect("metadata present");
        assert_eq!(stored.value.current, 2);
        assert_eq!(stored.value.maximum, 3);
        assert_eq!(stored.source, StateSource::UsbReadback);
        assert_eq!(stored.verification, StateVerification::Observed);
    }

    // ── ensure_device is idempotent for transport/selector ──────────────

    #[test]
    fn ensure_device_preserves_existing_entry() {
        let mut state = StateFile::default();
        let d1 = ensure_device(
            &mut state,
            "d",
            StoredTransport::Wired,
            StoredSelector::Unique,
        );
        d1.polling_rate = Some(VersionedState::now(
            1000u16,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
        ));

        let d2 = ensure_device(
            &mut state,
            "d",
            StoredTransport::Ble,
            StoredSelector::UniqueConnected,
        );
        assert_eq!(d2.transport, StoredTransport::Wired);
        assert_eq!(d2.selector, StoredSelector::Unique);
        assert!(d2.polling_rate.is_some());
    }

    // ── default StoredDpiState properties ───────────────────────────────

    #[test]
    fn default_dpi_state_has_four_stages_and_preserved_tail() {
        let dpi = StoredDpiState::default();
        assert_eq!(dpi.stages.len(), 4);
        assert_eq!(dpi.stages[0].dpi, 800);
        assert_eq!(dpi.stages[1].dpi, 1600);
        assert_eq!(dpi.stages[2].dpi, 3200);
        assert_eq!(dpi.stages[3].dpi, 6400);
        assert_eq!(dpi.active_stage, 1);
        assert_eq!(dpi.profile, 1);
        assert_eq!(dpi.sensor_options.lift_off_distance, 1);
        assert!(!dpi.sensor_options.ripple_control);
        assert!(!dpi.sensor_options.angle_snap);
        assert!(!dpi.sensor_options.motion_sync);
        assert_eq!(dpi.preserved_tail, [0u8; 25]);
    }

    #[test]
    fn dpi_with_captured_tail_uses_correct_tail() {
        let dpi = StoredDpiState::with_captured_tail(
            2,
            vec![StoredDpiStage { dpi: 400 }],
            1,
            StoredSensorOptions {
                lift_off_distance: 2,
                ripple_control: false,
                angle_snap: false,
                motion_sync: false,
            },
        );
        assert_eq!(
            dpi.preserved_tail,
            StoredDpiState::CAPTURED_EMPTY_PROFILE_TAIL
        );
        assert_eq!(dpi.profile, 2);
        assert_eq!(dpi.stages.len(), 1);
    }

    // ── DPI stage has no color field ────────────────────────────────────

    #[test]
    fn stored_dpi_stage_has_only_dpi() {
        let stage = StoredDpiStage { dpi: 1600 };
        let json = serde_json::to_string(&stage).unwrap();
        assert!(json.contains("dpi"));
        assert!(!json.contains("color"));
        // Verify round-trip.
        let back: StoredDpiStage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.dpi, 1600);
    }

    // ── SensorOptions has all four fields ───────────────────────────────

    #[test]
    fn stored_sensor_options_has_all_four_fields() {
        let opts = StoredSensorOptions {
            lift_off_distance: 2,
            ripple_control: true,
            angle_snap: false,
            motion_sync: true,
        };
        let json = serde_json::to_string(&opts).unwrap();
        assert!(json.contains("lift_off_distance"));
        assert!(json.contains("ripple_control"));
        assert!(json.contains("angle_snap"));
        assert!(json.contains("motion_sync"));
    }

    // ── Preferences has correct fields, no invented ones ────────────────

    #[test]
    fn stored_preferences_has_correct_fields() {
        let prefs = StoredPreferencesState::default();
        let json = serde_json::to_string(&prefs).unwrap();
        assert!(json.contains("light_mode"));
        assert!(json.contains("configuration"));
        assert!(json.contains("deep_sleep"));
        assert!(json.contains("host_color"));
        assert!(json.contains("sleep_timer"));
        assert!(json.contains("debounce"));
        // Must NOT contain invented fields.
        assert!(!json.contains("light_brightness"));
        assert!(!json.contains("light_speed"));
        assert!(!json.contains("motion_sync"));
        assert!(!json.contains("performance_mode"));
    }

    // ── Button slot uses key_code ───────────────────────────────────────

    #[test]
    fn stored_button_slot_uses_key_code() {
        let slot = StoredButtonSlot {
            action: 0x01,
            modifier: 0x00,
            key_code: 0x30,
        };
        let json = serde_json::to_string(&slot).unwrap();
        assert!(json.contains("key_code"));
        assert!(!json.contains("\"key\""));
    }

    // ── Profile metadata uses current/maximum ───────────────────────────

    #[test]
    fn stored_profile_metadata_uses_current_maximum() {
        let meta = StoredProfileMetadata {
            current: 2,
            maximum: 4,
        };
        let json = serde_json::to_string(&meta).unwrap();
        assert!(json.contains("current"));
        assert!(json.contains("maximum"));
        assert!(!json.contains("current_profile"));
        assert!(!json.contains("active_profile_count"));
    }

    // ── Merge helpers ───────────────────────────────────────────────────

    fn setup_merge_state() -> StateFile {
        let mut state = StateFile::default();
        let dev = ensure_device(
            &mut state,
            "dev1",
            StoredTransport::Wired,
            StoredSelector::Unique,
        );

        // Pre-populate with baseline DPI, prefs, buttons for profile 1.
        let profile = dev.profiles.entry(1).or_insert_with(|| {
            VersionedState::now(
                StoredProfileState::default(),
                StateSource::ExplicitDefaults,
                StateVerification::PersistenceUnknown,
            )
        });
        profile.value.dpi = Some(VersionedState::now(
            StoredDpiState::default(),
            StateSource::UsbReadback,
            StateVerification::Observed,
        ));
        profile.value.preferences = Some(VersionedState::now(
            StoredPreferencesState::default(),
            StateSource::UsbReadback,
            StateVerification::Observed,
        ));
        profile.value.buttons = Some(VersionedState::now(
            StoredButtonsState::default(),
            StateSource::UsbReadback,
            StateVerification::Observed,
        ));

        state
    }

    #[test]
    fn merge_dpi_delta_updates_stages_and_sensor() {
        let mut state = setup_merge_state();

        let delta = DpiMergeDelta {
            stages: Some(vec![400, 800]),
            active_stage: Some(2),
            sensor: Some(SensorMergeDelta {
                lift_off_distance: Some(2),
                ripple_control: None,
                angle_snap: Some(true),
                motion_sync: None,
            }),
        };

        merge_dpi_delta(
            &mut state,
            "dev1",
            1,
            &delta,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
            false,
        )
        .expect("merge should succeed");

        let stored = profile(&state, "dev1", 1)
            .and_then(|p| p.value.dpi.as_ref())
            .expect("dpi should exist");
        assert_eq!(stored.value.stages.len(), 2);
        assert_eq!(stored.value.stages[0].dpi, 400);
        assert_eq!(stored.value.stages[1].dpi, 800);
        assert_eq!(stored.value.active_stage, 2);
        assert_eq!(stored.value.sensor_options.lift_off_distance, 2);
        assert_eq!(stored.value.sensor_options.angle_snap, true);
        // Unchanged fields from baseline.
        assert_eq!(stored.value.sensor_options.ripple_control, false);
        assert_eq!(stored.value.sensor_options.motion_sync, false);
        // preserved_tail carried from baseline.
        assert_eq!(stored.value.preserved_tail, [0u8; 25]);
    }

    #[test]
    fn merge_dpi_delta_partial_leaves_unmentioned_unchanged() {
        let mut state = setup_merge_state();

        // Only change active_stage — leave stages and sensor alone.
        let delta = DpiMergeDelta {
            stages: None,
            active_stage: Some(3),
            sensor: None,
        };

        merge_dpi_delta(
            &mut state,
            "dev1",
            1,
            &delta,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
            false,
        )
        .expect("merge should succeed");

        let stored = profile(&state, "dev1", 1)
            .and_then(|p| p.value.dpi.as_ref())
            .expect("dpi should exist");
        // stages unchanged from default (4 stages).
        assert_eq!(stored.value.stages.len(), 4);
        assert_eq!(stored.value.stages[0].dpi, 800);
        assert_eq!(stored.value.active_stage, 3);
    }

    #[test]
    fn merge_dpi_delta_missing_device() {
        let mut state = StateFile::default();
        let delta = DpiMergeDelta {
            stages: None,
            active_stage: None,
            sensor: None,
        };
        let err = merge_dpi_delta(
            &mut state,
            "nobody",
            1,
            &delta,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
            false,
        )
        .expect_err("should fail for missing device");
        assert!(matches!(err, MergeError::NoDevice(_)));
    }

    #[test]
    fn merge_dpi_delta_missing_baseline_refused() {
        let mut state = StateFile::default();
        ensure_device(
            &mut state,
            "dev1",
            StoredTransport::Ble,
            StoredSelector::UniqueConnected,
        );
        // No profiles at all — no baseline.

        let delta = DpiMergeDelta {
            stages: Some(vec![800]),
            active_stage: Some(1),
            sensor: None,
        };

        let err = merge_dpi_delta(
            &mut state,
            "dev1",
            1,
            &delta,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
            false, // explicit_defaults NOT allowed
        )
        .expect_err("should fail without baseline");

        match &err {
            MergeError::MissingBaseline {
                device_key,
                profile_id,
                resource,
            } => {
                assert_eq!(device_key, "dev1");
                assert_eq!(*profile_id, 1);
                assert_eq!(*resource, "dpi");
            }
            _ => panic!("expected MissingBaseline, got {err:?}"),
        }
    }

    #[test]
    fn merge_dpi_delta_missing_baseline_allowed_with_explicit_defaults() {
        let mut state = StateFile::default();
        ensure_device(
            &mut state,
            "dev1",
            StoredTransport::Ble,
            StoredSelector::UniqueConnected,
        );

        let delta = DpiMergeDelta {
            stages: Some(vec![800, 1600]),
            active_stage: Some(1),
            sensor: None,
        };

        merge_dpi_delta(
            &mut state,
            "dev1",
            1,
            &delta,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
            true, // explicit_defaults ALLOWED
        )
        .expect("merge should succeed with explicit_defaults");

        let stored = profile(&state, "dev1", 1)
            .and_then(|p| p.value.dpi.as_ref())
            .expect("dpi should exist");
        assert_eq!(stored.value.stages.len(), 2);
        assert_eq!(stored.source, StateSource::LocallyWritten);
    }

    #[test]
    fn merge_prefs_delta_updates_correct_fields() {
        let mut state = setup_merge_state();

        let delta = PrefsMergeDelta {
            light_mode: Some(3),
            configuration: None,
            deep_sleep: None,
            host_color: Some([0xaa, 0xbb, 0xcc]),
            sleep_timer: Some(10),
            debounce: None,
        };

        merge_prefs_delta(
            &mut state,
            "dev1",
            1,
            &delta,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
            false,
        )
        .expect("merge should succeed");

        let stored = profile(&state, "dev1", 1)
            .and_then(|p| p.value.preferences.as_ref())
            .expect("prefs should exist");
        assert_eq!(stored.value.light_mode, 3);
        assert_eq!(stored.value.host_color, [0xaa, 0xbb, 0xcc]);
        assert_eq!(stored.value.sleep_timer, 10);
        // Unchanged fields from baseline.
        assert_eq!(
            stored.value.configuration,
            StoredPreferencesState::default().configuration
        );
        assert_eq!(
            stored.value.debounce,
            StoredPreferencesState::default().debounce
        );
    }

    #[test]
    fn merge_prefs_delta_missing_baseline_refused() {
        let mut state = StateFile::default();
        ensure_device(
            &mut state,
            "dev1",
            StoredTransport::Ble,
            StoredSelector::UniqueConnected,
        );

        let delta = PrefsMergeDelta {
            light_mode: Some(1),
            configuration: None,
            deep_sleep: None,
            host_color: None,
            sleep_timer: None,
            debounce: None,
        };

        let err = merge_prefs_delta(
            &mut state,
            "dev1",
            1,
            &delta,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
            false,
        )
        .expect_err("should fail without baseline");

        match &err {
            MergeError::MissingBaseline { resource, .. } => {
                assert_eq!(*resource, "preferences");
            }
            _ => panic!("expected MissingBaseline, got {err:?}"),
        }
    }

    #[test]
    fn merge_prefs_delta_missing_baseline_allowed() {
        let mut state = StateFile::default();
        ensure_device(
            &mut state,
            "dev1",
            StoredTransport::Ble,
            StoredSelector::UniqueConnected,
        );

        let delta = PrefsMergeDelta {
            light_mode: Some(1),
            configuration: None,
            deep_sleep: None,
            host_color: None,
            sleep_timer: None,
            debounce: None,
        };

        merge_prefs_delta(
            &mut state,
            "dev1",
            1,
            &delta,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
            true,
        )
        .expect("merge should succeed with explicit_defaults");
    }

    #[test]
    fn merge_button_delta_updates_single_slot() {
        let mut state = setup_merge_state();

        merge_button_delta(
            &mut state,
            "dev1",
            1,
            5,
            0x02,
            0x01,
            0x44,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
            false,
        )
        .expect("merge should succeed");

        let stored = profile(&state, "dev1", 1)
            .and_then(|p| p.value.buttons.as_ref())
            .expect("buttons should exist");
        assert_eq!(stored.value.slots[5].action, 0x02);
        assert_eq!(stored.value.slots[5].modifier, 0x01);
        assert_eq!(stored.value.slots[5].key_code, 0x44);
        // Other slots unchanged.
        assert_eq!(stored.value.slots[0].action, 0x00);
        assert_eq!(stored.value.slots[17].action, 0x00);
    }

    #[test]
    fn merge_button_delta_ignores_out_of_bounds() {
        let mut state = setup_merge_state();

        // Slot index 99 is out of bounds — should be ignored, not panic.
        merge_button_delta(
            &mut state,
            "dev1",
            1,
            99,
            0xFF,
            0xFF,
            0xFF,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
            false,
        )
        .expect("merge should succeed (out-of-bounds ignored)");

        let stored = profile(&state, "dev1", 1)
            .and_then(|p| p.value.buttons.as_ref())
            .expect("buttons should exist");
        // All slots at default.
        for slot in &stored.value.slots {
            assert_eq!(slot.action, 0);
        }
    }

    #[test]
    fn merge_button_delta_missing_baseline_refused() {
        let mut state = StateFile::default();
        ensure_device(
            &mut state,
            "dev1",
            StoredTransport::Ble,
            StoredSelector::UniqueConnected,
        );

        let err = merge_button_delta(
            &mut state,
            "dev1",
            1,
            0,
            0x01,
            0x00,
            0x30,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
            false,
        )
        .expect_err("should fail without baseline");

        match &err {
            MergeError::MissingBaseline { resource, .. } => {
                assert_eq!(*resource, "buttons");
            }
            _ => panic!("expected MissingBaseline, got {err:?}"),
        }
    }

    #[test]
    fn merge_button_delta_missing_baseline_allowed() {
        let mut state = StateFile::default();
        ensure_device(
            &mut state,
            "dev1",
            StoredTransport::Ble,
            StoredSelector::UniqueConnected,
        );

        merge_button_delta(
            &mut state,
            "dev1",
            1,
            0,
            0x01,
            0x00,
            0x30,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
            true,
        )
        .expect("merge should succeed with explicit_defaults");
    }

    // ── init_profile_defaults ───────────────────────────────────────────

    #[test]
    fn init_profile_defaults_creates_with_correct_provenance() {
        let mut state = StateFile::default();
        ensure_device(
            &mut state,
            "dev1",
            StoredTransport::Wired,
            StoredSelector::Unique,
        );

        let created = init_profile_defaults(&mut state, "dev1", 1).expect("init should succeed");
        assert!(created);

        let p = profile(&state, "dev1", 1).expect("profile should exist");
        assert_eq!(p.source, StateSource::ExplicitDefaults);
        assert_eq!(p.verification, StateVerification::PersistenceUnknown);

        // DPI should have captured tail, sourced explicitly.
        let dpi = p.value.dpi.as_ref().expect("dpi should be populated");
        assert_eq!(dpi.source, StateSource::ExplicitDefaults);
        assert_eq!(dpi.verification, StateVerification::PersistenceUnknown);
        assert_eq!(
            dpi.value.preserved_tail,
            StoredDpiState::CAPTURED_EMPTY_PROFILE_TAIL
        );

        // Preferences should use explicit defaults.
        let prefs = p
            .value
            .preferences
            .as_ref()
            .expect("prefs should be populated");
        assert_eq!(prefs.source, StateSource::ExplicitDefaults);

        // Buttons should use explicit defaults.
        let btns = p
            .value
            .buttons
            .as_ref()
            .expect("buttons should be populated");
        assert_eq!(btns.source, StateSource::ExplicitDefaults);
    }

    #[test]
    fn init_profile_defaults_is_idempotent() {
        let mut state = StateFile::default();
        ensure_device(
            &mut state,
            "dev1",
            StoredTransport::Wired,
            StoredSelector::Unique,
        );

        let created1 =
            init_profile_defaults(&mut state, "dev1", 1).expect("first init should succeed");
        assert!(created1);

        let created2 =
            init_profile_defaults(&mut state, "dev1", 1).expect("second init should succeed");
        assert!(
            !created2,
            "second init should return false (already exists)"
        );
    }

    #[test]
    fn init_profile_defaults_missing_device() {
        let mut state = StateFile::default();
        let err = init_profile_defaults(&mut state, "nobody", 1).expect_err("should fail");
        assert!(matches!(err, MergeError::NoDevice(_)));
    }

    // ── serialised shape ────────────────────────────────────────────────

    #[test]
    fn serialised_uses_expected_field_names() {
        let state = StateFile::default();
        let json = serde_json::to_string_pretty(&state).expect("serialise");
        assert!(json.contains("\"schema_version\""));
        assert!(!json.contains("selected_device"));
        assert!(json.contains("\"devices\""));
    }

    #[test]
    fn serialised_source_verification_are_kebab_case() {
        let vs = VersionedState::now(
            1000u16,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
        );
        let json = serde_json::to_string(&vs).expect("serialise");
        assert!(json.contains("locally-written"));
        assert!(json.contains("application-unknown"));
    }

    // ── utc_now returns reasonable timestamps ───────────────────────────

    #[test]
    fn utc_now_returns_iso8601_like_string() {
        let ts = utc_now_iso8601();
        assert!(ts.ends_with('Z'), "must end with Z: {ts}");
        assert!(ts.contains('T'), "must contain T separator: {ts}");
        assert_eq!(ts.len(), 20, "expected 20-char ISO-8601: {ts}");
    }

    // ── preserved_tail serde ────────────────────────────────────────────

    #[test]
    fn preserved_tail_hex_round_trips() {
        let mut tail = [0u8; 25];
        for i in 0..25 {
            tail[i] = i as u8;
        }
        let dpi = StoredDpiState {
            profile: 1,
            stages: vec![StoredDpiStage { dpi: 800 }],
            active_stage: 1,
            sensor_options: StoredSensorOptions {
                lift_off_distance: 1,
                ripple_control: false,
                angle_snap: false,
                motion_sync: false,
            },
            preserved_tail: tail,
        };
        let json = serde_json::to_string(&dpi).expect("serialise");
        assert!(
            json.contains("000102030405060708090a0b0c0d0e0f101112131415161718"),
            "preserved_tail should be hex: {json}"
        );
        let restored: StoredDpiState = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(restored.preserved_tail, tail);
    }

    #[test]
    fn preserved_tail_invalid_length_rejected() {
        let json = r#"{
            "profile":1,
            "stages":[{"dpi":800}],
            "active_stage":1,
            "sensor_options":{"lift_off_distance":1,"ripple_control":false,"angle_snap":false,"motion_sync":false},
            "preserved_tail":"ab"
        }"#;
        let result: Result<StoredDpiState, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    // ── Button slot count constant ──────────────────────────────────────

    #[test]
    fn stored_buttons_has_button_slot_count_slots() {
        let btns = StoredButtonsState::default();
        assert_eq!(btns.slots.len(), BUTTON_SLOT_COUNT);
        let json = serde_json::to_string(&btns).unwrap();
        let back: StoredButtonsState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.slots.len(), BUTTON_SLOT_COUNT);
    }

    // ── Baseline provenance enforcement ─────────────────────────────────

    /// Set up state with a DPI baseline sourced from ExplicitDefaults.
    fn setup_explicit_defaults_dpi_state() -> StateFile {
        let mut state = StateFile::default();
        let dev = ensure_device(
            &mut state,
            "dev1",
            StoredTransport::Ble,
            StoredSelector::UniqueConnected,
        );
        let profile = dev.profiles.entry(1).or_insert_with(|| {
            VersionedState::now(
                StoredProfileState::default(),
                StateSource::ExplicitDefaults,
                StateVerification::PersistenceUnknown,
            )
        });
        profile.value.dpi = Some(VersionedState::now(
            StoredDpiState::default(),
            StateSource::ExplicitDefaults,
            StateVerification::PersistenceUnknown,
        ));
        state
    }

    #[test]
    fn merge_dpi_rejects_explicit_defaults_baseline_when_disallowed() {
        let mut state = setup_explicit_defaults_dpi_state();

        let delta = DpiMergeDelta {
            stages: Some(vec![800]),
            active_stage: Some(1),
            sensor: None,
        };

        let err = merge_dpi_delta(
            &mut state,
            "dev1",
            1,
            &delta,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
            false,
        )
        .expect_err("should reject ExplicitDefaults baseline when disallowed");

        match &err {
            MergeError::MissingBaseline {
                device_key,
                profile_id,
                resource,
            } => {
                assert_eq!(device_key, "dev1");
                assert_eq!(*profile_id, 1);
                assert_eq!(*resource, "dpi");
            }
            _ => panic!("expected MissingBaseline, got {err:?}"),
        }
    }

    #[test]
    fn merge_dpi_allows_explicit_defaults_baseline_when_allowed() {
        let mut state = setup_explicit_defaults_dpi_state();

        let delta = DpiMergeDelta {
            stages: Some(vec![800]),
            active_stage: Some(1),
            sensor: None,
        };

        merge_dpi_delta(
            &mut state,
            "dev1",
            1,
            &delta,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
            true,
        )
        .expect("should accept ExplicitDefaults baseline when allowed");
    }

    #[test]
    fn merge_prefs_rejects_explicit_defaults_baseline_when_disallowed() {
        let mut state = StateFile::default();
        let dev = ensure_device(
            &mut state,
            "dev1",
            StoredTransport::Ble,
            StoredSelector::UniqueConnected,
        );
        let profile = dev.profiles.entry(1).or_insert_with(|| {
            VersionedState::now(
                StoredProfileState::default(),
                StateSource::ExplicitDefaults,
                StateVerification::PersistenceUnknown,
            )
        });
        profile.value.preferences = Some(VersionedState::now(
            StoredPreferencesState::default(),
            StateSource::ExplicitDefaults,
            StateVerification::PersistenceUnknown,
        ));

        let delta = PrefsMergeDelta {
            light_mode: Some(1),
            configuration: None,
            deep_sleep: None,
            host_color: None,
            sleep_timer: None,
            debounce: None,
        };

        let err = merge_prefs_delta(
            &mut state,
            "dev1",
            1,
            &delta,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
            false,
        )
        .expect_err("should reject ExplicitDefaults prefs baseline when disallowed");

        assert!(matches!(err, MergeError::MissingBaseline { .. }));
    }

    #[test]
    fn merge_buttons_rejects_explicit_defaults_baseline_when_disallowed() {
        let mut state = StateFile::default();
        let dev = ensure_device(
            &mut state,
            "dev1",
            StoredTransport::Ble,
            StoredSelector::UniqueConnected,
        );
        let profile = dev.profiles.entry(1).or_insert_with(|| {
            VersionedState::now(
                StoredProfileState::default(),
                StateSource::ExplicitDefaults,
                StateVerification::PersistenceUnknown,
            )
        });
        profile.value.buttons = Some(VersionedState::now(
            StoredButtonsState::default(),
            StateSource::ExplicitDefaults,
            StateVerification::PersistenceUnknown,
        ));

        let err = merge_button_delta(
            &mut state,
            "dev1",
            1,
            0,
            0x01,
            0x00,
            0x30,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
            false,
        )
        .expect_err("should reject ExplicitDefaults buttons baseline when disallowed");

        assert!(matches!(err, MergeError::MissingBaseline { .. }));
    }

    #[test]
    fn merge_accepts_usb_readback_baseline_when_disallowed() {
        let mut state = StateFile::default();
        let dev = ensure_device(
            &mut state,
            "dev1",
            StoredTransport::Ble,
            StoredSelector::UniqueConnected,
        );
        let profile = dev.profiles.entry(1).or_insert_with(|| {
            VersionedState::now(
                StoredProfileState::default(),
                StateSource::UsbReadback,
                StateVerification::Observed,
            )
        });
        profile.value.dpi = Some(VersionedState::now(
            StoredDpiState::default(),
            StateSource::UsbReadback,
            StateVerification::Observed,
        ));

        let delta = DpiMergeDelta {
            stages: Some(vec![800]),
            active_stage: Some(1),
            sensor: None,
        };

        merge_dpi_delta(
            &mut state,
            "dev1",
            1,
            &delta,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
            false,
        )
        .expect("should accept UsbReadback baseline even without explicit_defaults");
    }

    #[test]
    fn merge_accepts_imported_baseline_when_disallowed() {
        let mut state = StateFile::default();
        let dev = ensure_device(
            &mut state,
            "dev1",
            StoredTransport::Ble,
            StoredSelector::UniqueConnected,
        );
        let profile = dev.profiles.entry(1).or_insert_with(|| {
            VersionedState::now(
                StoredProfileState::default(),
                StateSource::Imported,
                StateVerification::PersistenceUnknown,
            )
        });
        profile.value.preferences = Some(VersionedState::now(
            StoredPreferencesState::default(),
            StateSource::Imported,
            StateVerification::PersistenceUnknown,
        ));

        let delta = PrefsMergeDelta {
            light_mode: Some(3),
            configuration: None,
            deep_sleep: None,
            host_color: None,
            sleep_timer: None,
            debounce: None,
        };

        merge_prefs_delta(
            &mut state,
            "dev1",
            1,
            &delta,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
            false,
        )
        .expect("should accept Imported baseline even without explicit_defaults");
    }

    // ── Captured DPI tail is canonical ──────────────────────────────────

    #[test]
    fn captured_dpi_tail_matches_canonical_25_bytes() {
        let tail = StoredDpiState::CAPTURED_EMPTY_PROFILE_TAIL;
        assert_eq!(tail.len(), 25, "captured tail must be exactly 25 bytes");
        assert_ne!(
            tail, [0u8; 25],
            "captured tail must differ from zeroed default tail"
        );
        let dpi = StoredDpiState::with_captured_tail(
            1,
            vec![StoredDpiStage { dpi: 800 }],
            1,
            StoredSensorOptions {
                lift_off_distance: 1,
                ripple_control: false,
                angle_snap: false,
                motion_sync: false,
            },
        );
        assert_eq!(dpi.preserved_tail, tail);
        assert_eq!(
            dpi.preserved_tail,
            StoredDpiState::CAPTURED_EMPTY_PROFILE_TAIL
        );
    }

    #[test]
    fn merge_dpi_replaces_stages_preserves_tail_and_sensors() {
        // Baseline DPI with captured tail and non-default sensor options.
        let mut state = StateFile::default();
        let dev = ensure_device(
            &mut state,
            "dev1",
            StoredTransport::Ble,
            StoredSelector::UniqueConnected,
        );
        let profile_entry = dev.profiles.entry(1).or_insert_with(|| {
            VersionedState::now(
                StoredProfileState::default(),
                StateSource::UsbReadback,
                StateVerification::Observed,
            )
        });
        profile_entry.value.dpi = Some(VersionedState::now(
            StoredDpiState::with_captured_tail(
                1,
                vec![
                    StoredDpiStage { dpi: 400 },
                    StoredDpiStage { dpi: 800 },
                    StoredDpiStage { dpi: 1600 },
                    StoredDpiStage { dpi: 3200 },
                ],
                2,
                StoredSensorOptions {
                    lift_off_distance: 2,
                    ripple_control: true,
                    angle_snap: true,
                    motion_sync: false,
                },
            ),
            StateSource::UsbReadback,
            StateVerification::Observed,
        ));

        // Replace stages with a shorter list, change nothing else.
        let delta = DpiMergeDelta {
            stages: Some(vec![800, 1600]),
            active_stage: None,
            sensor: None,
        };

        merge_dpi_delta(
            &mut state,
            "dev1",
            1,
            &delta,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
            false,
        )
        .expect("merge should succeed");

        let stored = profile(&state, "dev1", 1)
            .and_then(|p| p.value.dpi.as_ref())
            .expect("dpi should exist");

        // Stages must be replaced (2, not 4).
        assert_eq!(stored.value.stages.len(), 2);
        assert_eq!(stored.value.stages[0].dpi, 800);
        assert_eq!(stored.value.stages[1].dpi, 1600);

        // Active stage unchanged from baseline.
        assert_eq!(stored.value.active_stage, 2);

        // Sensor options unchanged from baseline.
        assert_eq!(stored.value.sensor_options.lift_off_distance, 2);
        assert!(stored.value.sensor_options.ripple_control);
        assert!(stored.value.sensor_options.angle_snap);
        assert!(!stored.value.sensor_options.motion_sync);

        // Preserved tail carried from baseline (must be the canonical captured tail).
        assert_eq!(
            stored.value.preserved_tail,
            StoredDpiState::CAPTURED_EMPTY_PROFILE_TAIL
        );
    }

    // ── Preferences raw defaults round-trip consistently ────────────────

    #[test]
    fn preferences_default_raw_bytes_round_trip() {
        let prefs = StoredPreferencesState::default();
        let json = serde_json::to_string(&prefs).expect("serialise");
        let back: StoredPreferencesState = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(prefs, back, "preferences must round-trip exactly");
        assert_eq!(prefs.light_mode, 0x02);
        assert_eq!(prefs.configuration, 0x01);
        assert_eq!(prefs.deep_sleep, 0x00);
        assert_eq!(prefs.host_color, [0xff, 0x00, 0x00]);
        assert_eq!(prefs.sleep_timer, 5);
        assert_eq!(prefs.debounce, 0x00);
    }

    #[test]
    fn preferences_default_delta_merge_preserves_untouched_bytes() {
        let mut state = StateFile::default();
        ensure_device(
            &mut state,
            "dev1",
            StoredTransport::Ble,
            StoredSelector::UniqueConnected,
        );

        let delta = PrefsMergeDelta {
            light_mode: None,
            configuration: None,
            deep_sleep: None,
            host_color: None,
            sleep_timer: Some(10),
            debounce: None,
        };

        merge_prefs_delta(
            &mut state,
            "dev1",
            1,
            &delta,
            StateSource::LocallyWritten,
            StateVerification::ApplicationUnknown,
            true,
        )
        .expect("merge should succeed");

        let stored = profile(&state, "dev1", 1)
            .and_then(|p| p.value.preferences.as_ref())
            .expect("prefs should exist");
        assert_eq!(stored.value.sleep_timer, 10);
        assert_eq!(
            stored.value.light_mode,
            StoredPreferencesState::default().light_mode
        );
        assert_eq!(
            stored.value.configuration,
            StoredPreferencesState::default().configuration
        );
        assert_eq!(
            stored.value.host_color,
            StoredPreferencesState::default().host_color
        );
        assert_eq!(
            stored.value.debounce,
            StoredPreferencesState::default().debounce
        );
    }
}
