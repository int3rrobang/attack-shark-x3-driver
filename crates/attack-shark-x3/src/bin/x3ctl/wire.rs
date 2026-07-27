//! Versioned, typed request/response protocol for the x3ctl broker.
//!
//! Every wire message is a [`Message`] envelope carrying either a [`Request`]
//! or a [`Response`].  The envelope includes a protocol version so the broker
//! can reject mismatched clients.
//!
//! # Design invariants
//!
//! * Operations are typed domain primitives — **never** raw packet bytes.
//! * [`Response`] carries an explicit [`Provenance`] so consumers know whether
//!   the data was validated via USB readback, acknowledged by a BLE parser,
//!   drawn from cached state, or is unverified.
//! * Stop and disconnect are explicit [`Request`] variants; the broker must
//!   never auto-apply configuration on start-up.
//! * [`ExecutionContext`] on every device-interacting request carries transport
//!   selection, optional device disambiguation, and state/defaults policy.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// ── Constants ────────────────────────────────────────────────────────────────

/// Current wire protocol version.
pub const WIRE_VERSION: u32 = 1;

/// Maximum JSON message size in bytes (256 KiB).
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;

/// Export document format version.
pub const EXPORT_FORMAT_VERSION: u32 = 1;

/// Required number of button-assignment slots in an import/export document.
pub const BUTTON_SLOT_COUNT: usize = 18;

// ── Top-level envelope ───────────────────────────────────────────────────────

/// A single versioned request or response.
///
/// Serialised as newline-delimited JSON on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Protocol version — must equal [`WIRE_VERSION`].
    pub v: u32,
    #[serde(flatten)]
    pub inner: MessageInner,
}

/// Discriminated payload inside a [`Message`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MessageInner {
    /// A client-to-broker request.
    #[serde(rename = "req")]
    Request(Request),
    /// A broker-to-client response.
    #[serde(rename = "res")]
    Response(Response),
}

// ── Execution context ────────────────────────────────────────────────────────

/// Per-request execution parameters carried by every device-interacting
/// [`Request`] variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionContext {
    /// Transport to use for this operation.
    pub transport: TransportKind,
    /// Optional device selector for disambiguation when multiple devices
    /// share the transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceSelector>,
    /// When `true`, the broker skips durable-state readback and persistence
    /// for this operation.
    #[serde(default)]
    pub no_state: bool,
    /// When `true`, the broker may substitute explicit built-in defaults for
    /// a resource that lacks a stored baseline (relevant for BLE operations
    /// where readback is incomplete).
    #[serde(default)]
    pub explicit_defaults: bool,
}

// ── Device selector ──────────────────────────────────────────────────────────

/// Platform-independent device selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum DeviceSelector {
    /// Open a specific USB HID path.
    UsbPath(String),
    /// Select a connected BLE device by its advertised name.
    BleName(String),
    /// Select a connected BLE device by its address.
    BleAddr(String),
}

// ── Request ──────────────────────────────────────────────────────────────────

/// Every operation the broker accepts.
///
/// Variants are serialised with `"op"` as the discriminator field so the JSON
/// is self-describing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum Request {
    /// Health check / liveness probe.  The broker responds with
    /// [`ResponseData::Empty`].
    #[serde(rename = "ping")]
    Ping,

    /// Graceful broker shutdown.  The broker finishes any in-flight request,
    /// releases the device session, and exits.
    #[serde(rename = "stop")]
    Stop,

    /// Enumerate attached X3 devices via USB and/or BLE.
    #[serde(rename = "list_devices")]
    ListDevices { ctx: ExecutionContext },

    /// Open (or replace) the device session.
    #[serde(rename = "use_device")]
    UseDevice {
        /// How to select the device.
        selector: DeviceSelector,
        /// Transport to use.
        transport: TransportKind,
        /// Execution context carrying state/defaults policy for this use.
        ctx: ExecutionContext,
    },

    /// Read full device state for a profile (composite snapshot).
    #[serde(rename = "status")]
    Status {
        /// 1-based profile index.
        profile: u8,
        ctx: ExecutionContext,
    },

    // ── Individual resource reads ────────────────────────────────────────
    /// Read only the DPI configuration for a profile.
    #[serde(rename = "read_dpi")]
    ReadDpi {
        /// 1-based profile index.
        profile: u8,
        ctx: ExecutionContext,
    },

    /// Read only the preferences for a profile.
    #[serde(rename = "read_prefs")]
    ReadPreferences {
        /// 1-based profile index.
        profile: u8,
        ctx: ExecutionContext,
    },

    /// Read only the button table for a profile.
    #[serde(rename = "read_buttons")]
    ReadButtons {
        /// 1-based profile index.
        profile: u8,
        ctx: ExecutionContext,
    },

    /// Read the global polling rate.
    #[serde(rename = "read_rate")]
    ReadRate { ctx: ExecutionContext },

    /// Read the battery percentage (receiver transport only).
    #[serde(rename = "battery")]
    Battery { ctx: ExecutionContext },

    // ── Writes ───────────────────────────────────────────────────────────
    /// Configure DPI stages and sensor options for a profile.
    ///
    /// Every field is optional — omitted fields leave the current value
    /// unchanged.
    #[serde(rename = "set_dpi")]
    SetDpi {
        /// 1-based profile index.
        profile: u8,
        /// Ordered DPI values in Hz (valid X3 DPI: 50..=26000, step 50).
        /// `None` = leave stages unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stages: Option<Vec<u16>>,
        /// 1-based active stage index.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_stage: Option<u8>,
        /// Global sensor tweaks (partial — omitted fields are unchanged).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sensor: Option<SensorOptionsDelta>,
        ctx: ExecutionContext,
    },

    /// Set profile-scoped preferences.  Every field in the delta is optional.
    #[serde(rename = "set_prefs")]
    SetPreferences {
        /// 1-based profile index.
        profile: u8,
        #[serde(default)]
        prefs: PreferencesDelta,
        ctx: ExecutionContext,
    },

    /// Bind a button slot to an assignment for a profile.
    #[serde(rename = "set_button")]
    SetButton {
        /// 1-based profile index.
        profile: u8,
        /// 0-based button-slot index (0–17).
        index: u8,
        /// The new button assignment.
        assignment: ButtonPayload,
        ctx: ExecutionContext,
    },

    /// Set the global polling rate.
    #[serde(rename = "set_rate")]
    SetRate {
        /// Polling rate: 125, 250, 500, or 1000.
        hz: u16,
        ctx: ExecutionContext,
    },

    // ── Profile control ──────────────────────────────────────────────────
    /// Switch the active profile.
    #[serde(rename = "profile_use")]
    ProfileUse {
        /// 1-based profile index.
        profile: u8,
        ctx: ExecutionContext,
    },

    /// Set the maximum enabled profile.
    #[serde(rename = "profile_max")]
    ProfileMax {
        /// Maximum profile number (1–5).
        max: u8,
        ctx: ExecutionContext,
    },

    // ── State management ─────────────────────────────────────────────────
    /// Apply the desired / last-known durable state to the device.
    #[serde(rename = "apply")]
    Apply { ctx: ExecutionContext },

    /// Reset the device to factory defaults and reapply configuration.
    #[serde(rename = "reset")]
    Reset {
        /// 1-based profile index to reset.
        profile: u8,
        ctx: ExecutionContext,
    },

    /// Export the current durable device state as a JSON document.
    #[serde(rename = "export")]
    Export { ctx: ExecutionContext },

    /// Import device state from a previously-exported document.
    #[serde(rename = "import")]
    Import {
        /// Previously exported state document.
        document: ExportDocument,
        ctx: ExecutionContext,
    },

    /// Initialise a device profile with explicit defaults.
    #[serde(rename = "init_defaults")]
    InitDefaults { ctx: ExecutionContext },

    /// Forget (remove) durable state for a device or a single profile.
    #[serde(rename = "forget")]
    Forget {
        /// What to forget.
        target: ForgetTarget,
        ctx: ExecutionContext,
    },

    // ── Session ──────────────────────────────────────────────────────────
    /// Close the device session without stopping the broker.
    #[serde(rename = "disconnect")]
    Disconnect,

    // ── Daemon ───────────────────────────────────────────────────────────
    /// Query broker daemon metadata (pid, uptime, session status).
    #[serde(rename = "daemon_status")]
    DaemonStatus,

    /// Request the broker daemon to stop (alias for [`Request::Stop`]).
    #[serde(rename = "daemon_stop")]
    DaemonStop,

    /// Dump internal broker / driver diagnostics.
    #[serde(rename = "debug")]
    Debug,
}

