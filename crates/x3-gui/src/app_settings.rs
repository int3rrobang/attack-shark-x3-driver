//! GUI-only preferences stored beside the manager state.
//!
//! The file is `gui-preferences.json` alongside `state.json` (via
//! `StatePaths::resolve().state_file().parent`). It is **not** part of the
//! manager device state, carries its own `schemaVersion = 1`, and never
//! triggers a state-file lock or hardware access.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use attack_shark_x3_manager::{StateError, StatePaths};
use serde::{Deserialize, Serialize};

use crate::presentation::{
    DEFAULT_DPI_DISPLAY_MAX as DEFAULT_DPI_MAX, DEFAULT_DPI_DISPLAY_MIN as DEFAULT_DPI_MIN,
    DPI_MAX, DPI_MIN, DPI_STEP,
};

const GUI_PREFERENCES_FILE_NAME: &str = "gui-preferences.json";
pub const GUI_PREFERENCES_SCHEMA_VERSION: u32 = 1;

const CUSTOM_NAME_DEFAULT: &str = "custom mouse";
const MAX_CUSTOM_NAME_CHARS: usize = 64;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn default_schema_version() -> u32 {
    GUI_PREFERENCES_SCHEMA_VERSION
}
fn default_dpi_min() -> f32 {
    DEFAULT_DPI_MIN
}
fn default_dpi_max() -> f32 {
    DEFAULT_DPI_MAX
}
fn default_true() -> bool {
    true
}
fn default_custom_name() -> String {
    CUSTOM_NAME_DEFAULT.to_owned()
}

/// GUI preferences persisted to `gui-preferences.json`.
///
/// Schema 1 contains only UI presentation state; device state remains in
/// `state.json` and is never rewritten by this module.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuiPreferences {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// 0 = follow system, 1 = light, 2 = dark
    #[serde(default)]
    pub appearance: i32,
    /// 0 = Attack Shark X3, 1 = Kysona M600, 2 = custom
    #[serde(default)]
    pub product_name_choice: i32,
    #[serde(default = "default_custom_name")]
    pub custom_product_name: String,
    #[serde(default = "default_dpi_min")]
    pub dpi_min: f32,
    #[serde(default = "default_dpi_max")]
    pub dpi_max: f32,
    #[serde(default = "default_true")]
    pub dpi_log_scale: bool,
    /// Last visible page (0..5) when the app closed.
    #[serde(default)]
    pub last_page: i32,
}

impl Default for GuiPreferences {
    fn default() -> Self {
        Self {
            schema_version: GUI_PREFERENCES_SCHEMA_VERSION,
            appearance: 0,
            product_name_choice: 0,
            custom_product_name: default_custom_name(),
            dpi_min: DEFAULT_DPI_MIN,
            dpi_max: DEFAULT_DPI_MAX,
            dpi_log_scale: true,
            last_page: 0,
        }
    }
}

impl GuiPreferences {
    /// Validate and clamp every UI-only field conservatively.
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.schema_version = GUI_PREFERENCES_SCHEMA_VERSION;

        self.appearance = match self.appearance {
            1 | 2 => self.appearance,
            _ => 0,
        };

        self.product_name_choice = match self.product_name_choice {
            1 | 2 => self.product_name_choice,
            _ => 0,
        };

        let trimmed = self.custom_product_name.trim().to_owned();
        let truncated: String = trimmed.chars().take(MAX_CUSTOM_NAME_CHARS).collect();
        if truncated.is_empty() {
            self.custom_product_name = default_custom_name();
        } else {
            self.custom_product_name = truncated;
        }

        // DPI min/max: finite and stepped, then bound to global min/max and ensure min < max.
        let min_raw = if self.dpi_min.is_finite() {
            self.dpi_min
        } else {
            DEFAULT_DPI_MIN
        };
        let max_raw = if self.dpi_max.is_finite() {
            self.dpi_max
        } else {
            DEFAULT_DPI_MAX
        };
        let (min_clamped, max_clamped) = dpi_bounds(min_raw, max_raw);
        self.dpi_min = min_clamped;
        self.dpi_max = max_clamped;

        self.last_page = if (0..=5).contains(&self.last_page) {
            self.last_page
        } else {
            0
        };

