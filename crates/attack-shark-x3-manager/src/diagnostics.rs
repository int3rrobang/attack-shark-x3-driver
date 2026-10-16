//! Tiered logging for Attack Shark X3 frontends and headless tools.
//!
//! Phase 1 (core + `ERROR`): a process-wide [`LogLevel`] gate, a synchronous
//! best-effort sink (rotating file and/or stderr), five `log_*!` macros that
//! never evaluate their arguments while disabled, and redaction helpers that
//! keep OS HID paths and full identity tokens out of low-verbosity logs.
//!
//! The sink is std-only and infallible from the caller's perspective: every
//! I/O error is ignored, logging never panics, and [`init`] is first-wins so
//! late or repeated configuration attempts are harmless no-ops.
//!
//! Line format is `{RFC3339} {LEVEL} {target}: {message}`, always a single
//! line. Targets in use: `worker`, `cli`, `manager.*`, `backend.*`, `state`.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Verbosity of one log event (or of the process-wide gate).
///
/// Numeric order matches severity filtering: a gate at level `L` records an
/// event `E` when `E <= L`. `Off` disables everything on either side.
///
/// Back-compat: the bare `X3_LOG=1` form maps to [`LogLevel::Error`], and an
/// unset or empty `X3_LOG` maps to [`LogLevel::Off`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Off = 0,
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

impl LogLevel {
    /// Parses a level name (`"off"`…`"trace"`, case-insensitive) or number
    /// (`"0"`…`"5"`). Surrounding whitespace is ignored. Anything else —
    /// including `""` — returns `None`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "0" => Some(Self::Off),
            "error" | "1" => Some(Self::Error),
            "warn" | "2" => Some(Self::Warn),
            "info" | "3" => Some(Self::Info),
            "debug" | "4" => Some(Self::Debug),
            "trace" | "5" => Some(Self::Trace),
            _ => None,
        }
    }

    /// Reads the level from `X3_LOG`: unset/empty yields `Off`, unrecognized
    /// values fall back to `Error` (see [`from_env_with_note`](Self::from_env_with_note)).
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_env_with_note().0
    }

    /// [`from_env`](Self::from_env) plus a human-readable note describing the
    /// garbage fallback; the note is `Some` only when `X3_LOG` is set to an
    /// unrecognized non-empty value.
    #[must_use]
    pub fn from_env_with_note() -> (Self, Option<String>) {
        // Unset, empty, and non-UTF-8 values all read as `None` and mean Off.
        Self::level_from_raw(
            std::env::var("X3_LOG")
                .ok()
                .filter(|raw| !raw.trim().is_empty()),
        )
    }

    /// Pure core of [`from_env_with_note`](Self::from_env_with_note): `None`
    /// is an unset/empty `X3_LOG` (Off, no note). Split out so the mapping is
    /// unit-testable without mutating the process environment (which is
    /// `unsafe` in edition 2024 and forbidden by this crate's
    /// `#![forbid(unsafe_code)]`).
    fn level_from_raw(raw: Option<String>) -> (Self, Option<String>) {
        match raw {
            None => (Self::Off, None),
            Some(value) if value.trim().is_empty() => (Self::Off, None),
            Some(value) => match Self::parse(&value) {
                Some(level) => (level, None),
                None => (
                    Self::Error,
                    Some(format!(
                        "X3_LOG={value:?} is not a recognized log level; falling back to ERROR"
                    )),
                ),
            },
        }
    }

    /// Uppercase wire name: `"OFF"`, `"ERROR"`, `"WARN"`, `"INFO"`,
    /// `"DEBUG"`, `"TRACE"`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "OFF",
            Self::Error => "ERROR",
            Self::Warn => "WARN",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
            Self::Trace => "TRACE",
        }
    }

    /// Whether a gate at `self` records an event at `event`: both sides must
    /// be enabled and the event must be at or below the gate.
    #[must_use]
    pub fn enabled(self, event: LogLevel) -> bool {
        self != Self::Off && event != Self::Off && event <= self
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Error,
            2 => Self::Warn,
            3 => Self::Info,
            4 => Self::Debug,
            5 => Self::Trace,
            _ => Self::Off,
        }
    }
}

