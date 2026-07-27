//! Durable-state paths, cross-process locking, and atomic transactions.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
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
/// Access to durable state.
///
/// Disk stores use the resolved state and lock paths. Memory stores keep the
/// complete state behind an in-process mutex; cloning a memory store therefore
/// shares both its state and transaction serialization.
#[derive(Debug)]
enum StoreBackend {
    Disk,
    Memory(Mutex<StateFile>),
}

#[derive(Debug, Clone)]
pub struct StateStore {
    paths: StatePaths,
    backend: Arc<StoreBackend>,
}

impl StateStore {
    /// Create a disk-backed store for explicitly resolved paths.
    pub fn open(paths: StatePaths) -> Self {
        Self {
            paths,
            backend: Arc::new(StoreBackend::Disk),
        }
    }

    /// Create a store using the default platform-resolved paths.
    pub fn with_default_paths() -> Result<Self, StateError> {
        let paths = StatePaths::resolve()?;
        Ok(Self::open(paths))
    }

    /// Create a shared in-memory store.
    ///
    /// This constructor does not resolve a path, read a file, create a lock,
    /// or perform any other filesystem operation. Clones share the same
    /// state and serialize transactions through one in-process mutex.
    pub fn memory() -> Self {
        Self {
            paths: StatePaths {
                state_file: PathBuf::from("<memory>/state.json"),
                lock_file: PathBuf::from("<memory>/state.lock"),
            },
            backend: Arc::new(StoreBackend::Memory(Mutex::new(StateFile::default()))),
        }
    }

    /// The resolved paths this store reads and writes.
    ///
    /// Memory stores expose synthetic paths for compatibility with the disk
    /// store API; they are never passed to filesystem APIs.
    pub fn paths(&self) -> &StatePaths {
        &self.paths
    }

    /// Load the latest state.
    ///
    /// A missing file yields `StateFile::default()`. A present file whose
    /// schema is not `SCHEMA_VERSION` is rejected.
    pub fn load(&self) -> Result<StateFile, StateError> {
        match self.backend.as_ref() {
            StoreBackend::Disk => load_state(&self.paths.state_file),
            StoreBackend::Memory(shared) => {
                let state = lock_memory(shared);
                state.validate()?;
                Ok(state.clone())
            }
        }
    }

    /// Acquire the cross-process or in-process lock, reload the latest state,
    /// and return a transaction guard. Dropping the guard without committing
    /// leaves the state unchanged; committing atomically replaces the disk
    /// file or the shared in-memory snapshot.
    pub fn transaction(&self) -> Result<StateTransaction<'_>, StateError> {
        match self.backend.as_ref() {
            StoreBackend::Disk => {
                let lock = LockGuard::acquire(&self.paths.lock_file)?;
                let state = self.load()?;
                Ok(StateTransaction {
                    store: self,
                    _lock: TransactionLock::Disk(lock),
                    state,
                })
            }
            StoreBackend::Memory(shared) => {
                let lock = lock_memory(shared);
                let state = lock.clone();
                Ok(StateTransaction {
                    store: self,
                    _lock: TransactionLock::Memory(lock),
                    state,
                })
            }
        }
    }

    fn write_atomic(&self, state: &StateFile) -> Result<(), StateError> {
        write_atomic(&self.paths.state_file, state)
    }
}

fn lock_memory<'a>(state: &'a Mutex<StateFile>) -> MutexGuard<'a, StateFile> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
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

/// A held disk or in-process transaction lock.
#[derive(Debug)]
enum TransactionLock<'a> {
    Disk(LockGuard),
    Memory(MutexGuard<'a, StateFile>),
}

/// A serialized state mutation. Holds the lock and the latest `StateFile`.
///
/// Commit consumes the guard and performs exactly one atomic write or shared
/// in-memory replacement. Dropping without committing performs no write.
#[derive(Debug)]
pub struct StateTransaction<'a> {
    store: &'a StateStore,
    _lock: TransactionLock<'a>,
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
        let Self {
            store,
            _lock: lock,
            state,
        } = self;
        match lock {
            TransactionLock::Disk(_lock) => store.write_atomic(&state),
            TransactionLock::Memory(mut shared) => {
                state.validate()?;
                *shared = state;
                Ok(())
            }
        }
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

/// Replace `target` by renaming the fully-written, fsynced `temp` over it.
///
/// On Unix this is a single `rename(2)` (atomic within a filesystem). On
/// Windows `std::fs::rename` resolves to `MoveFileExW` with
/// `MOVEFILE_REPLACE_EXISTING`, which atomically swaps the destination in a
/// single metadata operation; the temp content was already `sync_all`'d and
/// its handle dropped before this call, so there is no torn-write window. NTFS
/// journals metadata operations, so the rename is durable across crashes
/// without an explicit directory `fsync` (see the Windows `sync_parent` below).
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
    // No explicit directory fsync on Windows: NTFS journals the rename performed
    // by `atomic_replace`, so the metadata change is durable once it returns.
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
    fn memory_store_clones_share_state_but_separate_stores_do_not() {
        let store = StateStore::memory();
        let clone = store.clone();
        let separate = StateStore::memory();
        let identity =
            DeviceIdentity::ble("memory-test-device", None).expect("valid test identity");
        let id = identity.id.clone();

        let mut transaction = store.transaction().unwrap();
        transaction
            .state_mut()
            .devices
            .insert(id.clone(), DeviceState::new(identity));
        transaction.state_mut().selected_device = Some(id);
        transaction.commit().unwrap();

        assert_eq!(clone.load().unwrap(), store.load().unwrap());
        assert_eq!(separate.load().unwrap(), StateFile::default());
        assert!(!store.paths().state_file.exists());
        assert!(!store.paths().lock_file.exists());
    }

    #[test]
    fn dropped_memory_transaction_does_not_commit() {
        let store = StateStore::memory();
        let mut transaction = store.transaction().unwrap();
        transaction.state_mut().schema_version = 1;
        drop(transaction);

        assert_eq!(store.load().unwrap(), StateFile::default());
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
