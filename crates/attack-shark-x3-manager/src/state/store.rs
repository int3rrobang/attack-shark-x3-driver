//! Durable-state paths, cross-process locking, and atomic transactions.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;

use super::model::{SCHEMA_VERSION, StateFile};
use crate::error::StateError;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Environment variable that overrides the resolved `state.json` location.
pub const STATE_PATH_ENV: &str = "ATTACK_SHARK_X3_STATE_PATH";

const PRODUCT_DIR: &str = "attack-shark-x3";
const STATE_FILE_NAME: &str = "state.json";
const LOCK_FILE_NAME: &str = "state.lock";

/// Resolved on-disk locations for the durable state file and its lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatePaths {
    /// The `state.json` file holding serialized durable state.
    pub state_file: PathBuf,
    /// The sibling `state.lock` file used for cross-process serialization.
    pub lock_file: PathBuf,
}

impl StatePaths {
    /// Resolve state and lock paths using the documented platform priority.
    pub fn resolve() -> Result<Self, StateError> {
        let state_file = resolve_state_file()?;
        let lock_file = state_file.with_file_name(LOCK_FILE_NAME);
        Ok(Self {
            state_file,
            lock_file,
        })
    }
}

fn resolve_state_file() -> Result<PathBuf, StateError> {
    if let Some(value) = non_empty_var(STATE_PATH_ENV) {
        return Ok(PathBuf::from(value));
    }

    #[cfg(windows)]
    {
        if let Some(local) = non_empty_var("LOCALAPPDATA") {
            return Ok(PathBuf::from(local).join(PRODUCT_DIR).join(STATE_FILE_NAME));
        }
    }

    #[cfg(not(windows))]
    {
        if let Some(xdg) = non_empty_var("XDG_STATE_HOME") {
            return Ok(PathBuf::from(xdg).join(PRODUCT_DIR).join(STATE_FILE_NAME));
        }
    }

    if let Some(home) = non_empty_var("HOME") {
        return Ok(PathBuf::from(home)
            .join(".local")
            .join("state")
            .join(PRODUCT_DIR)
            .join(STATE_FILE_NAME));
    }

    Err(StateError::state_path_unavailable(
        "no state path override or platform state directory is available",
    ))
}

fn non_empty_var(name: &str) -> Option<String> {
    match env::var(name) {
        Ok(value) if !value.is_empty() => Some(value),
        _ => None,
    }
}

/// Access to the durable state file guarded by a cross-process lock.
#[derive(Debug, Clone)]
pub struct StateStore {
    paths: StatePaths,
}

impl StateStore {
    /// Create a store for explicitly resolved paths.
    pub fn open(paths: StatePaths) -> Self {
        Self { paths }
    }

    /// Create a store using the default platform-resolved paths.
    pub fn with_default_paths() -> Result<Self, StateError> {
        Ok(Self {
            paths: StatePaths::resolve()?,
        })
    }

    /// The resolved paths this store reads and writes.
    pub fn paths(&self) -> &StatePaths {
        &self.paths
    }

    /// Load the latest state.
    ///
    /// A missing file yields `StateFile::default()`. A present file whose
    /// schema is not `SCHEMA_VERSION` is rejected.
    pub fn load(&self) -> Result<StateFile, StateError> {
        load_state(&self.paths.state_file)
    }

    /// Acquire the cross-process lock, reload the latest state, and return a
    /// transaction guard. Dropping the guard without committing leaves the
    /// on-disk state unchanged; committing atomically replaces `state.json`.
    pub fn transaction(&self) -> Result<StateTransaction<'_>, StateError> {
        let lock = LockGuard::acquire(&self.paths.lock_file)?;
        let state = self.load()?;
        Ok(StateTransaction {
            store: self,
            _lock: lock,
            state,
        })
    }

    fn write_atomic(&self, state: &StateFile) -> Result<(), StateError> {
        write_atomic(&self.paths.state_file, state)
    }
}

fn load_state(path: &Path) -> Result<StateFile, StateError> {
    match fs::read(path) {
        Ok(bytes) => {
            let file: StateFile = serde_json::from_slice(&bytes)?;
            if file.schema_version != SCHEMA_VERSION {
                return Err(StateError::UnsupportedSchema {
                    found: file.schema_version,
                    expected: SCHEMA_VERSION,
                });
            }
            file.validate()?;
            Ok(file)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(StateFile::default()),
        Err(err) => Err(StateError::io(path.to_path_buf(), err)),
    }
}

/// A held cross-process lock, released automatically on drop.
#[derive(Debug)]
struct LockGuard {
    file: fs::File,
}

impl LockGuard {
    fn acquire(path: &Path) -> Result<Self, StateError> {
        ensure_parent(path).map_err(|err| StateError::io(path.to_path_buf(), err))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|err| StateError::io(path.to_path_buf(), err))?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Self { file }),
            Err(err) if is_lock_busy(&err) => Err(StateError::LockBusy {
                path: path.to_path_buf(),
            }),
            Err(err) => Err(StateError::io(path.to_path_buf(), err)),
        }
    }
}