/// Where log lines go.
#[derive(Debug, Clone)]
pub struct LoggerConfig {
    /// Process-wide gate level.
    pub level: LogLevel,
    /// Rotating log file, if any.
    pub file_path: Option<PathBuf>,
    /// Additionally mirror every line to stderr.
    pub mirror_stderr: bool,
}

impl LoggerConfig {
    /// GUI default: level from `X3_LOG`, file at [`default_gui_log_path`],
    /// no stderr mirror (a detached Windows GUI has no console).
    #[must_use]
    pub fn from_env_gui() -> Self {
        Self {
            level: LogLevel::from_env(),
            file_path: default_gui_log_path(),
            mirror_stderr: false,
        }
    }

    /// CLI construction: always mirrors to stderr; the file is optional.
    #[must_use]
    pub fn for_cli(level: LogLevel, file: Option<PathBuf>) -> Self {
        Self {
            level,
            file_path: file,
            mirror_stderr: true,
        }
    }
}

static INITIALIZED: AtomicBool = AtomicBool::new(false);
static LEVEL: AtomicU8 = AtomicU8::new(LogLevel::Off as u8);
static SINK: LazyLock<Mutex<SinkState>> = LazyLock::new(|| {
    Mutex::new(SinkState {
        file: None,
        stderr: false,
    })
});

struct SinkState {
    file: Option<RotatingFile>,
    stderr: bool,
}

/// Installs the process logger. First call wins; later calls are no-ops.
/// The session banner is attempted only when the gate records `INFO`
/// (best-effort, never panics), so a disabled logger touches no sink.
pub fn init(config: LoggerConfig) {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        return;
    }
    LEVEL.store(config.level as u8, Ordering::SeqCst);
    if let Ok(mut sink) = SINK.lock() {
        if config.level != LogLevel::Off
            && let Some(path) = config.file_path
        {
            sink.file = Some(RotatingFile::open(path));
        }
        sink.stderr = config.mirror_stderr;
    }
    if config.level.enabled(LogLevel::Info) {
        append(
            LogLevel::Info,
            "manager",
            &session_banner(env!("CARGO_PKG_NAME"), config.level),
        );
    }
}

/// Whether the process gate currently records events at `level`.
#[must_use]
pub fn is_enabled(level: LogLevel) -> bool {
    current_level().enabled(level)
}

/// The current process gate level (`Off` before [`init`]).
#[must_use]
pub fn current_level() -> LogLevel {
    LogLevel::from_u8(LEVEL.load(Ordering::SeqCst))
}

/// One-line session marker: app, crate version, gate level, the `X3_SIM`
/// selector (or `"(unset)"`), and the resolved state path (or `"(unknown)"`
/// when undiscoverable).
#[must_use]
pub fn session_banner(app: &str, level: LogLevel) -> String {
    let sim = std::env::var("X3_SIM")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "(unset)".to_owned());
    let state = crate::state::StatePaths::resolve()
        .map(|paths| paths.state_file().to_string_lossy().into_owned())
        .unwrap_or_else(|_| "(unknown)".to_owned());
    format!(
        "app={app} version={} level={} X3_SIM={sim} state={state}",
        env!("CARGO_PKG_VERSION"),
        level.as_str(),
    )
}

/// `%LOCALAPPDATA%/attack-shark-x3/gui.log`; `None` when `LOCALAPPDATA` is
/// unset or empty.
#[must_use]
pub fn default_gui_log_path() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    if local.is_empty() {
        return None;
    }
    Some(PathBuf::from(local).join("attack-shark-x3").join("gui.log"))
}

/// Current UTC time as `YYYY-MM-DDTHH:MM:SSZ`, std-only.
#[must_use]
pub fn rfc3339_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format_unix_timestamp(secs)
}

fn format_unix_timestamp(secs: u64) -> String {
    let (year, month, day) = days_to_civil((secs / 86_400) as i64);
    let clock = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        clock / 3_600,
        (clock % 3_600) / 60,
        clock % 60,
    )
}