// ── Forget target ────────────────────────────────────────────────────────────

/// What durable state to forget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum ForgetTarget {
    /// Forget an entire device and all its profiles.
    #[serde(rename = "device")]
    Device {
        /// Stable device key.
        key: String,
    },
    /// Forget a single profile within a device.
    #[serde(rename = "profile")]
    Profile {
        /// Stable device key.
        device_key: String,
        /// 1-based profile id.
        profile_id: u8,
    },
}

// ── Response ─────────────────────────────────────────────────────────────────

/// A broker response.
///
/// Every response declares its [`Provenance`] so clients can distinguish
/// USB-verified data from cached or unverified information.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Where the data came from.
    pub provenance: Provenance,
    #[serde(flatten)]
    pub result: ResponseResult,
}

/// Success or failure payload.
///
/// Serialised untagged: a successful response carries `"kind"` and `"value"`;
/// an error carries `"code"` and `"message"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseResult {
    Ok(ResponseData),
    Err { code: ErrorCode, message: String },
}

/// Typed payload for successful responses.
///
/// Individual resource responses avoid coupling consumers to a monolithic
/// status blob.  Write operations return lightweight summaries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub enum ResponseData {
    /// No data (ping, stop, disconnect acknowledgements).
    #[serde(rename = "empty")]
    Empty,

    /// Enumerated device list.
    #[serde(rename = "devices")]
    DeviceList(Vec<DeviceEntry>),

    // ── Individual resource reads ────────────────────────────────────────
    /// DPI configuration for a single profile.
    #[serde(rename = "dpi")]
    DpiRead(Box<DpiStatePayload>),

    /// Preferences for a single profile.
    #[serde(rename = "prefs")]
    PreferencesRead(Box<PreferencesPayload>),

    /// Button-assignment table for a single profile.
    #[serde(rename = "buttons")]
    ButtonsRead(Box<ButtonsStatePayload>),

    /// Global polling rate.
    #[serde(rename = "rate")]
    RateRead(Box<RatePayload>),

    /// Profile metadata (current + maximum).
    #[serde(rename = "profile_meta")]
    ProfileMetaRead(Box<ProfileMetadataPayload>),

    /// Battery percentage (0–100).
    #[serde(rename = "battery")]
    BatteryLevel(u8),

    // ── Write summaries ──────────────────────────────────────────────────
    /// Summary after a DPI write.
    #[serde(rename = "dpi_written")]
    DpiWritten(Box<DpiWriteSummary>),

    /// Summary after a preferences write.
    #[serde(rename = "prefs_written")]
    PreferencesWritten(Box<PrefsWriteSummary>),

    /// Summary after a button write.
    #[serde(rename = "button_written")]
    ButtonWritten(Box<ButtonWriteSummary>),

    /// Summary after a polling-rate write.
    #[serde(rename = "rate_written")]
    RateWritten(Box<RateWriteSummary>),

    // ── Composite / state ────────────────────────────────────────────────
    /// Full device-status snapshot.
    #[serde(rename = "device_status")]
    DeviceStatus(Box<DeviceStatusPayload>),

    /// Exported device state document.
    #[serde(rename = "exported")]
    ExportedState(Box<ExportDocument>),

    /// Summary after an import.
    #[serde(rename = "imported")]
    ImportedSummary(Box<ImportSummary>),

    /// Summary after a factory reset.
    #[serde(rename = "reset_done")]
    ResetDone { profile: u8 },

    // ── Daemon / debug ───────────────────────────────────────────────────
    /// Broker daemon metadata.
    #[serde(rename = "daemon")]
    DaemonInfo(DaemonStatusPayload),

    /// Debug dump.
    #[serde(rename = "debug")]
    DebugInfo(serde_json::Value),
}

// ── Shared enums ─────────────────────────────────────────────────────────────

/// Transport selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    /// Let the broker / driver probe.
    Auto,
    /// Wired USB (FA61 configuration collection).
    Wired,
    /// 2.4 GHz receiver (FA60).
    Receiver,
    /// Bluetooth LE.
    Ble,
}

/// Data provenance — how the broker obtained the information.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// Readback was validated against the device via USB feature-report
    /// round-trip.
    UsbValidated,
    /// Write was acknowledged by the BLE device or accepted by the local
    /// parser.
    BleAcknowledged,
    /// Response drawn from cached / last-known durable state — not freshly
    /// read from hardware.
    Cached,
    /// Information is unverified (e.g. candidate receiver battery signature
    /// that has not been live-confirmed).
    Unverified,
}