        self
    }
}

fn round_dpi_step(value: f32) -> u16 {
    let step = (value / DPI_STEP).round().clamp(0.0, f32::from(u16::MAX));
    (step * DPI_STEP) as u16
}

fn dpi_bounds(min: f32, max: f32) -> (f32, f32) {
    let min = (round_dpi_step(min) as f32).clamp(DPI_MIN, DPI_MAX - DPI_STEP);
    let max = (round_dpi_step(max) as f32).clamp(min + DPI_STEP, DPI_MAX);
    (min, max)
}

/// Resolve the sibling `gui-preferences.json` path.
pub fn gui_preferences_path() -> Result<PathBuf, StateError> {
    let state_file = StatePaths::resolve()?.state_file().to_path_buf();
    let parent = state_file
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    Ok(parent.join(GUI_PREFERENCES_FILE_NAME))
}

/// Load preferences from the resolved path, handling missing files as defaults.
pub fn load_gui_preferences() -> (GuiPreferences, Option<String>) {
    let path = match gui_preferences_path() {
        Ok(p) => p,
        Err(err) => {
            return (
                GuiPreferences::default(),
                Some(format!(
                    "GUI preferences path unavailable — using defaults ({err})"
                )),
            );
        }
    };
    load_gui_preferences_from_path(&path)
}