/// Days since the Unix epoch to civil (year, month, day) via Howard
/// Hinnant's days algorithm. `days` is always non-negative in practice
/// ([`rfc3339_now`] clamps clock skew to zero), so plain truncating division
/// is exact here.
fn days_to_civil(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

/// Laconic endpoint label: `{transport} {vid:pid}` (lowercase hex) for USB,
/// bare `{transport}` for BLE. Never includes the locator (OS HID path / BLE
/// platform id), serial, or display name.
#[must_use]
pub fn redact_endpoint_label(endpoint: &crate::device::DeviceEndpoint) -> String {
    let transport = format!("{:?}", endpoint.transport).to_lowercase();
    match (endpoint.vendor_id, endpoint.product_id) {
        (Some(vid), Some(pid)) => format!("{transport} {vid:04x}:{pid:04x}"),
        _ => transport,
    }
}

/// Token rendering for a gate at `level`:
/// - `Trace` discloses the full 16-byte lowercase hex token (all kinds);
/// - otherwise a `SIM:`-prefixed token renders as its `SIM:{name}` tag with
///   trailing NUL/control padding trimmed;
/// - otherwise only the first two bytes show (`hw:{first2bytes-hex}…`).
///
/// Note: the shorthand contract comment says `<=Info`; the effective rule is
/// `level != Trace` (i.e. `<=Debug`), so `Debug` stays redacted like `Info`.
#[must_use]
pub fn redact_token(token: crate::PhysicalId, level: LogLevel) -> String {
    let bytes = token.token_bytes();
    if level == LogLevel::Trace {
        let mut hex = String::with_capacity(32);
        for byte in bytes {
            let _ = std::fmt::Write::write_fmt(&mut hex, format_args!("{byte:02x}"));
        }
        return hex;
    }
    if bytes.starts_with(b"SIM:") {
        let rest = String::from_utf8_lossy(&bytes[4..]);
        let name = rest.trim_end_matches(|c: char| c == '\0' || c.is_control());
        return format!("SIM:{name}");
    }
    format!("hw:{:02x}{:02x}\u{2026}", bytes[0], bytes[1])
}

/// Length plus the first 64 bytes as lowercase hex: `len={n} hex={…}`.
/// `TRACE`-only helper for payload dumps.
#[must_use]
pub fn redact_bytes(bytes: &[u8]) -> String {
    let shown = bytes.len().min(64);
    let mut hex = String::with_capacity(shown * 2);
    for byte in &bytes[..shown] {
        let _ = std::fmt::Write::write_fmt(&mut hex, format_args!("{byte:02x}"));
    }
    format!("len={} hex={hex}", bytes.len())
}

/// Formats one log line. The target and message are flattened to a single
/// line (CR/LF become spaces) so the file stays one event per line.
fn format_line(level: LogLevel, target: &str, message: &str) -> String {
    format!(
        "{} {} {}: {}",
        rfc3339_now(),
        level.as_str(),
        single_line(target),
        single_line(message),
    )
}

fn single_line(text: &str) -> String {
    // `&[char]` (not the array by value) is the `Pattern` implementor.
    if text.contains(['\r', '\n'].as_slice()) {
        text.replace(['\r', '\n'].as_slice(), " ")
    } else {
        text.to_owned()
    }
}

/// Writes one pre-gated line to the configured sinks. Unconditional by
/// design: gating lives in the `log_*!` macros and [`is_enabled`]; [`init`]
/// uses this directly for the session banner. Best-effort: I/O errors are
/// ignored, and a poisoned sink mutex just drops the line.
#[doc(hidden)]
pub fn append(level: LogLevel, target: &str, message: &str) {
    let line = format_line(level, target, message);
    let Ok(mut sink) = SINK.lock() else {
        return;
    };
    if let Some(file) = sink.file.as_mut() {
        file.append_line(&line);
    }
    if sink.stderr {
        eprintln!("{line}");
    }
}

/// Bounded file sink: `gui.log` plus generations `.1` and `.2`.
///
/// The size counter starts at the existing file length and grows with every
/// write; once it exceeds the cap, rotation deletes `.2`, renames `.1` to
/// `.2` and the base to `.1`, then starts a fresh base carrying a rotation
/// banner. Any rotation I/O failure truncates the base to a banner note so
/// logging stays bounded. Handles are opened per write (never held across
/// renames), so rotation cannot strand a stale descriptor.
struct RotatingFile {
    path: PathBuf,
    max_bytes: u64,
    current_size: u64,
}

/// Default rotation cap: 2 MiB.
const DEFAULT_MAX_BYTES: u64 = 2 * 1024 * 1024;

impl RotatingFile {
    fn open(path: PathBuf) -> Self {
        Self::open_sized(path, DEFAULT_MAX_BYTES)
    }

    /// Tiny-cap constructor for rotation unit tests.
    #[cfg(test)]
    fn with_max_bytes(path: PathBuf, max_bytes: u64) -> Self {
        Self::open_sized(path, max_bytes)
    }

    fn open_sized(path: PathBuf, max_bytes: u64) -> Self {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            let _ = fs::create_dir_all(parent);
        }
        let current_size = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        Self {
            path,
            max_bytes,
            current_size,
        }
    }

    fn append_line(&mut self, line: &str) {
        let mut bytes = Vec::with_capacity(line.len() + 1);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        if append_bytes(&self.path, &bytes).is_err() {
            return;
        }
        self.current_size += bytes.len() as u64;
        if self.current_size > self.max_bytes {
            self.rotate();
        }
    }

    fn rotate(&mut self) {
        let gen1 = generation_path(&self.path, ".1");
        let gen2 = generation_path(&self.path, ".2");
        let rotated = (|| {
            let _ = fs::remove_file(&gen2);
            if gen1.exists() {
                fs::rename(&gen1, &gen2)?;
            }
            fs::rename(&self.path, &gen1)?;
            let banner = format_line(
                LogLevel::Info,
                "manager",
                &format!("rotated log after exceeding {} bytes", self.max_bytes),
            );
            append_bytes(&self.path, format!("{banner}\n").as_bytes())?;
            self.current_size = banner.len() as u64 + 1;
            Ok::<(), std::io::Error>(())
        })();
        if rotated.is_err() {
            let name = self
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.path.to_string_lossy().into_owned());
            let note = format_line(
                LogLevel::Warn,
                "manager",
                &format!("log rotation failed; truncated {name}"),
            );
            let noted = fs::File::create(&self.path)
                .and_then(|mut fresh| fresh.write_all(format!("{note}\n").as_bytes()));
            self.current_size = if noted.is_ok() {
                note.len() as u64 + 1
            } else {
                0
            };
        }
    }
}

