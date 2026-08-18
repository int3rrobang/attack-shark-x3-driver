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
        let target = &self.paths.state_file;
        let _lock = LockGuard::acquire(&self.paths.lock_file)?;
        let bytes = match fs::read(target) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                // Nothing to preserve; materialize a fresh default state.
                write_atomic(target, &StateFile::default())?;
                return Ok(StateReset {
                    backup: None,
                    discarded_schema: None,
                });
            }
            Err(err) => return Err(StateError::io(target.to_path_buf(), err)),
        };
        if load_state(target).is_ok() {
            return Err(StateError::invalid_state(format!(
                "state file {} is readable with the current schema; refusing to discard it",
                target.display()
            )));
        }
        let discarded_schema = serde_json::from_slice::<SchemaHeader>(&bytes)
            .ok()
            .map(|header| header.schema_version);
        let backup = unique_backup_path(target, discarded_schema);
        fs::write(&backup, &bytes).map_err(|err| StateError::io(backup.clone(), err))?;
        write_atomic(target, &StateFile::default())?;
        Ok(StateReset {
            backup: Some(backup),
            discarded_schema,
        })
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

fn unique_backup_path(target: &Path, schema: Option<u32>) -> PathBuf {
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
    let mut candidate = parent.join(format!("{stem}.bak"));
    let mut counter = 2u32;
    while candidate.exists() {
        candidate = parent.join(format!("{stem}-{counter}.bak"));
        counter += 1;
    }
    candidate
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
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    Ok(())
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
        fs::write(&paths.state_file, serde_json::to_vec(&value).unwrap()).unwrap();

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

    #[test]
    fn discard_unreadable_replaces_foreign_schema_with_backup() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let store = StateStore::open(paths.clone());

        let mut value = serde_json::to_value(StateFile::default()).unwrap();
        value["schemaVersion"] = serde_json::json!(7);
        let original = serde_json::to_vec(&value).unwrap();
        fs::write(&paths.state_file, &original).unwrap();

        let reset = store.discard_unreadable().unwrap();
        assert_eq!(reset.discarded_schema, Some(7));
        let backup = reset.backup.expect("old file must be preserved");
        assert_eq!(fs::read(&backup).unwrap(), original);
        assert_eq!(store.load().unwrap(), StateFile::default());

        // The replacement now loads, so a second discard must refuse.
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
        fs::write(&paths.state_file, &original).unwrap();

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
        let committed = store.transaction().unwrap().state().clone();
        store.transaction().unwrap().commit().unwrap();

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
        assert!(store.paths().state_file.exists());
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
        let identity = DeviceIdentity::ble("named-test", None).expect("valid test identity");
        let id = identity.id.clone();
        let profile = ProfileId::new(2).expect("profile");

        let mut txn = store.transaction().unwrap();
        txn.state_mut()
            .devices
            .entry(id.clone())
            .or_insert_with(|| DeviceState::new(identity))
            .profile_names
            .insert(profile, "Office".to_owned());
        txn.commit().unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.devices[&id].profile_names[&profile], "Office");
        // The additive field is persisted in the document, not memory-only.
        let text = fs::read_to_string(&paths.state_file).unwrap();
        assert!(text.contains("profileNames"));
    }

    #[test]
    fn state_without_profile_names_loads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let store = StateStore::open(paths.clone());
        let identity = DeviceIdentity::ble("old-test", None).expect("valid test identity");
        let id = identity.id.clone();
        let id_string = id.to_string();
        let value = serde_json::json!({
            "schemaVersion": SCHEMA_VERSION,
            "selectedDevice": id_string.clone(),
            "devices": {
                (id_string): {
                    "identity": identity,
                    "profileMetadata": { "desired": null, "observed": null },
                    "profiles": {},
                }
            },
        });
        fs::write(&paths.state_file, serde_json::to_vec(&value).unwrap()).unwrap();

        let loaded = store.load().unwrap();
        assert!(loaded.devices[&id].profile_names.is_empty());
    }

    #[test]
    fn discard_unreadable_handles_names_without_changing_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let store = StateStore::open(paths.clone());

        // A document from a newer schema is unreadable, even though its
        // profileNames bytes remain well-formed and must be preserved.
        let identity = DeviceIdentity::ble("bad-device", None).expect("valid test identity");
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
        fs::write(&paths.state_file, &original).unwrap();

        let reset = store.discard_unreadable().unwrap();
        assert_eq!(reset.discarded_schema, Some(7));
        let backup = reset.backup.expect("old file must be preserved");
        assert_eq!(fs::read(&backup).unwrap(), original);
        assert_eq!(store.load().unwrap(), StateFile::default());
    }
}