/// Load from an explicit path. Used by tests with temp dirs.
///
/// On malformed JSON or unknown schema version the raw bytes are preserved to
/// an exclusive backup (`*.unreadable.bak` or `*.unsupported-vN.bak`) and
/// defaults are returned with a restrained warning.
pub fn load_gui_preferences_from_path(path: &Path) -> (GuiPreferences, Option<String>) {
    match fs::read(path) {
        Ok(bytes) => {
            if bytes.is_empty() {
                let _ = create_gui_backup_exclusive(path, &bytes, None);
                return (
                    GuiPreferences::default(),
                    Some(
                        "GUI preferences were empty and have been reset to defaults — backup saved"
                            .to_owned(),
                    ),
                );
            }
            // Try to detect schema version first.
            match serde_json::from_slice::<serde_json::Value>(&bytes) {
                Ok(val) => {
                    if let Some(v) = val.get("schemaVersion").and_then(|x| x.as_u64()) {
                        if v != u64::from(GUI_PREFERENCES_SCHEMA_VERSION) {
                            let _ = create_gui_backup_exclusive(path, &bytes, Some(v as u32));
                            return (
                                GuiPreferences::default(),
                                Some(format!(
                                    "GUI preferences schema v{v} is not supported — reset to defaults — backup saved"
                                )),
                            );
                        }
                    } else {
                        // Missing schemaVersion -> treat as malformed.
                        let _ = create_gui_backup_exclusive(path, &bytes, None);
                        return (
                            GuiPreferences::default(),
                            Some(
                                "GUI preferences were unreadable and have been reset to defaults — backup saved"
                                    .to_owned(),
                            ),
                        );
                    }
                    match serde_json::from_slice::<GuiPreferences>(&bytes) {
                        Ok(prefs) => (prefs.normalized(), None),
                        Err(_) => {
                            let _ = create_gui_backup_exclusive(path, &bytes, None);
                            (
                                GuiPreferences::default(),
                                Some(
                                    "GUI preferences were unreadable and have been reset to defaults — backup saved"
                                        .to_owned(),
                                ),
                            )
                        }
                    }
                }
                Err(_) => {
                    let _ = create_gui_backup_exclusive(path, &bytes, None);
                    (
                        GuiPreferences::default(),
                        Some(
                            "GUI preferences were unreadable and have been reset to defaults — backup saved"
                                .to_owned(),
                        ),
                    )
                }
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => (GuiPreferences::default(), None),
        Err(err) => (
            GuiPreferences::default(),
            Some(format!(
                "unable to read GUI preferences — using defaults ({err})"
            )),
        ),
    }
}

/// Save preferences to the resolved path. Returns `true` iff a write occurred.
pub fn save_gui_preferences(prefs: &GuiPreferences) -> Result<bool, StateError> {
    let path = gui_preferences_path()?;
    save_gui_preferences_to_path(prefs, &path)
}

/// Save to an explicit path.
///
/// Coalesces: when the file already contains an equivalent normalized
/// document, no write is performed and `Ok(false)` is returned. No state-file
/// lock or hardware access is performed.
pub fn save_gui_preferences_to_path(
    prefs: &GuiPreferences,
    path: &Path,
) -> Result<bool, StateError> {
    let normalized = prefs.clone().normalized();

    // Coalesce: compare with existing normalized prefs.
    if path.exists() {
        if let Ok(bytes) = fs::read(path) {
            if let Ok(existing) = serde_json::from_slice::<GuiPreferences>(&bytes) {
                if existing.normalized() == normalized {
                    return Ok(false);
                }
            }
        }
    }

    write_atomic_gui(path, &normalized)?;
    Ok(true)
}

fn write_atomic_gui(target: &Path, prefs: &GuiPreferences) -> Result<(), StateError> {
    ensure_parent(target).map_err(|err| StateError::io(target.to_path_buf(), err))?;
    let bytes = serde_json::to_vec_pretty(prefs)?;
    let (temp, mut file) =
        create_temp_file(target).map_err(|err| StateError::io(target.to_path_buf(), err))?;

    let write_result = file.write_all(&bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(err) = write_result {
        let _ = fs::remove_file(&temp);
        return Err(StateError::io(temp, err));
    }

    if let Err(err) = atomic_replace(&temp, target) {
        let _ = fs::remove_file(&temp);
        return Err(StateError::io(target.to_path_buf(), err));
    }

    sync_parent(target).map_err(|err| StateError::io(target.to_path_buf(), err))
}

fn create_temp_file(target: &Path) -> io::Result<(PathBuf, File)> {
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(GUI_PREFERENCES_FILE_NAME);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    for attempt in 0..128_u64 {
        let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{file_name}.tmp.{}.{}.{}",
            std::process::id(),
            timestamp,
            seq.wrapping_add(attempt)
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "unable to allocate unique temp GUI preferences path",
    ))
}

fn atomic_replace(temp: &Path, target: &Path) -> io::Result<()> {
    fs::rename(temp, target)
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let dir = File::open(parent)?;
            dir.sync_all()?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn ensure_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

fn create_gui_backup_exclusive(
    target: &Path,
    bytes: &[u8],
    schema: Option<u32>,
) -> Result<PathBuf, StateError> {
    let file_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(GUI_PREFERENCES_FILE_NAME);
    let stem = match schema {
        Some(v) => format!("{file_name}.unsupported-v{v}"),
        None => format!("{file_name}.unreadable"),
    };
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    ensure_parent(target).map_err(|err| StateError::io(target.to_path_buf(), err))?;
    let mut counter = 1u32;
    loop {
        let candidate = if counter == 1 {
            parent.join(format!("{stem}.bak"))
        } else {
            parent.join(format!("{stem}-{counter}.bak"))
        };
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut f) => {
                f.write_all(bytes)
                    .map_err(|e| StateError::io(candidate.clone(), e))?;
                f.flush()
                    .map_err(|e| StateError::io(candidate.clone(), e))?;
                f.sync_all()
                    .map_err(|e| StateError::io(candidate.clone(), e))?;
                drop(f);
                let _ = sync_parent(&candidate);
                return Ok(candidate);
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                counter += 1;
                if counter > 1000 {
                    return Err(StateError::io(candidate, err));
                }
                continue;
            }
            Err(err) => return Err(StateError::io(candidate, err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn temp_path(dir: &TempDir) -> PathBuf {
        dir.path().join(GUI_PREFERENCES_FILE_NAME)
    }

    #[test]
    fn roundtrip_survives_restart() {
        let dir = TempDir::new().unwrap();
        let path = temp_path(&dir);
        let mut prefs = GuiPreferences::default();
        prefs.appearance = 2;
        prefs.product_name_choice = 1;
        prefs.custom_product_name = "  hello world  ".to_owned();
        prefs.dpi_min = 400.0;
        prefs.dpi_max = 8000.0;
        prefs.dpi_log_scale = false;
        prefs.last_page = 3;

        let wrote = save_gui_preferences_to_path(&prefs, &path).unwrap();
        assert!(wrote, "first write should occur");

        let (loaded, warning) = load_gui_preferences_from_path(&path);
        assert!(warning.is_none(), "valid file should not warn");
        assert_eq!(loaded, prefs.normalized());
    }

    #[test]
    fn unknown_schema_creates_backup_and_defaults() {
        let dir = TempDir::new().unwrap();
        let path = temp_path(&dir);

        let bad = serde_json::json!({
            "schemaVersion": 999,
            "appearance": 2,
            "productNameChoice": 1,
            "customProductName": "hi",
            "dpiMin": 100.0,
            "dpiMax": 5000.0,
            "dpiLogScale": true,
            "lastPage": 2
        });
        fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();

        let (loaded, warning) = load_gui_preferences_from_path(&path);
        assert!(warning.is_some(), "unknown schema should warn");
        assert_eq!(loaded, GuiPreferences::default());

        // Backup must exist exclusively and contain original bytes.
        let backup = dir
            .path()
            .join(format!("{GUI_PREFERENCES_FILE_NAME}.unsupported-v999.bak"));
        assert!(backup.exists(), "backup should exist at {backup:?}");
        let backup_bytes = fs::read(&backup).unwrap();
        let original = serde_json::to_vec(&bad).unwrap();
        assert_eq!(backup_bytes, original);
    }

    #[test]
    fn malformed_creates_backup_and_defaults() {
        let dir = TempDir::new().unwrap();
        let path = temp_path(&dir);
        let malformed = b"{ not valid json }";
        fs::write(&path, malformed).unwrap();

        let (loaded, warning) = load_gui_preferences_from_path(&path);
        assert!(warning.is_some(), "malformed should warn");
        assert_eq!(loaded, GuiPreferences::default());
        let backup = dir
            .path()
            .join(format!("{GUI_PREFERENCES_FILE_NAME}.unreadable.bak"));
        assert!(backup.exists());
        assert_eq!(fs::read(&backup).unwrap(), malformed);
    }

    #[test]
    fn unchanged_no_write() {
        let dir = TempDir::new().unwrap();
        let path = temp_path(&dir);
        let prefs = GuiPreferences {
            appearance: 1,
            product_name_choice: 2,
            custom_product_name: "my mouse".to_owned(),
            dpi_min: 200.0,
            dpi_max: 3200.0,
            dpi_log_scale: true,
            last_page: 1,
            ..Default::default()
        };
        let first = save_gui_preferences_to_path(&prefs, &path).unwrap();
        assert!(first);
        let meta_before = fs::metadata(&path).unwrap().modified().unwrap();

        // Sleep to ensure mtime would differ if rewrite happened (filesystem granularity).
        std::thread::sleep(std::time::Duration::from_millis(20));

        let second = save_gui_preferences_to_path(&prefs, &path).unwrap();
        assert!(!second, "second identical save should not write");
        let meta_after = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(
            meta_before, meta_after,
            "mtime must not change on coalesced save"
        );
    }

    #[test]
    fn validation_clamps_conservatively() {
        let mut prefs = GuiPreferences {
            appearance: 99,
            product_name_choice: -5,
            custom_product_name: "   ".to_owned(),
            dpi_min: 10.0,      // below DPI_MIN
            dpi_max: 100_000.0, // above DPI_MAX
            dpi_log_scale: true,
            last_page: 99,
            ..Default::default()
        };
        let normalized = prefs.clone().normalized();
        assert_eq!(normalized.appearance, 0);
        assert_eq!(normalized.product_name_choice, 0);
        assert_eq!(normalized.custom_product_name, CUSTOM_NAME_DEFAULT);
        assert!(normalized.dpi_min >= DPI_MIN);
        assert!(normalized.dpi_max <= DPI_MAX);
        assert!(normalized.dpi_min < normalized.dpi_max);
        assert_eq!(normalized.last_page, 0);

        // Custom name truncation.
        prefs.custom_product_name = "a".repeat(200);
        let n = prefs.normalized();
        assert_eq!(n.custom_product_name.chars().count(), MAX_CUSTOM_NAME_CHARS);
    }
}