fn generation_path(base: &Path, suffix: &str) -> PathBuf {
    let mut text = base.as_os_str().to_owned();
    text.push(suffix);
    PathBuf::from(text)
}

fn append_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

/// Records an `ERROR` event. Arguments are never evaluated while disabled.
#[macro_export]
macro_rules! log_error {
    ($target:expr, $($arg:tt)*) => {
        if $crate::diagnostics::is_enabled($crate::diagnostics::LogLevel::Error) {
            $crate::diagnostics::append(
                $crate::diagnostics::LogLevel::Error,
                $target,
                &format!($($arg)*),
            );
        }
    };
}

/// Records a `WARN` event. Arguments are never evaluated while disabled.
#[macro_export]
macro_rules! log_warn {
    ($target:expr, $($arg:tt)*) => {
        if $crate::diagnostics::is_enabled($crate::diagnostics::LogLevel::Warn) {
            $crate::diagnostics::append(
                $crate::diagnostics::LogLevel::Warn,
                $target,
                &format!($($arg)*),
            );
        }
    };
}

/// Records an `INFO` event. Arguments are never evaluated while disabled.
#[macro_export]
macro_rules! log_info {
    ($target:expr, $($arg:tt)*) => {
        if $crate::diagnostics::is_enabled($crate::diagnostics::LogLevel::Info) {
            $crate::diagnostics::append(
                $crate::diagnostics::LogLevel::Info,
                $target,
                &format!($($arg)*),
            );
        }
    };
}

