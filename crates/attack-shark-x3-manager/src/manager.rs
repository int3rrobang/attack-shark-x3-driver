use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use attack_shark_x3::driver::ProfileSnapshot;
use attack_shark_x3::{ProfileId, ProfileMetadata, TransportKind};

use crate::backend::{DeviceSession, RealSessionFactory, SessionFactory, SessionWrite};
use crate::device::{DeviceId, DeviceIdentity, TransportSelection};
use crate::error::ManagerError;
use crate::operation::{DeviceStatus, DiscoveredDevice, ResourceSnapshot};
use crate::state::{
    DeviceState, ObservationSource, ObservedState, ProfileState, ResourceState, StateStore,
    Timestamp,
};

pub struct DeviceManager {
    store: StateStore,
    factory: Arc<dyn SessionFactory>,
}

impl DeviceManager {
    pub fn new(store: StateStore) -> Result<Self, ManagerError> {
        Ok(Self {
            store,
            factory: Arc::new(RealSessionFactory),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_store_and_factory(
        store: StateStore,
        factory: Arc<dyn SessionFactory>,
    ) -> Self {
        Self { store, factory }
    }

    pub fn store(&self) -> &StateStore {
        &self.store
    }

    pub async fn list_devices(
        &self,
        selection: TransportSelection,
    ) -> Result<Vec<DiscoveredDevice>, ManagerError> {
        self.factory.list(selection).await
    }

    /// Returns the exact device currently selected in durable manager state.
    pub fn selected_device(&self) -> Result<Option<DeviceId>, ManagerError> {
        Ok(self.store.load()?.selected_device)
    }

    /// Selects an already-registered exact device and persists that choice.
    pub fn select_device(&self, device: &DeviceId) -> Result<(), ManagerError> {
        let mut transaction = self.store.transaction()?;
        if !transaction.state().devices.contains_key(device) {
            return Err(ManagerError::DeviceNotFound(device.clone()));
        }
        transaction.state_mut().selected_device = Some(device.clone());
        transaction.commit()?;
        Ok(())
    }

    /// Discovers exact devices, records their identities, and resolves one
    /// connected device without guessing.
    ///
    /// An explicit ID always takes precedence. Without one, a connected
    /// persisted selection wins; otherwise exactly one connected candidate is
    /// selected and persisted. Zero and multiple connected candidates are
    /// reported as distinct manager errors.
    pub async fn resolve_device(
        &self,
        explicit: Option<&DeviceId>,
        selection: TransportSelection,
    ) -> Result<DeviceId, ManagerError> {
        let discovered = self.factory.list(selection).await?;
        self.register_discovered(&discovered)?;

        if let Some(explicit) = explicit {
            return match discovered
                .iter()
                .find(|device| device.identity.id == *explicit)
            {
                None => Err(ManagerError::DeviceNotFound(explicit.clone())),
                Some(device) if device.connected => Ok(explicit.clone()),
                Some(_) => Err(ManagerError::DeviceDisconnected(explicit.clone())),
            };
        }

        let state = self.store.load()?;
        if let Some(selected) = state.selected_device.as_ref() {
            if discovered
                .iter()
                .any(|device| device.connected && device.identity.id == *selected)
            {
                return Ok(selected.clone());
            }
        }

        let mut candidates: Vec<DeviceId> = discovered
            .iter()
            .filter(|device| device.connected)
            .map(|device| device.identity.id.clone())
            .collect();
        candidates.sort();
        candidates.dedup();

        match candidates.as_slice() {
            [] => Err(ManagerError::NoDevice { selection }),
            [candidate] => {
                self.select_device(candidate)?;
                Ok(candidate.clone())
            }
            _ => Err(ManagerError::AmbiguousDevice {
                selection,
                candidates,
            }),
        }
    }

    pub fn register_device(&self, identity: DeviceIdentity) -> Result<(), ManagerError> {
        let mut txn = self.store.transaction()?;
        let state = txn.state_mut();
        if state.selected_device.is_none() {
            state.selected_device = Some(identity.id.clone());
        }
        state
            .devices
            .entry(identity.id.clone())
            .or_insert_with(|| DeviceState::new(identity));
        txn.commit()?;
        Ok(())
    }

    fn register_discovered(&self, discovered: &[DiscoveredDevice]) -> Result<(), ManagerError> {
        if discovered.is_empty() {
            return Ok(());
        }

        let mut transaction = self.store.transaction()?;
        for device in discovered {
            transaction
                .state_mut()
                .devices
                .entry(device.identity.id.clone())
                .or_insert_with(|| DeviceState::new(device.identity.clone()));
        }
        transaction.commit()?;
        Ok(())
    }

    pub async fn read_battery(&self, device: &DeviceId) -> Result<u8, ManagerError> {
        let (_identity, session) = self.open_session(device).await?;
        session.read_battery(Duration::from_secs(5)).await
    }

    pub async fn read_status(&self, device: &DeviceId) -> Result<DeviceStatus, ManagerError> {
        let (identity, session) = self.open_session(device).await?;
        let usb = is_usb_transport(session.transport());

        let battery = match session.read_battery(Duration::from_secs(5)).await {
            Ok(level) => Some(level),
            Err(ManagerError::UnsupportedOperation { .. }) => None,
            Err(error) => return Err(error),
        };

        let profile_metadata = match session.read_profile_metadata().await {
            Ok(metadata) if usb => {
                let resource = self.update_observed_profile_metadata(device, metadata)?;
                Some(ResourceSnapshot { resource })
            }
            Ok(_) => None,
            Err(ManagerError::UnsupportedOperation { .. }) => None,
            Err(error) => return Err(error),
        };

        let polling_rate = match session.read_polling_rate().await {
            Ok(rate) if usb => {
                let resource = self.update_observed_polling_rate(device, rate)?;
                Some(ResourceSnapshot { resource })
            }
            Ok(_) => None,
            Err(ManagerError::UnsupportedOperation { .. }) => None,
            Err(error) => return Err(error),
        };

        Ok(DeviceStatus {
            identity,
            battery,
            profile_metadata,
            polling_rate,
        })
    }

    pub async fn read_profile(
        &self,
        device: &DeviceId,
        profile: ProfileId,
    ) -> Result<ProfileSnapshot, ManagerError> {
        let (_identity, session) = self.open_session(device).await?;
        let snapshot = session.read_profile(profile).await?;

        if is_usb_transport(session.transport()) {
            let now = self.now();
            let mut txn = self.store.transaction()?;
            let device_state = txn
                .state_mut()
                .devices
                .get_mut(device)
                .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
            let profile_state = device_state
                .profiles
                .entry(profile)
                .or_insert_with(ProfileState::empty);
            set_observed(&mut profile_state.dpi, snapshot.dpi.clone(), now);
            set_observed(&mut profile_state.preferences, snapshot.preferences, now);
            set_observed(&mut profile_state.buttons, snapshot.buttons, now);
            txn.commit()?;
        }

        Ok(snapshot)
    }

    pub async fn activate_profile(
        &self,
        device: &DeviceId,
        profile: ProfileId,
    ) -> Result<ProfileMetadata, ManagerError> {
        let (_identity, session) = self.open_session(device).await?;
        let usb = is_usb_transport(session.transport());

        let maximum = if usb {
            session
                .read_profile_metadata()
                .await?
                .maximum()
                .max(profile)
        } else {
            ProfileId::new(ProfileId::MAX).expect("ProfileId::MAX must be valid")
        };
        let target = ProfileMetadata::new(profile, maximum)
            .map_err(|error| ManagerError::Driver(error.into()))?;

        match session.write_profile_metadata(target).await? {
            SessionWrite::ReadbackVerified(actual) => {
                if usb {
                    self.update_observed_profile_metadata(device, actual)?;
                }
                Ok(actual)
            }
            SessionWrite::Acknowledged => Ok(target),
        }
    }

    /// Returns the stored identity for an exact device ID.
    pub fn device_identity(&self, device: &DeviceId) -> Result<DeviceIdentity, ManagerError> {
        let state = self.store.load()?;
        state
            .devices
            .get(device)
            .map(|device_state| device_state.identity.clone())
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))
    }

    pub(crate) fn identity(&self, device: &DeviceId) -> Result<DeviceIdentity, ManagerError> {
        self.device_identity(device)
    }

    pub(crate) async fn open_session(
        &self,
        device: &DeviceId,
    ) -> Result<(DeviceIdentity, Box<dyn DeviceSession>), ManagerError> {
        let identity = self.identity(device)?;
        let session = self.factory.open(&identity).await?;
        Ok((identity, session))
    }

    pub(crate) fn now(&self) -> Timestamp {
        Timestamp {
            unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
        }
    }

    fn update_observed_profile_metadata(
        &self,
        device: &DeviceId,
        value: ProfileMetadata,
    ) -> Result<ResourceState<ProfileMetadata>, ManagerError> {
        let now = self.now();
        let mut txn = self.store.transaction()?;
        let device_state = txn
            .state_mut()
            .devices
            .get_mut(device)
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
        set_observed(&mut device_state.profile_metadata, value, now);
        let resource = device_state.profile_metadata.clone();
        txn.commit()?;
        Ok(resource)
    }

    fn update_observed_polling_rate(
        &self,
        device: &DeviceId,
        value: attack_shark_x3::PollingRate,
    ) -> Result<ResourceState<attack_shark_x3::PollingRate>, ManagerError> {
        let now = self.now();
        let mut txn = self.store.transaction()?;
        let device_state = txn
            .state_mut()
            .devices
            .get_mut(device)
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
        set_observed(&mut device_state.polling_rate, value, now);
        let resource = device_state.polling_rate.clone();
        txn.commit()?;
        Ok(resource)
    }
}

fn set_observed<T>(resource: &mut ResourceState<T>, value: T, now: Timestamp) {
    resource.observed = Some(ObservedState {
        value,
        source: ObservationSource::UsbReadback,
        observed_at: now,
    });
}

fn is_usb_transport(transport: TransportKind) -> bool {
    matches!(transport, TransportKind::Wired | TransportKind::Receiver)
}
#[cfg(test)]
mod tests {
    use super::DeviceManager;
    use crate::backend::{ScriptedFakeFactory, ScriptedFakeSession};
    use crate::device::{DeviceIdentity, TransportSelection};
    use crate::error::ManagerError;
    use crate::state::{DesiredSource, DesiredState, StatePaths, StateStore, Verification};
    use attack_shark_x3::{PollingRate, ProfileMetadata, TransportKind};
    use std::sync::Arc;