/// Machine-readable error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The requested operation is not supported by the current transport or
    /// device.
    Unsupported,
    /// The underlying transport (HID / BLE) encountered an error.
    TransportFailure,
    /// No matching X3 device was found.
    DeviceNotFound,
    /// The operation timed out.
    Timeout,
    /// The request was malformed or out of range.
    InvalidRequest,
    /// An unexpected internal broker or driver error.
    Internal,
    /// A BLE partial update was requested without a stored baseline, and
    /// explicit defaults were not permitted.
    MissingBaseline,
}

// ── Payload types ────────────────────────────────────────────────────────────

/// A discovered X3 device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceEntry {
    /// Platform-specific path (USB path or BLE identifier).
    pub path: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub interface_number: i32,
    pub product: Option<String>,
    pub serial_number: Option<String>,
    /// Best-guess transport for this entry.
    pub transport: TransportKind,
}

/// Full device-status snapshot returned by [`Request::Status`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceStatusPayload {
    /// Active profile index (1-based).
    pub profile: u8,
    /// Maximum enabled profile index (1-based).
    pub profile_max: u8,
    pub dpi: Option<DpiStatePayload>,
    pub rate_hz: Option<u16>,
    pub prefs: Option<PreferencesPayload>,
    pub buttons: Option<Vec<ButtonPayload>>,
    pub battery: Option<u8>,
}

// ── DPI ──────────────────────────────────────────────────────────────────────

/// DPI stage and sensor state, matching the FA61 report-0x04 decode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DpiStatePayload {
    /// Profile id (1-based).
    pub profile: u8,
    /// Ordered DPI values in Hz.
    pub stages: Vec<u16>,
    /// 1-based active stage index.
    pub active_stage: u8,
    /// Sensor-level options.
    pub sensor: SensorOptions,
    /// Bytes 25..=49 of the report — unresolved semantics preserved for
    /// safe read-modify-write round-trips.  Hex-encoded (50-char lowercase).
    #[serde(with = "hex_preserved_tail")]
    pub preserved_tail: [u8; 25],
}

/// Sensor options matching the FA61 DPI report's sensor field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensorOptions {
    /// Lift-off distance: 1 = Low (1 mm), 2 = High (2 mm).
    pub lift_off_distance: u8,
    /// Ripple control enabled.
    pub ripple_control: bool,
    /// Angle snapping enabled.
    pub angle_snap: bool,
    /// Motion Sync enabled.
    pub motion_sync: bool,
}

/// Partial sensor-options update — every field is optional.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensorOptionsDelta {
    /// Lift-off distance: 1 = Low, 2 = High.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lift_off_distance: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ripple_control: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle_snap: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion_sync: Option<bool>,
}

/// Summary after a DPI write operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DpiWriteSummary {
    pub profile: u8,
    /// DPI values written.
    pub stages: Vec<u16>,
    /// Number of stages written.
    pub stage_count: u8,
    /// Active stage (1-based).
    pub active_stage: u8,
}

// ── Preferences ──────────────────────────────────────────────────────────────

/// Preferences (FA61 report 0x05) in typed form.
///
/// Matches the actual [`PreferencesState`] protocol type exactly:
/// `light_mode`, `configuration`, `deep_sleep`, `host_color[3]`,
/// `sleep_timer`, `debounce`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreferencesPayload {
    /// Profile id (1-based).
    pub profile: u8,
    /// Lighting mode (raw firmware byte, 0–7).
    pub light_mode: u8,
    /// Configuration byte (raw firmware semantics).
    pub configuration: u8,
    /// Deep-sleep toggle byte (raw firmware semantics).
    pub deep_sleep: u8,
    /// Host-labeled RGB colour bytes (presence established; hardware effect
    /// is not).
    pub host_color: [u8; 3],
    /// Sleep timer in minutes.
    pub sleep_timer: u8,
    /// Debounce setting (raw firmware byte).
    pub debounce: u8,
}

/// Partial preferences update — every field is optional.
///
/// Used by [`Request::SetPreferences`] so the CLI can send sparse deltas.
/// BLE operations MUST have a complete stored baseline or explicit-defaults
/// permission to prevent silent zeroing of unread fields.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreferencesDelta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light_mode: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deep_sleep: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_color: Option<[u8; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sleep_timer: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debounce: Option<u8>,
}

/// Summary after a preferences write operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrefsWriteSummary {
    pub profile: u8,
    /// The preferences that were written.
    pub prefs: Box<PreferencesPayload>,
}

// ── Buttons ──────────────────────────────────────────────────────────────────

/// One raw FA61 button-assignment slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ButtonPayload {
    /// Raw action byte.
    pub action: u8,
    /// Raw modifier byte.
    pub modifier: u8,
    /// Raw key / action-value byte (internal name: `key_code` in protocol).
    pub key_code: u8,
}

/// Complete button-assignment table for one profile.
///
/// Matches the FA61 button report (0x08) decode: `profile` index and
/// ordered `slots`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ButtonsStatePayload {
    /// Profile id (1-based).
    pub profile: u8,
    /// Ordered button-assignment slots (typically 18).
    pub slots: Vec<ButtonPayload>,
}

/// Summary after a button write operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ButtonWriteSummary {
    pub profile: u8,
    /// 0-based slot index that was written.
    pub index: u8,
    /// The button assignment that was written.
    pub slot: ButtonPayload,
}

// ── Polling rate ─────────────────────────────────────────────────────────────

/// Global polling rate payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RatePayload {
    /// Polling rate in Hz: 125, 250, 500, or 1000.
    pub hz: u16,
}

/// Summary after a polling-rate write operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateWriteSummary {
    pub hz: u16,
}

// ── Profile metadata ─────────────────────────────────────────────────────────

/// Profile metadata (FA61 report 0x0c).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileMetadataPayload {
    /// Currently active profile (1-based).
    pub current: u8,
    /// Maximum enabled profile (1-based).
    pub maximum: u8,
}

// ── Import / Export ──────────────────────────────────────────────────────────

/// Exported device-state document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportDocument {
    /// Format version — must equal [`EXPORT_FORMAT_VERSION`].
    pub format_version: u32,
    /// ISO-8601 UTC timestamp of export.
    pub exported_at: String,
    /// Devices keyed by stable user-chosen name.
    #[serde(default)]
    pub devices: BTreeMap<String, ExportDeviceEntry>,
}

/// One device's full state as exported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportDeviceEntry {
    /// Transport discriminator: `"wired"`, `"receiver"`, or `"ble"`.
    pub transport: String,
    /// Platform-independent selector.
    pub selector: DeviceSelector,
    /// Desired global polling rate in Hz.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polling_rate: Option<ExportVersioned<u16>>,
    /// Per-profile state keyed by 1-based profile id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<u8, ExportVersioned<ExportProfileState>>,
    /// Cached profile metadata readback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_metadata: Option<ExportVersioned<ExportProfileMetadata>>,
}

