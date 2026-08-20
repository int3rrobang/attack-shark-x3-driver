//! Durable-state paths, cross-process locking, and atomic transactions.

use std::collections::HashSet;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fs2::FileExt;

use super::model::{SCHEMA_VERSION, StateFile};
use crate::device::DeviceId;
use crate::error::{ManagerError, StateError};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SchemaHeader {
    schema_version: u32,
}

/// Environment variable that overrides the resolved `state.json` location.
pub const STATE_PATH_ENV: &str = "ATTACK_SHARK_X3_STATE_PATH";

const PRODUCT_DIR: &str = "attack-shark-x3";
const STATE_FILE_NAME: &str = "state.json";
const LOCK_FILE_NAME: &str = "state.lock";

/// Resolved on-disk locations for the durable state file and its lock.
///
/// The lock is always the sibling `state.lock` next to `state.json`; it is
/// derived, not independently configurable, so two stores cannot protect the
/// same state file with different locks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatePaths {
    state_file: PathBuf,
    lock_file: PathBuf,
}

impl StatePaths {
    /// Create paths from the canonical state file, deriving the lock sibling.
    pub fn new(state_file: PathBuf) -> Self {
        let lock_file = state_file.with_file_name(LOCK_FILE_NAME);
        Self {
            state_file,
            lock_file,
        }
    }

    /// Resolve state and lock paths using the documented platform priority.
    pub fn resolve() -> Result<Self, StateError> {
        let state_file = resolve_state_file()?;
        Ok(Self::new(state_file))
    }

    /// The canonical `state.json` path.
    pub fn state_file(&self) -> &Path {
        &self.state_file
    }

    /// The derived `state.lock` sibling.
    pub fn lock_file(&self) -> &Path {
        &self.lock_file
    }
}

/// Outcome of discarding an unreadable state file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateReset {
    /// Where the unreadable file was preserved, when a file existed.
    pub backup: Option<PathBuf>,
    /// The schema version the discarded file declared, when its header parsed.
    pub discarded_schema: Option<u32>,
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
    Memory(MemoryBackend),
}

#[derive(Debug)]
struct MemoryBackend {
    state: Mutex<StateFile>,
    op_locks: Mutex<HashSet<String>>,
}

#[derive(Debug, Clone)]
pub struct StateStore {
    paths: StatePaths,
    backend: Arc<StoreBackend>,
}