fn is_lock_busy(err: &io::Error) -> bool {
    if err.kind() == io::ErrorKind::WouldBlock {
        return true;
    }

    #[cfg(windows)]
    {
        matches!(err.raw_os_error(), Some(32 | 33))
    }
    #[cfg(not(windows))]
    {
        false
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// A serialized state mutation. Holds the lock and the latest `StateFile`.
///
/// Commit consumes the guard and performs exactly one atomic write. Dropping
/// without committing performs no write.
#[derive(Debug)]
pub struct StateTransaction<'a> {
    store: &'a StateStore,
    _lock: LockGuard,
    state: StateFile,
}

impl<'a> StateTransaction<'a> {
    /// Immutable access to the loaded state.
    pub fn state(&self) -> &StateFile {
        &self.state
    }

    /// Mutable access to the state that will be persisted on commit.
    pub fn state_mut(&mut self) -> &mut StateFile {
        &mut self.state
    }

    /// Atomically persist the current state and release the lock.
    pub fn commit(self) -> Result<(), StateError> {
        self.store.write_atomic(&self.state)
    }
}

fn write_atomic(target: &Path, state: &StateFile) -> Result<(), StateError> {
    state.validate()?;
    ensure_parent(target).map_err(|err| StateError::io(target.to_path_buf(), err))?;
    let bytes = serde_json::to_vec_pretty(state)?;
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
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(STATE_FILE_NAME);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    for attempt in 0..128_u64 {
        let sequence = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{file_name}.tmp.{}.{}.{}",
            process::id(),
            timestamp,
            sequence.wrapping_add(attempt),
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
        "unable to allocate a unique temporary state path",
    ))
}

fn atomic_replace(temp: &Path, target: &Path) -> io::Result<()> {
    fs::rename(temp, target)
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let dir = fs::File::open(parent)?;
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

#[cfg(test)]
mod tests {
    use super::{StatePaths, StateStore};
    use crate::device::DeviceIdentity;
    use crate::error::StateError;
    use crate::state::model::{DeviceState, StateFile};
    use std::fs;
    use std::path::Path;

    fn paths_in(dir: &Path) -> StatePaths {
        StatePaths {
            state_file: dir.join("state.json"),
            lock_file: dir.join("state.lock"),
        }
    }

    #[test]
    fn missing_file_loads_default() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::open(paths_in(dir.path()));
        assert_eq!(store.load().unwrap(), StateFile::default());
    }

    #[test]
    fn committed_transaction_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::open(paths_in(dir.path()));

        let txn = store.transaction().unwrap();
        let snapshot = txn.state().clone();
        txn.commit().unwrap();

        assert!(store.paths().state_file.exists());
        assert_eq!(store.load().unwrap(), snapshot);
    }

    #[test]
    fn rejects_foreign_schema() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let store = StateStore::open(paths.clone());

        let mut value = serde_json::to_value(StateFile::default()).unwrap();
        value["schemaVersion"] = serde_json::json!(1);
        fs::write(&paths.state_file, serde_json::to_vec(&value).unwrap()).unwrap();

        match store.load() {
            Err(StateError::UnsupportedSchema { found, expected }) => {
                assert_eq!(found, 1);
                assert_eq!(expected, 2);
            }
            other => panic!("expected UnsupportedSchema, got {other:?}"),
        }
    }

    #[test]
    fn dropped_transaction_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::open(paths_in(dir.path()));

        let txn = store.transaction().unwrap();
        drop(txn);

        assert!(!store.paths().state_file.exists());
        assert_eq!(store.load().unwrap(), StateFile::default());
    }

    #[test]
    fn committed_transaction_persists_changes() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::open(paths_in(dir.path()));

        let mut txn = store.transaction().unwrap();
        let identity =
            DeviceIdentity::ble("test-device", None).expect("valid test device identity");
        let id = identity.id.clone();
        txn.state_mut()
            .devices
            .insert(id.clone(), DeviceState::new(identity));
        txn.state_mut().selected_device = Some(id);
        let updated = txn.state().clone();
        txn.commit().unwrap();

        assert_eq!(store.load().unwrap(), updated);
    }

    #[test]
    fn second_lock_is_rejected_while_held() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::open(paths_in(dir.path()));

        let first = store.transaction().unwrap();
        match store.transaction() {
            Err(StateError::LockBusy { .. }) => {}
            other => panic!("expected LockBusy, got {other:?}"),
        }
        drop(first);

        assert!(store.transaction().is_ok());
    }
}