    fn test_store(dir: &tempfile::TempDir) -> StateStore {
        StateStore::open(StatePaths {
            state_file: dir.path().join("state.json"),
            lock_file: dir.path().join("state.lock"),
        })
    }

    fn usb_identity() -> DeviceIdentity {
        DeviceIdentity::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa61,
            Some("TEST001"),
            r"\\?\hid#test",
            Some("Test Mouse"),
        )
        .expect("valid test identity")
    }

    fn usb_identity_named(serial: &str) -> DeviceIdentity {
        DeviceIdentity::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa61,
            Some(serial),
            &format!(r"\\?\hid#{serial}"),
            Some(serial),
        )
        .expect("valid named test identity")
    }

    #[tokio::test]
    async fn resolve_device_honors_explicit_id_and_registers_discovery() {
        let first = usb_identity_named("EXPLICIT-A");
        let second = usb_identity_named("EXPLICIT-B");
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_identity(first.clone(), true, ScriptedFakeSession::usb())
                .with_identity(second.clone(), true, ScriptedFakeSession::usb()),
        );
        let manager = DeviceManager::with_store_and_factory(StateStore::memory(), factory);

        let selected = manager
            .resolve_device(Some(&second.id), TransportSelection::Auto)
            .await
            .unwrap();

        assert_eq!(selected, second.id);
        assert_eq!(manager.selected_device().unwrap(), None);
        let state = manager.store().load().unwrap();
        assert!(state.devices.contains_key(&first.id));
        assert_eq!(manager.device_identity(&second.id).unwrap(), second);
        assert!(state.devices.contains_key(&second.id));
    }

    #[tokio::test]
    async fn resolve_device_honors_connected_stored_selection() {
        let first = usb_identity_named("STORED-A");
        let second = usb_identity_named("STORED-B");
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_identity(first.clone(), true, ScriptedFakeSession::usb())
                .with_identity(second.clone(), true, ScriptedFakeSession::usb()),
        );
        let manager = DeviceManager::with_store_and_factory(StateStore::memory(), factory);
        manager.register_device(first).unwrap();
        manager.register_device(second.clone()).unwrap();
        manager.select_device(&second.id).unwrap();

        assert_eq!(
            manager
                .resolve_device(None, TransportSelection::Auto)
                .await
                .unwrap(),
            second.id
        );
    }

    #[tokio::test]
    async fn resolve_device_selects_the_sole_connected_candidate() {
        let connected = usb_identity_named("SOLE-CONNECTED");
        let disconnected = usb_identity_named("SOLE-DISCONNECTED");
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_identity(connected.clone(), true, ScriptedFakeSession::usb())
                .with_identity(disconnected, false, ScriptedFakeSession::usb()),
        );
        let manager = DeviceManager::with_store_and_factory(StateStore::memory(), factory);

        assert_eq!(
            manager
                .resolve_device(None, TransportSelection::Auto)
                .await
                .unwrap(),
            connected.id
        );
        assert_eq!(manager.selected_device().unwrap(), Some(connected.id));
    }

    #[tokio::test]
    async fn resolve_device_reports_ambiguity_without_guessing() {
        let first = usb_identity_named("AMBIGUOUS-A");
        let second = usb_identity_named("AMBIGUOUS-B");
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_identity(first.clone(), true, ScriptedFakeSession::usb())
                .with_identity(second.clone(), true, ScriptedFakeSession::usb()),
        );
        let manager = DeviceManager::with_store_and_factory(StateStore::memory(), factory);

        let error = manager
            .resolve_device(None, TransportSelection::Auto)
            .await
            .expect_err("multiple connected candidates must be explicit");
        match error {
            ManagerError::AmbiguousDevice {
                selection,
                candidates,
            } => {
                assert_eq!(selection, TransportSelection::Auto);
                assert_eq!(candidates, vec![first.id.clone(), second.id.clone()]);
            }
            other => panic!("expected AmbiguousDevice, got {other:?}"),
        }
        assert_eq!(manager.selected_device().unwrap(), None);
    }

    #[tokio::test]
    async fn resolve_device_reports_no_connected_device() {
        let disconnected = usb_identity_named("NONE-DISCONNECTED");
        let factory = Arc::new(ScriptedFakeFactory::new().with_identity(
            disconnected.clone(),
            false,
            ScriptedFakeSession::usb(),
        ));
        let manager = DeviceManager::with_store_and_factory(StateStore::memory(), factory);

        let error = manager
            .resolve_device(None, TransportSelection::Exact(TransportKind::Wired))
            .await
            .expect_err("no connected candidates must be reported");
        match error {
            ManagerError::NoDevice { selection } => {
                assert_eq!(selection, TransportSelection::Exact(TransportKind::Wired));
            }
            other => panic!("expected NoDevice, got {other:?}"),
        }
        assert!(
            manager
                .store()
                .load()
                .unwrap()
                .devices
                .contains_key(&disconnected.id)
        );
        assert_eq!(manager.selected_device().unwrap(), None);
    }
    #[tokio::test]
    async fn register_device_inserts_and_selects_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let factory = Arc::new(ScriptedFakeFactory::new());
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);

        let identity = usb_identity();
        let id = identity.id.clone();
        manager.register_device(identity.clone()).unwrap();

        let state = store.load().unwrap();
        assert_eq!(state.selected_device, Some(id.clone()));
        assert!(state.devices.contains_key(&id));
        assert_eq!(state.devices[&id].identity, identity);
    }

    #[tokio::test]
    async fn register_device_preserves_existing_state_and_selection() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let factory = Arc::new(ScriptedFakeFactory::new());
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);

        let first = usb_identity();
        let first_id = first.id.clone();
        manager.register_device(first).unwrap();

        // Mutate the device state to verify preservation
        {
            let mut txn = store.transaction().unwrap();
            txn.state_mut()
                .devices
                .get_mut(&first_id)
                .unwrap()
                .polling_rate
                .desired = Some(DesiredState {
                value: PollingRate::Hz500,
                source: DesiredSource::UserWrite,
                verification: Verification::not_sent(),
                updated_at: crate::state::Timestamp { unix_seconds: 42 },
            });
            txn.commit().unwrap();
        }

        // Re-register same device: must not overwrite
        let identity_again = usb_identity();
        manager.register_device(identity_again).unwrap();

        let state = store.load().unwrap();
        assert_eq!(state.selected_device, Some(first_id.clone()));
        assert_eq!(
            state.devices[&first_id]
                .polling_rate
                .desired
                .as_ref()
                .unwrap()
                .value,
            PollingRate::Hz500
        );

        // Register a second device: selection must not change
        let second = DeviceIdentity::ble("ble-device-2", Some("Second")).unwrap();
        manager.register_device(second).unwrap();
        let state = store.load().unwrap();
        assert_eq!(state.selected_device, Some(first_id));
    }

    #[tokio::test]
    async fn read_status_usb_preserves_desired_and_sets_observed() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let identity = usb_identity();
        let id = identity.id.clone();

        let metadata = ProfileMetadata::new(
            attack_shark_x3::ProfileId::new(1).unwrap(),
            attack_shark_x3::ProfileId::new(3).unwrap(),
        )
        .unwrap();
        let session = ScriptedFakeSession::usb()
            .with_metadata(metadata)
            .with_polling_rate(PollingRate::Hz1000)
            .with_battery(87);
        let factory =
            Arc::new(ScriptedFakeFactory::new().with_identity(identity.clone(), true, session));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        // Seed a desired polling rate that must be preserved
        {
            let mut txn = store.transaction().unwrap();
            let device_state = txn.state_mut().devices.get_mut(&id).unwrap();
            device_state.polling_rate.desired = Some(DesiredState {
                value: PollingRate::Hz250,
                source: DesiredSource::UserWrite,
                verification: Verification::not_sent(),
                updated_at: crate::state::Timestamp { unix_seconds: 10 },
            });
            txn.commit().unwrap();
        }

        let status = manager.read_status(&id).await.unwrap();
        assert_eq!(status.battery, Some(87));

        // Polling rate: desired preserved, observed set from readback
        let polling = status.polling_rate.as_ref().unwrap();
        assert_eq!(
            polling.resource.desired.as_ref().unwrap().value,
            PollingRate::Hz250
        );
        assert_eq!(
            polling.resource.observed.as_ref().unwrap().value,
            PollingRate::Hz1000
        );
        assert_eq!(
            polling.resource.observed.as_ref().unwrap().source,
            crate::state::ObservationSource::UsbReadback
        );

        // Profile metadata: observed set
        let meta = status.profile_metadata.as_ref().unwrap();
        assert_eq!(meta.resource.observed.as_ref().unwrap().value, metadata);
        assert!(meta.resource.desired.is_none());

        // Verify persisted state matches
        let persisted = store.load().unwrap();
        let device_state = &persisted.devices[&id];
        assert_eq!(
            device_state.polling_rate.desired.as_ref().unwrap().value,
            PollingRate::Hz250
        );
        assert_eq!(
            device_state.polling_rate.observed.as_ref().unwrap().value,
            PollingRate::Hz1000
        );
    }

    #[tokio::test]
    async fn read_battery_returns_exact_backend_value() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let identity = usb_identity();
        let id = identity.id.clone();
        let session = ScriptedFakeSession::usb().with_battery(42);
        let factory =
            Arc::new(ScriptedFakeFactory::new().with_identity(identity.clone(), true, session));
        let manager = DeviceManager::with_store_and_factory(store, factory);
        manager.register_device(identity).unwrap();

        assert_eq!(manager.read_battery(&id).await.unwrap(), 42);
    }

    #[tokio::test]
    async fn read_status_maps_unsupported_battery_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let identity = DeviceIdentity::ble("ble-device", Some("BLE Test")).unwrap();
        let id = identity.id.clone();
        let session = ScriptedFakeSession::ble().with_battery(99);
        let factory =
            Arc::new(ScriptedFakeFactory::new().with_identity(identity.clone(), true, session));
        let manager = DeviceManager::with_store_and_factory(store, factory);
        manager.register_device(identity).unwrap();

        let status = manager.read_status(&id).await.unwrap();

        assert_eq!(status.battery, None);
        assert_eq!(status.profile_metadata, None);
        assert_eq!(status.polling_rate, None);
    }
}