impl StateStore {
    /// Create a disk-backed store for explicitly resolved paths.
    ///
    /// The `StatePaths` invariant guarantees the lock is the sibling
    /// `state.lock`; callers cannot supply a mismatched pair.
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
            paths: StatePaths::new(PathBuf::from("<memory>/state.json")),
            backend: Arc::new(StoreBackend::Memory(MemoryBackend {
                state: Mutex::new(StateFile::default()),
                op_locks: Mutex::new(HashSet::new()),
            })),
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
            StoreBackend::Disk => load_state(self.paths.state_file()),
            StoreBackend::Memory(mem) => {
                let state = lock_memory(&mem.state);
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
                let lock = LockGuard::acquire(self.paths.lock_file())?;
                let state = self.load()?;
                let original = state.clone();
                Ok(StateTransaction {
                    store: self,
                    _lock: TransactionLock::Disk(lock),
                    state,
                    original,
                })
            }
            StoreBackend::Memory(mem) => {
                let lock = lock_memory(&mem.state);
                let state = lock.clone();
                let original = state.clone();
                Ok(StateTransaction {
                    store: self,
                    _lock: TransactionLock::Memory(lock),
                    state,
                    original,
                })
            }
        }
    }

    /// Replace an unreadable state file with a fresh, empty, current-schema
    /// state, preserving the old bytes at a sibling backup path.
    ///
    /// Use when [`Self::load`] fails on a disk-backed store. The method
    /// refuses to discard a file that loads successfully, so a valid state can
    /// never be destroyed by accident. Memory stores hold no file and reject
    /// this operation.
    pub fn discard_unreadable(&self) -> Result<StateReset, StateError> {
        match self.backend.as_ref() {
            StoreBackend::Memory(_) => Err(StateError::invalid_state(
                "memory stores hold no state file to discard",
            )),
            StoreBackend::Disk => self.discard_disk_unreadable(),
        }
    }

    fn discard_disk_unreadable(&self) -> Result<StateReset, StateError> {
        let target = self.paths.state_file();
        let lock_path = self.paths.lock_file();
        let _lock = LockGuard::acquire(lock_path)?;
        let bytes = match fs::read(target) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                write_atomic(target, &StateFile::default())?;
                return Ok(StateReset {
                    backup: None,
                    discarded_schema: None,
                });
            }
            Err(err) => return Err(StateError::io(target.to_path_buf(), err)),
        };
        if is_loadable_bytes(&bytes).is_ok() {
            return Err(StateError::invalid_state(format!(
                "state file {} is readable with the current schema; refusing to discard it",
                target.display()
            )));
        }
        let discarded_schema = serde_json::from_slice::<SchemaHeader>(&bytes)
            .ok()
            .map(|header| header.schema_version);
        let backup = create_backup_exclusive(target, &bytes, discarded_schema)?;
        write_atomic(target, &StateFile::default())?;
        Ok(StateReset {
            backup: Some(backup),
            discarded_schema,
        })
    }

    fn write_atomic(&self, state: &StateFile) -> Result<(), StateError> {
        write_atomic(self.paths.state_file(), state)
    }

    fn device_lock_path(&self, device: &DeviceId) -> PathBuf {
        let state_path = self.paths.state_file();
        let parent = state_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        parent.join(format!("device-{device}.lock"))
    }

    /// Acquire a per-device operation lock with a finite timeout.
    ///
    /// The returned guard does **not** hold the state-file lock; the two
    /// scopes are distinct. Disk locks live beside the state file using a
    /// filesystem-safe encoding of the `DeviceId`. Memory stores use an
    /// in-process set with coherent contention semantics for tests.
    pub fn acquire_operation_lock(
        &self,
        device: &DeviceId,
        timeout: Duration,
        operation: &'static str,
    ) -> Result<DeviceOperationGuard, ManagerError> {
        match self.backend.as_ref() {
            StoreBackend::Disk => self.acquire_operation_lock_disk(device, timeout, operation),
            StoreBackend::Memory(_) => {
                self.acquire_operation_lock_memory(device, timeout, operation)
            }
        }
    }

    fn acquire_operation_lock_disk(
        &self,
        device: &DeviceId,
        timeout: Duration,
        operation: &'static str,
    ) -> Result<DeviceOperationGuard, ManagerError> {
        let path = self.device_lock_path(device);
        if let Err(err) = ensure_parent(&path) {
            return Err(ManagerError::State(StateError::io(path.clone(), err)));
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match LockGuard::try_acquire(&path) {
                Ok(guard) => {
                    return Ok(DeviceOperationGuard {
                        device: device.clone(),
                        path,
                        backend: OperationGuardBackend::Disk { _guard: guard },
                    });
                }
                Err(StateError::LockBusy { .. }) => {
                    if std::time::Instant::now() >= deadline {
                        return Err(ManagerError::DeviceOperationBusy {
                            device: device.clone(),
                            operation,
                            timeout,
                            path: path.clone(),
                        });
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(other) => return Err(ManagerError::State(other)),
            }
        }
    }

    fn acquire_operation_lock_memory(
        &self,
        device: &DeviceId,
        timeout: Duration,
        operation: &'static str,
    ) -> Result<DeviceOperationGuard, ManagerError> {
        let key = device.as_str().to_owned();
        let path = self.device_lock_path(device);
        let deadline = std::time::Instant::now() + timeout;
        let backend = Arc::clone(&self.backend);
        loop {
            let StoreBackend::Memory(mem) = backend.as_ref() else {
                debug_assert!(false, "memory operation lock called on disk backend");
                return Err(ManagerError::State(StateError::invalid_state(
                    "memory operation lock called on disk backend",
                )));
            };
            let mut set = lock_memory_hashset(&mem.op_locks);
            if !set.contains(&key) {
                set.insert(key.clone());
                drop(set);
                return Ok(DeviceOperationGuard {
                    device: device.clone(),
                    path,
                    backend: OperationGuardBackend::Memory {
                        key: key.clone(),
                        backend: Arc::clone(&backend),
                    },
                });
            }
            if std::time::Instant::now() >= deadline {
                return Err(ManagerError::DeviceOperationBusy {
                    device: device.clone(),
                    operation,
                    timeout,
                    path: path.clone(),
                });
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[cfg(any(feature = "usb", feature = "ble"))]
    /// Load the latest state without blocking the async executor.
    ///
    /// Clones the store and runs the complete blocking load/lock/parse/validate
    /// operation inside a single `spawn_blocking` call; the `JoinError` is
    /// mapped consistently with `mutate_async`.
    pub async fn load_async(&self) -> Result<StateFile, StateError> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.load())
            .await
            .map_err(|err| StateError::invalid_state(format!("spawn_blocking join error: {err}")))?
    }

    #[cfg(any(feature = "usb", feature = "ble"))]
    /// Run a complete blocking lock/load/mutate/serialize/fsync transaction
    /// inside `spawn_blocking`.
    ///
    /// The closure runs under the state-file lock and its mutation is committed
    /// atomically; the entire transaction (including `fsync`) executes off the
    /// async executor, not as piecemeal `tokio::fs` operations.
    pub async fn mutate_async<F, R>(&self, f: F) -> Result<R, StateError>
    where
        F: FnOnce(&mut StateFile) -> R + Send + 'static,
        R: Send + 'static,
    {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut txn = store.transaction()?;
            let result = f(txn.state_mut());
            txn.commit()?;
            Ok(result)
        })
        .await
        .map_err(|err| StateError::invalid_state(format!("spawn_blocking join error: {err}")))?
    }

    #[cfg(any(feature = "usb", feature = "ble"))]
    /// Runs a fallible mutation and commits only when the closure succeeds.
    pub async fn try_mutate_async<F, R>(&self, f: F) -> Result<R, StateError>
    where
        F: FnOnce(&mut StateFile) -> Result<R, StateError> + Send + 'static,
        R: Send + 'static,
    {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut txn = store.transaction()?;
            let result = f(txn.state_mut())?;
            txn.commit()?;
            Ok(result)
        })
        .await
        .map_err(|err| StateError::invalid_state(format!("spawn_blocking join error: {err}")))?
    }
}