/// A versioned value in export form (source + verification + timestamp).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportVersioned<T> {
    pub value: T,
    /// Where the value came from.
    pub source: String,
    /// How confident we are the device accepted it.
    pub verification: String,
    /// ISO-8601 UTC timestamp.
    pub updated_at: String,
}

/// Per-profile state in export form.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportProfileState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpi: Option<ExportVersioned<ExportDpiState>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferences: Option<ExportVersioned<ExportPreferencesState>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buttons: Option<ExportVersioned<ExportButtonsState>>,
}

/// DPI state in export form (no `VersionedState` wrapper).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportDpiState {
    pub profile: u8,
    pub stages: Vec<u16>,
    pub active_stage: u8,
    pub sensor: SensorOptions,
    #[serde(with = "hex_preserved_tail")]
    pub preserved_tail: [u8; 25],
}

/// Preferences state in export form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportPreferencesState {
    pub profile: u8,
    pub light_mode: u8,
    pub configuration: u8,
    pub deep_sleep: u8,
    pub host_color: [u8; 3],
    pub sleep_timer: u8,
    pub debounce: u8,
}

/// Buttons state in export form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportButtonsState {
    pub profile: u8,
    pub slots: Vec<ButtonPayload>,
}

/// Profile metadata in export form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportProfileMetadata {
    pub current: u8,
    pub maximum: u8,
}

/// Summary after an import operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportSummary {
    /// Number of devices imported.
    pub device_count: usize,
    /// Total number of profiles imported across all devices.
    pub profiles_imported: usize,
}

// ── Import validation ────────────────────────────────────────────────────────

/// Structured reason an [`ExportDocument`] is invalid, returned before
/// any hardware access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportValidationError {
    /// Machine-readable error code for the response envelope.
    pub code: ErrorCode,
    /// Human-readable description.
    pub message: String,
}

impl ImportValidationError {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Validate an [`ExportDocument`] before import.
///
/// Checks every structural invariant that the executor needs to trust
/// without hardware access:
///
/// * `format_version` must equal [`EXPORT_FORMAT_VERSION`].
/// * Every button-assignment table must have exactly
///   [`BUTTON_SLOT_COUNT`] slots — padding or truncation is rejected.
///
/// Returns `Ok(())` when the document is structurally valid.  Returns
/// [`ImportValidationError`] with [`ErrorCode::InvalidRequest`] otherwise.
#[must_use]
pub fn validate_export_document(doc: &ExportDocument) -> Result<(), ImportValidationError> {
    if doc.format_version != EXPORT_FORMAT_VERSION {
        return Err(ImportValidationError::new(
            ErrorCode::InvalidRequest,
            format!(
                "unsupported export format version {} (expected {})",
                doc.format_version, EXPORT_FORMAT_VERSION
            ),
        ));
    }

    for (dev_key, dev) in &doc.devices {
        for (&profile_id, profile_entry) in &dev.profiles {
            if let Some(buttons) = &profile_entry.value.buttons {
                let slot_count = buttons.value.slots.len();
                if slot_count != BUTTON_SLOT_COUNT {
                    return Err(ImportValidationError::new(
                        ErrorCode::InvalidRequest,
                        format!(
                            "device \"{dev_key}\" profile {profile_id}: \
                             button table has {slot_count} slots; \
                             exactly {expected} are required",
                            expected = BUTTON_SLOT_COUNT
                        ),
                    ));
                }
            }
        }
    }

    Ok(())
}

// ── Daemon ───────────────────────────────────────────────────────────────────

/// Broker daemon metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonStatusPayload {
    /// OS process ID of the broker.
    pub pid: u32,
    /// Seconds since the broker started.
    pub uptime_secs: u64,
    /// Whether a device session is currently open.
    pub connected: bool,
    /// The device path if connected.
    pub device_path: Option<String>,
    /// The transport in use if connected.
    pub transport: Option<TransportKind>,
    /// Seconds since the last request was processed.
    pub idle_secs: u64,
}

// ── Serde helpers ────────────────────────────────────────────────────────────

/// Serde module for hex-encoding `[u8; 25]` as a 50-char lowercase string.
mod hex_preserved_tail {
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

// ── Constructors ─────────────────────────────────────────────────────────────

impl Message {
    /// Wrap a [`Request`] in a versioned envelope.
    pub fn request(req: Request) -> Self {
        Self {
            v: WIRE_VERSION,
            inner: MessageInner::Request(req),
        }
    }

    /// Wrap a [`Response`] in a versioned envelope.
    pub fn response(res: Response) -> Self {
        Self {
            v: WIRE_VERSION,
            inner: MessageInner::Response(res),
        }
    }
}

impl Response {
    /// Build a successful response.
    pub fn ok(data: ResponseData, provenance: Provenance) -> Self {
        Self {
            provenance,
            result: ResponseResult::Ok(data),
        }
    }