/// Records a `DEBUG` event. Arguments are never evaluated while disabled.
#[macro_export]
macro_rules! log_debug {
    ($target:expr, $($arg:tt)*) => {
        if $crate::diagnostics::is_enabled($crate::diagnostics::LogLevel::Debug) {
            $crate::diagnostics::append(
                $crate::diagnostics::LogLevel::Debug,
                $target,
                &format!($($arg)*),
            );
        }
    };
}

/// Records a `TRACE` event. Arguments are never evaluated while disabled.
#[macro_export]
macro_rules! log_trace {
    ($target:expr, $($arg:tt)*) => {
        if $crate::diagnostics::is_enabled($crate::diagnostics::LogLevel::Trace) {
            $crate::diagnostics::append(
                $crate::diagnostics::LogLevel::Trace,
                $target,
                &format!($($arg)*),
            );
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::DeviceEndpoint;
    use attack_shark_x3::{PhysicalId, TransportKind};

    #[test]
    fn parse_names_numbers_and_case() {
        let cases = [
            ("off", LogLevel::Off),
            ("OFF", LogLevel::Off),
            ("Off", LogLevel::Off),
            ("0", LogLevel::Off),
            ("error", LogLevel::Error),
            ("ERROR", LogLevel::Error),
            ("Error", LogLevel::Error),
            ("1", LogLevel::Error),
            ("warn", LogLevel::Warn),
            ("WARN", LogLevel::Warn),
            ("2", LogLevel::Warn),
            ("info", LogLevel::Info),
            ("INFO", LogLevel::Info),
            ("3", LogLevel::Info),
            ("debug", LogLevel::Debug),
            ("DEBUG", LogLevel::Debug),
            ("4", LogLevel::Debug),
            ("trace", LogLevel::Trace),
            ("TRACE", LogLevel::Trace),
            ("Trace", LogLevel::Trace),
            ("5", LogLevel::Trace),
            ("  info  ", LogLevel::Info),
        ];
        for (raw, expected) in cases {
            assert_eq!(LogLevel::parse(raw), Some(expected), "input {raw:?}");
        }
    }

    #[test]
    fn parse_rejects_garbage() {
        for raw in [
            "", " ", "6", "-1", "00", "warning", "err", "verbose", "offf", "infox",
        ] {
            assert_eq!(LogLevel::parse(raw), None, "input {raw:?}");
        }
    }

    #[test]
    fn env_back_compat() {
        // `from_env*` read the real process environment, so the mapping is
        // tested through the pure `level_from_raw` core instead: mutating the
        // environment is `unsafe` in edition 2024 and this crate forbids it.
        // Unset/empty `X3_LOG` means Off with no note.
        assert_eq!(LogLevel::level_from_raw(None), (LogLevel::Off, None));
        assert_eq!(
            LogLevel::level_from_raw(Some(String::new())),
            (LogLevel::Off, None)
        );
        assert_eq!(
            LogLevel::level_from_raw(Some("   ".to_owned())),
            (LogLevel::Off, None)
        );

        // Bare `X3_LOG=1` keeps working as Error; valid values carry no note.
        assert_eq!(
            LogLevel::level_from_raw(Some("1".to_owned())),
            (LogLevel::Error, None)
        );
        assert_eq!(
            LogLevel::level_from_raw(Some("DeBuG".to_owned())),
            (LogLevel::Debug, None)
        );

        // Garbage falls back to Error with a note naming value + fallback.
        let (level, note) = LogLevel::level_from_raw(Some("verbose".to_owned()));
        assert_eq!(level, LogLevel::Error);
        let note = note.expect("garbage X3_LOG must produce a note");
        assert!(note.contains("verbose"), "note names the value: {note}");
        assert!(note.contains("ERROR"), "note names the fallback: {note}");

        // The live `from_env` agrees with the ambient variable: on a clean
        // checkout `X3_LOG` is unset (Off); anything else just documents that
        // the developer's shell set it and still round-trips through parse.
        let observed = LogLevel::from_env();
        match std::env::var("X3_LOG")
            .ok()
            .filter(|raw| !raw.trim().is_empty())
        {
            None => assert_eq!(observed, LogLevel::Off),
            Some(raw) => assert_eq!(
                Some(observed),
                LogLevel::parse(&raw).or(Some(LogLevel::Error)),
                "from_env matches parse or the garbage fallback"
            ),
        }
    }

    #[test]
    fn enabled_ordering() {
        assert!(!LogLevel::Off.enabled(LogLevel::Error));
        assert!(!LogLevel::Off.enabled(LogLevel::Trace));
        assert!(!LogLevel::Error.enabled(LogLevel::Off));
        assert!(!LogLevel::Trace.enabled(LogLevel::Off));

        assert!(LogLevel::Error.enabled(LogLevel::Error));
        assert!(!LogLevel::Error.enabled(LogLevel::Warn));

        assert!(LogLevel::Info.enabled(LogLevel::Error));
        assert!(LogLevel::Info.enabled(LogLevel::Warn));
        assert!(LogLevel::Info.enabled(LogLevel::Info));
        assert!(!LogLevel::Info.enabled(LogLevel::Debug));

        assert!(LogLevel::Trace.enabled(LogLevel::Trace));
        assert!(!LogLevel::Trace.enabled(LogLevel::Off));
        for level in [
            LogLevel::Error,
            LogLevel::Warn,
            LogLevel::Info,
            LogLevel::Debug,
            LogLevel::Trace,
        ] {
            assert!(LogLevel::Trace.enabled(level), "{level:?}");
        }
    }

    #[test]
    fn as_str_table() {
        assert_eq!(LogLevel::Off.as_str(), "OFF");
        assert_eq!(LogLevel::Error.as_str(), "ERROR");
        assert_eq!(LogLevel::Warn.as_str(), "WARN");
        assert_eq!(LogLevel::Info.as_str(), "INFO");
        assert_eq!(LogLevel::Debug.as_str(), "DEBUG");
        assert_eq!(LogLevel::Trace.as_str(), "TRACE");
    }

    #[test]
    fn rfc3339_known_timestamps() {
        assert_eq!(format_unix_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_unix_timestamp(1), "1970-01-01T00:00:01Z");
        assert_eq!(format_unix_timestamp(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn rfc3339_now_shape() {
        let now = rfc3339_now();
        assert_eq!(now.len(), 20, "{now}");
        assert!(now.ends_with('Z'), "{now}");
        assert_eq!(&now[10..11], "T", "{now}");
        assert_eq!(&now[4..5], "-", "{now}");
        assert!(
            now[..19]
                .chars()
                .all(|c| c.is_ascii_digit() || c == '-' || c == 'T' || c == ':')
        );
    }

    #[test]
    fn session_banner_shape() {
        let banner = session_banner("my-app", LogLevel::Error);
        assert!(banner.contains("app=my-app"), "{banner}");
        assert!(banner.contains("version="), "{banner}");
        assert!(banner.contains("level=ERROR"), "{banner}");
        assert!(banner.contains("X3_SIM="), "{banner}");
        assert!(banner.contains("state="), "{banner}");
        assert!(!banner.contains('\n'), "single line: {banner}");
    }

    #[test]
    fn redact_endpoint_label_hides_os_path() {
        let path =
            r"\\?\hid#vid_1d57&pid_fa61#7&2d1aa2b&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";
        let endpoint = DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa61,
            None,
            path,
            Some("Attack Shark X3"),
        )
        .expect("endpoint");
        let label = redact_endpoint_label(&endpoint);
        assert_eq!(label, "wired 1d57:fa61");
        assert!(!label.contains("hid"), "no OS path leaks: {label}");

        let ble = DeviceEndpoint::ble("ble-platform-id-XYZ", Some("Office Mouse")).expect("ble");
        let ble_label = redact_endpoint_label(&ble);
        assert_eq!(ble_label, "ble");
        assert!(!ble_label.contains("ble-platform-id-XYZ"), "{ble_label}");
    }

    #[test]
    fn redact_token_sim_vs_hw() {
        let sim = PhysicalId::from_token_bytes([
            b'S', b'I', b'M', b':', b't', b'e', b's', b't', 0, 0, 0, 0, 0, 0, 0, 0,
        ]);
        assert_eq!(redact_token(sim, LogLevel::Info), "SIM:test");
        assert_eq!(redact_token(sim, LogLevel::Debug), "SIM:test");

        let hw = PhysicalId::from_token_bytes([
            0xab, 0xcd, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
            0x0d, 0x0e,
        ]);
        assert_eq!(redact_token(hw, LogLevel::Off), "hw:abcd…");
        assert_eq!(redact_token(hw, LogLevel::Info), "hw:abcd…");
        assert_eq!(redact_token(hw, LogLevel::Debug), "hw:abcd…");
        assert_eq!(
            redact_token(hw, LogLevel::Trace),
            "abcd0102030405060708090a0b0c0d0e"
        );

        // Trace discloses sim tokens byte-for-byte as well.
        let sim_trace = redact_token(sim, LogLevel::Trace);
        assert_eq!(sim_trace.len(), 32, "{sim_trace}");
        assert!(sim_trace.starts_with("53494d3a"), "{sim_trace}");
    }

    #[test]
    fn redact_bytes_truncates() {
        assert_eq!(redact_bytes(&[0x01, 0x02, 0xff]), "len=3 hex=0102ff");
        assert_eq!(redact_bytes(&[]), "len=0 hex=");
        let long: Vec<u8> = (0..=255u8).collect();
        let rendered = redact_bytes(&long);
        assert_eq!(rendered.len(), "len=256 hex=".len() + 128, "{rendered}");
        assert!(rendered.starts_with("len=256 hex=000102"), "{rendered}");
        assert!(rendered.ends_with("3e3f"), "{rendered}");
    }

    #[test]
    fn rotation_keeps_three_generations() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().join("gui.log");
        let mut file = RotatingFile::with_max_bytes(base.clone(), 256);
        for i in 0..50 {
            file.append_line(&format!("line {i:02} padding padding padding padding"));
        }
        assert!(base.exists(), "base always exists");
        assert!(
            generation_path(&base, ".1").exists(),
            "first generation exists after rotation"
        );
        assert!(
            !generation_path(&base, ".3").exists(),
            "never more than base + .1 + .2"
        );
        let current = std::fs::read_to_string(&base).expect("read base");
        assert!(
            current.contains("rotat"),
            "fresh base carries a rotation banner: {current}"
        );
        assert!(
            current.lines().all(|line| !line.is_empty()),
            "one event per line"
        );
    }

    #[test]
    fn rotating_file_tracks_existing_size() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().join("gui.log");
        std::fs::write(&base, vec![b'x'; 100]).expect("seed file");
        let mut file = RotatingFile::with_max_bytes(base.clone(), 1_000_000);
        assert_eq!(file.current_size, 100);
        file.append_line("hello");
        assert_eq!(file.current_size, 100 + "hello".len() as u64 + 1);
    }

    #[test]
    fn format_line_is_single_line() {
        let line = format_line(LogLevel::Info, "worker", "a\nb\rc");
        assert!(!line.contains('\n'), "{line:?}");
        assert!(!line.contains('\r'), "{line:?}");
        assert!(line.contains("INFO"), "{line}");
        assert!(line.contains("worker: a b c"), "{line}");
    }

    #[test]
    fn disabled_macros_do_not_evaluate_args() {
        // The process gate is Off by default, or Error once the init test has
        // run; Trace stays disabled under both, independent of test order.
        let evaluated = std::cell::Cell::new(false);
        crate::log_trace!("test-target", "{}", {
            evaluated.set(true);
            "evaluated"
        });
        assert!(
            !evaluated.get(),
            "disabled log_trace! must not evaluate its arguments"
        );
    }

    #[test]
    fn init_is_first_wins_and_writes_banner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("gui.log");
        super::init(LoggerConfig {
            level: LogLevel::Info,
            file_path: Some(path.clone()),
            mirror_stderr: false,
        });
        let text = std::fs::read_to_string(&path).expect("banner written");
        assert!(text.contains("app=attack-shark-x3-manager"), "{text}");
        assert!(text.contains("level=INFO"), "{text}");

        super::init(LoggerConfig {
            level: LogLevel::Trace,
            file_path: None,
            mirror_stderr: true,
        });
        assert_eq!(
            std::fs::read_to_string(&path).expect("reread").len(),
            text.len(),
            "second init is a no-op"
        );
    }
}