fn lock_memory<'a>(state: &'a Mutex<StateFile>) -> MutexGuard<'a, StateFile> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn lock_memory_hashset<'a>(state: &'a Mutex<HashSet<String>>) -> MutexGuard<'a, HashSet<String>> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn is_loadable_bytes(bytes: &[u8]) -> Result<StateFile, StateError> {
    let header: SchemaHeader = serde_json::from_slice(bytes)?;
    if header.schema_version != SCHEMA_VERSION {
        return Err(StateError::UnsupportedSchema {
            found: header.schema_version,
            expected: SCHEMA_VERSION,
        });
    }
    let file: StateFile = serde_json::from_slice(bytes)?;
    file.validate()?;
    Ok(file)
}

fn create_backup_exclusive(
    target: &Path,
    bytes: &[u8],
    schema: Option<u32>,
) -> Result<PathBuf, StateError> {
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(STATE_FILE_NAME);
    let stem = match schema {
        Some(found) => format!("{file_name}.unsupported-v{found}"),
        None => format!("{file_name}.unreadable"),
    };
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
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
            Ok(mut file) => {
                file.write_all(bytes)
                    .map_err(|err| StateError::io(candidate.clone(), err))?;
                file.flush()
                    .map_err(|err| StateError::io(candidate.clone(), err))?;
                file.sync_all()
                    .map_err(|err| StateError::io(candidate.clone(), err))?;
                drop(file);
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

fn load_state(path: &Path) -> Result<StateFile, StateError> {
    match fs::read(path) {
        Ok(bytes) => {
            let header: SchemaHeader = serde_json::from_slice(&bytes)?;
            if header.schema_version != SCHEMA_VERSION {
                return Err(StateError::UnsupportedSchema {
                    found: header.schema_version,
                    expected: SCHEMA_VERSION,
                });
            }
            let file: StateFile = serde_json::from_slice(&bytes)?;
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
        Self::try_acquire(path)
    }

    fn try_acquire(path: &Path) -> Result<Self, StateError> {
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
    original: StateFile,
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
    ///
    /// Validation occurs once at this authoritative boundary; callers do not
    /// need to call `validate()` before committing. If the state is unchanged
    /// from the original loaded snapshot, no serialization or `fsync` is
    /// performed.
    pub fn commit(self) -> Result<(), StateError> {
        let Self {
            store,
            _lock: lock,
            state,
            original,
        } = self;
        if state == original {
            return Ok(());
        }
        match lock {
            TransactionLock::Disk(_lock) => store.write_atomic(&state),
            TransactionLock::Memory(mut shared) => {
                state.validate()?;
                *shared = state;
                Ok(())
            }
        }
    }

    /// Explicitly validate the current state without committing.
    ///
    /// Provided for early feedback; `commit` will still validate once at the
    /// boundary, so duplicate validation is not required.
    pub fn validate(&self) -> Result<(), StateError> {
        self.state.validate()
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
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Per-device operation lock guard – distinct from the state-file lock
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum OperationGuardBackend {
    Disk {
        _guard: LockGuard,
    },
    Memory {
        key: String,
        backend: Arc<StoreBackend>,
    },
}

/// A held per-device operation lock.
///
/// Holding this guard does **not** hold the state-file lock; the two scopes
/// are distinct. Disk locks are `device-<encoded>.lock` files beside the state
/// file. Memory stores use an in-process set for coherent test behavior.
#[derive(Debug)]
pub struct DeviceOperationGuard {
    device: DeviceId,
    path: PathBuf,
    backend: OperationGuardBackend,
}

impl DeviceOperationGuard {
    /// The device this guard protects.
    pub fn device(&self) -> &DeviceId {
        &self.device
    }

    /// The lock file path (synthetic for memory stores).
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DeviceOperationGuard {
    fn drop(&mut self) {
        if let OperationGuardBackend::Memory { key, backend } = &self.backend {
            let StoreBackend::Memory(mem) = backend.as_ref() else {
                debug_assert!(false, "memory guard on disk backend");
                return;
            };
            let mut set = lock_memory_hashset(&mem.op_locks);
            set.remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{StatePaths, StateStore};
    use crate::device::DeviceIdentity;
    use crate::error::StateError;
    use crate::state::model::{DeviceState, SCHEMA_VERSION, StateFile};
    use attack_shark_x3::ProfileId;
    use std::fs;
    use std::path::Path;
    use std::time::Duration;

    fn paths_in(dir: &Path) -> StatePaths {
        StatePaths::new(dir.join("state.json"))
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

        // Use a changed transaction so the atomic write is exercised.
        let mut txn = store.transaction().unwrap();
        let identity = DeviceIdentity::test_ble("round-trip-test", None).unwrap();
        let id = identity.id.clone();
        txn.state_mut()
            .devices
            .insert(id.clone(), DeviceState::new(identity));
        if txn.state().next_device_number <= 999 {
            txn.state_mut().next_device_number = 1000;
        }
        let snapshot = txn.state().clone();
        txn.commit().unwrap();

        assert!(store.paths().state_file().exists());
        assert_eq!(store.load().unwrap(), snapshot);
    }

    #[test]
    fn rejects_foreign_schema() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let store = StateStore::open(paths.clone());

        let mut value = serde_json::to_value(StateFile::default()).unwrap();
        value["schemaVersion"] = serde_json::json!(1);
        fs::write(paths.state_file(), serde_json::to_vec(&value).unwrap()).unwrap();

        match store.load() {
            Err(StateError::UnsupportedSchema { found, expected }) => {
                assert_eq!(found, 1);
                assert_eq!(expected, SCHEMA_VERSION);
            }
            other => panic!("expected UnsupportedSchema, got {other:?}"),
        }
    }

    #[test]
    fn rejects_foreign_schema_before_decoding_body() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let store = StateStore::open(paths.clone());
        let foreign_schema = SCHEMA_VERSION - 1;
        let value = serde_json::json!({
            "schemaVersion": foreign_schema,
        });
        fs::write(paths.state_file(), serde_json::to_vec(&value).unwrap()).unwrap();

        match store.load() {
            Err(StateError::UnsupportedSchema { found, expected }) => {
                assert_eq!(found, foreign_schema);
                assert_eq!(expected, SCHEMA_VERSION);
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

        assert!(!store.paths().state_file().exists());
        assert_eq!(store.load().unwrap(), StateFile::default());
    }

    #[test]
    fn committed_transaction_persists_changes() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::open(paths_in(dir.path()));

        let mut txn = store.transaction().unwrap();
        let identity =
            DeviceIdentity::test_ble("test-device", None).expect("valid test device identity");
        let id = identity.id.clone();
        txn.state_mut()
            .devices
            .insert(id.clone(), DeviceState::new(identity));
        // Ensure nextDeviceNumber is monotonic beyond the placeholder 999.
        if txn.state().next_device_number <= 999 {
            txn.state_mut().next_device_number = 1000;
        }
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
            DeviceIdentity::test_ble("memory-test-device", None).expect("valid test identity");
        let id = identity.id.clone();

        let mut transaction = store.transaction().unwrap();
        transaction
            .state_mut()
            .devices
            .insert(id.clone(), DeviceState::new(identity));
        if transaction.state().next_device_number <= 999 {
            transaction.state_mut().next_device_number = 1000;
        }
        transaction.state_mut().selected_device = Some(id);
        transaction.commit().unwrap();

        assert_eq!(clone.load().unwrap(), store.load().unwrap());
        assert_eq!(separate.load().unwrap(), StateFile::default());
        assert!(!store.paths().state_file().exists());
        assert!(!store.paths().lock_file().exists());
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

    #[test]
    fn discard_unreadable_replaces_foreign_schema_with_backup() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let store = StateStore::open(paths.clone());

        let mut value = serde_json::to_value(StateFile::default()).unwrap();
        value["schemaVersion"] = serde_json::json!(7);
        let original = serde_json::to_vec(&value).unwrap();
        fs::write(paths.state_file(), &original).unwrap();

        let reset = store.discard_unreadable().unwrap();
        assert_eq!(reset.discarded_schema, Some(7));
        let backup = reset.backup.expect("old file must be preserved");
        assert_eq!(fs::read(&backup).unwrap(), original);
        assert_eq!(store.load().unwrap(), StateFile::default());

        match store.discard_unreadable() {
            Err(StateError::InvalidState(_)) => {}
            other => panic!("expected InvalidState refusal, got {other:?}"),
        }
    }

    #[test]
    fn discard_unreadable_replaces_unparseable_state_with_backup() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let store = StateStore::open(paths.clone());

        let original = b"{ this is not a state document".to_vec();
        fs::write(paths.state_file(), &original).unwrap();

        let reset = store.discard_unreadable().unwrap();
        assert_eq!(reset.discarded_schema, None);
        let backup = reset.backup.expect("old file must be preserved");
        assert_eq!(fs::read(&backup).unwrap(), original);
        assert_eq!(store.load().unwrap(), StateFile::default());
    }

    #[test]
    fn discard_unreadable_refuses_readable_state() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let store = StateStore::open(paths.clone());
        // Ensure a readable file exists.
        {
            let mut txn = store.transaction().unwrap();
            let identity = DeviceIdentity::test_ble("readable-test", None).unwrap();
            let id = identity.id.clone();
            txn.state_mut()
                .devices
                .insert(id.clone(), DeviceState::new(identity));
            if txn.state().next_device_number <= 999 {
                txn.state_mut().next_device_number = 1000;
            }
            txn.state_mut().selected_device = Some(id);
            txn.commit().unwrap();
        }
        let committed = store.load().unwrap();

        match store.discard_unreadable() {
            Err(StateError::InvalidState(_)) => {}
            other => panic!("expected InvalidState refusal, got {other:?}"),
        }
        assert_eq!(store.load().unwrap(), committed);
    }

    #[test]
    fn discard_unreadable_without_file_writes_default() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let store = StateStore::open(paths);

        let reset = store.discard_unreadable().unwrap();
        assert_eq!(reset.backup, None);
        assert_eq!(reset.discarded_schema, None);
        assert!(store.paths().state_file().exists());
        assert_eq!(store.load().unwrap(), StateFile::default());
    }

    #[test]
    fn discard_unreadable_rejects_memory_store() {
        let store = StateStore::memory();
        match store.discard_unreadable() {
            Err(StateError::InvalidState(_)) => {}
            other => panic!("expected InvalidState refusal, got {other:?}"),
        }
    }

    #[test]
    fn profile_names_round_trip_through_disk_store() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let store = StateStore::open(paths.clone());
        let identity = DeviceIdentity::test_ble("named-test", None).expect("valid test identity");
        let id = identity.id.clone();
        let profile = ProfileId::new(2).expect("profile");

        let mut txn = store.transaction().unwrap();
        if txn.state().next_device_number <= 999 {
            txn.state_mut().next_device_number = 1000;
        }
        txn.state_mut()
            .devices
            .entry(id.clone())
            .or_insert_with(|| DeviceState::new(identity))
            .profile_names
            .insert(profile, "Office".to_owned());
        txn.commit().unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.devices[&id].profile_names[&profile], "Office");
        let text = fs::read_to_string(paths.state_file()).unwrap();
        assert!(text.contains("profileNames"));
    }

    #[test]
    fn state_without_profile_names_loads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let store = StateStore::open(paths.clone());
        let identity = DeviceIdentity::test_ble("old-test", None).expect("valid test identity");
        let id = identity.id.clone();
        let id_string = id.to_string();
        let value = serde_json::json!({
            "schemaVersion": SCHEMA_VERSION,
            "nextDeviceNumber": 1000,
            "selectedDevice": id_string.clone(),
            "devices": {
                (id_string): {
                    "identity": identity,
                    "profileMetadata": { "desired": null, "observed": null },
                    "profiles": {},
                }
            },
        });
        fs::write(paths.state_file(), serde_json::to_vec(&value).unwrap()).unwrap();

        let loaded = store.load().unwrap();
        assert!(loaded.devices[&id].profile_names.is_empty());
    }

    #[test]
    fn discard_unreadable_handles_names_without_changing_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let store = StateStore::open(paths.clone());

        let identity = DeviceIdentity::test_ble("bad-device", None).expect("valid test identity");
        let id = identity.id.clone();
        let id_string = id.to_string();
        let mut value = serde_json::to_value(StateFile::default()).unwrap();
        value["schemaVersion"] = serde_json::json!(7);
        value["selectedDevice"] = serde_json::json!(id_string.clone());
        value["devices"] = serde_json::json!({
            (id_string): {
                "identity": identity,
                "profileMetadata": { "desired": null, "observed": null },
                "profiles": {},
                "profileNames": { "2": "Office" },
            },
        });
        let original = serde_json::to_vec(&value).unwrap();
        fs::write(paths.state_file(), &original).unwrap();

        let reset = store.discard_unreadable().unwrap();
        assert_eq!(reset.discarded_schema, Some(7));
        let backup = reset.backup.expect("old file must be preserved");
        assert_eq!(fs::read(&backup).unwrap(), original);
        assert_eq!(store.load().unwrap(), StateFile::default());
    }

    #[test]
    fn lock_path_is_derived_from_state_path() {
        let dir = tempfile::tempdir().unwrap();
        let state_file = dir.path().join("state.json");
        let paths = StatePaths::new(state_file.clone());
        assert_eq!(paths.lock_file(), state_file.with_file_name("state.lock"));
    }

    #[test]
    fn unchanged_commit_avoids_io() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::open(paths_in(dir.path()));
        {
            let mut txn = store.transaction().unwrap();
            let identity = DeviceIdentity::test_ble("unchanged-device", None).unwrap();
            let id = identity.id.clone();
            txn.state_mut()
                .devices
                .insert(id.clone(), DeviceState::new(identity));
            if txn.state().next_device_number <= 999 {
                txn.state_mut().next_device_number = 1000;
            }
            txn.state_mut().selected_device = Some(id);
            txn.commit().unwrap();
        }
        let before = fs::metadata(store.paths().state_file())
            .unwrap()
            .modified()
            .unwrap();
        std::thread::sleep(Duration::from_millis(15));
        {
            let txn = store.transaction().unwrap();
            txn.commit().unwrap();
        }
        let after = fs::metadata(store.paths().state_file())
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after, "unchanged commit must not rewrite file");

        let mem = StateStore::memory();
        {
            let txn = mem.transaction().unwrap();
            txn.commit().unwrap();
        }
        assert_eq!(mem.load().unwrap(), StateFile::default());
    }

    #[test]
    fn durable_backup_naming_and_exclusive_creation() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let store = StateStore::open(paths.clone());

        let original = b"not json".to_vec();
        fs::write(paths.state_file(), &original).unwrap();
        let first = store.discard_unreadable().unwrap();
        let first_backup = first.backup.unwrap();
        assert!(
            first_backup
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains("unreadable")
        );
        assert_eq!(fs::read(&first_backup).unwrap(), original);

        let original2 = b"still not json".to_vec();
        fs::write(paths.state_file(), &original2).unwrap();
        let second = store.discard_unreadable().unwrap();
        let second_backup = second.backup.unwrap();
        assert_ne!(first_backup, second_backup);
        assert!(
            second_backup
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains("unreadable")
        );
        assert_eq!(fs::read(&second_backup).unwrap(), original2);
        assert_eq!(fs::read(&first_backup).unwrap(), original);

        let mut value = serde_json::to_value(StateFile::default()).unwrap();
        value["schemaVersion"] = serde_json::json!(99);
        let bytes = serde_json::to_vec(&value).unwrap();
        fs::write(paths.state_file(), &bytes).unwrap();
        let third = store.discard_unreadable().unwrap();
        assert_eq!(third.discarded_schema, Some(99));
        let third_backup = third.backup.unwrap();
        assert!(
            third_backup
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains("unsupported-v99")
        );

        fs::write(paths.state_file(), &bytes).unwrap();
        let fourth = store.discard_unreadable().unwrap();
        assert_ne!(third_backup, fourth.backup.unwrap());
    }

    #[cfg(any(feature = "usb", feature = "ble"))]
    #[tokio::test]
    async fn async_mutation_runs_blocking_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::open(paths_in(dir.path()));
        let identity = DeviceIdentity::test_ble("async-device", None).unwrap();
        let id = identity.id.clone();

        store
            .mutate_async(move |state| {
                if state.next_device_number <= 999 {
                    state.next_device_number = 1000;
                }
                state.devices.insert(id.clone(), DeviceState::new(identity));
            })
            .await
            .unwrap();

        let store2 = store.clone();
        store2
            .mutate_async(|state| {
                state.selected_device =
                    Some(DeviceIdentity::test_ble("async-device", None).unwrap().id);
            })
            .await
            .unwrap();

        let loaded = store.load().unwrap();
        assert!(
            loaded
                .devices
                .contains_key(&DeviceIdentity::test_ble("async-device", None).unwrap().id)
        );

        let res: Result<(), StateError> = store
            .try_mutate_async(|state| {
                state.schema_version = SCHEMA_VERSION;
                Ok(())
            })
            .await;
        assert!(res.is_ok());
    }

    #[cfg(any(feature = "usb", feature = "ble"))]
    #[tokio::test]
    async fn async_load_from_disk_returns_default_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::open(paths_in(dir.path()));
        assert_eq!(store.load_async().await.unwrap(), StateFile::default());
        let identity = DeviceIdentity::test_ble("async-load-disk", None).unwrap();
        let id = identity.id.clone();
        store
            .mutate_async(move |state| {
                if state.next_device_number <= 999 {
                    state.next_device_number = 1000;
                }
                state.devices.insert(id.clone(), DeviceState::new(identity));
            })
            .await
            .unwrap();
        let loaded = store.load_async().await.unwrap();
        assert!(
            loaded.devices.contains_key(
                &DeviceIdentity::test_ble("async-load-disk", None)
                    .unwrap()
                    .id
            )
        );
        // In-memory clone shares state via load_async as well.
        let mem = StateStore::memory();
        assert_eq!(mem.load_async().await.unwrap(), StateFile::default());
        let mem_clone = mem.clone();
        let identity2 = DeviceIdentity::test_ble("async-load-mem", None).unwrap();
        let id2 = identity2.id.clone();
        mem.mutate_async(move |state| {
            if state.next_device_number <= 999 {
                state.next_device_number = 1000;
            }
            state
                .devices
                .insert(id2.clone(), DeviceState::new(identity2));
        })
        .await
        .unwrap();
        let mem_loaded = mem_clone.load_async().await.unwrap();
        assert!(
            mem_loaded
                .devices
                .contains_key(&DeviceIdentity::test_ble("async-load-mem", None).unwrap().id)
        );
    }

    #[cfg(any(feature = "usb", feature = "ble"))]
    #[tokio::test]
    async fn async_load_and_try_mutate_propagate_errors() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let store = StateStore::open(paths.clone());
        // Unsupported schema propagates through load_async as StateError.
        let mut value = serde_json::to_value(StateFile::default()).unwrap();
        value["schemaVersion"] = serde_json::json!(999);
        fs::write(paths.state_file(), serde_json::to_vec(&value).unwrap()).unwrap();
        match store.load_async().await {
            Err(StateError::UnsupportedSchema {
                found: 999,
                expected,
            }) => {
                assert_eq!(expected, SCHEMA_VERSION);
            }
            other => panic!("expected UnsupportedSchema via load_async, got {other:?}"),
        }
        // Restore a valid empty file for subsequent checks.
        fs::remove_file(paths.state_file()).unwrap();
        assert_eq!(store.load_async().await.unwrap(), StateFile::default());
        // Closure error from try_mutate_async propagates without committing.
        let err = store
            .try_mutate_async(|_| Err::<(), _>(StateError::invalid_state("closure error")))
            .await
            .unwrap_err();
        match err {
            StateError::InvalidState(msg) => assert!(msg.contains("closure error")),
            other => panic!("expected InvalidState from closure, got {other:?}"),
        }
        assert_eq!(store.load_async().await.unwrap(), StateFile::default());
        // Commit validation error propagates (e.g., invalid next_device_number).
        let err = store
            .mutate_async(|state| {
                state.next_device_number = 0;
            })
            .await
            .unwrap_err();
        assert!(matches!(err, StateError::InvalidState(_)));
        assert_eq!(store.load_async().await.unwrap(), StateFile::default());
    }

    #[cfg(any(feature = "usb", feature = "ble"))]
    #[tokio::test]
    async fn async_join_error_is_mapped_to_invalid_state() {
        let store = StateStore::memory();
        let err = store
            .mutate_async(|_| panic!("intentional panic for join-error test"))
            .await
            .unwrap_err();
        match err {
            StateError::InvalidState(msg) => assert!(msg.contains("spawn_blocking join error")),
            other => panic!("expected InvalidState join mapping, got {other:?}"),
        }
        let err = store.load_async().await.unwrap();
        // memory load still works after a panicked mutate (no poison).
        assert_eq!(err, StateFile::default());
        // Same mapping for load_async via direct spawn_blocking panic simulation.
        // We exercise the mutate path above; load_async uses identical mapping.
    }

    #[test]
    fn operation_lock_contention_timeout() {
        let store = StateStore::memory();
        let device = DeviceIdentity::test_ble("lock-device", None).unwrap().id;
        let guard = store
            .acquire_operation_lock(&device, Duration::from_millis(100), "test-op")
            .expect("first lock should succeed");
        let start = std::time::Instant::now();
        let err = store
            .acquire_operation_lock(&device, Duration::from_millis(50), "test-op")
            .unwrap_err();
        assert!(start.elapsed() >= Duration::from_millis(40));
        match err {
            crate::error::ManagerError::DeviceOperationBusy {
                device: d,
                operation,
                timeout,
                path,
            } => {
                assert_eq!(d, device);
                assert_eq!(operation, "test-op");
                assert_eq!(timeout, Duration::from_millis(50));
                assert!(path.to_string_lossy().contains("device-"));
            }
            other => panic!("expected DeviceOperationBusy, got {other:?}"),
        }
        drop(guard);
        assert!(
            store
                .acquire_operation_lock(&device, Duration::from_millis(100), "test-op")
                .is_ok()
        );
    }

    #[test]
    fn operation_lock_independent_devices_do_not_contend() {
        let store = StateStore::memory();
        let a = crate::device::DeviceId::from_number(1).unwrap();
        let b = crate::device::DeviceId::from_number(2).unwrap();
        let _guard_a = store
            .acquire_operation_lock(&a, Duration::from_millis(100), "test-op")
            .unwrap();
        let guard_b = store
            .acquire_operation_lock(&b, Duration::from_millis(10), "test-op")
            .expect("independent device should not contend");
        drop(guard_b);
        drop(_guard_a);
    }

    #[test]
    fn operation_lock_does_not_hold_state_lock() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::open(paths_in(dir.path()));
        let device = crate::device::DeviceId::from_number(10).unwrap();
        let _op_guard = store
            .acquire_operation_lock(&device, Duration::from_millis(100), "test-op")
            .unwrap();
        let txn = store.transaction().expect("state lock must be independent");
        txn.commit().unwrap();
    }

    #[test]
    fn device_lock_path_is_filesystem_safe_and_beside_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::open(paths_in(dir.path()));
        let device = crate::device::DeviceId::from_number(42).unwrap();
        let path = store.device_lock_path(&device);
        assert_eq!(path.parent().unwrap(), dir.path());
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("device-"), "got {name}");
        assert!(!name.contains('/') && !name.contains('\\') && !name.contains(':'));
        let guard = store
            .acquire_operation_lock(&device, Duration::from_millis(100), "test-op")
            .unwrap();
        assert!(guard.path().exists());
        drop(guard);
    }
}