    /// Build an error response.
    pub fn err(code: ErrorCode, message: impl Into<String>, provenance: Provenance) -> Self {
        Self {
            provenance,
            result: ResponseResult::Err {
                code,
                message: message.into(),
            },
        }
    }
}

impl Default for ExecutionContext {
    fn default() -> Self {
        Self {
            transport: TransportKind::Auto,
            device: None,
            no_state: false,
            explicit_defaults: false,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Round-trip helpers ───────────────────────────────────────────────

    /// Serialise → deserialise and assert structural equality.
    fn round_trip<T: Serialize + for<'de> Deserialize<'de> + std::fmt::Debug + PartialEq>(
        value: &T,
    ) {
        let json = serde_json::to_string(value).expect("serialise");
        let restored: T = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(value, &restored, "round-trip mismatch for {json}");
    }

    fn default_ctx() -> ExecutionContext {
        ExecutionContext::default()
    }

    // ── Envelope round-trips ─────────────────────────────────────────────

    #[test]
    fn envelope_request() {
        let msg = Message::request(Request::Ping);
        round_trip(&msg);
        assert_eq!(msg.v, WIRE_VERSION);
    }

    #[test]
    fn envelope_response() {
        let msg = Message::response(Response::ok(ResponseData::Empty, Provenance::UsbValidated));
        round_trip(&msg);
    }

    // ── Request round-trips ──────────────────────────────────────────────

    #[test]
    fn req_ping() {
        round_trip(&Request::Ping);
    }

    #[test]
    fn req_stop() {
        round_trip(&Request::Stop);
    }

    #[test]
    fn req_list_devices() {
        round_trip(&Request::ListDevices { ctx: default_ctx() });
    }

    #[test]
    fn req_use_device() {
        round_trip(&Request::UseDevice {
            selector: DeviceSelector::UsbPath("\\\\?\\HID#VID_1D57&PID_FA61#...".into()),
            transport: TransportKind::Wired,
            ctx: default_ctx(),
        });
    }

    #[test]
    fn req_use_device_ble_name() {
        round_trip(&Request::UseDevice {
            selector: DeviceSelector::BleName("Attack Shark X3".into()),
            transport: TransportKind::Ble,
            ctx: default_ctx(),
        });
    }

    #[test]
    fn req_status() {
        round_trip(&Request::Status {
            profile: 2,
            ctx: default_ctx(),
        });
    }

    #[test]
    fn req_read_dpi() {
        round_trip(&Request::ReadDpi {
            profile: 1,
            ctx: default_ctx(),
        });
    }

    #[test]
    fn req_read_preferences() {
        round_trip(&Request::ReadPreferences {
            profile: 1,
            ctx: default_ctx(),
        });
    }

    #[test]
    fn req_read_buttons() {
        round_trip(&Request::ReadButtons {
            profile: 1,
            ctx: default_ctx(),
        });
    }

    #[test]
    fn req_read_rate() {
        round_trip(&Request::ReadRate { ctx: default_ctx() });
    }

    #[test]
    fn req_battery() {
        round_trip(&Request::Battery { ctx: default_ctx() });
    }

    #[test]
    fn req_set_dpi_full() {
        round_trip(&Request::SetDpi {
            profile: 1,
            stages: Some(vec![400, 800, 1600, 3200]),
            active_stage: Some(2),
            sensor: Some(SensorOptionsDelta {
                lift_off_distance: Some(1),
                ripple_control: Some(true),
                angle_snap: Some(false),
                motion_sync: Some(true),
            }),
            ctx: default_ctx(),
        });
    }

    #[test]
    fn req_set_dpi_partial() {
        // Only change the active stage — leave stages and sensor alone.
        round_trip(&Request::SetDpi {
            profile: 1,
            stages: None,
            active_stage: Some(3),
            sensor: None,
            ctx: default_ctx(),
        });
    }

    #[test]
    fn req_set_dpi_sensor_only() {
        round_trip(&Request::SetDpi {
            profile: 1,
            stages: None,
            active_stage: None,
            sensor: Some(SensorOptionsDelta {
                lift_off_distance: Some(2),
                ripple_control: None,
                angle_snap: Some(true),
                motion_sync: None,
            }),
            ctx: default_ctx(),
        });
    }

    #[test]
    fn req_set_rate() {
        round_trip(&Request::SetRate {
            hz: 1000,
            ctx: default_ctx(),
        });
    }

    #[test]
    fn req_set_prefs_full() {
        round_trip(&Request::SetPreferences {
            profile: 1,
            prefs: PreferencesDelta {
                light_mode: Some(2),
                configuration: Some(1),
                deep_sleep: Some(0),
                host_color: Some([0x12, 0x34, 0x56]),
                sleep_timer: Some(5),
                debounce: Some(0),
            },
            ctx: default_ctx(),
        });
    }

    #[test]
    fn req_set_prefs_partial() {
        // Only change host_color.
        round_trip(&Request::SetPreferences {
            profile: 1,
            prefs: PreferencesDelta {
                light_mode: None,
                configuration: None,
                deep_sleep: None,
                host_color: Some([0xff, 0x00, 0xff]),
                sleep_timer: None,
                debounce: None,
            },
            ctx: default_ctx(),
        });
    }

    #[test]
    fn req_set_prefs_empty_delta() {
        // All-None delta (no-op).
        round_trip(&Request::SetPreferences {
            profile: 1,
            prefs: PreferencesDelta {
                light_mode: None,
                configuration: None,
                deep_sleep: None,
                host_color: None,
                sleep_timer: None,
                debounce: None,
            },
            ctx: default_ctx(),
        });
    }

    #[test]
    fn req_set_button() {
        round_trip(&Request::SetButton {
            profile: 1,
            index: 3,
            assignment: ButtonPayload {
                action: 0x01,
                modifier: 0x00,
                key_code: 0x30,
            },
            ctx: default_ctx(),
        });
    }

    #[test]
    fn req_profile_use() {
        round_trip(&Request::ProfileUse {
            profile: 1,
            ctx: default_ctx(),
        });
    }

    #[test]
    fn req_profile_max() {
        round_trip(&Request::ProfileMax {
            max: 4,
            ctx: default_ctx(),
        });
    }

    #[test]
    fn req_apply() {
        round_trip(&Request::Apply { ctx: default_ctx() });
    }

    #[test]
    fn req_export() {
        round_trip(&Request::Export { ctx: default_ctx() });
    }

    #[test]
    fn req_import() {
        let doc = ExportDocument {
            format_version: EXPORT_FORMAT_VERSION,
            exported_at: "2026-07-24T12:00:00Z".into(),
            devices: BTreeMap::new(),
        };
        round_trip(&Request::Import {
            document: doc,
            ctx: default_ctx(),
        });
    }

    #[test]
    fn req_init_defaults() {
        round_trip(&Request::InitDefaults { ctx: default_ctx() });
    }

    #[test]
    fn req_forget_device() {
        round_trip(&Request::Forget {
            target: ForgetTarget::Device {
                key: "my-x3".into(),
            },
            ctx: default_ctx(),
        });
    }

    #[test]
    fn req_forget_profile() {
        round_trip(&Request::Forget {
            target: ForgetTarget::Profile {
                device_key: "my-x3".into(),
                profile_id: 2,
            },
            ctx: default_ctx(),
        });
    }

    #[test]
    fn req_disconnect() {
        round_trip(&Request::Disconnect);
    }

    #[test]
    fn req_daemon_status() {
        round_trip(&Request::DaemonStatus);
    }

    #[test]
    fn req_daemon_stop() {
        round_trip(&Request::DaemonStop);
    }

    #[test]
    fn req_debug() {
        round_trip(&Request::Debug);
    }

    // ── ExecutionContext ─────────────────────────────────────────────────

    #[test]
    fn execution_context_default() {
        let ctx = ExecutionContext::default();
        assert_eq!(ctx.transport, TransportKind::Auto);
        assert!(ctx.device.is_none());
        assert!(!ctx.no_state);
        assert!(!ctx.explicit_defaults);
        round_trip(&ctx);
    }

    #[test]
    fn execution_context_with_device() {
        let ctx = ExecutionContext {
            transport: TransportKind::Ble,
            device: Some(DeviceSelector::BleName("My X3".into())),
            no_state: true,
            explicit_defaults: true,
        };
        round_trip(&ctx);
    }

    #[test]
    fn device_selector_all_variants() {
        for sel in [
            DeviceSelector::UsbPath("\\\\?\\HID#...".into()),
            DeviceSelector::BleName("Attack Shark X3".into()),
            DeviceSelector::BleAddr("AA:BB:CC:DD:EE:FF".into()),
        ] {
            round_trip(&sel);
        }
    }

    // ── Response round-trips ─────────────────────────────────────────────

    #[test]
    fn res_empty_ok() {
        round_trip(&Response::ok(ResponseData::Empty, Provenance::UsbValidated));
    }

    #[test]
    fn res_device_list() {
        round_trip(&Response::ok(
            ResponseData::DeviceList(vec![DeviceEntry {
                path: "\\\\?\\HID#...".into(),
                vendor_id: 0x1d57,
                product_id: 0xfa61,
                interface_number: 2,
                product: Some("Attack Shark X3".into()),
                serial_number: None,
                transport: TransportKind::Wired,
            }]),
            Provenance::UsbValidated,
        ));
    }

    #[test]
    fn res_dpi_read() {
        let mut tail = [0u8; 25];
        tail[0] = 0xab;
        tail[24] = 0xcd;
        round_trip(&Response::ok(
            ResponseData::DpiRead(Box::new(DpiStatePayload {
                profile: 1,
                stages: vec![800, 1600, 3200, 6400],
                active_stage: 2,
                sensor: SensorOptions {
                    lift_off_distance: 1,
                    ripple_control: true,
                    angle_snap: false,
                    motion_sync: true,
                },
                preserved_tail: tail,
            })),
            Provenance::UsbValidated,
        ));
    }

    #[test]
    fn res_preferences_read() {
        round_trip(&Response::ok(
            ResponseData::PreferencesRead(Box::new(PreferencesPayload {
                profile: 1,
                light_mode: 2,
                configuration: 1,
                deep_sleep: 0,
                host_color: [0x12, 0x34, 0x56],
                sleep_timer: 5,
                debounce: 0,
            })),
            Provenance::UsbValidated,
        ));
    }

    #[test]
    fn res_buttons_read() {
        round_trip(&Response::ok(
            ResponseData::ButtonsRead(Box::new(ButtonsStatePayload {
                profile: 1,
                slots: vec![
                    ButtonPayload {
                        action: 0x01,
                        modifier: 0x00,
                        key_code: 0x30,
                    },
                    ButtonPayload {
                        action: 0x00,
                        modifier: 0x00,
                        key_code: 0x00,
                    },
                ],
            })),
            Provenance::UsbValidated,
        ));
    }

    #[test]
    fn res_rate_read() {
        round_trip(&Response::ok(
            ResponseData::RateRead(Box::new(RatePayload { hz: 1000 })),
            Provenance::UsbValidated,
        ));
    }

    #[test]
    fn res_profile_meta() {
        round_trip(&Response::ok(
            ResponseData::ProfileMetaRead(Box::new(ProfileMetadataPayload {
                current: 2,
                maximum: 4,
            })),
            Provenance::UsbValidated,
        ));
    }

    #[test]
    fn res_battery() {
        round_trip(&Response::ok(
            ResponseData::BatteryLevel(72),
            Provenance::Unverified,
        ));
    }

    #[test]
    fn res_write_summaries() {
        round_trip(&Response::ok(
            ResponseData::DpiWritten(Box::new(DpiWriteSummary {
                profile: 1,
                stages: vec![800, 1600, 3200, 6400],
                stage_count: 4,
                active_stage: 2,
            })),
            Provenance::BleAcknowledged,
        ));
        round_trip(&Response::ok(
            ResponseData::PreferencesWritten(Box::new(PrefsWriteSummary {
                profile: 1,
                prefs: Box::new(PreferencesPayload {
                    profile: 1,
                    light_mode: 2,
                    configuration: 1,
                    deep_sleep: 0,
                    host_color: [0x12, 0x34, 0x56],
                    sleep_timer: 5,
                    debounce: 0,
                }),
            })),
            Provenance::BleAcknowledged,
        ));
        round_trip(&Response::ok(
            ResponseData::ButtonWritten(Box::new(ButtonWriteSummary {
                profile: 1,
                index: 3,
                slot: ButtonPayload {
                    action: 0x01,
                    modifier: 0x00,
                    key_code: 0x30,
                },
            })),
            Provenance::BleAcknowledged,
        ));
        round_trip(&Response::ok(
            ResponseData::RateWritten(Box::new(RateWriteSummary { hz: 500 })),
            Provenance::UsbValidated,
        ));
    }

    #[test]
    fn res_device_status() {
        let tail = [0u8; 25];
        round_trip(&Response::ok(
            ResponseData::DeviceStatus(Box::new(DeviceStatusPayload {
                profile: 1,
                profile_max: 4,
                dpi: Some(DpiStatePayload {
                    profile: 1,
                    stages: vec![800],
                    active_stage: 1,
                    sensor: SensorOptions {
                        lift_off_distance: 1,
                        ripple_control: true,
                        angle_snap: false,
                        motion_sync: false,
                    },
                    preserved_tail: tail,
                }),
                rate_hz: Some(1000),
                prefs: Some(PreferencesPayload {
                    profile: 1,
                    light_mode: 0,
                    configuration: 0,
                    deep_sleep: 0,
                    host_color: [0, 0, 0],
                    sleep_timer: 0,
                    debounce: 0,
                }),
                buttons: Some(vec![ButtonPayload {
                    action: 0x01,
                    modifier: 0x00,
                    key_code: 0x30,
                }]),
                battery: Some(85),
            })),
            Provenance::UsbValidated,
        ));
    }

    #[test]
    fn res_exported() {
        let doc = ExportDocument {
            format_version: EXPORT_FORMAT_VERSION,
            exported_at: "2026-07-24T12:00:00Z".into(),
            devices: BTreeMap::new(),
        };
        round_trip(&Response::ok(
            ResponseData::ExportedState(Box::new(doc)),
            Provenance::Cached,
        ));
    }

    #[test]
    fn res_imported() {
        round_trip(&Response::ok(
            ResponseData::ImportedSummary(Box::new(ImportSummary {
                device_count: 1,
                profiles_imported: 3,
            })),
            Provenance::Cached,
        ));
    }

    #[test]
    fn res_daemon_info() {
        round_trip(&Response::ok(
            ResponseData::DaemonInfo(DaemonStatusPayload {
                pid: 12345,
                uptime_secs: 3600,
                connected: true,
                device_path: Some("\\\\?\\HID#...".into()),
                transport: Some(TransportKind::Wired),
                idle_secs: 10,
            }),
            Provenance::Cached,
        ));
    }

    #[test]
    fn res_error() {
        // Standard error
        round_trip(&Response::err(
            ErrorCode::Unsupported,
            "battery reads are not available on the wired transport",
            Provenance::Unverified,
        ));
    }

    #[test]
    fn res_missing_baseline_error() {
        round_trip(&Response::err(
            ErrorCode::MissingBaseline,
            "no stored baseline for DPI on profile 1; use --explicit-defaults to allow defaults",
            Provenance::Unverified,
        ));
    }

    // ── Error-code round-trips ───────────────────────────────────────────

    #[test]
    fn error_code_all_variants() {
        for code in [
            ErrorCode::Unsupported,
            ErrorCode::TransportFailure,
            ErrorCode::DeviceNotFound,
            ErrorCode::Timeout,
            ErrorCode::InvalidRequest,
            ErrorCode::Internal,
            ErrorCode::MissingBaseline,
        ] {
            round_trip(&code);
        }
    }

    // ── Transport kind round-trips ───────────────────────────────────────

    #[test]
    fn transport_kind_all_variants() {
        for tk in [
            TransportKind::Auto,
            TransportKind::Wired,
            TransportKind::Receiver,
            TransportKind::Ble,
        ] {
            round_trip(&tk);
        }
    }

    // ── Provenance round-trips ───────────────────────────────────────────

    #[test]
    fn provenance_all_variants() {
        for p in [
            Provenance::UsbValidated,
            Provenance::BleAcknowledged,
            Provenance::Cached,
            Provenance::Unverified,
        ] {
            round_trip(&p);
        }
    }

    // ── DpiStatePayload preserved_tail round-trips ────────────────────────

    #[test]
    fn preserved_tail_round_trips() {
        let mut tail = [0u8; 25];
        for i in 0..25 {
            tail[i] = i as u8;
        }
        let payload = DpiStatePayload {
            profile: 1,
            stages: vec![400, 800],
            active_stage: 1,
            sensor: SensorOptions {
                lift_off_distance: 1,
                ripple_control: false,
                angle_snap: false,
                motion_sync: false,
            },
            preserved_tail: tail,
        };
        let json = serde_json::to_string(&payload).expect("serialise");
        // preserved_tail should be a hex string, not a raw array.
        assert!(
            json.contains("000102030405060708090a0b0c0d0e0f101112131415161718"),
            "preserved_tail should be hex-encoded: {json}"
        );
        let restored: DpiStatePayload = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(restored.preserved_tail, tail);
    }

    #[test]
    fn preserved_tail_invalid_length_rejected() {
        let json = r#"{
            "profile":1,
            "stages":[800],
            "active_stage":1,
            "sensor":{"lift_off_distance":1,"ripple_control":false,"angle_snap":false,"motion_sync":false},
            "preserved_tail":"dead"
        }"#;
        let result: Result<DpiStatePayload, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    // ── PreferencesDelta bounds ──────────────────────────────────────────

    #[test]
    fn prefs_delta_all_fields_independent() {
        // Verify that setting one field doesn't require others.
        let delta = PreferencesDelta {
            light_mode: None,
            configuration: None,
            deep_sleep: None,
            host_color: Some([0xaa, 0xbb, 0xcc]),
            sleep_timer: None,
            debounce: None,
        };
        round_trip(&delta);
    }

    // ── Buttons have exactly 18 slots ─────────────────────────────────────

    #[test]
    fn buttons_vec_18_slots() {
        let slots: Vec<ButtonPayload> = vec![
            ButtonPayload {
                action: 0,
                modifier: 0,
                key_code: 0,
            };
            18
        ];
        assert_eq!(slots.len(), 18);
        round_trip(&slots);
    }

    // ── Export document round-trips ──────────────────────────────────────

    #[test]
    fn export_document_round_trip() {
        let mut devices = BTreeMap::new();
        devices.insert(
            "my-x3".to_string(),
            ExportDeviceEntry {
                transport: "wired".into(),
                selector: DeviceSelector::UsbPath("\\\\?\\HID#...".into()),
                polling_rate: Some(ExportVersioned {
                    value: 1000u16,
                    source: "locally-written".into(),
                    verification: "application-unknown".into(),
                    updated_at: "2026-07-24T12:00:00Z".into(),
                }),
                profiles: BTreeMap::new(),
                profile_metadata: None,
            },
        );
        let doc = ExportDocument {
            format_version: EXPORT_FORMAT_VERSION,
            exported_at: "2026-07-24T12:00:00Z".into(),
            devices,
        };
        round_trip(&doc);
    }

    // ── Oversized / malformed rejection ──────────────────────────────────

    #[test]
    fn oversized_message_rejected() {
        assert!(MAX_MESSAGE_BYTES > 1024);
        assert!(MAX_MESSAGE_BYTES < 16 * 1024 * 1024);
    }

    #[test]
    fn malformed_json_rejected() {
        let result: Result<Message, _> = serde_json::from_str("not json");
        assert!(result.is_err());
    }

    #[test]
    fn missing_version_rejected() {
        let result: Result<Message, _> = serde_json::from_str(r#"{"type":"req","op":"ping"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn wrong_version_accepted_with_warning() {
        let msg: Message =
            serde_json::from_str(r#"{"v":99,"type":"req","op":"ping"}"#).expect("deserialise");
        assert_eq!(msg.v, 99);
    }

    #[test]
    fn unknown_op_rejected() {
        let result: Result<Message, _> =
            serde_json::from_str(r#"{"v":1,"type":"req","op":"nonsense"}"#);
        assert!(result.is_err());
    }

    // ── JSON shape assertions ────────────────────────────────────────────

    #[test]
    fn request_json_has_version_and_op() {
        let json = serde_json::to_string(&Message::request(Request::Ping)).unwrap();
        assert!(json.contains(r#""v":1"#));
        assert!(json.contains(r#""op":"ping""#));
        assert!(json.contains(r#""type":"req""#));
    }

    #[test]
    fn response_json_has_provenance() {
        let json = serde_json::to_string(&Message::response(Response::ok(
            ResponseData::Empty,
            Provenance::Cached,
        )))
        .unwrap();
        assert!(json.contains(r#""provenance":"cached""#));
        assert!(json.contains(r#""kind":"empty""#));
    }

    #[test]
    fn error_response_json_has_code_and_message() {
        let json = serde_json::to_string(&Message::response(Response::err(
            ErrorCode::Timeout,
            "too slow",
            Provenance::Unverified,
        )))
        .unwrap();
        assert!(json.contains(r#""code":"timeout""#));
        assert!(json.contains(r#""message":"too slow""#));
    }

    // ── Request JSON value-round-trips (structural asserts) ──────────────

    #[test]
    fn use_device_json_fields() {
        let req = Request::UseDevice {
            selector: DeviceSelector::UsbPath("/dev/hidraw0".into()),
            transport: TransportKind::Receiver,
            ctx: default_ctx(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&json).unwrap();
        match back {
            Request::UseDevice {
                selector,
                transport,
                ..
            } => {
                assert_eq!(transport, TransportKind::Receiver);
                assert!(matches!(selector, DeviceSelector::UsbPath(p) if p == "/dev/hidraw0"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn status_json_fields() {
        let req = Request::Status {
            profile: 3,
            ctx: default_ctx(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""profile":3"#));
        let back: Request = serde_json::from_str(&json).unwrap();
        match back {
            Request::Status { profile, .. } => assert_eq!(profile, 3),
            _ => panic!("wrong variant"),
        }
    }

    // ── Context no_state / explicit_defaults serialisation ───────────────

    #[test]
    fn ctx_no_state_false_omitted() {
        let req = Request::ReadDpi {
            profile: 1,
            ctx: ExecutionContext {
                transport: TransportKind::Wired,
                device: None,
                no_state: false,
                explicit_defaults: false,
            },
        };
        let json = serde_json::to_string(&req).unwrap();
        // no_state and explicit_defaults are false, so they should be omitted
        // when serialising (serde default = false).
        let back: Request = serde_json::from_str(&json).unwrap();
        match back {
            Request::ReadDpi { ctx, .. } => {
                assert!(!ctx.no_state);
                assert!(!ctx.explicit_defaults);
            }
            _ => panic!("wrong variant"),
        }
    }

    // ── PreferencesPayload matches protocol field names ──────────────────

    #[test]
    fn preferences_payload_has_correct_field_names() {
        let payload = PreferencesPayload {
            profile: 1,
            light_mode: 2,
            configuration: 1,
            deep_sleep: 0,
            host_color: [0x11, 0x22, 0x33],
            sleep_timer: 5,
            debounce: 0,
        };
        let json = serde_json::to_string(&payload).unwrap();
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

    // ── SensorOptions has all four fields ────────────────────────────────

    #[test]
    fn sensor_options_has_all_four_fields() {
        let opts = SensorOptions {
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

    // ── Export document validation ───────────────────────────────────────

    fn make_valid_export_doc() -> ExportDocument {
        let mut devices = BTreeMap::new();
        let mut profiles = BTreeMap::new();
        let slots: Vec<ButtonPayload> = vec![
            ButtonPayload {
                action: 0,
                modifier: 0,
                key_code: 0,
            };
            BUTTON_SLOT_COUNT
        ];
        profiles.insert(
            1u8,
            ExportVersioned {
                value: ExportProfileState {
                    dpi: None,
                    preferences: None,
                    buttons: Some(ExportVersioned {
                        value: ExportButtonsState { profile: 1, slots },
                        source: "imported".into(),
                        verification: "persistence-unknown".into(),
                        updated_at: "2026-07-24T12:00:00Z".into(),
                    }),
                },
                source: "imported".into(),
                verification: "persistence-unknown".into(),
                updated_at: "2026-07-24T12:00:00Z".into(),
            },
        );
        devices.insert(
            "my-x3".into(),
            ExportDeviceEntry {
                transport: "ble".into(),
                selector: DeviceSelector::BleName("Attack Shark X3".into()),
                polling_rate: None,
                profiles,
                profile_metadata: None,
            },
        );
        ExportDocument {
            format_version: EXPORT_FORMAT_VERSION,
            exported_at: "2026-07-24T12:00:00Z".into(),
            devices,
        }
    }

    #[test]
    fn validate_export_document_accepts_valid() {
        let doc = make_valid_export_doc();
        assert!(
            validate_export_document(&doc).is_ok(),
            "valid document should pass validation"
        );
    }

    #[test]
    fn validate_export_document_rejects_wrong_version() {
        let mut doc = make_valid_export_doc();
        doc.format_version = 999;
        let err = validate_export_document(&doc).expect_err("wrong version should be rejected");
        assert_eq!(err.code, ErrorCode::InvalidRequest);
        assert!(err.message.contains("999"));
        assert!(err.message.contains(&EXPORT_FORMAT_VERSION.to_string()));
    }

    #[test]
    fn validate_export_document_rejects_short_button_table() {
        let mut doc = make_valid_export_doc();
        // Replace the button table with 17 slots instead of 18.
        if let Some(profile_entry) = doc
            .devices
            .get_mut("my-x3")
            .and_then(|d| d.profiles.get_mut(&1))
        {
            if let Some(buttons_entry) = &mut profile_entry.value.buttons {
                buttons_entry.value.slots = vec![
                    ButtonPayload {
                        action: 0,
                        modifier: 0,
                        key_code: 0
                    };
                    17
                ];
            }
        }
        let err =
            validate_export_document(&doc).expect_err("short button table should be rejected");
        assert_eq!(err.code, ErrorCode::InvalidRequest);
        assert!(err.message.contains("17"));
    }

    #[test]
    fn validate_export_document_rejects_long_button_table() {
        let mut doc = make_valid_export_doc();
        if let Some(profile_entry) = doc
            .devices
            .get_mut("my-x3")
            .and_then(|d| d.profiles.get_mut(&1))
        {
            if let Some(buttons_entry) = &mut profile_entry.value.buttons {
                buttons_entry.value.slots = vec![
                    ButtonPayload {
                        action: 0,
                        modifier: 0,
                        key_code: 0
                    };
                    19
                ];
            }
        }
        let err = validate_export_document(&doc).expect_err("long button table should be rejected");
        assert_eq!(err.code, ErrorCode::InvalidRequest);
        assert!(err.message.contains("19"));
    }

    #[test]
    fn validate_export_document_accepts_no_buttons() {
        let mut doc = make_valid_export_doc();
        // Remove button table entirely — profiles without buttons are fine.
        if let Some(profile_entry) = doc
            .devices
            .get_mut("my-x3")
            .and_then(|d| d.profiles.get_mut(&1))
        {
            profile_entry.value.buttons = None;
        }
        assert!(
            validate_export_document(&doc).is_ok(),
            "document without button tables should pass"
        );
    }
}
