#[cfg(feature = "usb")]
use std::collections::HashSet;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use attack_shark_x3::driver::ProfileSnapshot;
use attack_shark_x3::{
    DpiState, PhysicalId, ProfileId, ProfileMetadata, TransportKind, WatermarkDecode,
    decode_watermark,
};
#[cfg(feature = "usb")]
use futures_lite::StreamExt;

use crate::backend::{DeviceSession, RealSessionFactory, SessionFactory, SessionWrite};
use crate::device::{DeviceEndpoint, DeviceId, DeviceIdentity, DeviceLocator, TransportSelection};
use crate::error::ManagerError;
#[cfg(feature = "usb")]
use crate::operation::DeviceTopologyEvent;
use crate::operation::{
    DeviceStatus, DiscoveredDevice, DiscoveredEndpoint, DiscoveryView, IdentityCeremonyAction,
    IdentityCeremonyKind, IdentityCeremonyProgress, IdentityCeremonyStage, IdentityResolution,
    LiveProfileSnapshot, ResolvedConnection, ResourceSnapshot, UnassociatedReason,
    VerificationMethod,
};
use crate::refresh;
use crate::state::{
    CapturedProfileImage, DeviceState, IdentityMode, IdentitySetupJournal, IdentitySetupPhase,
    IdentitySetupStage, IdentitySetupSubject, IdentityStampProgress, ProfileState, ResourceState,
    StateFile, StateStore, Timestamp,
};

const BATTERY_READ_TIMEOUT: Duration = Duration::from_secs(20);
const OPERATION_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

pub struct DeviceManager {
    store: StateStore,
    pub(crate) factory: Arc<dyn SessionFactory>,
    /// Per-attachment watermark authentication cache.
    ///
    /// In persistent identity mode a connected USB endpoint is authenticated
    /// once per continuous attachment: the decoded current-profile watermark
    /// is cached and reused by later scans. Entries are keyed by transport +
    /// locator but only trusted when the cached endpoint object (VID/PID/
    /// serial) still matches the connection, and endpoint disappearance
    /// evicts the entry — so an OS path reuse never reuses a stale
    /// authentication, and a different unit presented at the same path is
    /// never mistaken for the cached one. Short-lived CLI manager instances
    /// therefore authenticate independently.
    identity_cache:
        Mutex<BTreeMap<(TransportKind, DeviceLocator), (WatermarkDecode, DeviceEndpoint)>>,
}

impl DeviceManager {
    pub fn new(store: StateStore) -> Result<Self, ManagerError> {
        Ok(Self {
            store,
            factory: Arc::new(RealSessionFactory),
            identity_cache: Mutex::new(BTreeMap::new()),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_store_and_factory(
        store: StateStore,
        factory: Arc<dyn SessionFactory>,
    ) -> Self {
        Self {
            store,
            factory,
            identity_cache: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn store(&self) -> &StateStore {
        &self.store
    }

    /// Returns the decoded current-profile watermark of a connected USB
    /// endpoint. Transports that cannot produce one report `Absent`.
    async fn read_endpoint_identity(
        &self,
        endpoint: &DeviceEndpoint,
    ) -> Result<WatermarkDecode, ManagerError> {
        let session = self.factory.open(endpoint).await?;
        match self.current_watermark(session.as_ref()).await {
            Ok((_profile, decode)) => Ok(decode),
            Err(ManagerError::UnsupportedOperation { .. }) => Ok(WatermarkDecode::Absent),
            Err(error) => Err(error),
        }
    }

    /// Reads the current profile's watermark through an open session.
    async fn current_watermark(
        &self,
        session: &dyn DeviceSession,
    ) -> Result<(ProfileId, WatermarkDecode), ManagerError> {
        let metadata = session.read_profile_metadata().await?;
        let current = metadata.current();
        let dpi = session.read_dpi(current).await?;
        Ok((current, decode_watermark(&dpi.preserved_tail)))
    }

    /// Lists compatible transport endpoints without opening a device,
    /// authenticating identity, or mutating durable state.
    ///
    /// Frontends use this lightweight scan to notice topology changes and
    /// then run full discovery only when an endpoint appears or disappears.
    pub async fn scan_endpoints(
        &self,
        selection: TransportSelection,
    ) -> Result<Vec<DiscoveredEndpoint>, ManagerError> {
        self.factory.list(selection).await
    }

    /// Subscribes to operating-system USB hotplug notifications for this
    /// mouse family. The watcher reports physical USB arrival/removal without
    /// opening HID interfaces or reading device configuration.
    #[cfg(feature = "usb")]
    pub async fn subscribe_topology_events(
        &self,
    ) -> Result<tokio::sync::mpsc::Receiver<DeviceTopologyEvent>, ManagerError> {
        let mut watcher = nusb::watch_devices().map_err(|error| {
            ManagerError::InvalidUpdate(format!("USB topology watcher could not start: {error}"))
        })?;
        let devices = nusb::list_devices().await.map_err(|error| {
            ManagerError::InvalidUpdate(format!(
                "USB topology watcher could not list devices: {error}"
            ))
        })?;
        let mut supported = devices
            .filter(is_supported_usb_device)
            .map(|device| device.id())
            .collect::<HashSet<_>>();
        let (sender, receiver) = tokio::sync::mpsc::channel(16);
        tokio::spawn(async move {
            while let Some(event) = watcher.next().await {
                let topology = match event {
                    nusb::hotplug::HotplugEvent::Connected(device)
                        if is_supported_usb_device(&device) =>
                    {
                        supported.insert(device.id());
                        Some(DeviceTopologyEvent::Connected)
                    }
                    nusb::hotplug::HotplugEvent::Disconnected(id) if supported.remove(&id) => {
                        Some(DeviceTopologyEvent::Disconnected)
                    }
                    _ => None,
                };
                if let Some(topology) = topology
                    && sender.send(topology).await.is_err()
                {
                    break;
                }
            }
        });
        Ok(receiver)
    }

    /// Runs a mode-aware discovery scan.
    ///
    /// Lists every compatible connection, authenticates USB endpoints by
    /// their current DPI watermark once per continuous attachment (persistent
    /// mode only), resolves each connection against the durable logical
    /// mice, persists endpoint associations, and returns the aggregate view.
    /// Discovery never manufactures logical identities: unassociated
    /// connections are never persisted and never receive a temporary
    /// `mouse-N`.
    pub async fn discover(
        &self,
        selection: TransportSelection,
    ) -> Result<DiscoveryView, ManagerError> {
        let discovered = self.factory.list(selection).await?;
        let mode = self.store.load_async().await?.identity_mode;

        // Legacy mode: exactly one fuzzy logical mouse owns every compatible
        // connection. Create it on first discovery; port changes update its
        // available connections and never allocate another logical mouse.
        if mode.is_legacy() && !discovered.is_empty() {
            self.ensure_legacy_mouse(&discovered).await?;
        }

        // Authenticate USB endpoints once per continuous attachment.
        // Persistent mode only: legacy single-mouse mode makes no identity
        // claim, reads no watermark on startup, and never pays the armed-read
        // cursor freeze. The cache is trusted only while the cached endpoint
        // object (VID/PID/serial) still matches the connection, so a detach/
        // replug at the same path never reuses a stale authentication.
        if !mode.is_legacy() {
            let mut to_read: Vec<DeviceEndpoint> = Vec::new();
            {
                let cache = self.identity_cache.lock().expect("identity cache poisoned");
                for disc in &discovered {
                    if disc.connected && is_usb_transport(disc.endpoint.transport) {
                        let key = (disc.endpoint.transport, disc.endpoint.locator.clone());
                        let needs_read = match cache.get(&key) {
                            Some((_decode, cached_endpoint)) => cached_endpoint != &disc.endpoint,
                            None => true,
                        };
                        if needs_read {
                            to_read.push(disc.endpoint.clone());
                        }
                    }
                }
            }
            for endpoint in to_read {
                let decode = self.read_endpoint_identity(&endpoint).await?;
                let key = (endpoint.transport, endpoint.locator.clone());
                self.identity_cache
                    .lock()
                    .expect("identity cache poisoned")
                    .insert(key, (decode, endpoint));
            }
            // Endpoint disappearance invalidates the cache: evict entries
            // whose endpoint was not listed as connected this scan, so an OS
            // path reuse never reuses a stale authentication.
            let seen: BTreeSet<(TransportKind, DeviceLocator)> = discovered
                .iter()
                .filter(|d| d.connected && is_usb_transport(d.endpoint.transport))
                .map(|d| (d.endpoint.transport, d.endpoint.locator.clone()))
                .collect();
            let mut cache = self.identity_cache.lock().expect("identity cache poisoned");
            cache.retain(|key, (_, cached_endpoint)| {
                seen.contains(key)
                    && discovered
                        .iter()
                        .any(|d| d.connected && d.endpoint == *cached_endpoint)
            });
        }
        let identities = self
            .identity_cache
            .lock()
            .expect("identity cache poisoned")
            .clone();

        let state = self.store.load_async().await?;
        let legacy_id = if mode.is_legacy() {
            state
                .devices
                .values()
                .next()
                .map(|dev| dev.identity.id.clone())
        } else {
            None
        };

        // Resolve every connection, then fold same-scan duplicate tokens.
        let mut resolved: Vec<ResolvedConnection> = Vec::with_capacity(discovered.len());
        for disc in &discovered {
            let decode =
                if !mode.is_legacy() && disc.connected && is_usb_transport(disc.endpoint.transport)
                {
                    identities
                        .get(&(disc.endpoint.transport, disc.endpoint.locator.clone()))
                        .filter(|(_, cached_endpoint)| cached_endpoint == &disc.endpoint)
                        .map(|(decode, _)| *decode)
                } else {
                    None
                };
            let resolution = resolve_connection(
                &state.devices,
                state.identity_setup.as_ref(),
                legacy_id.as_ref(),
                &disc.endpoint,
                decode,
            );
            resolved.push(ResolvedConnection {
                endpoint: disc.endpoint.clone(),
                connected: disc.connected,
                resolution,
            });
        }
        mark_scan_duplicates(&mut resolved);

        // Persist endpoint associations for resolved mice only.
        self.sync_endpoint_associations(&resolved, selection)
            .await?;

        let state = self.store.load_async().await?;
        let devices = aggregate_discovered_devices(&state, &resolved);
        Ok(DiscoveryView {
            mode,
            devices,
            connections: resolved,
        })
    }

    async fn ensure_legacy_mouse(
        &self,
        discovered: &[DiscoveredEndpoint],
    ) -> Result<(), ManagerError> {
        let discovered = discovered.to_vec();
        let inner: Result<(), ManagerError> = self
            .store
            .mutate_async(move |state| {
                if !state.devices.is_empty() {
                    return Ok(());
                }
                let new_id = state.allocate_device_id()?;
                let display_name = discovered
                    .first()
                    .and_then(|d| d.endpoint.display_name.clone());
                let mut identity = DeviceIdentity::new(new_id.clone(), display_name);
                for disc in &discovered {
                    identity.upsert_endpoint(disc.endpoint.clone());
                }
                state.devices.insert(new_id, DeviceState::new(identity));
                Ok(())
            })
            .await?;
        inner?;
        Ok(())
    }

    /// Replaces the persisted endpoint associations of resolved logical mice
    /// with the connections seen this scan. USB endpoints of scanned
    /// transports that did not appear are dropped (attachment cache
    /// invalidation); BLE associations are never dropped, only updated.
    async fn sync_endpoint_associations(
        &self,
        resolved: &[ResolvedConnection],
        selection: TransportSelection,
    ) -> Result<(), ManagerError> {
        let scan_transports: BTreeSet<TransportKind> = match selection {
            TransportSelection::Auto => [
                TransportKind::Wired,
                TransportKind::Receiver,
                TransportKind::Ble,
            ]
            .into_iter()
            .collect(),
            TransportSelection::Exact(transport) => [transport].into_iter().collect(),
        };
        let mut by_device: BTreeMap<DeviceId, Vec<ResolvedConnection>> = BTreeMap::new();
        for row in resolved {
            if let IdentityResolution::Resolved { identity } = &row.resolution {
                by_device
                    .entry(identity.clone())
                    .or_default()
                    .push(row.clone());
            }
        }
        if by_device.is_empty() {
            return Ok(());
        }
        let inner: Result<(), ManagerError> = self
            .store
            .mutate_async(move |state| {
                for (device_id, rows) in by_device {
                    let Some(device_state) = state.devices.get_mut(&device_id) else {
                        // The device disappeared between resolution and the
                        // state lock; leave its associations untouched rather
                        // than committing a partial sync.
                        continue;
                    };
                    let mut endpoints: BTreeMap<TransportKind, DeviceEndpoint> = BTreeMap::new();
                    for (transport, endpoint) in &device_state.identity.endpoints {
                        if *transport == TransportKind::Ble || !scan_transports.contains(transport)
                        {
                            endpoints.insert(*transport, endpoint.clone());
                        }
                    }
                    for row in rows {
                        endpoints.insert(row.endpoint.transport, row.endpoint.clone());
                    }
                    device_state.identity.endpoints = endpoints;
                }
                Ok(())
            })
            .await?;
        inner?;
        Ok(())
    }

    pub async fn list_devices(
        &self,
        selection: TransportSelection,
    ) -> Result<Vec<DiscoveredDevice>, ManagerError> {
        Ok(self.discover(selection).await?.devices)
    }

    pub fn selected_device(&self) -> Result<Option<DeviceId>, ManagerError> {
        Ok(self.store.load()?.selected_device)
    }

    pub fn select_device(&self, device: &DeviceId) -> Result<(), ManagerError> {
        let mut transaction = self.store.transaction()?;
        if !transaction.state().devices.contains_key(device) {
            return Err(ManagerError::DeviceNotFound(device.clone()));
        }
        transaction.state_mut().selected_device = Some(device.clone());
        transaction.commit()?;
        Ok(())
    }

    pub async fn resolve_device(
        &self,
        explicit: Option<&DeviceId>,
        selection: TransportSelection,
    ) -> Result<DeviceId, ManagerError> {
        let discovered = self.discover(selection).await?.devices;

        if let Some(explicit_id) = explicit {
            let state = self.store.load_async().await?;
            if !state.devices.contains_key(explicit_id) {
                return Err(ManagerError::DeviceNotFound(explicit_id.clone()));
            }
            let is_connected = discovered
                .iter()
                .any(|d| d.connected && &d.identity.id == explicit_id);
            if !is_connected {
                return Err(ManagerError::DeviceDisconnected(explicit_id.clone()));
            }
            if let TransportSelection::Exact(transport) = selection {
                self.set_preferred_transport(explicit_id, transport).await?;
            }
            return Ok(explicit_id.clone());
        }

        let state = self.store.load_async().await?;
        if let Some(selected) = state.selected_device.clone()
            && discovered
                .iter()
                .any(|d| d.connected && d.identity.id == selected)
        {
            if let TransportSelection::Exact(transport) = selection {
                self.set_preferred_transport(&selected, transport).await?;
            }
            return Ok(selected);
        }

        let mut candidates: Vec<DeviceId> = discovered
            .iter()
            .filter(|d| d.connected)
            .map(|d| d.identity.id.clone())
            .collect();
        candidates.sort();
        candidates.dedup();

        match candidates.as_slice() {
            [] => Err(ManagerError::NoDevice { selection }),
            [candidate] => {
                if let TransportSelection::Exact(transport) = selection {
                    self.set_preferred_transport(candidate, transport).await?;
                }
                let candidate_owned = candidate.clone();
                let inner: Result<(), ManagerError> = self
                    .store
                    .mutate_async(move |state| {
                        if !state.devices.contains_key(&candidate_owned) {
                            return Err(ManagerError::DeviceNotFound(candidate_owned.clone()));
                        }
                        state.selected_device = Some(candidate_owned.clone());
                        Ok(())
                    })
                    .await?;
                inner?;
                Ok(candidate.clone())
            }
            _ => {
                // Edge case: several logical mice connected, none selected.
                // If exactly one connected candidate has a wired transport,
                // auto-select it instead of forcing the user to manually pick.
                let wired_ids: Vec<DeviceId> = discovered
                    .iter()
                    .filter(|d| d.connected && d.transports.contains(&TransportKind::Wired))
                    .map(|d| d.identity.id.clone())
                    .collect();
                let mut wired_dedup = wired_ids;
                wired_dedup.sort();
                wired_dedup.dedup();
                if wired_dedup.len() == 1 {
                    let candidate = wired_dedup[0].clone();
                    if let TransportSelection::Exact(transport) = selection {
                        self.set_preferred_transport(&candidate, transport).await?;
                    }
                    let candidate_owned = candidate.clone();
                    let inner: Result<(), ManagerError> = self
                        .store
                        .mutate_async(move |state| {
                            if !state.devices.contains_key(&candidate_owned) {
                                return Err(ManagerError::DeviceNotFound(candidate_owned.clone()));
                            }
                            state.selected_device = Some(candidate_owned.clone());
                            Ok(())
                        })
                        .await?;
                    inner?;
                    return Ok(candidate);
                }
                Err(ManagerError::AmbiguousDevice {
                    selection,
                    candidates,
                })
            }
        }
    }

    async fn set_preferred_transport(
        &self,
        device: &DeviceId,
        transport: TransportKind,
    ) -> Result<(), ManagerError> {
        let device = device.clone();
        self.store
            .mutate_async(move |state| {
                let device_state = state
                    .devices
                    .get_mut(&device)
                    .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
                device_state.identity.preferred_transport = Some(transport);
                Ok::<_, ManagerError>(())
            })
            .await?
    }

    pub fn register_device(&self, identity: DeviceIdentity) -> Result<(), ManagerError> {
        let mut txn = self.store.transaction()?;
        let state = txn.state_mut();
        if let Some(num) = identity.id.number()
            && state.next_device_number <= num
        {
            state.next_device_number = num.checked_add(1).ok_or_else(|| {
                ManagerError::State(crate::error::StateError::invalid_state(
                    "nextDeviceNumber overflow",
                ))
            })?;
        }
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

    /// Removes a logical identity from durable state.
    ///
    /// Refuses when the identity carries configuration evidence unless
    /// `force` is set; clearing the selected device clears the selection.
    pub fn forget_device(&self, device: &DeviceId, force: bool) -> Result<bool, ManagerError> {
        let mut txn = self.store.transaction()?;
        let state = txn
            .state_mut()
            .devices
            .get(device)
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
        if device_has_evidence(state) && !force {
            return Ok(false);
        }
        txn.state_mut().devices.remove(device);
        if txn.state().selected_device.as_ref() == Some(device) {
            txn.state_mut().selected_device = None;
        }
        txn.commit()?;
        Ok(true)
    }

    /// Sets the presentation-only display name; a blank name clears it.
    pub fn rename_device(&self, device: &DeviceId, name: &str) -> Result<(), ManagerError> {
        let trimmed = name.trim();
        let display_name = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        };
        let mut txn = self.store.transaction()?;
        txn.state_mut()
            .devices
            .get_mut(device)
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?
            .identity
            .display_name = display_name;
        txn.commit()?;
        Ok(())
    }

    /// Resolves a case-insensitive unique display name to its logical identity.
    pub fn find_device_by_name(&self, name: &str) -> Result<DeviceId, ManagerError> {
        let state = self.store.load()?;
        let needle = name.trim().to_lowercase();
        let matches: Vec<&DeviceId> = state
            .devices
            .values()
            .filter(|device| {
                device
                    .identity
                    .display_name
                    .as_ref()
                    .is_some_and(|display| display.to_lowercase() == needle)
            })
            .map(|device| &device.identity.id)
            .collect();
        match matches.as_slice() {
            [id] => Ok((*id).clone()),
            [] => Err(ManagerError::InvalidUpdate(format!(
                "no device with display name {name:?}"
            ))),
            _ => Err(ManagerError::AmbiguousDevice {
                selection: TransportSelection::Auto,
                candidates: matches.iter().map(|id| (*id).clone()).collect(),
            }),
        }
    }

    pub(crate) async fn open_locked(
        &self,
        device: &DeviceId,
        operation: &'static str,
    ) -> Result<
        (
            DeviceIdentity,
            Box<dyn DeviceSession>,
            crate::state::DeviceOperationGuard,
        ),
        ManagerError,
    > {
        let guard = self
            .store
            .acquire_operation_lock_async(device, OPERATION_LOCK_TIMEOUT, operation)
            .await?;
        let device_owned = device.clone();
        let state = self.store.load_async().await?;
        if let Some(journal) = &state.identity_setup
            && journal_blocks_device(journal, device)
        {
            return Err(ManagerError::DeviceInvolvedInSetup {
                device: device.clone(),
                phase: journal.phase,
            });
        }
        let identity = state
            .devices
            .get(&device_owned)
            .ok_or_else(|| ManagerError::DeviceNotFound(device_owned.clone()))?
            .identity
            .clone();
        let endpoint = identity
            .selected_endpoint()
            .cloned()
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
        let session = self.factory.open(&endpoint).await?;

        // Persistent-mode writes re-stamp the physical mouse's own watermark,
        // so the opened endpoint must be authenticated as that physical unit
        // for THIS attachment before any physical-id overlay can reach the
        // wire. The per-attachment cache keeps this to one read per
        // continuous attachment (the FA60 receiver's armed read is
        // expensive). BLE cannot verify a watermark and is documented as an
        // unverifiable cached locator, so it is exempt from this gate.
        if let Some(physical_id) = identity.physical_id
            && is_usb_transport(endpoint.transport)
        {
            let decode = self
                .authenticate_attachment(&endpoint, session.as_ref())
                .await?;
            if decode != WatermarkDecode::Valid(physical_id) {
                let detail = match decode {
                    WatermarkDecode::Valid(other) => {
                        format!("watermark {other:?} does not match this logical mouse")
                    }
                    WatermarkDecode::Absent => {
                        "no watermark present; the physical identity is lost and must be restored"
                            .to_owned()
                    }
                    WatermarkDecode::Malformed => {
                        "watermark marker is malformed; the device is not authenticated".to_owned()
                    }
                    WatermarkDecode::UnsupportedVersion { version } => {
                        format!("unsupported watermark version {version}")
                    }
                };
                return Err(ManagerError::AttachmentNotAuthenticated {
                    device: device.clone(),
                    endpoint: Box::new(endpoint),
                    detail,
                });
            }
        }
        Ok((identity, session, guard))
    }

    /// Returns the current watermark of one USB attachment, reading it through
    /// the open session only when the per-attachment cache holds no matching
    /// entry for this exact endpoint. Cached entries are keyed by transport +
    /// locator but only trusted when the cached endpoint object still matches,
    /// so a different unit presented at the same path is re-authenticated.
    async fn authenticate_attachment(
        &self,
        endpoint: &DeviceEndpoint,
        session: &dyn DeviceSession,
    ) -> Result<WatermarkDecode, ManagerError> {
        let key = (endpoint.transport, endpoint.locator.clone());
        {
            let cache = self.identity_cache.lock().expect("identity cache poisoned");
            if let Some((decode, cached_endpoint)) = cache.get(&key)
                && cached_endpoint == endpoint
            {
                return Ok(*decode);
            }
        }
        let decode = self.current_watermark(session).await?.1;
        let key = (endpoint.transport, endpoint.locator.clone());
        self.identity_cache
            .lock()
            .expect("identity cache poisoned")
            .insert(key, (decode, endpoint.clone()));
        Ok(decode)
    }

    pub async fn read_battery(&self, device: &DeviceId) -> Result<u8, ManagerError> {
        let (_identity, session, _guard) = self.open_locked(device, "read_battery").await?;
        session.read_battery(BATTERY_READ_TIMEOUT).await
    }

    pub async fn read_status(&self, device: &DeviceId) -> Result<DeviceStatus, ManagerError> {
        let (identity, session, _guard) = self.open_locked(device, "read_status").await?;
        let transport = session.transport();
        let usb = is_usb_transport(transport);

        let battery = match transport {
            TransportKind::Wired => None,
            TransportKind::Receiver | TransportKind::Ble => {
                match session.read_battery(BATTERY_READ_TIMEOUT).await {
                    Ok(level) => Some(level),
                    Err(ManagerError::UnsupportedOperation { .. }) => None,
                    Err(error) => return Err(error),
                }
            }
        };

        let profile_metadata = match session.read_profile_metadata().await {
            Ok(metadata) if usb => {
                let resource = self
                    .update_observed_profile_metadata_async(device, metadata)
                    .await?;
                Some(ResourceSnapshot { resource })
            }
            Ok(_) => None,
            Err(ManagerError::UnsupportedOperation { .. }) => None,
            Err(error) => return Err(error),
        };

        let current_profile = profile_metadata
            .as_ref()
            .and_then(|snapshot| snapshot.resource.observed.as_ref())
            .map(|observed| observed.value.current());
        let polling_rate = match current_profile {
            Some(profile) if usb => match session.read_profile(profile).await {
                Ok(_) => match session.read_live_polling_rate(profile).await {
                    Ok(rate) => {
                        let resource = self
                            .update_observed_polling_rate_async(device, profile, rate)
                            .await?;
                        Some(ResourceSnapshot { resource })
                    }
                    Err(ManagerError::UnsupportedOperation { .. }) => None,
                    Err(error) => return Err(error),
                },
                Err(ManagerError::UnsupportedOperation { .. }) => None,
                Err(error) => return Err(error),
            },
            _ => None,
        };

        Ok(DeviceStatus {
            identity,
            battery,
            profile_metadata,
            polling_rate,
        })
    }

    /// Reads the current or requested USB profile and its status through one
    /// uninterrupted session, loading the target profile exactly once.
    pub async fn read_live_profile(
        &self,
        device: &DeviceId,
        requested: Option<ProfileId>,
    ) -> Result<LiveProfileSnapshot, ManagerError> {
        let (identity, session, _guard) = self.open_locked(device, "read_live_profile").await?;
        let transport = session.transport();
        if !is_usb_transport(transport) {
            return Err(ManagerError::UnsupportedOperation {
                operation: "read-live-profile",
                transport,
            });
        }

        let battery = match transport {
            TransportKind::Receiver => match session.read_battery(BATTERY_READ_TIMEOUT).await {
                Ok(level) => Some(level),
                Err(ManagerError::UnsupportedOperation { .. }) => None,
                Err(error) => return Err(error),
            },
            TransportKind::Wired => None,
            TransportKind::Ble => unreachable!("BLE was rejected above"),
        };
        let profile_metadata = session.read_profile_metadata().await?;
        let target = requested
            .filter(|profile| *profile <= profile_metadata.maximum())
            .unwrap_or(profile_metadata.current());
        let profile = session.read_profile(target).await?;
        let polling_rate = session.read_live_polling_rate(target).await?;

        let now = self.now();
        let device_owned = device.clone();
        let profile_for_state = profile.clone();
        let inner: Result<(), ManagerError> = self
            .store
            .mutate_async(move |state| {
                let device_state = state
                    .devices
                    .get_mut(&device_owned)
                    .ok_or_else(|| ManagerError::DeviceNotFound(device_owned.clone()))?;
                crate::resources::state::reconcile_observed(
                    &mut device_state.profile_metadata,
                    profile_metadata,
                    now,
                );
                let profile_state = device_state
                    .profiles
                    .entry(target)
                    .or_insert_with(ProfileState::empty);
                crate::resources::state::reconcile_observed(
                    &mut profile_state.dpi,
                    profile_for_state.dpi,
                    now,
                );
                crate::resources::state::reconcile_observed(
                    &mut profile_state.preferences,
                    profile_for_state.preferences,
                    now,
                );
                crate::resources::state::reconcile_observed(
                    &mut profile_state.buttons,
                    profile_for_state.buttons,
                    now,
                );
                crate::resources::state::reconcile_observed(
                    &mut profile_state.polling_rate,
                    polling_rate,
                    now,
                );
                Ok(())
            })
            .await?;
        inner?;

        Ok(LiveProfileSnapshot {
            identity,
            battery,
            profile_metadata,
            profile,
            polling_rate,
        })
    }

    pub async fn read_profile(
        &self,
        device: &DeviceId,
        profile: ProfileId,
    ) -> Result<ProfileSnapshot, ManagerError> {
        let (_identity, session, _guard) = self.open_locked(device, "read_profile").await?;
        let snapshot = session.read_profile(profile).await?;

        if is_usb_transport(session.transport()) {
            let now = self.now();
            let device_owned = device.clone();
            let dpi = snapshot.dpi.clone();
            let preferences = snapshot.preferences;
            let buttons = snapshot.buttons;
            let inner: Result<(), ManagerError> = self
                .store
                .mutate_async(move |state| {
                    let device_state = state
                        .devices
                        .get_mut(&device_owned)
                        .ok_or_else(|| ManagerError::DeviceNotFound(device_owned.clone()))?;
                    let profile_state = device_state
                        .profiles
                        .entry(profile)
                        .or_insert_with(ProfileState::empty);
                    crate::resources::state::reconcile_observed(
                        &mut profile_state.dpi,
                        dpi.clone(),
                        now,
                    );
                    crate::resources::state::reconcile_observed(
                        &mut profile_state.preferences,
                        preferences,
                        now,
                    );
                    crate::resources::state::reconcile_observed(
                        &mut profile_state.buttons,
                        buttons,
                        now,
                    );
                    Ok(())
                })
                .await?;
            inner?;
        }

        Ok(snapshot)
    }

    pub async fn activate_profile(
        &self,
        device: &DeviceId,
        profile: ProfileId,
    ) -> Result<ProfileMetadata, ManagerError> {
        let (_identity, session, _guard) = self.open_locked(device, "activate_profile").await?;
        let maximum = if is_usb_transport(session.transport()) {
            session
                .read_profile_metadata()
                .await?
                .maximum()
                .max(profile)
        } else {
            ProfileId::MAX_ID
        };
        let target = ProfileMetadata::new(profile, maximum)
            .map_err(|error| ManagerError::Driver(error.into()))?;
        self.write_profile_metadata(device, session.as_ref(), target)
            .await
    }

    pub async fn set_profile_metadata(
        &self,
        device: &DeviceId,
        current: ProfileId,
        maximum: ProfileId,
    ) -> Result<ProfileMetadata, ManagerError> {
        let (_identity, session, _guard) = self.open_locked(device, "set_profile_metadata").await?;
        let target = ProfileMetadata::new(current, maximum)
            .map_err(|error| ManagerError::Driver(error.into()))?;
        self.write_profile_metadata(device, session.as_ref(), target)
            .await
    }

    async fn write_profile_metadata(
        &self,
        device: &DeviceId,
        session: &dyn DeviceSession,
        target: ProfileMetadata,
    ) -> Result<ProfileMetadata, ManagerError> {
        let usb = is_usb_transport(session.transport());
        let (actual, observed) = match session.write_profile_metadata(target).await? {
            SessionWrite::ReadbackVerified(actual) => (actual, usb.then_some(actual)),
            SessionWrite::Acknowledged => (target, None),
        };
        let mismatch = usb && actual != target;
        let now = self.now();
        let device = device.clone();
        let result = self
            .store
            .mutate_async(move |state| {
                let resource = &mut state
                    .devices
                    .get_mut(&device)
                    .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?
                    .profile_metadata;
                if let Some(observed) = observed {
                    crate::resources::state::record_readback(resource, target, observed, now);
                } else {
                    crate::resources::state::record_ack(resource, target, now);
                }
                Ok::<_, ManagerError>(())
            })
            .await?;
        result?;
        if mismatch {
            return Err(ManagerError::VerificationMismatch {
                resource: "profile metadata",
                profile: Some(target.current()),
            });
        }
        Ok(actual)
    }
    pub async fn apply_profile_update(
        &self,
        device: &DeviceId,
        profile: ProfileId,
        update: crate::operation::ProfileUpdate,
        policy: crate::operation::UpdatePolicy,
    ) -> Result<crate::operation::ProfileUpdateOutcome, ManagerError> {
        self.apply_profile_update_inner(device, profile, update, policy, None)
            .await
    }

    /// Applies a GUI-held draft against the complete profile image from which
    /// that draft was created. Live-baseline resources use the supplied image
    /// instead of performing another pre-write hardware read; write
    /// verification retains the requested policy.
    pub async fn apply_profile_update_from_snapshot(
        &self,
        device: &DeviceId,
        profile: ProfileId,
        update: crate::operation::ProfileUpdate,
        policy: crate::operation::UpdatePolicy,
        baseline: crate::operation::ProfileUpdateBaseline,
    ) -> Result<crate::operation::ProfileUpdateOutcome, ManagerError> {
        if baseline.dpi.profile != profile
            || baseline.preferences.profile != profile
            || baseline.buttons.profile != profile
        {
            return Err(ManagerError::InvalidUpdate(format!(
                "provided stored/profile copy does not target profile {profile}"
            )));
        }
        self.apply_profile_update_inner(device, profile, update, policy, Some(baseline))
            .await
    }

    async fn apply_profile_update_inner(
        &self,
        device: &DeviceId,
        profile: ProfileId,
        update: crate::operation::ProfileUpdate,
        policy: crate::operation::UpdatePolicy,
        provided_baseline: Option<crate::operation::ProfileUpdateBaseline>,
    ) -> Result<crate::operation::ProfileUpdateOutcome, ManagerError> {
        if update.is_empty() {
            return Err(ManagerError::InvalidUpdate(
                "profile update must provide at least one field".to_owned(),
            ));
        }
        if update.has_polling() && update.has_non_rate() {
            return Err(ManagerError::InvalidUpdate(
                "polling rate cannot be combined with DPI/preferences/buttons; it must be set on its own".to_owned(),
            ));
        }
        if let Some(dpi_delta) = &update.dpi
            && dpi_delta.is_empty()
        {
            return Err(ManagerError::InvalidUpdate(
                "DPI delta must provide at least one field".to_owned(),
            ));
        }
        if let Some(prefs_delta) = &update.preferences
            && prefs_delta.is_empty()
        {
            return Err(ManagerError::InvalidUpdate(
                "preferences delta must provide at least one field".to_owned(),
            ));
        }

        let (identity, session, _guard) = self.open_locked(device, "apply_profile_update").await?;
        let transport = session.transport();
        let session_ref: &dyn DeviceSession = session.as_ref();

        if let Some(desired_rate) = update.polling_rate {
            if transport == TransportKind::Ble {
                return Err(ManagerError::ExplicitAuthorizationRequired {
                    operation: "update_polling_rate",
                    transport: TransportKind::Ble,
                });
            }
            let image = self.require_complete_desired_image(device, profile).await?;
            let snapshot = session_ref.read_profile(profile).await?;
            if snapshot.persistent_metadata.current() != profile {
                return Err(ManagerError::VerificationMismatch {
                    resource: "profile metadata",
                    profile: Some(profile),
                });
            }
            if snapshot.dpi != image.dpi {
                return Err(ManagerError::MissingBaseline {
                    resource: "DPI",
                    profile: Some(profile),
                });
            }
            if snapshot.preferences != image.preferences {
                return Err(ManagerError::MissingBaseline {
                    resource: "preferences",
                    profile: Some(profile),
                });
            }
            if snapshot.buttons != image.buttons {
                return Err(ManagerError::MissingBaseline {
                    resource: "buttons",
                    profile: Some(profile),
                });
            }
            let current = session_ref.read_live_polling_rate(profile).await?;
            let write = if current == desired_rate {
                SessionWrite::ReadbackVerified(current)
            } else {
                session_ref
                    .write_polling_rate_unchecked(profile, desired_rate, policy.verification)
                    .await?
            };
            let now = self.now();
            let device_id = device.clone();
            let outcome = self
                .store
                .mutate_async(move |state| {
                    crate::resources::state::persist_write(
                        state,
                        &device_id,
                        profile,
                        |ps| &mut ps.polling_rate,
                        desired_rate,
                        write,
                        now,
                    )
                })
                .await??;
            let finished = crate::resources::state::finish_write(outcome, "polling rate", profile)?;
            return Ok(crate::operation::ProfileUpdateOutcome {
                polling_rate: Some(finished),
                ..Default::default()
            });
        }

        // Non-rate composite: DPI, preferences, buttons share one session.
        let mut dpi_pair: Option<(
            attack_shark_x3::DpiState,
            SessionWrite<attack_shark_x3::DpiState>,
        )> = None;
        let mut prefs_pair: Option<(
            attack_shark_x3::PreferencesState,
            SessionWrite<attack_shark_x3::PreferencesState>,
        )> = None;
        let mut buttons_pair: Option<(
            attack_shark_x3::ButtonsState,
            SessionWrite<attack_shark_x3::ButtonsState>,
        )> = None;

        if let Some(delta) = update.dpi {
            let baseline = match transport {
                TransportKind::Ble => {
                    self.load_stored_dpi_baseline(device, profile, policy.allow_explicit_defaults)
                        .await?
                }
                TransportKind::Wired | TransportKind::Receiver => match policy.baseline {
                    crate::operation::BaselineSource::Live => {
                        if let Some(baseline) = provided_baseline.as_ref() {
                            baseline.dpi.clone()
                        } else {
                            session_ref.read_dpi(profile).await?
                        }
                    }
                    crate::operation::BaselineSource::Stored => {
                        self.load_stored_dpi_baseline(device, profile, false)
                            .await?
                    }
                },
            };
            let desired = crate::resources::dpi::merge_dpi_delta(baseline, &delta)?;
            // Every production DPI write re-stamps this physical mouse's own
            // watermark, so no foreign or missing marker can reach the wire.
            let desired = crate::resources::with_physical_watermark(desired, identity.physical_id);
            let write = session_ref
                .write_dpi(desired.clone(), policy.verification)
                .await?;
            dpi_pair = Some((desired, write));
        }

        if let Some(delta) = update.preferences {
            let baseline = match transport {
                TransportKind::Ble => {
                    self.load_stored_preferences_baseline(
                        device,
                        profile,
                        policy.allow_explicit_defaults,
                    )
                    .await?
                }
                TransportKind::Wired | TransportKind::Receiver => match policy.baseline {
                    crate::operation::BaselineSource::Live => {
                        if let Some(baseline) = provided_baseline.as_ref() {
                            baseline.preferences
                        } else {
                            session_ref.read_preferences(profile).await?
                        }
                    }
                    crate::operation::BaselineSource::Stored => {
                        self.load_stored_preferences_baseline(device, profile, false)
                            .await?
                    }
                },
            };
            let desired = crate::resources::settings::merge_preferences_delta(baseline, delta);
            let write = session_ref
                .write_preferences(desired, policy.verification)
                .await?;
            prefs_pair = Some((desired, write));
        }

        if !update.buttons.is_empty() {
            let baseline = match transport {
                TransportKind::Ble => {
                    self.load_stored_buttons_baseline(
                        device,
                        profile,
                        policy.allow_explicit_defaults,
                    )
                    .await?
                }
                TransportKind::Wired | TransportKind::Receiver => match policy.baseline {
                    crate::operation::BaselineSource::Live => {
                        if let Some(baseline) = provided_baseline.as_ref() {
                            baseline.buttons
                        } else {
                            session_ref.read_buttons(profile).await?
                        }
                    }
                    crate::operation::BaselineSource::Stored => {
                        self.load_stored_buttons_baseline(device, profile, false)
                            .await?
                    }
                },
            };
            let mut desired = baseline;
            for delta in &update.buttons {
                desired.slots[delta.slot_index()] = delta.assignment();
            }
            let write = session_ref
                .write_buttons(desired, policy.verification)
                .await?;
            buttons_pair = Some((desired, write));
        }

        let now = self.now();
        let device_id = device.clone();
        let (dpi_outcome, prefs_outcome, buttons_outcome) = self
            .store
            .mutate_async(move |state| {
                let mut dpi_out: Option<crate::operation::WriteOutcome<attack_shark_x3::DpiState>> =
                    None;
                let mut prefs_out: Option<
                    crate::operation::WriteOutcome<attack_shark_x3::PreferencesState>,
                > = None;
                let mut btn_out: Option<
                    crate::operation::WriteOutcome<attack_shark_x3::ButtonsState>,
                > = None;
                if let Some((desired, write)) = dpi_pair {
                    let out = crate::resources::state::persist_write(
                        state,
                        &device_id,
                        profile,
                        |ps| &mut ps.dpi,
                        desired,
                        write,
                        now,
                    )?;
                    dpi_out = Some(out);
                }
                if let Some((desired, write)) = prefs_pair {
                    let out = crate::resources::state::persist_write(
                        state,
                        &device_id,
                        profile,
                        |ps| &mut ps.preferences,
                        desired,
                        write,
                        now,
                    )?;
                    prefs_out = Some(out);
                }
                if let Some((desired, write)) = buttons_pair {
                    let profile = desired.profile;
                    let out = crate::resources::state::persist_write(
                        state,
                        &device_id,
                        profile,
                        |ps| &mut ps.buttons,
                        desired,
                        write,
                        now,
                    )?;
                    btn_out = Some(out);
                }
                Ok::<_, ManagerError>((dpi_out, prefs_out, btn_out))
            })
            .await??;

        let dpi_final = if let Some(out) = dpi_outcome {
            Some(crate::resources::state::finish_write(out, "DPI", profile)?)
        } else {
            None
        };
        let prefs_final = if let Some(out) = prefs_outcome {
            Some(crate::resources::state::finish_write(
                out,
                "preferences",
                profile,
            )?)
        } else {
            None
        };
        let buttons_final = if let Some(out) = buttons_outcome {
            Some(crate::resources::state::finish_write(
                out, "buttons", profile,
            )?)
        } else {
            None
        };

        Ok(crate::operation::ProfileUpdateOutcome {
            dpi: dpi_final,
            preferences: prefs_final,
            buttons: buttons_final,
            polling_rate: None,
        })
    }

    pub fn device_identity(&self, device: &DeviceId) -> Result<DeviceIdentity, ManagerError> {
        let state = self.store.load()?;
        state
            .devices
            .get(device)
            .map(|device_state| device_state.identity.clone())
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))
    }

    pub(crate) fn now(&self) -> Timestamp {
        Timestamp {
            unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
        }
    }

    async fn update_observed_profile_metadata_async(
        &self,
        device: &DeviceId,
        value: ProfileMetadata,
    ) -> Result<ResourceState<ProfileMetadata>, ManagerError> {
        let now = self.now();
        let device_owned = device.clone();
        let inner: Result<ResourceState<ProfileMetadata>, ManagerError> = self
            .store
            .mutate_async(move |state| {
                let device_state = state
                    .devices
                    .get_mut(&device_owned)
                    .ok_or_else(|| ManagerError::DeviceNotFound(device_owned.clone()))?;
                crate::resources::state::reconcile_observed(
                    &mut device_state.profile_metadata,
                    value,
                    now,
                );
                Ok(device_state.profile_metadata.clone())
            })
            .await?;
        inner
    }

    async fn update_observed_polling_rate_async(
        &self,
        device: &DeviceId,
        profile: ProfileId,
        value: attack_shark_x3::PollingRate,
    ) -> Result<ResourceState<attack_shark_x3::PollingRate>, ManagerError> {
        let now = self.now();
        let device_owned = device.clone();
        let inner: Result<ResourceState<attack_shark_x3::PollingRate>, ManagerError> = self
            .store
            .mutate_async(move |state| {
                let device_state = state
                    .devices
                    .get_mut(&device_owned)
                    .ok_or_else(|| ManagerError::DeviceNotFound(device_owned.clone()))?;
                let profile_state = device_state
                    .profiles
                    .entry(profile)
                    .or_insert_with(ProfileState::empty);
                crate::resources::state::reconcile_observed(
                    &mut profile_state.polling_rate,
                    value,
                    now,
                );
                Ok(profile_state.polling_rate.clone())
            })
            .await?;
        inner
    }

    // ---------------------------------------------------------------------
    // Physical-identity ceremonies
    //
    // Ceremonies are driven through the durable, resumable
    // [`IdentitySetupJournal`]. Every irreversible hardware write (a
    // watermark stamp) is preceded by a journal persistence describing it
    // (reserved token + captured image + per-profile stamp progress), so an
    // interrupted ceremony resumes idempotently and never silently returns to
    // legacy mode or mints a replacement token. Cancel is only accepted
    // before any profile has been stamped.
    // ---------------------------------------------------------------------

    /// Returns the current identity-ceremony progress, when one is in flight.
    ///
    /// The journal is durable and resumable: after an interruption the
    /// frontend inspects this and issues the action its stage requires
    /// (reconnect for `AwaitingReconnect`, stamp/adopt for `Stamping`).
    pub fn identity_ceremony_progress(
        &self,
    ) -> Result<Option<IdentityCeremonyProgress>, ManagerError> {
        let state = self.store.load()?;
        Ok(state
            .identity_setup
            .as_ref()
            .map(ceremony_progress_from_journal))
    }

    /// Begins a physical-identity ceremony.
    ///
    /// `target` selects the logical mouse for Restore and BLE association and
    /// is ignored otherwise. Refuses while another ceremony is in flight and
    /// when the kind does not fit the installation identity mode. Restore
    /// also refuses while the saved mouse still has a connected USB endpoint:
    /// the reconnect gesture is the authentication.
    pub async fn begin_identity_ceremony(
        &self,
        kind: IdentityCeremonyKind,
        target: Option<DeviceId>,
    ) -> Result<IdentityCeremonyProgress, ManagerError> {
        let phase = match kind {
            IdentityCeremonyKind::InitialEnrollment => IdentitySetupPhase::InitialEnrollment,
            IdentityCeremonyKind::AddMouse => IdentitySetupPhase::AddMouse,
            IdentityCeremonyKind::Restore => IdentitySetupPhase::Restore,
            IdentityCeremonyKind::ForeignAdoption => IdentitySetupPhase::ForeignAdoption,
            IdentityCeremonyKind::BleAssociation => IdentitySetupPhase::BleAssociation,
        };
        let state = self.store.load_async().await?;
        if let Some(journal) = &state.identity_setup {
            return Err(ManagerError::IdentitySetupInProgress {
                phase: journal.phase,
            });
        }
        match phase {
            IdentitySetupPhase::InitialEnrollment => {
                if state.identity_mode != IdentityMode::Legacy {
                    return Err(ManagerError::IdentityCeremonyRefused {
                        phase,
                        reason: "already-persistent",
                        detail: "initial enrollment is the Legacy -> Persistent transition; this installation already has persistent physical identity"
                            .to_owned(),
                    });
                }
            }
            IdentitySetupPhase::AddMouse
            | IdentitySetupPhase::Restore
            | IdentitySetupPhase::ForeignAdoption
            | IdentitySetupPhase::BleAssociation => {
                if state.identity_mode != IdentityMode::Persistent {
                    return Err(ManagerError::IdentityCeremonyRefused {
                        phase,
                        reason: "requires-persistent",
                        detail: "this ceremony requires persistent physical identity".to_owned(),
                    });
                }
            }
        }
        if matches!(
            phase,
            IdentitySetupPhase::Restore | IdentitySetupPhase::BleAssociation
        ) {
            let target_id =
                target
                    .as_ref()
                    .ok_or_else(|| ManagerError::IdentityCeremonyRefused {
                        phase,
                        reason: "requires-target",
                        detail:
                            "restore and BLE association require the logical mouse to operate on"
                                .to_owned(),
                    })?;
            let device = state
                .devices
                .get(target_id)
                .ok_or_else(|| ManagerError::DeviceNotFound(target_id.clone()))?;
            if phase == IdentitySetupPhase::Restore && device.identity.physical_id.is_none() {
                return Err(ManagerError::IdentityCeremonyRefused {
                    phase,
                    reason: "requires-physical-id",
                    detail: format!("{target_id} has no physical identity to restore"),
                });
            }
        }
        // Restore must not run while the old unit is still attached: the
        // reconnect gesture is the authentication for the rotated token.
        if phase == IdentitySetupPhase::Restore {
            let target_id = target.as_ref().expect("validated above");
            let device = &state.devices[target_id];
            let mut connected_locators: Vec<DeviceLocator> = Vec::new();
            for transport in [TransportKind::Wired, TransportKind::Receiver] {
                let listed = self
                    .factory
                    .list(TransportSelection::Exact(transport))
                    .await?;
                connected_locators.extend(
                    listed
                        .into_iter()
                        .filter(|discovered| discovered.connected)
                        .map(|discovered| discovered.endpoint.locator),
                );
            }
            if connected_locators.iter().any(|locator| {
                device
                    .identity
                    .endpoints
                    .values()
                    .any(|ep| &ep.locator == locator)
            }) {
                return Err(ManagerError::IdentityCeremonyRefused {
                    phase,
                    reason: "device-connected",
                    detail: "the saved mouse still has a connected USB endpoint; reconnect the physical mouse being assigned"
                        .to_owned(),
                });
            }
        }
        self.store
            .mutate_async(move |state| {
                state.identity_setup = Some(IdentitySetupJournal {
                    phase,
                    stage: IdentitySetupStage::AwaitingCapture,
                    subjects: Vec::new(),
                });
            })
            .await?;
        let state = self.store.load_async().await?;
        Ok(ceremony_progress_from_journal(
            state.identity_setup.as_ref().expect("journal just created"),
        ))
    }

    /// Advances the in-flight ceremony with a typed action.
    ///
    /// `endpoint` supplies the presented physical mouse for
    /// [`IdentityCeremonyAction::Reconnected`]/[`IdentityCeremonyAction::Capture`]
    /// and the BLE endpoint for [`IdentityCeremonyAction::Associate`].
    /// `target` selects the logical mouse for a restore reconnect. All other
    /// actions ignore both.
    pub async fn identity_ceremony_action(
        &self,
        action: IdentityCeremonyAction,
        endpoint: Option<DeviceEndpoint>,
        target: Option<DeviceId>,
    ) -> Result<IdentityCeremonyProgress, ManagerError> {
        match action {
            IdentityCeremonyAction::Begin(kind) => self.begin_identity_ceremony(kind, target).await,
            IdentityCeremonyAction::Reconnected | IdentityCeremonyAction::Capture => {
                let endpoint = endpoint.ok_or_else(|| {
                    ManagerError::InvalidUpdate(format!(
                        "{action:?} requires the presented endpoint"
                    ))
                })?;
                self.ceremony_reconnected(endpoint, target).await
            }
            IdentityCeremonyAction::Stamp => self.ceremony_stamp().await,
            IdentityCeremonyAction::Adopt => self.ceremony_adopt().await,
            IdentityCeremonyAction::Associate => {
                let endpoint = endpoint.ok_or_else(|| {
                    ManagerError::InvalidUpdate("associate requires the BLE endpoint".to_owned())
                })?;
                self.ceremony_associate(endpoint, target).await
            }
            IdentityCeremonyAction::AcceptMigration | IdentityCeremonyAction::SkipMigration => {
                self.ceremony_finalize_migration(action).await
            }
            IdentityCeremonyAction::Cancel => self.ceremony_cancel().await,
        }
    }

    /// Accepts a physically reconnected endpoint: authenticates the presented
    /// mouse against the ceremony's eligibility rules, captures its complete
    /// profile state, mints or adopts the token, and persists the subject
    /// journal (stage `Stamping`) before any watermark write can happen.
    async fn ceremony_reconnected(
        &self,
        endpoint: DeviceEndpoint,
        target: Option<DeviceId>,
    ) -> Result<IdentityCeremonyProgress, ManagerError> {
        let state = self.store.load_async().await?;
        let Some(journal) = state.identity_setup.as_ref() else {
            return Err(ManagerError::NoIdentityCeremony);
        };
        let phase = journal.phase;
        if journal.stage != IdentitySetupStage::AwaitingCapture {
            return Err(ManagerError::InvalidCeremonyAction {
                action: IdentityCeremonyAction::Reconnected,
                phase,
                stage: journal.stage,
            });
        }
        if journal.phase != IdentitySetupPhase::InitialEnrollment && !journal.subjects.is_empty() {
            return Err(ManagerError::InvalidCeremonyAction {
                action: IdentityCeremonyAction::Reconnected,
                phase,
                stage: journal.stage,
            });
        }
        if journal.phase == IdentitySetupPhase::InitialEnrollment && journal.subjects.len() > 1 {
            return Err(ManagerError::InvalidCeremonyAction {
                action: IdentityCeremonyAction::Reconnected,
                phase,
                stage: journal.stage,
            });
        }
        if !is_usb_transport(endpoint.transport) {
            return Err(ManagerError::IdentityCeremonyRefused {
                phase,
                reason: "requires-usb",
                detail: "physical enrollment and watermark verification require a wired or receiver USB connection"
                    .to_owned(),
            });
        }

        let session = self.factory.open(&endpoint).await?;
        let (_, decode) = self.current_watermark(session.as_ref()).await?;

        // Eligibility: intact known/foreign/reserved/unsupported markers
        // redirect or refuse; they are never overwritten.
        let mut adopted_token: Option<PhysicalId> = None;
        match decode {
            WatermarkDecode::Valid(token) => {
                if is_reserved_token(token) {
                    return Err(ManagerError::ReservedPhysicalId { physical_id: token });
                }
                let known = state
                    .devices
                    .values()
                    .any(|device| device.identity.physical_id == Some(token));
                match phase {
                    IdentitySetupPhase::ForeignAdoption => {
                        if known {
                            return Err(ManagerError::IdentityCeremonyRefused {
                                phase,
                                reason: "already-registered",
                                detail: format!(
                                    "token {token:?} already identifies a logical mouse here"
                                ),
                            });
                        }
                        if journal_reserves_token(journal, token) {
                            return Err(ManagerError::IdentityCeremonyRefused {
                                phase,
                                reason: "reserved-by-journal",
                                detail: "the presented token is reserved by an in-flight ceremony"
                                    .to_owned(),
                            });
                        }
                        adopted_token = Some(token);
                    }
                    IdentitySetupPhase::InitialEnrollment => {
                        // Enrollment stamps fresh tokens; an already-tagged
                        // mouse cannot be enrolled without overwriting its
                        // marker.
                        return Err(ManagerError::IntactIdentityMarker {
                            endpoint: Box::new(endpoint.clone()),
                            detail: format!(
                                "the presented mouse carries a valid watermark {token:?}; enrollment would overwrite it — adopt or restore instead"
                            ),
                        });
                    }
                    IdentitySetupPhase::AddMouse | IdentitySetupPhase::Restore => {
                        return Err(ManagerError::IdentityCeremonyRefused {
                            phase,
                            reason: if known {
                                "already-registered"
                            } else {
                                "adopt-instead"
                            },
                            detail: format!(
                                "the presented mouse carries a valid watermark {token:?}; a marked device is never re-stamped"
                            ),
                        });
                    }
                    IdentitySetupPhase::BleAssociation => {
                        unreachable!("BLE association never reconnects a USB mouse")
                    }
                }
            }
            WatermarkDecode::UnsupportedVersion { version } => {
                return Err(ManagerError::IntactIdentityMarker {
                    endpoint: Box::new(endpoint.clone()),
                    detail: format!(
                        "unsupported future watermark version {version}; upgrade required and the marker must not be overwritten"
                    ),
                });
            }
            WatermarkDecode::Absent | WatermarkDecode::Malformed => {}
        }

        // The restore target is re-validated against current state.
        let (device_id, old_token) = match phase {
            IdentitySetupPhase::Restore => {
                let target_id =
                    target
                        .as_ref()
                        .ok_or_else(|| ManagerError::IdentityCeremonyRefused {
                            phase,
                            reason: "requires-target",
                            detail: "restore requires the logical mouse being restored".to_owned(),
                        })?;
                let device = state
                    .devices
                    .get(target_id)
                    .ok_or_else(|| ManagerError::DeviceNotFound(target_id.clone()))?;
                let Some(old) = device.identity.physical_id else {
                    return Err(ManagerError::IdentityCeremonyRefused {
                        phase,
                        reason: "requires-physical-id",
                        detail: format!("{target_id} has no physical identity to restore"),
                    });
                };
                (Some(target_id.clone()), Some(old))
            }
            _ => (None, None),
        };

        // Capture the complete live state of the presented mouse, then turn
        // it into the durable evidence form embedded in the journal.
        let capture = refresh::capture_all_profiles(session.as_ref()).await?;
        let now = self.now();
        let evidence = capture.into_evidence(now);

        // Per-profile stamp plan. Adoption never replaces a valid foreign
        // token: profiles already carrying the adopted token need no write;
        // unmarked profiles are stamped later. Different valid tokens across
        // profiles mean damaged identity state and are refused.
        let mut stamp_progress: BTreeMap<ProfileId, IdentityStampProgress> = BTreeMap::new();
        if phase == IdentitySetupPhase::ForeignAdoption {
            let adopted = adopted_token.expect("validated: adoption requires a valid token");
            for (&profile, profile_state) in &evidence.profiles {
                let tail = profile_state
                    .dpi
                    .observed
                    .as_ref()
                    .map(|observation| &observation.value.preserved_tail);
                match tail.map(|tail| decode_watermark(tail)) {
                    Some(WatermarkDecode::Valid(token)) if token == adopted => {
                        stamp_progress.insert(profile, IdentityStampProgress::Stamped);
                    }
                    Some(WatermarkDecode::Absent) | None => {
                        stamp_progress.insert(profile, IdentityStampProgress::Captured);
                    }
                    other => {
                        return Err(ManagerError::IdentityCeremonyRefused {
                            phase,
                            reason: "token-disagreement",
                            detail: format!(
                                "profile {profile:?} carries {other:?}; profiles disagree on the physical token — treat as damaged identity and re-enroll"
                            ),
                        });
                    }
                }
            }
        } else {
            for profile in evidence.profiles.keys() {
                stamp_progress.insert(*profile, IdentityStampProgress::Captured);
            }
        }

        let subject = IdentitySetupSubject {
            endpoint: endpoint.clone(),
            device_id,
            token: adopted_token,
            old_token,
            captured: Some(evidence),
            stamp_progress,
        };
        let all_valid = phase == IdentitySetupPhase::ForeignAdoption && subject.is_fully_stamped();
        let completed_progress = all_valid.then(|| {
            let mut progress = ceremony_progress_from_journal(&IdentitySetupJournal {
                phase,
                stage: IdentitySetupStage::Finalizing,
                subjects: vec![subject.clone()],
            });
            progress.stage = IdentityCeremonyStage::Complete;
            progress
        });
        let inner: Result<(), ManagerError> = self
            .store
            .mutate_async(move |state| {
                let Some(current) = state.identity_setup.as_mut() else {
                    return Err(ManagerError::NoIdentityCeremony);
                };
                if current.phase != phase || current.stage != IdentitySetupStage::AwaitingCapture {
                    return Err(ManagerError::InvalidCeremonyAction {
                        action: IdentityCeremonyAction::Reconnected,
                        phase: current.phase,
                        stage: current.stage,
                    });
                }
                current.subjects.push(subject);
                if all_valid {
                    // Every profile already carries the adopted token: the
                    // adoption completes without any hardware write.
                    current.stage = IdentitySetupStage::Finalizing;
                    finalize_ceremony(state, false)?;
                } else {
                    current.stage = IdentitySetupStage::Stamping;
                }
                Ok(())
            })
            .await?;
        inner?;
        if let Some(progress) = completed_progress {
            return Ok(progress);
        }
        let state = self.store.load_async().await?;
        Ok(ceremony_progress_from_journal(
            state.identity_setup.as_ref().expect("journal persisted"),
        ))
    }

    /// Stamps the in-flight subject's profiles with its reserved token,
    /// persisting per-profile journal progress between hardware writes, and
    /// advances the ceremony (awaits the second mouse during initial
    /// enrollment, otherwise finalizes). Resume-safe: profiles already
    /// marked `Stamped` are never rewritten.
    async fn ceremony_stamp(&self) -> Result<IdentityCeremonyProgress, ManagerError> {
        let state = self.store.load_async().await?;
        let Some(journal) = state.identity_setup.as_ref() else {
            return Err(ManagerError::NoIdentityCeremony);
        };
        if journal.stage != IdentitySetupStage::Stamping {
            return Err(ManagerError::InvalidCeremonyAction {
                action: IdentityCeremonyAction::Stamp,
                phase: journal.phase,
                stage: journal.stage,
            });
        }
        let phase = journal.phase;
        if phase == IdentitySetupPhase::BleAssociation {
            return Err(ManagerError::InvalidCeremonyAction {
                action: IdentityCeremonyAction::Stamp,
                phase,
                stage: journal.stage,
            });
        }
        if journal.subjects.is_empty() {
            return Err(ManagerError::InvalidCeremonyAction {
                action: IdentityCeremonyAction::Stamp,
                phase,
                stage: journal.stage,
            });
        }

        // Mint and persist the fresh token before any watermark write. The
        // journal reservation makes the token unadoptable and survives
        // interruption. Adoption keeps the observed foreign token.
        if journal
            .subjects
            .last()
            .is_some_and(|subject| subject.token.is_none())
        {
            let token = self.mint_unique_token(
                &state,
                phase,
                journal.subjects.last().and_then(|s| s.old_token),
            )?;
            let token_copy = token;
            let inner: Result<(), ManagerError> =
                self.store
                    .mutate_async(move |current_state| {
                        let Some(current) = current_state.identity_setup.as_mut() else {
                            return Err(ManagerError::NoIdentityCeremony);
                        };
                        let subject = current.subjects.last_mut().ok_or(
                            ManagerError::InvalidCeremonyAction {
                                action: IdentityCeremonyAction::Stamp,
                                phase: current.phase,
                                stage: current.stage,
                            },
                        )?;
                        subject.token = Some(token_copy);
                        Ok(())
                    })
                    .await?;
            inner?;
        }

        let state = self.store.load_async().await?;
        let Some(journal) = state.identity_setup.as_ref() else {
            return Err(ManagerError::NoIdentityCeremony);
        };
        let subject = journal
            .subjects
            .last()
            .expect("validated: a subject exists");
        let token = subject.token.expect("persisted above");
        let captured = subject
            .captured
            .as_ref()
            .expect("validated: stamping requires a captured image");
        let original_metadata = captured
            .profile_metadata
            .observed
            .as_ref()
            .map(|observation| observation.value);
        let endpoint = subject.endpoint.clone();
        let journal_snapshot = journal.clone();
        let await_second =
            phase == IdentitySetupPhase::InitialEnrollment && journal.subjects.len() == 1;

        // Stamp every profile that is not yet marked `Stamped`, persisting
        // progress between writes so an interruption resumes exactly where
        // the ceremony stopped.
        let pending: Vec<ProfileId> = captured
            .profiles
            .iter()
            .filter(|(profile, _)| {
                subject.stamp_progress.get(profile) != Some(&IdentityStampProgress::Stamped)
            })
            .map(|(profile, _)| *profile)
            .collect();

        let session = self.factory.open(&endpoint).await?;
        let session_ref: &dyn DeviceSession = session.as_ref();

        for (index, profile) in pending.iter().enumerate() {
            let last = index + 1 == pending.len();
            let dpi = captured
                .profiles
                .get(profile)
                .and_then(|profile_state| profile_state.dpi.observed.as_ref())
                .map(|observation| observation.value.clone())
                .ok_or(ManagerError::VerificationMismatch {
                    resource: "physical identity watermark capture",
                    profile: Some(*profile),
                })?;
            self.stamp_one_profile(session_ref, *profile, token, &dpi)
                .await?;
            if !last {
                let profile_copy = *profile;
                let inner: Result<(), ManagerError> = self
                    .store
                    .mutate_async(move |current_state| {
                        let Some(current) = current_state.identity_setup.as_mut() else {
                            return Err(ManagerError::NoIdentityCeremony);
                        };
                        let subject = current.subjects.last_mut().ok_or(
                            ManagerError::InvalidCeremonyAction {
                                action: IdentityCeremonyAction::Stamp,
                                phase: current.phase,
                                stage: current.stage,
                            },
                        )?;
                        subject
                            .stamp_progress
                            .insert(profile_copy, IdentityStampProgress::Stamped);
                        Ok(())
                    })
                    .await?;
                inner?;
            }
        }

        // Restore the original profile metadata exactly after stamping
        // (best-effort; expansion during the ceremony is acceptable but the
        // device should not be left expanded or switched).
        if let Some(original) = original_metadata
            && let Err(restore) =
                crate::verification::write_exact_metadata(session_ref, original).await
        {
            return Err(ManagerError::RefreshRestorationFailed {
                restore: restore.to_string(),
            });
        }

        // One atomic transition: mark the last pending profile stamped (a
        // no-op on a clean resume) and advance — await the second mouse,
        // pause first-time setup for the migration decision, or finalize the
        // ceremony into durable devices.
        let pending_last = pending.last().copied();
        let transition: Result<(), ManagerError> =
            self.store
                .mutate_async(move |current_state| {
                    {
                        let Some(current) = current_state.identity_setup.as_mut() else {
                            return Err(ManagerError::NoIdentityCeremony);
                        };
                        let subject = current.subjects.last_mut().ok_or(
                            ManagerError::InvalidCeremonyAction {
                                action: IdentityCeremonyAction::Stamp,
                                phase: current.phase,
                                stage: current.stage,
                            },
                        )?;
                        if let Some(pending_last) = pending_last {
                            subject
                                .stamp_progress
                                .insert(pending_last, IdentityStampProgress::Stamped);
                        }
                        if await_second {
                            current.stage = IdentitySetupStage::AwaitingCapture;
                        } else if phase == IdentitySetupPhase::InitialEnrollment {
                            // Both mice are stamped; first-time setup parks at
                            // `Finalizing` (a durable pending-choice state) so
                            // the legacy-migration decision
                            // (`accept-migration` / `skip-migration`) finishes
                            // the enrollment as its own step. The choice is
                            // applied in the same call that finalizes, never
                            // recorded in process-local state.
                            current.stage = IdentitySetupStage::Finalizing;
                        } else {
                            current.stage = IdentitySetupStage::Finalizing;
                            finalize_ceremony(current_state, false)?;
                        }
                    }
                    Ok(())
                })
                .await?;
        transition?;

        let state = self.store.load_async().await?;
        match state.identity_setup.as_ref() {
            Some(journal) => Ok(ceremony_progress_from_journal(journal)),
            None => {
                let mut progress = ceremony_progress_from_journal(&journal_snapshot);
                progress.stage = IdentityCeremonyStage::Complete;
                Ok(progress)
            }
        }
    }

    /// Explicitly confirms a foreign-token adoption and completes it.
    ///
    /// The observed foreign token is already reserved on the journal subject;
    /// adoption stamps only profiles that do not yet carry it and finalizes.
    /// This is the explicit confirmation action required before any adoption
    /// watermark write.
    async fn ceremony_adopt(&self) -> Result<IdentityCeremonyProgress, ManagerError> {
        let state = self.store.load_async().await?;
        let Some(journal) = state.identity_setup.as_ref() else {
            return Err(ManagerError::NoIdentityCeremony);
        };
        if journal.phase != IdentitySetupPhase::ForeignAdoption {
            return Err(ManagerError::InvalidCeremonyAction {
                action: IdentityCeremonyAction::Adopt,
                phase: journal.phase,
                stage: journal.stage,
            });
        }
        self.ceremony_stamp().await
    }

    /// Applies the final legacy-migration decision of first-time setup.
    ///
    /// The decision is the enrollment's last step: when both mice are captured
    /// and fully stamped, the choice finalizes the ceremony and clears the
    /// journal in the same call. Any other stage is refused with the ceremony
    /// error model — the choice is never recorded in process-local state for a
    /// later step, so it survives only by completing the work it governs.
    async fn ceremony_finalize_migration(
        &self,
        action: IdentityCeremonyAction,
    ) -> Result<IdentityCeremonyProgress, ManagerError> {
        let state = self.store.load_async().await?;
        let Some(journal) = state.identity_setup.as_ref() else {
            return Err(ManagerError::NoIdentityCeremony);
        };
        if journal.phase != IdentitySetupPhase::InitialEnrollment
            || journal.stage != IdentitySetupStage::Finalizing
            || journal.subjects.len() != 2
            || !journal
                .subjects
                .iter()
                .all(|subject| subject.is_fully_stamped())
        {
            return Err(ManagerError::InvalidCeremonyAction {
                action,
                phase: journal.phase,
                stage: journal.stage,
            });
        }
        let skip = matches!(action, IdentityCeremonyAction::SkipMigration);
        let snapshot = journal.clone();
        let inner: Result<(), ManagerError> = self
            .store
            .mutate_async(move |state| {
                finalize_ceremony(state, skip)?;
                Ok(())
            })
            .await?;
        inner?;
        let mut progress = ceremony_progress_from_journal(&snapshot);
        progress.stage = IdentityCeremonyStage::Complete;
        Ok(progress)
    }

    /// Associates a presented BLE endpoint with the target logical mouse.
    ///
    /// BLE can never establish or verify a watermark; the platform id is
    /// cached as the locator for an explicitly associated connection only.
    /// The association is one atomic transaction with no hardware writes.
    async fn ceremony_associate(
        &self,
        endpoint: DeviceEndpoint,
        target: Option<DeviceId>,
    ) -> Result<IdentityCeremonyProgress, ManagerError> {
        if endpoint.transport != TransportKind::Ble {
            return Err(ManagerError::IdentityCeremonyRefused {
                phase: IdentitySetupPhase::BleAssociation,
                reason: "requires-ble",
                detail: "BLE association requires a BLE endpoint".to_owned(),
            });
        }
        let device_id = target.ok_or_else(|| ManagerError::IdentityCeremonyRefused {
            phase: IdentitySetupPhase::BleAssociation,
            reason: "requires-target",
            detail: "BLE association requires the logical mouse to associate".to_owned(),
        })?;
        let inner: Result<IdentityCeremonyProgress, ManagerError> = self
            .store
            .mutate_async(move |state| {
                let progress = {
                    let current = state
                        .identity_setup
                        .as_mut()
                        .ok_or(ManagerError::NoIdentityCeremony)?;
                    if current.phase != IdentitySetupPhase::BleAssociation
                        || current.stage != IdentitySetupStage::AwaitingCapture
                        || !current.subjects.is_empty()
                    {
                        return Err(ManagerError::InvalidCeremonyAction {
                            action: IdentityCeremonyAction::Associate,
                            phase: current.phase,
                            stage: current.stage,
                        });
                    }
                    // The endpoint must not already belong to another logical
                    // mouse.
                    for (other_id, other) in &state.devices {
                        if other_id == &device_id {
                            continue;
                        }
                        if other
                            .identity
                            .endpoints
                            .values()
                            .any(|ep| ep.locator == endpoint.locator)
                        {
                            return Err(ManagerError::IdentityCeremonyRefused {
                                phase: IdentitySetupPhase::BleAssociation,
                                reason: "endpoint-taken",
                                detail: format!("BLE endpoint already belongs to {other_id}"),
                            });
                        }
                    }
                    IdentityCeremonyProgress {
                        kind: IdentityCeremonyKind::BleAssociation,
                        stage: IdentityCeremonyStage::Complete,
                        step: Some(1),
                        total_steps: Some(1),
                        identity: Some(device_id.clone()),
                        physical_id: None,
                        endpoint: Some(endpoint.clone()),
                    }
                };
                {
                    let current = state.identity_setup.as_mut().expect("rechecked above");
                    current.subjects.push(IdentitySetupSubject {
                        endpoint: endpoint.clone(),
                        device_id: Some(device_id.clone()),
                        token: None,
                        old_token: None,
                        captured: None,
                        stamp_progress: BTreeMap::new(),
                    });
                    current.stage = IdentitySetupStage::Finalizing;
                }
                finalize_ceremony(state, false)?;
                Ok(progress)
            })
            .await?;
        inner
    }

    /// Cancels the in-flight ceremony.
    ///
    /// Only accepted before any profile has been stamped: once a watermark
    /// write has been confirmed the physical token is irreversible and the
    /// ceremony must be completed or resumed instead. Cancelling clears the
    /// journal; no tombstone is ever written.
    async fn ceremony_cancel(&self) -> Result<IdentityCeremonyProgress, ManagerError> {
        let state = self.store.load_async().await?;
        let Some(journal) = state.identity_setup.as_ref() else {
            return Err(ManagerError::NoIdentityCeremony);
        };
        let irreversible = journal.subjects.iter().any(|subject| {
            subject
                .stamp_progress
                .values()
                .any(|progress| matches!(progress, IdentityStampProgress::Stamped))
        });
        if irreversible {
            return Err(ManagerError::IdentityCeremonyRefused {
                phase: journal.phase,
                reason: "stamps-irreversible",
                detail: "profiles have already been stamped; complete or resume the ceremony instead of cancelling"
                    .to_owned(),
            });
        }
        let mut progress = ceremony_progress_from_journal(journal);
        progress.stage = IdentityCeremonyStage::Cancelled;
        let cancelled = progress.clone();
        self.store
            .mutate_async(move |state| {
                state.identity_setup = None;
            })
            .await?;
        Ok(cancelled)
    }

    /// Mints a fresh random physical token that is not reserved and collides
    /// with nothing in the current state or journal.
    fn mint_unique_token(
        &self,
        state: &StateFile,
        phase: IdentitySetupPhase,
        old_token: Option<PhysicalId>,
    ) -> Result<PhysicalId, ManagerError> {
        for _ in 0..16 {
            let mut bytes = [0_u8; 16];
            getrandom::fill(&mut bytes)
                .map_err(|source| ManagerError::TokenGeneration { source })?;
            let token = PhysicalId::from_token_bytes(bytes);
            if is_reserved_token(token) {
                continue;
            }
            if state
                .devices
                .values()
                .any(|device| device.identity.physical_id == Some(token))
            {
                continue;
            }
            if state
                .identity_setup
                .as_ref()
                .is_some_and(|journal| journal_reserves_token(journal, token))
            {
                continue;
            }
            if old_token == Some(token) {
                continue;
            }
            return Ok(token);
        }
        Err(ManagerError::IdentityCeremonyRefused {
            phase,
            reason: "token-generation",
            detail: "could not mint a unique physical identity token".to_owned(),
        })
    }

    /// Writes one profile's captured DPI state overlaid with the reserved
    /// token and confirms the watermark by readback.
    async fn stamp_one_profile(
        &self,
        session: &dyn DeviceSession,
        profile: ProfileId,
        token: PhysicalId,
        dpi: &DpiState,
    ) -> Result<(), ManagerError> {
        let maximum = session
            .read_profile_metadata()
            .await?
            .maximum()
            .max(profile);
        let metadata =
            ProfileMetadata::new(profile, maximum).map_err(|source| ManagerError::Protocol {
                operation: "profile metadata",
                source,
            })?;
        crate::verification::write_exact_metadata(session, metadata).await?;
        let desired = crate::resources::with_physical_watermark(dpi.clone(), Some(token));
        let write = session
            .write_dpi(desired, VerificationMethod::Readback)
            .await?;
        let verified = match write {
            SessionWrite::ReadbackVerified(actual) => {
                actual.physical_id() == WatermarkDecode::Valid(token)
            }
            SessionWrite::Acknowledged => {
                session.read_dpi(profile).await?.physical_id() == WatermarkDecode::Valid(token)
            }
        };
        if !verified {
            return Err(ManagerError::VerificationMismatch {
                resource: "physical identity watermark",
                profile: Some(profile),
            });
        }
        Ok(())
    }
}

fn device_has_evidence(state: &DeviceState) -> bool {
    if state.profile_metadata.desired.is_some() || state.profile_metadata.observed.is_some() {
        return true;
    }
    for profile_state in state.profiles.values() {
        if profile_state.dpi.desired.is_some() || profile_state.dpi.observed.is_some() {
            return true;
        }
        if profile_state.preferences.desired.is_some()
            || profile_state.preferences.observed.is_some()
        {
            return true;
        }
        if profile_state.buttons.desired.is_some() || profile_state.buttons.observed.is_some() {
            return true;
        }
        if profile_state.polling_rate.desired.is_some()
            || profile_state.polling_rate.observed.is_some()
        {
            return true;
        }
    }
    false
}

fn is_usb_transport(transport: TransportKind) -> bool {
    matches!(transport, TransportKind::Wired | TransportKind::Receiver)
}
#[cfg(feature = "usb")]
fn is_supported_usb_device(device: &nusb::DeviceInfo) -> bool {
    device.vendor_id() == 0x1d57 && matches!(device.product_id(), 0xfa60 | 0xfa61)
}

/// Resolves one discovered connection against the durable logical mice.
///
/// In legacy mode every compatible connection resolves to the single fuzzy
/// logical mouse (no identity claim is made). In persistent mode a valid
/// known token resolves to its logical mouse; every other case is an explicit
/// non-association with a reason.
fn resolve_connection(
    devices: &BTreeMap<DeviceId, DeviceState>,
    journal: Option<&IdentitySetupJournal>,
    legacy_id: Option<&DeviceId>,
    endpoint: &DeviceEndpoint,
    decode: Option<WatermarkDecode>,
) -> IdentityResolution {
    if let Some(legacy_id) = legacy_id {
        return IdentityResolution::Resolved {
            identity: legacy_id.clone(),
        };
    }
    match endpoint.transport {
        TransportKind::Ble => {
            // BLE cannot establish or verify a watermark; only an explicitly
            // associated platform id resolves to a logical mouse.
            let matched = devices.values().find(|device| {
                device
                    .identity
                    .endpoint(TransportKind::Ble)
                    .is_some_and(|ep| ep.locator == endpoint.locator)
            });
            match matched {
                Some(device) => IdentityResolution::Resolved {
                    identity: device.identity.id.clone(),
                },
                None => IdentityResolution::Unassociated {
                    reason: UnassociatedReason::Absent,
                    physical_id: None,
                },
            }
        }
        TransportKind::Wired | TransportKind::Receiver => match decode {
            Some(WatermarkDecode::Valid(token)) => resolve_valid_token(devices, journal, token),
            Some(WatermarkDecode::Absent) | None => IdentityResolution::Unassociated {
                reason: UnassociatedReason::Absent,
                physical_id: None,
            },
            Some(WatermarkDecode::Malformed) => IdentityResolution::Unassociated {
                reason: UnassociatedReason::Malformed,
                physical_id: None,
            },
            Some(WatermarkDecode::UnsupportedVersion { version }) => {
                IdentityResolution::Unassociated {
                    reason: UnassociatedReason::Unsupported { version },
                    physical_id: None,
                }
            }
        },
    }
}

/// Resolves a valid watermark token: known token to its exact logical mouse;
/// reserved-range, journal-reserved, unknown, and duplicate tokens stay
/// unassociated. Unknown beats incorrect identity: never guess.
fn resolve_valid_token(
    devices: &BTreeMap<DeviceId, DeviceState>,
    journal: Option<&IdentitySetupJournal>,
    token: PhysicalId,
) -> IdentityResolution {
    if is_reserved_token(token) {
        return IdentityResolution::Unassociated {
            reason: UnassociatedReason::Reserved,
            physical_id: Some(token),
        };
    }
    if journal.is_some_and(|journal| journal_reserves_token(journal, token)) {
        return IdentityResolution::Unassociated {
            reason: UnassociatedReason::ReservedByJournal,
            physical_id: Some(token),
        };
    }
    let mut owners: Vec<DeviceId> = devices
        .values()
        .filter(|device| device.identity.physical_id == Some(token))
        .map(|device| device.identity.id.clone())
        .collect();
    match owners.as_mut_slice() {
        [] => IdentityResolution::Unassociated {
            reason: UnassociatedReason::Unknown,
            physical_id: Some(token),
        },
        [id] => IdentityResolution::Resolved {
            identity: id.clone(),
        },
        _ => IdentityResolution::Unassociated {
            reason: UnassociatedReason::Duplicate,
            physical_id: Some(token),
        },
    }
}

/// Returns true when the active journal reserves `token` (a pending
/// enrollment, adoption, or restore rotation).
fn journal_reserves_token(journal: &IdentitySetupJournal, token: PhysicalId) -> bool {
    journal
        .subjects
        .iter()
        .any(|subject| subject.token == Some(token) || subject.old_token == Some(token))
}

/// Reserved tokens (all-zero) are never assignable to a device.
fn is_reserved_token(token: PhysicalId) -> bool {
    token.token_bytes() == [0_u8; 16]
}

/// Folds same-scan duplicate unknown tokens: when two independently
/// authenticated connections present the same valid-but-unknown token,
/// neither may be adopted automatically.
///
/// Folding is per transport: one physical mouse exposes at most one endpoint
/// per transport, so the same unknown token on two wired (or two receiver)
/// connections means two different physical mice carrying the same token and
/// automatic adoption of either must be refused. The same token across wired
/// + receiver is one physical mouse on two transports (see the endpoint
///   linking rule) and is never folded into a duplicate.
fn mark_scan_duplicates(resolved: &mut [ResolvedConnection]) {
    let mut counts: BTreeMap<(TransportKind, PhysicalId), usize> = BTreeMap::new();
    for row in resolved.iter() {
        if let IdentityResolution::Unassociated {
            reason: UnassociatedReason::Unknown,
            physical_id: Some(token),
        } = &row.resolution
        {
            *counts.entry((row.endpoint.transport, *token)).or_default() += 1;
        }
    }
    for row in resolved.iter_mut() {
        if let IdentityResolution::Unassociated {
            reason: UnassociatedReason::Unknown,
            physical_id: Some(token),
        } = &row.resolution
            && counts
                .get(&(row.endpoint.transport, *token))
                .copied()
                .unwrap_or(0)
                > 1
        {
            row.resolution = IdentityResolution::Unassociated {
                reason: UnassociatedReason::Duplicate,
                physical_id: Some(*token),
            };
        }
    }
}

/// Aggregates resolved connections into one row per logical mouse.
fn aggregate_discovered_devices(
    state: &StateFile,
    resolved: &[ResolvedConnection],
) -> Vec<DiscoveredDevice> {
    let mut result: BTreeMap<DeviceId, DiscoveredDevice> = BTreeMap::new();
    for row in resolved {
        let IdentityResolution::Resolved { identity } = &row.resolution else {
            continue;
        };
        let Some(device_state) = state.devices.get(identity) else {
            continue;
        };
        let entry = result
            .entry(identity.clone())
            .or_insert_with(|| DiscoveredDevice {
                identity: device_state.identity.clone(),
                connected: false,
                transports: Vec::new(),
            });
        entry.connected |= row.connected;
        if !entry.transports.contains(&row.endpoint.transport) {
            entry.transports.push(row.endpoint.transport);
        }
    }
    for device in result.values_mut() {
        device.transports.sort();
    }
    result.into_values().collect()
}

/// Returns true when ordinary operations on `device` must be blocked by the
/// active identity-setup journal: the involved logical mice may not receive
/// configuration writes while a ceremony is in flight.
fn journal_blocks_device(journal: &IdentitySetupJournal, device: &DeviceId) -> bool {
    if journal.phase == IdentitySetupPhase::InitialEnrollment {
        // The single legacy fuzzy mouse is involved in the transition.
        return true;
    }
    journal
        .subjects
        .iter()
        .any(|subject| subject.device_id.as_ref() == Some(device))
}

/// Maps the durable journal onto the frontend-facing ceremony progress.
fn ceremony_progress_from_journal(journal: &IdentitySetupJournal) -> IdentityCeremonyProgress {
    let kind = match journal.phase {
        IdentitySetupPhase::InitialEnrollment => IdentityCeremonyKind::InitialEnrollment,
        IdentitySetupPhase::AddMouse => IdentityCeremonyKind::AddMouse,
        IdentitySetupPhase::Restore => IdentityCeremonyKind::Restore,
        IdentitySetupPhase::ForeignAdoption => IdentityCeremonyKind::ForeignAdoption,
        IdentitySetupPhase::BleAssociation => IdentityCeremonyKind::BleAssociation,
    };
    let stage = match journal.stage {
        IdentitySetupStage::AwaitingCapture => IdentityCeremonyStage::AwaitingReconnect,
        IdentitySetupStage::Stamping => IdentityCeremonyStage::Stamping,
        IdentitySetupStage::Finalizing => IdentityCeremonyStage::Complete,
    };
    let (step, total_steps) = match journal.phase {
        IdentitySetupPhase::InitialEnrollment => {
            let step = match journal.stage {
                IdentitySetupStage::AwaitingCapture if journal.subjects.is_empty() => 1,
                IdentitySetupStage::AwaitingCapture => 2,
                IdentitySetupStage::Stamping => journal.subjects.len().max(1) as u8,
                IdentitySetupStage::Finalizing => 2,
            };
            (Some(step), Some(2_u8))
        }
        _ => (Some(1), Some(1)),
    };
    let subject = journal.subjects.last();
    IdentityCeremonyProgress {
        kind,
        stage,
        step,
        total_steps,
        identity: subject.and_then(|subject| subject.device_id.clone()),
        physical_id: subject.and_then(|subject| subject.token.or(subject.old_token)),
        endpoint: subject.map(|subject| subject.endpoint.clone()),
    }
}

/// Legacy name migration: the old single-mouse state matches a captured
/// subject only when the entire durable evidence agrees exactly. Matching is
/// deliberately not weighted; incomplete or ambiguous comparisons never
/// migrate a name.
fn legacy_capture_matches(legacy: &DeviceState, captured: &CapturedProfileImage) -> bool {
    legacy.profile_metadata == captured.profile_metadata && legacy.profiles == captured.profiles
}

/// Applies a durable captured profile image (the evidence form persisted on a
/// journal subject) to a fresh or restored logical device. This is the
/// finalize-time counterpart of `ProfileCapture::into_evidence`, which builds
/// the persisted image from the transient capture at enrollment time.
fn apply_captured_image(device_state: &mut DeviceState, captured: Option<&CapturedProfileImage>) {
    if let Some(captured) = captured {
        device_state.profile_metadata = captured.profile_metadata.clone();
        device_state.profiles = captured.profiles.clone();
    }
}

/// Applies a completed ceremony to durable state: creates fresh logical mice
/// (or rotates/updates the restore/BLE targets), switches identity mode for
/// initial enrollment, and clears the journal. Every fallible step runs
/// before any mutation, so an error never leaves a partially applied ceremony
/// behind; `commit` re-validates the result.
fn finalize_ceremony(state: &mut StateFile, skip_migration: bool) -> Result<(), ManagerError> {
    let Some(journal) = state.identity_setup.clone() else {
        return Err(ManagerError::NoIdentityCeremony);
    };
    match journal.phase {
        IdentitySetupPhase::InitialEnrollment => {
            finalize_initial_enrollment(state, &journal, skip_migration)?;
        }
        IdentitySetupPhase::AddMouse | IdentitySetupPhase::ForeignAdoption => {
            let subject =
                journal
                    .subjects
                    .first()
                    .ok_or_else(|| ManagerError::IdentityCeremonyRefused {
                        phase: journal.phase,
                        reason: "no-subject",
                        detail: "finalizing requires a captured subject".to_owned(),
                    })?;
            let token = subject
                .token
                .ok_or_else(|| ManagerError::IdentityCeremonyRefused {
                    phase: journal.phase,
                    reason: "no-token",
                    detail: "finalizing requires a reserved token".to_owned(),
                })?;
            // Fallible steps first: allocate the fresh logical id.
            let id = state.allocate_device_id()?;
            let mut identity = DeviceIdentity::new(id.clone(), None);
            identity.physical_id = Some(token);
            identity
                .endpoints
                .insert(subject.endpoint.transport, subject.endpoint.clone());
            let mut device_state = DeviceState::new(identity);
            apply_captured_image(&mut device_state, subject.captured.as_ref());
            state.devices.insert(id, device_state);
        }
        IdentitySetupPhase::Restore => {
            let subject =
                journal
                    .subjects
                    .first()
                    .ok_or_else(|| ManagerError::IdentityCeremonyRefused {
                        phase: journal.phase,
                        reason: "no-subject",
                        detail: "finalizing requires a captured subject".to_owned(),
                    })?;
            let device_id =
                subject
                    .device_id
                    .clone()
                    .ok_or_else(|| ManagerError::IdentityCeremonyRefused {
                        phase: journal.phase,
                        reason: "requires-target",
                        detail: "restore finalization requires the bound logical mouse".to_owned(),
                    })?;
            let token = subject
                .token
                .ok_or_else(|| ManagerError::IdentityCeremonyRefused {
                    phase: journal.phase,
                    reason: "no-token",
                    detail: "restore finalization requires the rotated-in token".to_owned(),
                })?;
            // Fallible steps first: the target must still exist.
            if !state.devices.contains_key(&device_id) {
                return Err(ManagerError::DeviceNotFound(device_id.clone()));
            }
            let device_state = state.devices.get_mut(&device_id).expect("checked above");
            // Rotate to the fresh token. The old token leaves the local token
            // index and becomes valid-but-unknown: there is no tombstone, so
            // a returning old-token mouse is offered confirmed adoption again.
            device_state.identity.physical_id = Some(token);
            device_state.identity.preferred_transport = None;
            // Drop every old endpoint association (old wired/receiver
            // locators and the cached BLE platform id); keep only the USB
            // endpoint authenticated by this reconnect.
            device_state.identity.endpoints =
                BTreeMap::from([(subject.endpoint.transport, subject.endpoint.clone())]);
            apply_captured_image(device_state, subject.captured.as_ref());
        }
        IdentitySetupPhase::BleAssociation => {
            let subject =
                journal
                    .subjects
                    .first()
                    .ok_or_else(|| ManagerError::IdentityCeremonyRefused {
                        phase: journal.phase,
                        reason: "no-subject",
                        detail: "finalizing requires a captured subject".to_owned(),
                    })?;
            let device_id =
                subject
                    .device_id
                    .clone()
                    .ok_or_else(|| ManagerError::IdentityCeremonyRefused {
                        phase: journal.phase,
                        reason: "requires-target",
                        detail: "BLE association finalization requires the bound logical mouse"
                            .to_owned(),
                    })?;
            if !state.devices.contains_key(&device_id) {
                return Err(ManagerError::DeviceNotFound(device_id.clone()));
            }
            let device_state = state.devices.get_mut(&device_id).expect("checked above");
            device_state
                .identity
                .endpoints
                .insert(subject.endpoint.transport, subject.endpoint.clone());
        }
    }
    state.identity_setup = None;
    Ok(())
}

/// Completes initial enrollment: removes the fuzzy legacy mouse, creates two
/// fresh physically identified logical mice from the captured subjects, and
/// switches the installation into persistent mode. Legacy name migration is
/// applied only on a unique exact match; otherwise fresh `Mouse N` names are
/// used and the user can rename afterwards.
fn finalize_initial_enrollment(
    state: &mut StateFile,
    journal: &IdentitySetupJournal,
    skip_migration: bool,
) -> Result<(), ManagerError> {
    let legacy = state.devices.values().next().cloned();
    let unique_match = if skip_migration {
        None
    } else {
        legacy.as_ref().and_then(|legacy| {
            let matches: Vec<usize> = journal
                .subjects
                .iter()
                .enumerate()
                .filter_map(|(index, subject)| {
                    subject
                        .captured
                        .as_ref()
                        .filter(|captured| legacy_capture_matches(legacy, captured))
                        .map(|_| index)
                })
                .collect();
            match matches.as_slice() {
                [index] => Some(*index),
                _ => None,
            }
        })
    };
    // Fallible steps first: allocate both logical ids before mutating.
    let ids: Vec<DeviceId> = (0..journal.subjects.len())
        .map(|_| state.allocate_device_id())
        .collect::<Result<_, _>>()?;
    state.devices.clear();
    let mut first_id: Option<DeviceId> = None;
    for ((index, subject), id) in journal.subjects.iter().enumerate().zip(ids) {
        if first_id.is_none() {
            first_id = Some(id.clone());
        }
        let display_name = if unique_match == Some(index) {
            legacy
                .as_ref()
                .and_then(|legacy| legacy.identity.display_name.clone())
        } else {
            Some(format!("Mouse {}", index + 1))
        };
        let mut identity = DeviceIdentity::new(id.clone(), display_name);
        identity.physical_id = subject.token;
        identity
            .endpoints
            .insert(subject.endpoint.transport, subject.endpoint.clone());
        let mut device_state = DeviceState::new(identity);
        apply_captured_image(&mut device_state, subject.captured.as_ref());
        state.devices.insert(id, device_state);
    }
    state.identity_mode = IdentityMode::Persistent;
    state.selected_device = first_id;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::DeviceManager;
    use crate::backend::{
        DeviceSession, ScriptedFakeFactory, ScriptedFakeSession, ScriptedWrite, SessionWrite,
    };
    use crate::device::{
        DeviceEndpoint, DeviceId, DeviceIdentity, DeviceLocator, TransportSelection,
    };
    use crate::error::ManagerError;
    use crate::operation::{
        DiscoveredEndpoint, IdentityCeremonyAction, IdentityCeremonyKind, IdentityCeremonyStage,
        IdentityResolution, UnassociatedReason,
    };
    use crate::state::{
        ApplicationVerification, DesiredSource, DesiredState, DeviceState, IdentityMode,
        IdentitySetupStage, IdentityStampProgress, StatePaths, StateStore, Verification,
    };
    use attack_shark_x3::{
        ButtonAssignment, ButtonsState, DpiState, DpiValue, PhysicalId, PollingRate,
        PreferencesState, ProfileId, ProfileMetadata, StageIndex, TransportKind,
    };
    use std::sync::Arc;
    use std::time::Duration;

    fn test_store(dir: &tempfile::TempDir) -> StateStore {
        StateStore::open(StatePaths::new(dir.path().join("state.json")))
    }

    fn wired_endpoint(path: &str) -> DeviceEndpoint {
        DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa61,
            Some("SN001"),
            path,
            Some("Test Mouse"),
        )
        .unwrap()
    }
    fn wired_endpoint_named(path: &str, serial: &str) -> DeviceEndpoint {
        DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa61,
            Some(serial),
            path,
            Some(serial),
        )
        .unwrap()
    }
    fn receiver_endpoint(path: &str) -> DeviceEndpoint {
        DeviceEndpoint::usb(TransportKind::Receiver, 0x1d57, 0xfa60, None, path, None).unwrap()
    }
    fn ble_endpoint(id: &str) -> DeviceEndpoint {
        DeviceEndpoint::ble(id, Some("BLE")).unwrap()
    }

    fn make_discovered(endpoint: DeviceEndpoint, connected: bool) -> DiscoveredEndpoint {
        DiscoveredEndpoint {
            endpoint,
            connected,
        }
    }

    fn insert_device_with_endpoint(store: &StateStore, endpoint: DeviceEndpoint) -> DeviceId {
        let mut txn = store.transaction().unwrap();
        let id = txn.state_mut().allocate_device_id().unwrap();
        let mut identity = DeviceIdentity::new(id.clone(), endpoint.display_name.clone());
        identity.upsert_endpoint(endpoint);
        txn.state_mut()
            .devices
            .insert(id.clone(), crate::state::DeviceState::new(identity));
        if txn.state().selected_device.is_none() {
            txn.state_mut().selected_device = Some(id.clone());
        }
        txn.commit().unwrap();
        id
    }

    /// Inserts a physically identified logical mouse and switches the
    /// installation into persistent identity mode.
    fn insert_persistent_device(
        store: &StateStore,
        endpoint: DeviceEndpoint,
        token: PhysicalId,
    ) -> DeviceId {
        let mut txn = store.transaction().unwrap();
        let id = txn.state_mut().allocate_device_id().unwrap();
        let mut identity = DeviceIdentity::new(id.clone(), endpoint.display_name.clone());
        identity.physical_id = Some(token);
        identity.upsert_endpoint(endpoint);
        txn.state_mut()
            .devices
            .insert(id.clone(), crate::state::DeviceState::new(identity));
        txn.state_mut().identity_mode = IdentityMode::Persistent;
        if txn.state().selected_device.is_none() {
            txn.state_mut().selected_device = Some(id.clone());
        }
        txn.commit().unwrap();
        id
    }

    /// Builds a full profile snapshot whose DPI carries `tail` verbatim.
    fn snapshot_with_tail(
        profile: ProfileId,
        tail: [u8; 25],
    ) -> attack_shark_x3::driver::ProfileSnapshot {
        attack_shark_x3::driver::ProfileSnapshot {
            target_profile: profile,
            persistent_metadata: ProfileMetadata::new(profile, ProfileId::new(5).unwrap()).unwrap(),
            dpi: DpiState::new(
                profile,
                vec![DpiValue::new(800).unwrap()],
                StageIndex::new(1).unwrap(),
                tail,
            )
            .unwrap(),
            preferences: PreferencesState::new(profile, 0, 0, 0, [0, 0, 0], 0, 0),
            buttons: ButtonsState::new(
                profile,
                [ButtonAssignment::default();
                    attack_shark_x3::protocol::buttons::BUTTON_SLOT_COUNT],
            ),
        }
    }

    /// A scripted USB session whose every profile (1..=5) is capturable and
    /// whose DPI tails carry `tail` — so the current-profile watermark
    /// decodes from it.
    fn session_with_tail(tail: [u8; 25]) -> ScriptedFakeSession {
        let profile = ProfileId::new(1).unwrap();
        let mut session = ScriptedFakeSession::usb()
            .with_metadata(ProfileMetadata::new(profile, ProfileId::new(5).unwrap()).unwrap())
            .with_polling_rate(PollingRate::Hz1000);
        for target in ProfileId::MIN..=ProfileId::MAX {
            let target = ProfileId::new(target).expect("test profile must be valid");
            session = session.with_profile(snapshot_with_tail(target, tail));
        }
        session
    }

    /// A scripted session for an unmarked physical mouse (no watermark).
    fn unmarked_session() -> ScriptedFakeSession {
        session_with_tail([0_u8; 25])
    }

    /// A scripted session whose every profile carries `token`'s watermark.
    fn session_with_watermark(token: PhysicalId) -> ScriptedFakeSession {
        session_with_tail(token.to_watermark_bytes())
    }

    struct MismatchWrapper {
        inner: ScriptedFakeSession,
        mismatched: ProfileMetadata,
    }
    #[async_trait::async_trait(?Send)]
    impl DeviceSession for MismatchWrapper {
        fn transport(&self) -> TransportKind {
            self.inner.transport()
        }
        async fn read_profile_metadata(&self) -> Result<ProfileMetadata, ManagerError> {
            self.inner.read_profile_metadata().await
        }
        async fn read_profile(
            &self,
            profile: ProfileId,
        ) -> Result<attack_shark_x3::driver::ProfileSnapshot, ManagerError> {
            self.inner.read_profile(profile).await
        }
        async fn read_dpi(&self, profile: ProfileId) -> Result<DpiState, ManagerError> {
            self.inner.read_dpi(profile).await
        }
        async fn read_preferences(
            &self,
            profile: ProfileId,
        ) -> Result<PreferencesState, ManagerError> {
            self.inner.read_preferences(profile).await
        }
        async fn read_buttons(&self, profile: ProfileId) -> Result<ButtonsState, ManagerError> {
            self.inner.read_buttons(profile).await
        }
        async fn read_live_polling_rate(
            &self,
            alias: ProfileId,
        ) -> Result<PollingRate, ManagerError> {
            self.inner.read_live_polling_rate(alias).await
        }
        async fn write_dpi(
            &self,
            state: DpiState,
            verification: crate::operation::VerificationMethod,
        ) -> Result<SessionWrite<DpiState>, ManagerError> {
            self.inner.write_dpi(state, verification).await
        }
        async fn write_preferences(
            &self,
            state: PreferencesState,
            verification: crate::operation::VerificationMethod,
        ) -> Result<SessionWrite<PreferencesState>, ManagerError> {
            self.inner.write_preferences(state, verification).await
        }
        async fn write_buttons(
            &self,
            state: ButtonsState,
            verification: crate::operation::VerificationMethod,
        ) -> Result<SessionWrite<ButtonsState>, ManagerError> {
            self.inner.write_buttons(state, verification).await
        }
        async fn write_polling_rate_unchecked(
            &self,
            profile: ProfileId,
            rate: PollingRate,
            verification: crate::operation::VerificationMethod,
        ) -> Result<SessionWrite<PollingRate>, ManagerError> {
            self.inner
                .write_polling_rate_unchecked(profile, rate, verification)
                .await
        }
        async fn write_profile_metadata(
            &self,
            _metadata: ProfileMetadata,
        ) -> Result<SessionWrite<ProfileMetadata>, ManagerError> {
            Ok(SessionWrite::ReadbackVerified(self.mismatched))
        }
        async fn read_battery(&self, timeout: Duration) -> Result<u8, ManagerError> {
            self.inner.read_battery(timeout).await
        }
        fn subscribe_events(&self) -> crate::backend::SessionEvents {
            self.inner.subscribe_events()
        }
    }

    struct UnsupportedPollSession {
        metadata: ProfileMetadata,
        snapshot: attack_shark_x3::driver::ProfileSnapshot,
    }
    #[async_trait::async_trait(?Send)]
    impl DeviceSession for UnsupportedPollSession {
        fn transport(&self) -> TransportKind {
            TransportKind::Wired
        }
        async fn read_profile_metadata(&self) -> Result<ProfileMetadata, ManagerError> {
            Ok(self.metadata)
        }
        async fn read_profile(
            &self,
            _profile: ProfileId,
        ) -> Result<attack_shark_x3::driver::ProfileSnapshot, ManagerError> {
            Ok(self.snapshot.clone())
        }
        async fn read_dpi(&self, _p: ProfileId) -> Result<DpiState, ManagerError> {
            unreachable!()
        }
        async fn read_preferences(&self, _p: ProfileId) -> Result<PreferencesState, ManagerError> {
            unreachable!()
        }
        async fn read_buttons(&self, _p: ProfileId) -> Result<ButtonsState, ManagerError> {
            unreachable!()
        }
        async fn read_live_polling_rate(
            &self,
            _alias: ProfileId,
        ) -> Result<PollingRate, ManagerError> {
            Err(ManagerError::UnsupportedOperation {
                operation: "read_live_polling_rate",
                transport: TransportKind::Wired,
            })
        }
        async fn write_dpi(
            &self,
            _s: DpiState,
            _v: crate::operation::VerificationMethod,
        ) -> Result<SessionWrite<DpiState>, ManagerError> {
            unreachable!()
        }
        async fn write_preferences(
            &self,
            _s: PreferencesState,
            _v: crate::operation::VerificationMethod,
        ) -> Result<SessionWrite<PreferencesState>, ManagerError> {
            unreachable!()
        }
        async fn write_buttons(
            &self,
            _s: ButtonsState,
            _v: crate::operation::VerificationMethod,
        ) -> Result<SessionWrite<ButtonsState>, ManagerError> {
            unreachable!()
        }
        async fn write_polling_rate_unchecked(
            &self,
            _p: ProfileId,
            _r: PollingRate,
            _v: crate::operation::VerificationMethod,
        ) -> Result<SessionWrite<PollingRate>, ManagerError> {
            unreachable!()
        }
        async fn write_profile_metadata(
            &self,
            _m: ProfileMetadata,
        ) -> Result<SessionWrite<ProfileMetadata>, ManagerError> {
            unreachable!()
        }
        async fn read_battery(&self, _t: Duration) -> Result<u8, ManagerError> {
            Err(ManagerError::UnsupportedOperation {
                operation: "read_battery",
                transport: TransportKind::Wired,
            })
        }
        fn subscribe_events(&self) -> crate::backend::SessionEvents {
            crate::backend::SessionEvents { input: None }
        }
    }

    #[tokio::test]
    async fn resolve_device_honors_explicit_id_and_registers_discovery() {
        // Legacy mode: the pre-registered fuzzy mouse is the only logical
        // mouse, and an explicit request for it resolves. Discovery of a
        // second connection on another port registers it onto that same
        // mouse — no second logical device is ever allocated.
        let store = StateStore::memory();
        let ep_a = wired_endpoint_named(r"\\?\hid#explicit-a", "EXPLICIT-A");
        let ep_b = wired_endpoint_named(r"\\?\hid#explicit-b", "EXPLICIT-B");
        let id = insert_device_with_endpoint(&store, ep_a.clone());

        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(
                    make_discovered(ep_a.clone(), true),
                    ScriptedFakeSession::usb(),
                )
                .with_endpoint(
                    make_discovered(ep_b.clone(), true),
                    ScriptedFakeSession::usb(),
                ),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);

        let selected = manager
            .resolve_device(Some(&id), TransportSelection::Auto)
            .await
            .unwrap();

        assert_eq!(selected, id);
        let state = manager.store().load().unwrap();
        assert_eq!(
            state.devices.len(),
            1,
            "one fuzzy logical mouse owns every connection"
        );
        assert!(state.devices.contains_key(&id));
        assert_eq!(
            state.devices[&id]
                .identity
                .endpoint(TransportKind::Wired)
                .unwrap()
                .locator,
            DeviceLocator::UsbPath(r"\\?\hid#explicit-b".to_string()),
            "the discovered port change registers on the existing fuzzy mouse"
        );
    }

    #[tokio::test]
    async fn resolve_device_honors_connected_stored_selection() {
        // Legacy mode: the stored selection is the single fuzzy logical
        // mouse, and a connected scan of it resolves without ambiguity.
        let store = StateStore::memory();
        let ep = wired_endpoint_named(r"\\?\hid#stored-a", "STORED-A");
        let id = insert_device_with_endpoint(&store, ep.clone());
        {
            let mut txn = store.transaction().unwrap();
            txn.state_mut().selected_device = Some(id.clone());
            txn.commit().unwrap();
        }
        let factory = Arc::new(ScriptedFakeFactory::new().with_endpoint(
            make_discovered(ep.clone(), true),
            ScriptedFakeSession::usb(),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);

        assert_eq!(
            manager
                .resolve_device(None, TransportSelection::Auto)
                .await
                .unwrap(),
            id
        );
        assert_eq!(manager.store().load().unwrap().selected_device, Some(id));
    }

    #[tokio::test]
    async fn resolve_device_selects_the_sole_connected_candidate() {
        let store = StateStore::memory();
        let ep_connected = wired_endpoint_named(r"\\?\hid#sole-connected", "SOLE-CONNECTED");
        let ep_disconnected =
            wired_endpoint_named(r"\\?\hid#sole-disconnected", "SOLE-DISCONNECTED");
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(
                    make_discovered(ep_connected.clone(), true),
                    ScriptedFakeSession::usb(),
                )
                .with_endpoint(
                    make_discovered(ep_disconnected.clone(), false),
                    ScriptedFakeSession::usb(),
                ),
        );
        let manager = DeviceManager::with_store_and_factory(store, factory);

        let selected = manager
            .resolve_device(None, TransportSelection::Auto)
            .await
            .unwrap();
        let state = manager.store().load().unwrap();
        assert_eq!(manager.selected_device().unwrap(), Some(selected.clone()));
        assert!(state.devices.contains_key(&selected));
        let dev = state.devices.get(&selected).unwrap();
        assert!(dev.identity.has_endpoint(TransportKind::Wired));
    }

    #[tokio::test]
    async fn resolve_device_reports_ambiguity_without_guessing() {
        // Persistent mode: two known logical mice, both connected, none
        // selected — resolve_device must refuse to guess between them.
        let store = StateStore::memory();
        let token_a = PhysicalId::from_token_bytes([0xaa; 16]);
        let token_b = PhysicalId::from_token_bytes([0xbb; 16]);
        let ep_a = wired_endpoint_named(r"\\?\hid#ambig-a", "AMBIGUOUS-A");
        let ep_b = wired_endpoint_named(r"\\?\hid#ambig-b", "AMBIGUOUS-B");
        let id_a = insert_persistent_device(&store, ep_a.clone(), token_a);
        let id_b = insert_persistent_device(&store, ep_b.clone(), token_b);
        {
            let mut txn = store.transaction().unwrap();
            txn.state_mut().selected_device = None;
            txn.commit().unwrap();
        }
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(
                    make_discovered(ep_a.clone(), true),
                    session_with_watermark(token_a),
                )
                .with_endpoint(
                    make_discovered(ep_b.clone(), true),
                    session_with_watermark(token_b),
                ),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);

        let error = manager
            .resolve_device(None, TransportSelection::Auto)
            .await
            .expect_err("multiple connected logical mice must be explicit");
        match error {
            ManagerError::AmbiguousDevice {
                selection,
                candidates,
            } => {
                assert_eq!(selection, TransportSelection::Auto);
                assert_eq!(candidates.len(), 2);
                assert!(candidates.contains(&id_a));
                assert!(candidates.contains(&id_b));
            }
            other => panic!("expected AmbiguousDevice, got {other:?}"),
        }
        assert_eq!(manager.selected_device().unwrap(), None);
    }

    #[tokio::test]
    async fn resolve_device_reports_no_connected_device() {
        let ep = wired_endpoint_named(r"\\?\hid#none-disconnected", "NONE-DISCONNECTED");
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(ep, false), ScriptedFakeSession::usb()),
        );
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
        assert_eq!(manager.selected_device().unwrap(), None);
    }

    #[tokio::test]
    async fn register_device_inserts_and_selects_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let factory = Arc::new(ScriptedFakeFactory::new());
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);

        let endpoint = wired_endpoint(r"\\?\hid#test");
        let mut txn = store.transaction().unwrap();
        let id = txn.state_mut().allocate_device_id().unwrap();
        txn.commit().unwrap();
        let identity = DeviceIdentity::new(id.clone(), Some("Test".to_string()))
            .with_endpoint(endpoint.clone());

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

        let endpoint = wired_endpoint(r"\\?\hid#first");
        let mut txn = store.transaction().unwrap();
        txn.state_mut().identity_mode = IdentityMode::Persistent;
        let first_id = txn.state_mut().allocate_device_id().unwrap();
        txn.commit().unwrap();
        let mut first = DeviceIdentity::new(first_id.clone(), None).with_endpoint(endpoint);
        first.physical_id = Some(PhysicalId::from_token_bytes([0x21; 16]));
        manager.register_device(first).unwrap();

        let profile = attack_shark_x3::ProfileId::new(1).unwrap();
        {
            let mut txn = store.transaction().unwrap();
            txn.state_mut()
                .devices
                .get_mut(&first_id)
                .unwrap()
                .profiles
                .entry(profile)
                .or_insert_with(crate::state::ProfileState::empty)
                .polling_rate
                .desired = Some(DesiredState {
                value: PollingRate::Hz500,
                source: DesiredSource::UserWrite,
                verification: Verification::not_sent(),
                updated_at: crate::state::Timestamp { unix_seconds: 42 },
            });
            txn.commit().unwrap();
        }

        let existing = manager.device_identity(&first_id).unwrap();
        manager.register_device(existing).unwrap();

        let state = store.load().unwrap();
        assert_eq!(state.selected_device, Some(first_id.clone()));
        assert_eq!(
            state.devices[&first_id].profiles[&profile]
                .polling_rate
                .desired
                .as_ref()
                .unwrap()
                .value,
            PollingRate::Hz500
        );

        let second_endpoint = ble_endpoint("ble-device-2");
        let mut txn = store.transaction().unwrap();
        let second_id = txn.state_mut().allocate_device_id().unwrap();
        txn.commit().unwrap();
        let mut second = DeviceIdentity::new(second_id.clone(), Some("Second".to_string()))
            .with_endpoint(second_endpoint);
        second.physical_id = Some(PhysicalId::from_token_bytes([0x22; 16]));
        manager.register_device(second).unwrap();
        let state = store.load().unwrap();
        assert_eq!(state.selected_device, Some(first_id));
    }

    #[tokio::test]
    async fn read_status_usb_preserves_desired_and_sets_observed() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);

        let endpoint = wired_endpoint(r"\\?\hid#test-status");
        let mut txn = store.transaction().unwrap();
        let id = txn.state_mut().allocate_device_id().unwrap();
        txn.commit().unwrap();
        let identity = DeviceIdentity::new(id.clone(), Some("Test Mouse".to_string()))
            .with_endpoint(endpoint.clone());

        let metadata = ProfileMetadata::new(
            attack_shark_x3::ProfileId::new(1).unwrap(),
            attack_shark_x3::ProfileId::new(3).unwrap(),
        )
        .unwrap();
        let snapshot = attack_shark_x3::driver::ProfileSnapshot {
            target_profile: ProfileId::new(1).unwrap(),
            persistent_metadata: metadata,
            dpi: DpiState::new(
                ProfileId::new(1).unwrap(),
                vec![DpiValue::new(800).unwrap()],
                StageIndex::new(1).unwrap(),
                [0; 25],
            )
            .unwrap(),
            preferences: PreferencesState::new(
                ProfileId::new(1).unwrap(),
                0,
                0,
                0,
                [0, 0, 0],
                0,
                0,
            ),
            buttons: ButtonsState::new(
                ProfileId::new(1).unwrap(),
                [ButtonAssignment::default();
                    attack_shark_x3::protocol::buttons::BUTTON_SLOT_COUNT],
            ),
        };
        let session = ScriptedFakeSession::usb()
            .with_metadata(metadata)
            .with_profile(snapshot)
            .with_polling_rate(PollingRate::Hz1000)
            .with_battery(87);
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(endpoint.clone(), true), session),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager.register_device(identity).unwrap();

        let profile = attack_shark_x3::ProfileId::new(1).unwrap();
        {
            let mut txn = store.transaction().unwrap();
            let device_state = txn.state_mut().devices.get_mut(&id).unwrap();
            device_state
                .profiles
                .entry(profile)
                .or_insert_with(crate::state::ProfileState::empty)
                .polling_rate
                .desired = Some(DesiredState {
                value: PollingRate::Hz250,
                source: DesiredSource::UserWrite,
                verification: Verification::not_sent(),
                updated_at: crate::state::Timestamp { unix_seconds: 10 },
            });
            txn.commit().unwrap();
        }

        let status = manager.read_status(&id).await.unwrap();
        assert_eq!(status.battery, None);
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
        let meta = status.profile_metadata.as_ref().unwrap();
        assert_eq!(meta.resource.observed.as_ref().unwrap().value, metadata);
        assert!(meta.resource.desired.is_none());
        let persisted = store.load().unwrap();
        let device_state = &persisted.devices[&id];
        let profile_state = &device_state.profiles[&profile];
        assert_eq!(
            profile_state.polling_rate.desired.as_ref().unwrap().value,
            PollingRate::Hz250
        );
        assert_eq!(
            profile_state.polling_rate.observed.as_ref().unwrap().value,
            PollingRate::Hz1000
        );
    }

    #[tokio::test]
    async fn live_profile_snapshot_loads_the_target_profile_once() {
        let store = StateStore::memory();
        let endpoint = wired_endpoint_named(r"\\?\hid#live-profile", "LIVE-PROFILE");
        let id = insert_device_with_endpoint(&store, endpoint.clone());
        let profile = ProfileId::new(1).unwrap();
        let snapshot = snapshot_with_tail(profile, [0; 25]);
        let metadata = snapshot.persistent_metadata;
        let session = ScriptedFakeSession::usb()
            .with_metadata(metadata)
            .with_profile(snapshot.clone())
            .with_polling_rate(PollingRate::Hz1000);
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(endpoint, true), session.clone()),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);

        let live = manager.read_live_profile(&id, None).await.unwrap();

        assert_eq!(live.profile, snapshot);
        assert_eq!(live.polling_rate, PollingRate::Hz1000);
        assert_eq!(session.profile_read_count(), 1);
        let persisted = store.load().unwrap();
        let profile_state = &persisted.devices[&id].profiles[&profile];
        assert!(profile_state.dpi.observed.is_some());
        assert!(profile_state.preferences.observed.is_some());
        assert!(profile_state.buttons.observed.is_some());
        assert!(profile_state.polling_rate.observed.is_some());
    }

    #[tokio::test]
    async fn supplied_profile_snapshot_avoids_the_prewrite_dpi_read() {
        let store = StateStore::memory();
        let endpoint = wired_endpoint_named(r"\\?\hid#snapshot-write", "SNAPSHOT-WRITE");
        let id = insert_device_with_endpoint(&store, endpoint.clone());
        let profile = ProfileId::new(1).unwrap();
        let snapshot = snapshot_with_tail(profile, [0; 25]);
        let session = ScriptedFakeSession::usb()
            .with_metadata(snapshot.persistent_metadata)
            .with_profile(snapshot.clone())
            .with_polling_rate(PollingRate::Hz1000);
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(endpoint, true), session.clone()),
        );
        let manager = DeviceManager::with_store_and_factory(store, factory);
        let update = crate::operation::ProfileUpdate {
            dpi: Some(crate::resources::dpi::DpiDelta {
                stages: Some(vec![DpiValue::new(1600).unwrap()]),
                active_stage: Some(StageIndex::new(1).unwrap()),
                sensor: None,
            }),
            ..Default::default()
        };
        let baseline = crate::operation::ProfileUpdateBaseline {
            dpi: snapshot.dpi,
            preferences: snapshot.preferences,
            buttons: snapshot.buttons,
        };
        let policy = crate::operation::UpdatePolicy {
            verification: crate::operation::VerificationMethod::Transport,
            baseline: crate::operation::BaselineSource::Live,
            ..Default::default()
        };

        manager
            .apply_profile_update_from_snapshot(&id, profile, update, policy, baseline)
            .await
            .unwrap();

        assert_eq!(
            session.dpi_read_count(),
            0,
            "the already-loaded snapshot must replace the live baseline read"
        );
        assert_eq!(session.writes().len(), 1);
    }

    #[tokio::test]
    async fn read_battery_returns_exact_backend_value() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let endpoint = receiver_endpoint(r"\\?\hid#receiver-bat");
        let mut txn = store.transaction().unwrap();
        let id = txn.state_mut().allocate_device_id().unwrap();
        txn.commit().unwrap();
        let identity = DeviceIdentity::new(id.clone(), None).with_endpoint(endpoint.clone());
        let session = ScriptedFakeSession::usb().with_battery(42);
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(endpoint.clone(), true), session),
        );
        let manager = DeviceManager::with_store_and_factory(store, factory);
        manager.register_device(identity).unwrap();

        assert_eq!(manager.read_battery(&id).await.unwrap(), 42);
    }

    #[tokio::test]
    async fn read_status_maps_unsupported_battery_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store(&dir);
        let endpoint = ble_endpoint("ble-device");
        let mut txn = store.transaction().unwrap();
        let id = txn.state_mut().allocate_device_id().unwrap();
        txn.commit().unwrap();
        let identity = DeviceIdentity::new(id.clone(), Some("BLE Test".to_string()))
            .with_endpoint(endpoint.clone());
        let session = ScriptedFakeSession::ble().with_battery(99);
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(endpoint.clone(), true), session),
        );
        let manager = DeviceManager::with_store_and_factory(store, factory);
        manager.register_device(identity).unwrap();

        let status = manager.read_status(&id).await.unwrap();

        assert_eq!(status.battery, None);
        assert_eq!(status.profile_metadata, None);
        assert_eq!(status.polling_rate, None);
    }

    #[tokio::test]
    async fn legacy_mode_uses_one_fuzzy_mouse_with_separate_connections() {
        // Legacy mode: wired and receiver connections with the same VID/PID
        // are separate connections of the single fuzzy logical mouse — they
        // never allocate a second logical device.
        let e_wired = DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa61,
            Some("SN-X"),
            "/dev/hidraw0",
            None,
        )
        .unwrap();
        let e_receiver = DeviceEndpoint::usb(
            TransportKind::Receiver,
            0x1d57,
            0xfa61,
            Some("SN-X"),
            "/dev/hidraw1",
            None,
        )
        .unwrap();
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(
                    make_discovered(e_wired.clone(), true),
                    ScriptedFakeSession::usb(),
                )
                .with_endpoint(
                    make_discovered(e_receiver.clone(), true),
                    ScriptedFakeSession::usb(),
                ),
        );
        let manager = DeviceManager::with_store_and_factory(StateStore::memory(), factory);
        let view = manager.discover(TransportSelection::Auto).await.unwrap();
        assert_eq!(
            view.devices.len(),
            1,
            "legacy discovery allocates exactly one fuzzy logical mouse"
        );
        let fuzzy_id = view.devices[0].identity.id.clone();
        assert_eq!(view.connections.len(), 2);
        for connection in &view.connections {
            assert!(
                matches!(
                    &connection.resolution,
                    IdentityResolution::Resolved { identity } if *identity == fuzzy_id
                ),
                "every separate connection resolves to the one fuzzy mouse"
            );
        }
        assert_eq!(
            view.devices[0].transports,
            vec![TransportKind::Wired, TransportKind::Receiver]
        );
        assert!(view.devices[0].connected);
    }

    #[tokio::test]
    async fn multi_transport_logical_device_aggregates_to_one_row() {
        let store = StateStore::memory();
        let wired = DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa61,
            None,
            "/dev/hidraw0",
            None,
        )
        .unwrap();
        let receiver = DeviceEndpoint::usb(
            TransportKind::Receiver,
            0x1d57,
            0xfa60,
            None,
            "/dev/hidraw1",
            None,
        )
        .unwrap();
        let mut txn = store.transaction().unwrap();
        let id = txn.state_mut().allocate_device_id().unwrap();
        let mut identity = DeviceIdentity::new(id.clone(), None);
        identity.upsert_endpoint(wired.clone());
        identity.upsert_endpoint(receiver.clone());
        txn.state_mut()
            .devices
            .insert(id.clone(), DeviceState::new(identity));
        txn.commit().unwrap();

        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(
                    DiscoveredEndpoint {
                        endpoint: wired,
                        connected: true,
                    },
                    ScriptedFakeSession::usb(),
                )
                .with_endpoint(
                    DiscoveredEndpoint {
                        endpoint: receiver,
                        connected: true,
                    },
                    ScriptedFakeSession::usb(),
                ),
        );
        let manager = DeviceManager::with_store_and_factory(store, factory);
        let devices = manager
            .list_devices(TransportSelection::Auto)
            .await
            .unwrap();
        assert_eq!(devices.len(), 1, "one logical mouse must be one row");
        let row = &devices[0];
        assert_eq!(row.identity.id, id);
        assert!(row.connected, "connected on either transport");
        assert_eq!(
            row.transports,
            vec![TransportKind::Wired, TransportKind::Receiver]
        );
    }

    /// Inserts desired profile-1 DPI with a distinctive stage value so tests
    /// can prove which device's configuration evidence was preserved.
    fn insert_dpi_evidence(store: &StateStore, id: &DeviceId, stage: u16) {
        let mut txn = store.transaction().unwrap();
        let dev = txn.state_mut().devices.get_mut(id).unwrap();
        dev.profiles
            .insert(attack_shark_x3::ProfileId::new(1).unwrap(), {
                let mut ps = crate::state::ProfileState::empty();
                ps.dpi.desired = Some(DesiredState {
                    value: attack_shark_x3::DpiState::captured_empty_profile_one(
                        vec![attack_shark_x3::DpiValue::new(stage).unwrap()],
                        attack_shark_x3::StageIndex::new(1).unwrap(),
                    )
                    .unwrap(),
                    source: DesiredSource::UserWrite,
                    verification: Verification::not_sent(),
                    updated_at: crate::state::Timestamp { unix_seconds: 1 },
                });
                ps
            });
        txn.commit().unwrap();
    }

    #[tokio::test]
    async fn association_does_not_overwrite_cleared_display_name() {
        let store = StateStore::memory();
        let endpoint = DeviceEndpoint::usb(
            TransportKind::Receiver,
            0x1d57,
            0xfa60,
            None,
            r"\\?\hid#named-receiver",
            Some("2.4G Wireless Device"),
        )
        .unwrap();
        let id = insert_device_with_endpoint(&store, endpoint.clone());
        let manager = DeviceManager::with_store_and_factory(
            store.clone(),
            Arc::new(ScriptedFakeFactory::new().with_endpoint(
                DiscoveredEndpoint {
                    endpoint,
                    connected: true,
                },
                ScriptedFakeSession::usb(),
            )),
        );
        manager.rename_device(&id, "").unwrap();

        manager
            .list_devices(TransportSelection::Auto)
            .await
            .unwrap();
        let state = store.load().unwrap();
        assert!(
            state.devices[&id].identity.display_name.is_none(),
            "a cleared name must survive discovery even when the endpoint advertises one"
        );
    }

    #[tokio::test]
    async fn forget_device_refuses_evidence_without_force() {
        let store = StateStore::memory();
        let endpoint = wired_endpoint("/dev/hidraw0");
        let id = insert_device_with_endpoint(&store, endpoint);
        insert_dpi_evidence(&store, &id, 800);
        let manager = DeviceManager::with_store_and_factory(
            store.clone(),
            Arc::new(ScriptedFakeFactory::new()),
        );

        let forgotten = manager.forget_device(&id, false).unwrap();
        assert!(!forgotten, "evidence-bearing device must be protected");
        assert!(store.load().unwrap().devices.contains_key(&id));

        let forgotten = manager.forget_device(&id, true).unwrap();
        assert!(forgotten);
        assert!(!store.load().unwrap().devices.contains_key(&id));
    }

    #[tokio::test]
    async fn forget_device_clears_selection() {
        let store = StateStore::memory();
        let id = insert_device_with_endpoint(&store, wired_endpoint("/dev/hidraw0"));
        let mut txn = store.transaction().unwrap();
        txn.state_mut().selected_device = Some(id.clone());
        txn.commit().unwrap();
        let manager = DeviceManager::with_store_and_factory(
            store.clone(),
            Arc::new(ScriptedFakeFactory::new()),
        );

        manager.forget_device(&id, false).unwrap();
        let state = store.load().unwrap();
        assert!(!state.devices.contains_key(&id));
        assert_eq!(state.selected_device, None);
    }

    #[tokio::test]
    async fn rename_device_sets_and_clears_display_name() {
        let store = StateStore::memory();
        let id = insert_device_with_endpoint(&store, wired_endpoint("/dev/hidraw0"));
        let manager = DeviceManager::with_store_and_factory(
            store.clone(),
            Arc::new(ScriptedFakeFactory::new()),
        );

        manager.rename_device(&id, "  Desk Mouse  ").unwrap();
        assert_eq!(
            manager.find_device_by_name("desk mouse").unwrap(),
            id,
            "name lookup is case-insensitive and trimmed"
        );

        manager.rename_device(&id, "   ").unwrap();
        assert!(
            store.load().unwrap().devices[&id]
                .identity
                .display_name
                .is_none()
        );
        assert!(manager.find_device_by_name("desk mouse").is_err());
    }

    #[tokio::test]
    async fn find_device_by_name_rejects_ambiguous_names() {
        let store = StateStore::memory();
        let first = insert_persistent_device(
            &store,
            wired_endpoint("/dev/hidraw0"),
            PhysicalId::from_token_bytes([0x11; 16]),
        );
        let second = insert_persistent_device(
            &store,
            wired_endpoint("/dev/hidraw1"),
            PhysicalId::from_token_bytes([0x12; 16]),
        );
        let manager =
            DeviceManager::with_store_and_factory(store, Arc::new(ScriptedFakeFactory::new()));
        manager.rename_device(&first, "Mouse").unwrap();
        manager.rename_device(&second, "mouse").unwrap();

        let err = manager.find_device_by_name("mouse").unwrap_err();
        assert!(matches!(err, ManagerError::AmbiguousDevice { .. }));
    }

    #[tokio::test]
    async fn preferred_transport_exact_sets_and_auto_uses_priority() {
        let store = StateStore::memory();
        let wired = DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa61,
            None,
            "/dev/wired0",
            None,
        )
        .unwrap();
        let receiver = DeviceEndpoint::usb(
            TransportKind::Receiver,
            0x1d57,
            0xfa60,
            None,
            "/dev/receiver0",
            None,
        )
        .unwrap();
        let ble = DeviceEndpoint::ble("ble-addr-1", None).unwrap();
        let mut txn = store.transaction().unwrap();
        let id = txn.state_mut().allocate_device_id().unwrap();
        let mut identity = DeviceIdentity::new(id.clone(), None);
        identity.upsert_endpoint(wired.clone());
        identity.upsert_endpoint(receiver.clone());
        identity.upsert_endpoint(ble.clone());
        txn.state_mut()
            .devices
            .insert(id.clone(), crate::state::DeviceState::new(identity));
        txn.state_mut().selected_device = Some(id.clone());
        txn.commit().unwrap();

        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(
                    DiscoveredEndpoint {
                        endpoint: wired.clone(),
                        connected: true,
                    },
                    ScriptedFakeSession::usb(),
                )
                .with_endpoint(
                    DiscoveredEndpoint {
                        endpoint: receiver.clone(),
                        connected: true,
                    },
                    ScriptedFakeSession::usb(),
                )
                .with_endpoint(
                    DiscoveredEndpoint {
                        endpoint: ble.clone(),
                        connected: true,
                    },
                    ScriptedFakeSession::ble(),
                ),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);

        let id_no_pref = manager.device_identity(&id).unwrap();
        assert_eq!(
            id_no_pref.selected_endpoint().unwrap().transport,
            TransportKind::Wired
        );

        let ep_receiver_disc = DiscoveredEndpoint {
            endpoint: receiver.clone(),
            connected: true,
        };
        let factory2 = Arc::new(
            ScriptedFakeFactory::new().with_endpoint(ep_receiver_disc, ScriptedFakeSession::usb()),
        );
        let manager2 = DeviceManager::with_store_and_factory(store.clone(), factory2);
        manager2
            .resolve_device(
                Some(&id),
                TransportSelection::Exact(TransportKind::Receiver),
            )
            .await
            .unwrap();
        let after = manager2.device_identity(&id).unwrap();
        assert_eq!(after.preferred_transport, Some(TransportKind::Receiver));
        assert_eq!(
            after.selected_endpoint().unwrap().transport,
            TransportKind::Receiver
        );
    }

    #[tokio::test]
    async fn operation_lock_is_acquired_for_hardware_operations() {
        let store = StateStore::memory();
        let endpoint = receiver_endpoint(r"\\?\hid#receiver-lock");
        let id = insert_device_with_endpoint(&store, endpoint.clone());
        let session = ScriptedFakeSession::usb().with_battery(55);
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(endpoint.clone(), true), session),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);

        let guard = store
            .acquire_operation_lock(&id, Duration::from_millis(200), "test")
            .unwrap();
        drop(guard);
        let battery = manager.read_battery(&id).await.unwrap();
        assert_eq!(battery, 55);
        let _guard2 = store
            .acquire_operation_lock(&id, Duration::from_millis(50), "test2")
            .unwrap();
    }

    #[tokio::test]
    async fn read_status_loads_target_before_live_rate_and_stores_under_target() {
        let store = StateStore::memory();
        let endpoint = wired_endpoint(r"\\?\hid#status-seq");
        let id = insert_device_with_endpoint(&store, endpoint.clone());
        let target = ProfileId::new(2).unwrap();
        let metadata = ProfileMetadata::new(target, ProfileId::new(3).unwrap()).unwrap();
        let snapshot2 = attack_shark_x3::driver::ProfileSnapshot {
            target_profile: target,
            persistent_metadata: metadata,
            dpi: DpiState::new(
                target,
                vec![DpiValue::new(800).unwrap()],
                StageIndex::new(1).unwrap(),
                [0; 25],
            )
            .unwrap(),
            preferences: PreferencesState::new(target, 1, 1, 0, [0, 0, 0], 0, 0),
            buttons: ButtonsState::new(
                target,
                [ButtonAssignment::default();
                    attack_shark_x3::protocol::buttons::BUTTON_SLOT_COUNT],
            ),
        };
        let session = ScriptedFakeSession::usb()
            .with_metadata(metadata)
            .with_profile(snapshot2.clone())
            .with_polling_rate_for(ProfileId::new(1).unwrap(), PollingRate::Hz125)
            .with_polling_rate_for(target, PollingRate::Hz1000)
            .with_live_profile(ProfileId::new(1).unwrap());
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(endpoint.clone(), true), session.clone()),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        manager
            .register_device(DeviceIdentity::new(id.clone(), None).with_endpoint(endpoint.clone()))
            .unwrap_or(());
        let status = manager.read_status(&id).await.unwrap();
        let polling = status
            .polling_rate
            .expect("status must include polling for USB current");
        assert_eq!(
            polling.resource.observed.as_ref().unwrap().value,
            PollingRate::Hz1000
        );
        let persisted = store.load().unwrap();
        assert_eq!(
            persisted.devices[&id].profiles[&target]
                .polling_rate
                .observed
                .as_ref()
                .unwrap()
                .value,
            PollingRate::Hz1000
        );
        assert!(
            !persisted.devices[&id]
                .profiles
                .get(&ProfileId::new(1).unwrap())
                .map(|p| p.polling_rate.observed.is_some())
                .unwrap_or(false)
                || persisted.devices[&id].profiles[&ProfileId::new(1).unwrap()]
                    .polling_rate
                    .observed
                    .as_ref()
                    .map(|o| o.value)
                    != Some(PollingRate::Hz1000)
        );
        assert_eq!(session.last_polling_alias(), Some(target));
    }

    #[tokio::test]
    async fn read_status_omits_unsupported_rate_without_misassociation() {
        let store = StateStore::memory();
        let endpoint = wired_endpoint(r"\\?\hid#status-unsupported");
        let id = insert_device_with_endpoint(&store, endpoint.clone());
        let metadata =
            ProfileMetadata::new(ProfileId::new(1).unwrap(), ProfileId::new(3).unwrap()).unwrap();
        let snapshot = attack_shark_x3::driver::ProfileSnapshot {
            target_profile: ProfileId::new(1).unwrap(),
            persistent_metadata: metadata,
            dpi: DpiState::new(
                ProfileId::new(1).unwrap(),
                vec![DpiValue::new(800).unwrap()],
                StageIndex::new(1).unwrap(),
                [0; 25],
            )
            .unwrap(),
            preferences: PreferencesState::new(
                ProfileId::new(1).unwrap(),
                0,
                0,
                0,
                [0, 0, 0],
                0,
                0,
            ),
            buttons: ButtonsState::new(
                ProfileId::new(1).unwrap(),
                [ButtonAssignment::default();
                    attack_shark_x3::protocol::buttons::BUTTON_SLOT_COUNT],
            ),
        };
        struct Factory {
            endpoint: DeviceEndpoint,
            metadata: ProfileMetadata,
            snapshot: attack_shark_x3::driver::ProfileSnapshot,
        }
        #[async_trait::async_trait(?Send)]
        impl crate::backend::SessionFactory for Factory {
            async fn list(
                &self,
                _selection: TransportSelection,
            ) -> Result<Vec<DiscoveredEndpoint>, ManagerError> {
                Ok(vec![DiscoveredEndpoint {
                    endpoint: self.endpoint.clone(),
                    connected: true,
                }])
            }
            async fn open(
                &self,
                _endpoint: &DeviceEndpoint,
            ) -> Result<Box<dyn DeviceSession>, ManagerError> {
                Ok(Box::new(UnsupportedPollSession {
                    metadata: self.metadata,
                    snapshot: self.snapshot.clone(),
                }))
            }
        }
        let factory = Arc::new(Factory {
            endpoint: endpoint.clone(),
            metadata,
            snapshot,
        });
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        let status = manager.read_status(&id).await.unwrap();
        assert!(status.profile_metadata.is_some());
        assert!(
            status.polling_rate.is_none(),
            "unsupported live rate must be omitted, not misassociated"
        );
        let persisted = store.load().unwrap();
        assert!(
            persisted.devices[&id]
                .profiles
                .get(&ProfileId::new(1).unwrap())
                .map(|p| p.polling_rate.observed.is_none())
                .unwrap_or(true)
        );
    }

    #[tokio::test]
    async fn activate_profile_usb_mismatch_is_persisted_and_errors() {
        let store = StateStore::memory();
        let endpoint = wired_endpoint(r"\\?\hid#activate-mismatch");
        let id = insert_device_with_endpoint(&store, endpoint.clone());
        let target = ProfileId::new(2).unwrap();
        let mismatched =
            ProfileMetadata::new(ProfileId::new(3).unwrap(), ProfileId::new(5).unwrap()).unwrap();
        let target_metadata = ProfileMetadata::new(target, ProfileId::new(5).unwrap()).unwrap();
        let inner = ScriptedFakeSession::usb().with_metadata(
            ProfileMetadata::new(ProfileId::new(1).unwrap(), ProfileId::new(5).unwrap()).unwrap(),
        );
        struct Factory {
            endpoint: DeviceEndpoint,
            inner: ScriptedFakeSession,
            mismatched: ProfileMetadata,
        }
        #[async_trait::async_trait(?Send)]
        impl crate::backend::SessionFactory for Factory {
            async fn list(
                &self,
                _selection: TransportSelection,
            ) -> Result<Vec<DiscoveredEndpoint>, ManagerError> {
                Ok(vec![DiscoveredEndpoint {
                    endpoint: self.endpoint.clone(),
                    connected: true,
                }])
            }
            async fn open(
                &self,
                _endpoint: &DeviceEndpoint,
            ) -> Result<Box<dyn DeviceSession>, ManagerError> {
                Ok(Box::new(MismatchWrapper {
                    inner: self.inner.clone(),
                    mismatched: self.mismatched,
                }))
            }
        }
        let factory = Arc::new(Factory {
            endpoint: endpoint.clone(),
            inner,
            mismatched,
        });
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        let err = manager
            .activate_profile(&id, target)
            .await
            .expect_err("mismatch must error");
        assert!(
            matches!(err, ManagerError::VerificationMismatch { resource: "profile metadata", profile: Some(p) } if p == target)
        );
        let persisted = store.load().unwrap();
        let res = &persisted.devices[&id].profile_metadata;
        assert_eq!(res.desired.as_ref().unwrap().value, target_metadata);
        assert_eq!(res.observed.as_ref().unwrap().value, mismatched);
        assert_eq!(
            res.desired.as_ref().unwrap().verification.application,
            ApplicationVerification::Mismatch
        );
    }

    #[tokio::test]
    async fn set_profile_metadata_usb_mismatch_is_persisted_and_errors() {
        let store = StateStore::memory();
        let endpoint = wired_endpoint(r"\\?\hid#set-mismatch");
        let id = insert_device_with_endpoint(&store, endpoint.clone());
        let current = ProfileId::new(2).unwrap();
        let maximum = ProfileId::new(5).unwrap();
        let target = ProfileMetadata::new(current, maximum).unwrap();
        let mismatched = ProfileMetadata::new(ProfileId::new(4).unwrap(), maximum).unwrap();
        let inner = ScriptedFakeSession::usb();
        struct Factory {
            endpoint: DeviceEndpoint,
            inner: ScriptedFakeSession,
            mismatched: ProfileMetadata,
        }
        #[async_trait::async_trait(?Send)]
        impl crate::backend::SessionFactory for Factory {
            async fn list(
                &self,
                _s: TransportSelection,
            ) -> Result<Vec<DiscoveredEndpoint>, ManagerError> {
                Ok(vec![DiscoveredEndpoint {
                    endpoint: self.endpoint.clone(),
                    connected: true,
                }])
            }
            async fn open(
                &self,
                _e: &DeviceEndpoint,
            ) -> Result<Box<dyn DeviceSession>, ManagerError> {
                Ok(Box::new(MismatchWrapper {
                    inner: self.inner.clone(),
                    mismatched: self.mismatched,
                }))
            }
        }
        let factory = Arc::new(Factory {
            endpoint: endpoint.clone(),
            inner,
            mismatched,
        });
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        let err = manager
            .set_profile_metadata(&id, current, maximum)
            .await
            .expect_err("mismatch must error");
        assert!(
            matches!(err, ManagerError::VerificationMismatch { resource: "profile metadata", profile: Some(p) } if p == current)
        );
        let persisted = store.load().unwrap();
        let res = &persisted.devices[&id].profile_metadata;
        assert_eq!(res.desired.as_ref().unwrap().value, target);
        assert_eq!(res.observed.as_ref().unwrap().value, mismatched);
        assert_eq!(
            res.desired.as_ref().unwrap().verification.application,
            ApplicationVerification::Mismatch
        );
    }

    #[tokio::test]
    async fn activate_profile_ble_ack_succeeds_without_mismatch_check() {
        let store = StateStore::memory();
        let endpoint = ble_endpoint("ble-activate-ack");
        let id = insert_device_with_endpoint(&store, endpoint.clone());
        let session = ScriptedFakeSession::ble();
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(endpoint.clone(), true), session),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        let target = ProfileId::new(2).unwrap();
        let result = manager.activate_profile(&id, target).await.unwrap();
        assert_eq!(result.current(), target);
        let persisted = store.load().unwrap();
        let res = &persisted.devices[&id].profile_metadata;
        assert_eq!(
            res.desired.as_ref().unwrap().verification.application,
            ApplicationVerification::Acknowledged
        );
        assert!(res.observed.is_none());
    }
    // -----------------------------------------------------------------------
    // Composite profile update scripted tests
    // -----------------------------------------------------------------------
    #[tokio::test]
    async fn apply_profile_update_empty_rejected_before_open() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct CountingFactory {
            inner: ScriptedFakeFactory,
            opens: Arc<AtomicUsize>,
        }
        #[async_trait::async_trait(?Send)]
        impl crate::backend::SessionFactory for CountingFactory {
            async fn list(
                &self,
                s: TransportSelection,
            ) -> Result<Vec<DiscoveredEndpoint>, ManagerError> {
                self.inner.list(s).await
            }
            async fn open(
                &self,
                e: &DeviceEndpoint,
            ) -> Result<Box<dyn DeviceSession>, ManagerError> {
                self.opens.fetch_add(1, Ordering::SeqCst);
                self.inner.open(e).await
            }
        }
        let store = StateStore::memory();
        let endpoint = wired_endpoint(r"\\?\hid#empty");
        let id = insert_device_with_endpoint(&store, endpoint.clone());
        let opens = Arc::new(AtomicUsize::new(0));
        let inner = ScriptedFakeFactory::new().with_endpoint(
            make_discovered(endpoint.clone(), true),
            ScriptedFakeSession::usb(),
        );
        let factory = Arc::new(CountingFactory {
            inner,
            opens: opens.clone(),
        });
        let manager = DeviceManager::with_store_and_factory(store, factory);
        let err = manager
            .apply_profile_update(
                &id,
                ProfileId::new(1).unwrap(),
                crate::operation::ProfileUpdate::default(),
                crate::operation::UpdatePolicy::default(),
            )
            .await
            .expect_err("empty must be rejected");
        assert!(matches!(err, ManagerError::InvalidUpdate(_)));
        assert_eq!(
            opens.load(Ordering::SeqCst),
            0,
            "empty must be rejected before open"
        );
    }

    #[tokio::test]
    async fn apply_profile_update_mixed_rate_rejected_before_open() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct CountingFactory {
            inner: ScriptedFakeFactory,
            opens: Arc<AtomicUsize>,
        }
        #[async_trait::async_trait(?Send)]
        impl crate::backend::SessionFactory for CountingFactory {
            async fn list(
                &self,
                s: TransportSelection,
            ) -> Result<Vec<DiscoveredEndpoint>, ManagerError> {
                self.inner.list(s).await
            }
            async fn open(
                &self,
                e: &DeviceEndpoint,
            ) -> Result<Box<dyn DeviceSession>, ManagerError> {
                self.opens.fetch_add(1, Ordering::SeqCst);
                self.inner.open(e).await
            }
        }
        let store = StateStore::memory();
        let endpoint = wired_endpoint(r"\\?\hid#mixed");
        let id = insert_device_with_endpoint(&store, endpoint.clone());
        let opens = Arc::new(AtomicUsize::new(0));
        let inner = ScriptedFakeFactory::new().with_endpoint(
            make_discovered(endpoint.clone(), true),
            ScriptedFakeSession::usb(),
        );
        let factory = Arc::new(CountingFactory {
            inner,
            opens: opens.clone(),
        });
        let manager = DeviceManager::with_store_and_factory(store, factory);
        let update = crate::operation::ProfileUpdate {
            dpi: Some(crate::resources::dpi::DpiDelta {
                active_stage: Some(StageIndex::new(1).unwrap()),
                ..Default::default()
            }),
            polling_rate: Some(PollingRate::Hz1000),
            ..Default::default()
        };
        let err = manager
            .apply_profile_update(
                &id,
                ProfileId::new(1).unwrap(),
                update,
                crate::operation::UpdatePolicy::default(),
            )
            .await
            .expect_err("mixed must be rejected");
        assert!(matches!(err, ManagerError::InvalidUpdate(_)));
        assert!(err.to_string().contains("polling rate cannot be combined"));
        assert_eq!(
            opens.load(Ordering::SeqCst),
            0,
            "mixed must be rejected before open"
        );
    }

    #[tokio::test]
    async fn apply_profile_update_multi_button_one_write() {
        let store = StateStore::memory();
        let endpoint = wired_endpoint(r"\\?\hid#multi-btn");
        let id = insert_device_with_endpoint(&store, endpoint.clone());
        let profile = ProfileId::new(1).unwrap();
        let baseline = ButtonsState::default_for_profile(profile);
        let snapshot = attack_shark_x3::driver::ProfileSnapshot {
            target_profile: profile,
            persistent_metadata: ProfileMetadata::new(profile, ProfileId::new(5).unwrap()).unwrap(),
            dpi: DpiState::new(
                profile,
                vec![DpiValue::new(800).unwrap()],
                StageIndex::new(1).unwrap(),
                [0; 25],
            )
            .unwrap(),
            preferences: PreferencesState::new(profile, 0, 0, 0, [0, 0, 0], 0, 0),
            buttons: baseline,
        };
        let session = ScriptedFakeSession::usb().with_profile(snapshot);
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(endpoint.clone(), true), session.clone()),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        let update = crate::operation::ProfileUpdate {
            buttons: vec![
                crate::resources::buttons::ButtonSlotDelta::new(
                    crate::resources::buttons::SafeButtonSlot::Forward,
                    crate::resources::buttons::SafeButtonAction::Copy,
                ),
                crate::resources::buttons::ButtonSlotDelta::new(
                    crate::resources::buttons::SafeButtonSlot::Backward,
                    crate::resources::buttons::SafeButtonAction::Paste,
                ),
            ],
            ..Default::default()
        };
        let outcome = manager
            .apply_profile_update(
                &id,
                profile,
                update,
                crate::operation::UpdatePolicy::default(),
            )
            .await
            .unwrap();
        assert!(outcome.buttons.is_some());
        let writes = session.writes();
        let button_writes: Vec<_> = writes
            .iter()
            .filter(|w| matches!(w, crate::backend::ScriptedWrite::Buttons(_)))
            .collect();
        assert_eq!(
            button_writes.len(),
            1,
            "multiple button deltas must merge into one write"
        );
        if let crate::backend::ScriptedWrite::Buttons(state) = &button_writes[0] {
            assert_eq!(
                state.slots[crate::resources::buttons::SafeButtonSlot::Forward.index()],
                crate::resources::buttons::SafeButtonAction::Copy.to_assignment()
            );
            assert_eq!(
                state.slots[crate::resources::buttons::SafeButtonSlot::Backward.index()],
                crate::resources::buttons::SafeButtonAction::Paste.to_assignment()
            );
        }
        let persisted = store.load().unwrap().devices[&id].profiles[&profile]
            .buttons
            .desired
            .as_ref()
            .unwrap()
            .value;
        assert_eq!(
            persisted.slots[crate::resources::buttons::SafeButtonSlot::Forward.index()],
            crate::resources::buttons::SafeButtonAction::Copy.to_assignment()
        );
    }

    #[tokio::test]
    async fn apply_profile_update_non_rate_one_session() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct CountingFactory {
            inner: ScriptedFakeFactory,
            opens: Arc<AtomicUsize>,
        }
        #[async_trait::async_trait(?Send)]
        impl crate::backend::SessionFactory for CountingFactory {
            async fn list(
                &self,
                s: TransportSelection,
            ) -> Result<Vec<DiscoveredEndpoint>, ManagerError> {
                self.inner.list(s).await
            }
            async fn open(
                &self,
                e: &DeviceEndpoint,
            ) -> Result<Box<dyn DeviceSession>, ManagerError> {
                self.opens.fetch_add(1, Ordering::SeqCst);
                self.inner.open(e).await
            }
        }
        let store = StateStore::memory();
        let endpoint = wired_endpoint(r"\\?\hid#one-session");
        let id = insert_device_with_endpoint(&store, endpoint.clone());
        let profile = ProfileId::new(1).unwrap();
        let dpi = DpiState::new(
            profile,
            vec![DpiValue::new(800).unwrap(), DpiValue::new(1600).unwrap()],
            StageIndex::new(1).unwrap(),
            [0; 25],
        )
        .unwrap();
        let prefs = PreferencesState::new(profile, 0, 0, 0, [0, 0, 0], 0, 0);
        let buttons = ButtonsState::default_for_profile(profile);
        let snapshot = attack_shark_x3::driver::ProfileSnapshot {
            target_profile: profile,
            persistent_metadata: ProfileMetadata::new(profile, ProfileId::new(5).unwrap()).unwrap(),
            dpi: dpi.clone(),
            preferences: prefs,
            buttons,
        };
        let session = ScriptedFakeSession::usb()
            .with_profile(snapshot.clone())
            .with_metadata(ProfileMetadata::new(profile, ProfileId::new(5).unwrap()).unwrap())
            .with_polling_rate(PollingRate::Hz1000);
        // Provide complete desired image for polling isolation not needed here; for non-rate we need baselines via live reads.
        let opens = Arc::new(AtomicUsize::new(0));
        let inner = ScriptedFakeFactory::new()
            .with_endpoint(make_discovered(endpoint.clone(), true), session.clone());
        let factory = Arc::new(CountingFactory {
            inner,
            opens: opens.clone(),
        });
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        // Seed stored baseline for polling metadata requirement is not needed for non-rate path.
        let update = crate::operation::ProfileUpdate {
            dpi: Some(crate::resources::dpi::DpiDelta {
                active_stage: Some(StageIndex::new(2).unwrap()),
                ..Default::default()
            }),
            preferences: Some(crate::resources::settings::PreferencesDelta {
                sleep_timer: Some(10),
                ..Default::default()
            }),
            buttons: vec![crate::resources::buttons::ButtonSlotDelta::new(
                crate::resources::buttons::SafeButtonSlot::Forward,
                crate::resources::buttons::SafeButtonAction::Copy,
            )],
            ..Default::default()
        };
        let outcome = manager
            .apply_profile_update(
                &id,
                profile,
                update,
                crate::operation::UpdatePolicy::default(),
            )
            .await
            .unwrap();
        assert!(outcome.dpi.is_some());
        assert!(outcome.preferences.is_some());
        assert!(outcome.buttons.is_some());
        assert_eq!(
            opens.load(Ordering::SeqCst),
            1,
            "non-rate composite must open exactly one session"
        );
        let writes = session.writes();
        assert_eq!(writes.len(), 3, "should have three resource writes");
    }

    #[tokio::test]
    async fn apply_profile_update_rate_only_safe_path() {
        let store = StateStore::memory();
        let endpoint = wired_endpoint(r"\\?\hid#rate-only");
        let id = insert_device_with_endpoint(&store, endpoint.clone());
        let profile = ProfileId::new(2).unwrap();
        let dpi = DpiState::new(
            profile,
            vec![DpiValue::new(800).unwrap()],
            StageIndex::new(1).unwrap(),
            [0; 25],
        )
        .unwrap();
        let prefs = PreferencesState::new(profile, 1, 1, 0, [0, 0, 0], 0, 0);
        let buttons = ButtonsState::default_for_profile(profile);
        let metadata = ProfileMetadata::new(profile, ProfileId::new(5).unwrap()).unwrap();
        let snapshot = attack_shark_x3::driver::ProfileSnapshot {
            target_profile: profile,
            persistent_metadata: metadata,
            dpi: dpi.clone(),
            preferences: prefs,
            buttons,
        };
        // Seed durable desired image and metadata for safe polling preflight.
        {
            let mut txn = store.transaction().unwrap();
            let dev = txn.state_mut().devices.get_mut(&id).unwrap();
            dev.profile_metadata.desired = Some(DesiredState {
                value: metadata,
                source: DesiredSource::UserWrite,
                verification: Verification::not_sent(),
                updated_at: crate::state::Timestamp { unix_seconds: 1 },
            });
            dev.profile_metadata.observed = Some(crate::state::ObservedState {
                value: metadata,
                source: crate::state::ObservationSource::UsbReadback,
                observed_at: crate::state::Timestamp { unix_seconds: 1 },
            });
            let ps = dev
                .profiles
                .entry(profile)
                .or_insert_with(crate::state::ProfileState::empty);
            ps.dpi.desired = Some(DesiredState {
                value: dpi.clone(),
                source: DesiredSource::UserWrite,
                verification: Verification::not_sent(),
                updated_at: crate::state::Timestamp { unix_seconds: 1 },
            });
            ps.preferences.desired = Some(DesiredState {
                value: prefs,
                source: DesiredSource::UserWrite,
                verification: Verification::not_sent(),
                updated_at: crate::state::Timestamp { unix_seconds: 1 },
            });
            ps.buttons.desired = Some(DesiredState {
                value: buttons,
                source: DesiredSource::UserWrite,
                verification: Verification::not_sent(),
                updated_at: crate::state::Timestamp { unix_seconds: 1 },
            });
            txn.commit().unwrap();
        }
        let session = ScriptedFakeSession::usb()
            .with_metadata(metadata)
            .with_profile(snapshot)
            .with_polling_rate_for(ProfileId::new(1).unwrap(), PollingRate::Hz125)
            .with_polling_rate_for(profile, PollingRate::Hz500)
            .with_live_profile(ProfileId::new(1).unwrap());
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(endpoint.clone(), true), session.clone()),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        // Updating to same rate should not perform hardware write, just record.
        let same_update = crate::operation::ProfileUpdate {
            polling_rate: Some(PollingRate::Hz1000),
            ..Default::default()
        };
        // First set current live to 1000 via snapshot? Actually live is 1 with 125, after loading target 2 it becomes 500, so writing to 1000 will be new.
        let outcome = manager
            .apply_profile_update(
                &id,
                profile,
                same_update,
                crate::operation::UpdatePolicy::default(),
            )
            .await
            .unwrap();
        assert!(outcome.polling_rate.is_some());
        // Polling write should have occurred (since current 500 != 1000)
        let writes = session.writes();
        assert!(writes.iter().any(|w| matches!(w, crate::backend::ScriptedWrite::PollingRate(p, r) if *p==profile && *r==PollingRate::Hz1000)));
        // Now repeat with rate already 1000 -> no hardware write (current equals desired)
        // After previous write, live rate for profile 2 is 1000, but live_profile is still profile 2 after read_profile, so current == desired.
        let second = crate::operation::ProfileUpdate {
            polling_rate: Some(PollingRate::Hz1000),
            ..Default::default()
        };
        let writes_before = session.writes().len();
        let outcome2 = manager
            .apply_profile_update(
                &id,
                profile,
                second,
                crate::operation::UpdatePolicy::default(),
            )
            .await
            .unwrap();
        assert!(outcome2.polling_rate.is_some());
        assert_eq!(
            session.writes().len(),
            writes_before,
            "redundant rate write must be avoided"
        );
        let persisted = store.load().unwrap().devices[&id].profiles[&profile]
            .polling_rate
            .desired
            .as_ref()
            .unwrap()
            .value;
        assert_eq!(persisted, PollingRate::Hz1000);
    }

    #[tokio::test]
    async fn apply_profile_update_readback_mismatch_is_persisted_and_errors() {
        struct MismatchSession {
            inner: ScriptedFakeSession,
            mismatch_dpi: DpiState,
        }
        #[async_trait::async_trait(?Send)]
        impl DeviceSession for MismatchSession {
            fn transport(&self) -> TransportKind {
                self.inner.transport()
            }
            async fn read_profile_metadata(&self) -> Result<ProfileMetadata, ManagerError> {
                self.inner.read_profile_metadata().await
            }
            async fn read_profile(
                &self,
                p: ProfileId,
            ) -> Result<attack_shark_x3::driver::ProfileSnapshot, ManagerError> {
                self.inner.read_profile(p).await
            }
            async fn read_dpi(&self, p: ProfileId) -> Result<DpiState, ManagerError> {
                self.inner.read_dpi(p).await
            }
            async fn read_preferences(
                &self,
                p: ProfileId,
            ) -> Result<PreferencesState, ManagerError> {
                self.inner.read_preferences(p).await
            }
            async fn read_buttons(&self, p: ProfileId) -> Result<ButtonsState, ManagerError> {
                self.inner.read_buttons(p).await
            }
            async fn read_live_polling_rate(
                &self,
                a: ProfileId,
            ) -> Result<PollingRate, ManagerError> {
                self.inner.read_live_polling_rate(a).await
            }
            async fn write_dpi(
                &self,
                _state: DpiState,
                _v: crate::operation::VerificationMethod,
            ) -> Result<SessionWrite<DpiState>, ManagerError> {
                // Return mismatched readback regardless of requested.
                Ok(SessionWrite::ReadbackVerified(self.mismatch_dpi.clone()))
            }
            async fn write_preferences(
                &self,
                s: PreferencesState,
                v: crate::operation::VerificationMethod,
            ) -> Result<SessionWrite<PreferencesState>, ManagerError> {
                self.inner.write_preferences(s, v).await
            }
            async fn write_buttons(
                &self,
                s: ButtonsState,
                v: crate::operation::VerificationMethod,
            ) -> Result<SessionWrite<ButtonsState>, ManagerError> {
                self.inner.write_buttons(s, v).await
            }
            async fn write_polling_rate_unchecked(
                &self,
                p: ProfileId,
                r: PollingRate,
                v: crate::operation::VerificationMethod,
            ) -> Result<SessionWrite<PollingRate>, ManagerError> {
                self.inner.write_polling_rate_unchecked(p, r, v).await
            }
            async fn write_profile_metadata(
                &self,
                m: ProfileMetadata,
            ) -> Result<SessionWrite<ProfileMetadata>, ManagerError> {
                self.inner.write_profile_metadata(m).await
            }
            async fn read_battery(&self, t: Duration) -> Result<u8, ManagerError> {
                self.inner.read_battery(t).await
            }
            fn subscribe_events(&self) -> crate::backend::SessionEvents {
                self.inner.subscribe_events()
            }
        }
        let store = StateStore::memory();
        let endpoint = wired_endpoint(r"\\?\hid#mismatch");
        let id = insert_device_with_endpoint(&store, endpoint.clone());
        let profile = ProfileId::new(1).unwrap();
        let baseline = DpiState::new(
            profile,
            vec![DpiValue::new(800).unwrap()],
            StageIndex::new(1).unwrap(),
            [0; 25],
        )
        .unwrap();
        let snapshot = attack_shark_x3::driver::ProfileSnapshot {
            target_profile: profile,
            persistent_metadata: ProfileMetadata::new(profile, ProfileId::new(5).unwrap()).unwrap(),
            dpi: baseline.clone(),
            preferences: PreferencesState::new(profile, 0, 0, 0, [0, 0, 0], 0, 0),
            buttons: ButtonsState::default_for_profile(profile),
        };
        let inner = ScriptedFakeSession::usb().with_profile(snapshot);
        let mismatch = DpiState::new(
            profile,
            vec![DpiValue::new(1600).unwrap()],
            StageIndex::new(1).unwrap(),
            [0; 25],
        )
        .unwrap();
        let session = MismatchSession {
            inner: inner.clone(),
            mismatch_dpi: mismatch.clone(),
        };
        struct Factory {
            endpoint: DeviceEndpoint,
            session: MismatchSession,
        }
        #[async_trait::async_trait(?Send)]
        impl crate::backend::SessionFactory for Factory {
            async fn list(
                &self,
                _s: TransportSelection,
            ) -> Result<Vec<DiscoveredEndpoint>, ManagerError> {
                Ok(vec![DiscoveredEndpoint {
                    endpoint: self.endpoint.clone(),
                    connected: true,
                }])
            }
            async fn open(
                &self,
                _e: &DeviceEndpoint,
            ) -> Result<Box<dyn DeviceSession>, ManagerError> {
                Ok(Box::new(MismatchSession {
                    inner: self.session.inner.clone(),
                    mismatch_dpi: self.session.mismatch_dpi.clone(),
                }))
            }
        }
        let factory = Arc::new(Factory {
            endpoint: endpoint.clone(),
            session,
        });
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        let update = crate::operation::ProfileUpdate {
            dpi: Some(crate::resources::dpi::DpiDelta {
                stages: Some(vec![DpiValue::new(1200).unwrap()]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let err = manager
            .apply_profile_update(
                &id,
                profile,
                update,
                crate::operation::UpdatePolicy {
                    verification: crate::operation::VerificationMethod::Readback,
                    ..Default::default()
                },
            )
            .await
            .expect_err("mismatch must error");
        assert!(matches!(
            err,
            ManagerError::VerificationMismatch {
                resource: "DPI",
                ..
            }
        ));
        let persisted = store.load().unwrap();
        let dpi_state = &persisted.devices[&id].profiles[&profile].dpi;
        assert_eq!(
            dpi_state.desired.as_ref().unwrap().verification.application,
            ApplicationVerification::Mismatch
        );
        assert!(
            dpi_state
                .desired
                .as_ref()
                .unwrap()
                .value
                .stages
                .contains(&DpiValue::new(1200).unwrap())
        );
        assert_eq!(dpi_state.observed.as_ref().unwrap().value, mismatch);
    }

    #[tokio::test]
    async fn apply_profile_update_ble_baseline_policy() {
        let store = StateStore::memory();
        let endpoint = ble_endpoint("ble-baseline");
        let id = insert_device_with_endpoint(&store, endpoint.clone());
        let profile = ProfileId::new(1).unwrap();
        // Seed stored baseline for BLE.
        let stored_dpi = DpiState::new(
            profile,
            vec![DpiValue::new(800).unwrap()],
            StageIndex::new(1).unwrap(),
            [0; 25],
        )
        .unwrap();
        {
            let mut txn = store.transaction().unwrap();
            let ps = txn
                .state_mut()
                .devices
                .get_mut(&id)
                .unwrap()
                .profiles
                .entry(profile)
                .or_default();
            ps.dpi.desired = Some(DesiredState {
                value: stored_dpi.clone(),
                source: DesiredSource::UserWrite,
                verification: Verification::not_sent(),
                updated_at: crate::state::Timestamp { unix_seconds: 1 },
            });
            txn.commit().unwrap();
        }
        let session = ScriptedFakeSession::ble();
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(endpoint.clone(), true), session.clone()),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);
        // BLE with stored baseline should succeed using stored baseline.
        let update = crate::operation::ProfileUpdate {
            dpi: Some(crate::resources::dpi::DpiDelta {
                active_stage: Some(StageIndex::new(1).unwrap()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let outcome = manager
            .apply_profile_update(
                &id,
                profile,
                update.clone(),
                crate::operation::UpdatePolicy::default(),
            )
            .await
            .unwrap();
        assert!(outcome.dpi.is_some());
        // BLE without baseline and without allow_explicit_defaults must fail MissingBaseline before write.
        let store2 = StateStore::memory();
        let endpoint2 = ble_endpoint("ble-no-baseline");
        let id2 = insert_device_with_endpoint(&store2, endpoint2.clone());
        let session2 = ScriptedFakeSession::ble();
        let factory2 = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(endpoint2.clone(), true), session2),
        );
        let manager2 = DeviceManager::with_store_and_factory(store2, factory2);
        let err = manager2
            .apply_profile_update(
                &id2,
                profile,
                update,
                crate::operation::UpdatePolicy::default(),
            )
            .await
            .expect_err("missing baseline");
        assert!(matches!(
            err,
            ManagerError::MissingBaseline {
                resource: "DPI",
                ..
            }
        ));
        // With allow_explicit_defaults, BLE should use captured evidence.
        let err2 = manager2
            .apply_profile_update(
                &id2,
                profile,
                crate::operation::ProfileUpdate {
                    dpi: Some(crate::resources::dpi::DpiDelta {
                        active_stage: Some(StageIndex::new(1).unwrap()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                crate::operation::UpdatePolicy {
                    allow_explicit_defaults: true,
                    ..Default::default()
                },
            )
            .await;
        assert!(
            err2.is_ok(),
            "with allow_explicit_defaults BLE should succeed via captured evidence, got {err2:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Physical-identity discovery and ceremony tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn legacy_discovery_reads_no_watermarks_and_keeps_one_mouse_across_port_changes() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct CountingFactory {
            inner: ScriptedFakeFactory,
            opens: Arc<AtomicUsize>,
        }
        #[async_trait::async_trait(?Send)]
        impl crate::backend::SessionFactory for CountingFactory {
            async fn list(
                &self,
                s: TransportSelection,
            ) -> Result<Vec<DiscoveredEndpoint>, ManagerError> {
                self.inner.list(s).await
            }
            async fn open(
                &self,
                e: &DeviceEndpoint,
            ) -> Result<Box<dyn DeviceSession>, ManagerError> {
                self.opens.fetch_add(1, Ordering::SeqCst);
                self.inner.open(e).await
            }
        }
        let store = StateStore::memory();
        let port_a = wired_endpoint_named(r"\\?\hid#legacy-port-a", "SAME-SN");
        let port_b = wired_endpoint_named(r"\\?\hid#legacy-port-b", "SAME-SN");
        let opens = Arc::new(AtomicUsize::new(0));
        let inner = ScriptedFakeFactory::new().with_discovery_sequence(vec![
            vec![make_discovered(port_a.clone(), true)],
            vec![make_discovered(port_b.clone(), true)],
        ]);
        let factory = Arc::new(CountingFactory {
            inner,
            opens: opens.clone(),
        });
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);

        let first = manager.discover(TransportSelection::Auto).await.unwrap();
        assert_eq!(first.mode, IdentityMode::Legacy);
        assert_eq!(
            first.devices.len(),
            1,
            "legacy discovery creates exactly one fuzzy logical mouse"
        );
        let id = first.devices[0].identity.id.clone();

        let second = manager.discover(TransportSelection::Auto).await.unwrap();
        assert_eq!(
            second.devices.len(),
            1,
            "a port change never allocates another logical mouse"
        );
        assert_eq!(second.devices[0].identity.id, id);
        assert_eq!(
            second.devices[0]
                .identity
                .endpoint(TransportKind::Wired)
                .unwrap()
                .locator,
            DeviceLocator::UsbPath(r"\\?\hid#legacy-port-b".to_string()),
            "the port change updates the fuzzy mouse's available connection"
        );
        assert_eq!(
            opens.load(Ordering::SeqCst),
            0,
            "legacy discovery must never read a watermark"
        );
    }

    #[tokio::test]
    async fn persistent_discovery_resolves_known_token_to_its_logical_mouse() {
        let store = StateStore::memory();
        let token = PhysicalId::from_token_bytes([0x11; 16]);
        let ep = wired_endpoint_named(r"\\?\hid#known-token", "KNOWN");
        let id = insert_persistent_device(&store, ep.clone(), token);
        let factory = Arc::new(ScriptedFakeFactory::new().with_endpoint(
            make_discovered(ep.clone(), true),
            session_with_watermark(token),
        ));
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);

        let view = manager.discover(TransportSelection::Auto).await.unwrap();
        assert_eq!(view.mode, IdentityMode::Persistent);
        assert_eq!(view.devices.len(), 1);
        assert_eq!(view.devices[0].identity.id, id);
        assert!(view.devices[0].connected);
        assert_eq!(
            view.connections[0].resolution,
            IdentityResolution::Resolved {
                identity: id.clone()
            }
        );
        assert_eq!(
            view.devices[0].identity.physical_id,
            Some(token),
            "the durable physical identity is preserved"
        );
    }

    #[tokio::test]
    async fn unknown_malformed_unsupported_connections_stay_unassociated() {
        let store = StateStore::memory();
        {
            let mut txn = store.transaction().unwrap();
            txn.state_mut().identity_mode = IdentityMode::Persistent;
            txn.commit().unwrap();
        }
        let token = PhysicalId::from_token_bytes([0x42; 16]);
        let ep_unknown = wired_endpoint_named(r"\\?\hid#unassoc-unknown", "U-UNKNOWN");
        let ep_malformed = wired_endpoint_named(r"\\?\hid#unassoc-malformed", "U-MALFORMED");
        let ep_unsupported = wired_endpoint_named(r"\\?\hid#unassoc-unsupported", "U-UNSUPPORTED");
        let mut malformed = token.to_watermark_bytes();
        malformed[5] ^= 0xFF; // corrupt a token byte: the CRC then fails
        let mut unsupported = token.to_watermark_bytes();
        unsupported[4] = 0x7F; // future watermark format version
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(
                    make_discovered(ep_unknown.clone(), true),
                    session_with_tail(token.to_watermark_bytes()),
                )
                .with_endpoint(
                    make_discovered(ep_malformed.clone(), true),
                    session_with_tail(malformed),
                )
                .with_endpoint(
                    make_discovered(ep_unsupported.clone(), true),
                    session_with_tail(unsupported),
                ),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);

        let view = manager.discover(TransportSelection::Auto).await.unwrap();
        assert!(
            view.devices.is_empty(),
            "unassociated connections are never persisted as logical mice"
        );
        let resolution_for = |path: &str| {
            view.connections
                .iter()
                .find(|c| c.endpoint.locator == DeviceLocator::UsbPath(path.to_string()))
                .expect("connection present")
                .resolution
                .clone()
        };
        assert_eq!(
            resolution_for(r"\\?\hid#unassoc-unknown"),
            IdentityResolution::Unassociated {
                reason: UnassociatedReason::Unknown,
                physical_id: Some(token),
            }
        );
        assert_eq!(
            resolution_for(r"\\?\hid#unassoc-malformed"),
            IdentityResolution::Unassociated {
                reason: UnassociatedReason::Malformed,
                physical_id: None,
            }
        );
        assert_eq!(
            resolution_for(r"\\?\hid#unassoc-unsupported"),
            IdentityResolution::Unassociated {
                reason: UnassociatedReason::Unsupported { version: 0x7F },
                physical_id: None,
            }
        );
        assert!(store.load().unwrap().devices.is_empty());
    }

    #[tokio::test]
    async fn same_token_on_wired_and_receiver_is_not_a_duplicate() {
        let store = StateStore::memory();
        {
            let mut txn = store.transaction().unwrap();
            txn.state_mut().identity_mode = IdentityMode::Persistent;
            txn.commit().unwrap();
        }
        let token = PhysicalId::from_token_bytes([0x77; 16]);
        let ep_wired = wired_endpoint_named(r"\\?\hid#same-token-wired", "SAME-TOKEN");
        let ep_receiver = receiver_endpoint(r"\\?\hid#same-token-receiver");
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(
                    make_discovered(ep_wired.clone(), true),
                    session_with_watermark(token),
                )
                .with_endpoint(
                    make_discovered(ep_receiver.clone(), true),
                    session_with_watermark(token),
                ),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);

        let view = manager.discover(TransportSelection::Auto).await.unwrap();
        assert!(view.devices.is_empty());
        for connection in &view.connections {
            assert_eq!(
                connection.resolution,
                IdentityResolution::Unassociated {
                    reason: UnassociatedReason::Unknown,
                    physical_id: Some(token),
                },
                "the same unknown token across wired + receiver is one physical mouse, not a conflict"
            );
        }
    }

    #[tokio::test]
    async fn same_transport_duplicate_token_conflicts() {
        let store = StateStore::memory();
        {
            let mut txn = store.transaction().unwrap();
            txn.state_mut().identity_mode = IdentityMode::Persistent;
            txn.commit().unwrap();
        }
        let token = PhysicalId::from_token_bytes([0x88; 16]);
        let ep_a = wired_endpoint_named(r"\\?\hid#duplicate-a", "DUP-TOKEN");
        let ep_b = wired_endpoint_named(r"\\?\hid#duplicate-b", "DUP-TOKEN");
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(
                    make_discovered(ep_a.clone(), true),
                    session_with_watermark(token),
                )
                .with_endpoint(
                    make_discovered(ep_b.clone(), true),
                    session_with_watermark(token),
                ),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);

        let view = manager.discover(TransportSelection::Auto).await.unwrap();
        assert!(view.devices.is_empty());
        assert_eq!(view.connections.len(), 2);
        for connection in &view.connections {
            assert_eq!(
                connection.resolution,
                IdentityResolution::Unassociated {
                    reason: UnassociatedReason::Duplicate,
                    physical_id: Some(token),
                },
                "two same-transport mice carrying one token must both be refused"
            );
        }
    }

    #[tokio::test]
    async fn open_locked_refuses_mismatched_attachment_before_any_dpi_write() {
        let store = StateStore::memory();
        let token_expected = PhysicalId::from_token_bytes([0x91; 16]);
        let token_presented = PhysicalId::from_token_bytes([0x92; 16]);
        let ep = wired_endpoint_named(r"\\?\hid#mismatch-attach", "MISMATCH");
        let id = insert_persistent_device(&store, ep.clone(), token_expected);
        let session = session_with_watermark(token_presented);
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(ep.clone(), true), session.clone()),
        );
        let manager = DeviceManager::with_store_and_factory(store, factory);

        match manager.open_locked(&id, "test").await {
            Err(ManagerError::AttachmentNotAuthenticated { .. }) => {}
            Err(other) => {
                panic!("expected AttachmentNotAuthenticated for a foreign unit, got {other:?}")
            }
            Ok(_) => panic!("a foreign unit at the endpoint must not pass the attachment gate"),
        }
        assert!(
            session.writes().is_empty(),
            "refusal must happen before any DPI (or other) write reaches the wire"
        );
    }

    #[tokio::test]
    async fn initial_enrollment_ceremony_completes_two_mice() {
        let store = StateStore::memory();
        insert_device_with_endpoint(&store, wired_endpoint_named(r"\\?\hid#legacy", "LEGACY"));
        let ep_a = wired_endpoint_named(r"\\?\hid#enroll-a", "ENROLL-A");
        let ep_b = wired_endpoint_named(r"\\?\hid#enroll-b", "ENROLL-B");
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(ep_a.clone(), true), unmarked_session())
                .with_endpoint(make_discovered(ep_b.clone(), true), unmarked_session()),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);

        let progress = manager
            .identity_ceremony_action(
                IdentityCeremonyAction::Begin(IdentityCeremonyKind::InitialEnrollment),
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(progress.stage, IdentityCeremonyStage::AwaitingReconnect);

        let progress = manager
            .identity_ceremony_action(
                IdentityCeremonyAction::Reconnected,
                Some(ep_a.clone()),
                None,
            )
            .await
            .unwrap();
        assert_eq!(progress.stage, IdentityCeremonyStage::Stamping);

        let progress = manager
            .identity_ceremony_action(IdentityCeremonyAction::Stamp, None, None)
            .await
            .unwrap();
        assert_eq!(
            progress.stage,
            IdentityCeremonyStage::AwaitingReconnect,
            "after the first mouse is stamped the ceremony awaits the second"
        );

        let progress = manager
            .identity_ceremony_action(
                IdentityCeremonyAction::Reconnected,
                Some(ep_b.clone()),
                None,
            )
            .await
            .unwrap();
        assert_eq!(progress.stage, IdentityCeremonyStage::Stamping);

        let progress = manager
            .identity_ceremony_action(IdentityCeremonyAction::Stamp, None, None)
            .await
            .unwrap();
        assert_eq!(
            progress.stage,
            IdentityCeremonyStage::Complete,
            "after both mice are stamped, first-time setup parks at Finalizing for the migration decision"
        );
        assert_eq!(progress.step, Some(2));
        // The pending choice is durable: the journal persists at `Finalizing`
        // with both subjects fully stamped until a migration choice runs.
        let state = manager.store().load().unwrap();
        let journal = state
            .identity_setup
            .as_ref()
            .expect("the journal persists awaiting the migration decision");
        assert_eq!(journal.stage, IdentitySetupStage::Finalizing);
        assert_eq!(journal.subjects.len(), 2);
        assert!(
            journal
                .subjects
                .iter()
                .all(|subject| subject.is_fully_stamped()),
            "a Finalizing journal holds only fully stamped subjects"
        );

        // The migration decision finalizes the enrollment in the same call.
        let progress = manager
            .identity_ceremony_action(IdentityCeremonyAction::AcceptMigration, None, None)
            .await
            .unwrap();
        assert_eq!(progress.stage, IdentityCeremonyStage::Complete);

        let state = manager.store().load().unwrap();
        assert_eq!(state.identity_mode, IdentityMode::Persistent);
        assert_eq!(state.devices.len(), 2);
        assert!(state.identity_setup.is_none());
        assert!(
            state
                .devices
                .values()
                .all(|device| device.identity.display_name.as_deref() != Some("LEGACY")),
            "a non-matching legacy identity must not migrate to either captured mouse"
        );
        assert!(
            state.devices.values().all(|device| matches!(
                device.identity.display_name.as_deref(),
                Some("Mouse 1") | Some("Mouse 2")
            )),
            "accepted enrollment with no legacy match uses fresh Mouse N names"
        );
        let tokens: Vec<PhysicalId> = state
            .devices
            .values()
            .map(|device| {
                device
                    .identity
                    .physical_id
                    .expect("enrolled mouse has a token")
            })
            .collect();
        assert_ne!(tokens[0], tokens[1], "each mouse mints its own token");
        for device in state.devices.values() {
            assert!(
                device.profiles.contains_key(&ProfileId::new(1).unwrap()),
                "captured profile evidence is applied to the enrolled mouse"
            );
            assert!(device.identity.physical_id.is_some());
        }
    }

    #[tokio::test]
    async fn migration_decision_is_refused_until_both_mice_are_stamped() {
        let store = StateStore::memory();
        insert_device_with_endpoint(&store, wired_endpoint_named(r"\\?\hid#legacy", "LEGACY"));
        let ep_a = wired_endpoint_named(r"\\?\hid#migrate-a", "MIGRATE-A");
        let ep_b = wired_endpoint_named(r"\\?\hid#migrate-b", "MIGRATE-B");
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(ep_a.clone(), true), unmarked_session())
                .with_endpoint(make_discovered(ep_b.clone(), true), unmarked_session()),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);

        async fn assert_refused(
            manager: &DeviceManager,
            action: IdentityCeremonyAction,
        ) -> ManagerError {
            manager
                .identity_ceremony_action(action, None, None)
                .await
                .expect_err("migration decision requires a captured and stamped ceremony")
        }

        // No ceremony yet: nothing to finalize.
        match assert_refused(&manager, IdentityCeremonyAction::AcceptMigration).await {
            ManagerError::NoIdentityCeremony => {}
            other => panic!("expected NoIdentityCeremony, got {other:?}"),
        }

        manager
            .identity_ceremony_action(
                IdentityCeremonyAction::Begin(IdentityCeremonyKind::InitialEnrollment),
                None,
                None,
            )
            .await
            .unwrap();
        // Awaiting the first reconnect: not ready.
        match assert_refused(&manager, IdentityCeremonyAction::SkipMigration).await {
            ManagerError::InvalidCeremonyAction { action, .. } => {
                assert_eq!(action, IdentityCeremonyAction::SkipMigration)
            }
            other => panic!("expected InvalidCeremonyAction at AwaitingCapture, got {other:?}"),
        }

        manager
            .identity_ceremony_action(
                IdentityCeremonyAction::Reconnected,
                Some(ep_a.clone()),
                None,
            )
            .await
            .unwrap();
        // One mouse captured, none stamped: not ready.
        match assert_refused(&manager, IdentityCeremonyAction::AcceptMigration).await {
            ManagerError::InvalidCeremonyAction { action, .. } => {
                assert_eq!(action, IdentityCeremonyAction::AcceptMigration)
            }
            other => panic!("expected InvalidCeremonyAction with one subject, got {other:?}"),
        }

        manager
            .identity_ceremony_action(IdentityCeremonyAction::Stamp, None, None)
            .await
            .unwrap();
        manager
            .identity_ceremony_action(
                IdentityCeremonyAction::Reconnected,
                Some(ep_b.clone()),
                None,
            )
            .await
            .unwrap();
        // Both mice captured, but the second mouse is not stamped yet: not
        // ready, and the refusal must not have finalized or mutated anything.
        match assert_refused(&manager, IdentityCeremonyAction::AcceptMigration).await {
            ManagerError::InvalidCeremonyAction { action, .. } => {
                assert_eq!(action, IdentityCeremonyAction::AcceptMigration)
            }
            other => panic!("expected InvalidCeremonyAction before stamps, got {other:?}"),
        }
        let state = manager.store().load().unwrap();
        assert!(
            state.identity_setup.is_some(),
            "a refused migration decision leaves the journal in flight"
        );
        assert_eq!(state.devices.len(), 1, "no finalize happened on refusal");
        assert_eq!(state.identity_mode, IdentityMode::Legacy);
    }

    #[tokio::test]
    async fn accepted_migration_applies_unique_legacy_match_in_the_same_call() {
        let store = StateStore::memory();
        insert_device_with_endpoint(&store, wired_endpoint_named(r"\\?\hid#legacy", "LEGACY"));
        let ep_a = wired_endpoint_named(r"\\?\hid#legacy-match-a", "LEGACY-MATCH-A");
        let ep_b = wired_endpoint_named(r"\\?\hid#legacy-match-b", "LEGACY-MATCH-B");
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(ep_a.clone(), true), unmarked_session())
                .with_endpoint(make_discovered(ep_b.clone(), true), unmarked_session()),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);

        manager
            .identity_ceremony_action(
                IdentityCeremonyAction::Begin(IdentityCeremonyKind::InitialEnrollment),
                None,
                None,
            )
            .await
            .unwrap();
        manager
            .identity_ceremony_action(
                IdentityCeremonyAction::Reconnected,
                Some(ep_a.clone()),
                None,
            )
            .await
            .unwrap();
        manager
            .identity_ceremony_action(IdentityCeremonyAction::Stamp, None, None)
            .await
            .unwrap();
        manager
            .identity_ceremony_action(
                IdentityCeremonyAction::Reconnected,
                Some(ep_b.clone()),
                None,
            )
            .await
            .unwrap();
        manager
            .identity_ceremony_action(IdentityCeremonyAction::Stamp, None, None)
            .await
            .unwrap();

        // Seed the legacy mouse with the exact durable evidence captured from
        // the first enrolled mouse, so the unique-match rule applies.
        {
            let mut txn = store.transaction().unwrap();
            let captured = txn
                .state()
                .identity_setup
                .as_ref()
                .unwrap()
                .subjects
                .first()
                .unwrap()
                .captured
                .clone()
                .expect("the first subject was captured");
            let mut legacy = DeviceState::new(DeviceIdentity::new(
                DeviceId::new("mouse-1").unwrap(),
                Some("Desk Mouse".to_string()),
            ));
            legacy.profile_metadata = captured.profile_metadata;
            legacy.profiles = captured.profiles;
            txn.state_mut().devices.clear();
            txn.state_mut()
                .devices
                .insert(DeviceId::new("mouse-1").unwrap(), legacy);
            txn.commit().unwrap();
        }

        // Skip: fresh names despite the unique match.
        let progress = manager
            .identity_ceremony_action(IdentityCeremonyAction::SkipMigration, None, None)
            .await
            .unwrap();
        assert_eq!(progress.stage, IdentityCeremonyStage::Complete);
        let state = manager.store().load().unwrap();
        assert!(state.identity_setup.is_none());
        assert!(
            state.devices.values().all(|device| matches!(
                device.identity.display_name.as_deref(),
                Some("Mouse 1") | Some("Mouse 2")
            )),
            "skip migration must use fresh names even on a unique match"
        );
    }

    #[tokio::test]
    async fn stamp_confirms_partially_marked_foreign_adoption() {
        let store = StateStore::memory();
        {
            let mut txn = store.transaction().unwrap();
            txn.state_mut().identity_mode = IdentityMode::Persistent;
            txn.commit().unwrap();
        }
        let token = PhysicalId::from_token_bytes([0x77; 16]);
        let ep = wired_endpoint_named(r"\\?\hid#foreign-partial", "FOREIGN-PARTIAL");
        // Only the current profile (1) carries the foreign token; the other
        // profiles are unmarked, so the adoption needs a confirmation stamp.
        let mut session = ScriptedFakeSession::usb()
            .with_metadata(
                ProfileMetadata::new(ProfileId::new(1).unwrap(), ProfileId::new(5).unwrap())
                    .unwrap(),
            )
            .with_polling_rate(PollingRate::Hz1000);
        for target in ProfileId::MIN..=ProfileId::MAX {
            let target = ProfileId::new(target).expect("test profile must be valid");
            let tail = if target == ProfileId::new(1).unwrap() {
                token.to_watermark_bytes()
            } else {
                [0_u8; 25]
            };
            session = session.with_profile(snapshot_with_tail(target, tail));
        }
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(ep.clone(), true), session.clone()),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);

        let progress = manager
            .identity_ceremony_action(
                IdentityCeremonyAction::Begin(IdentityCeremonyKind::ForeignAdoption),
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(progress.stage, IdentityCeremonyStage::AwaitingReconnect);

        // `identity adopt` equivalent: begin only. Reconnect captures and
        // reserves the foreign token; profiles 2..=5 await the confirmation
        // stamp.
        let progress = manager
            .identity_ceremony_action(IdentityCeremonyAction::Reconnected, Some(ep.clone()), None)
            .await
            .unwrap();
        assert_eq!(progress.stage, IdentityCeremonyStage::Stamping);

        // `identity stamp` confirms the adoption: it writes only the unmarked
        // profiles and finalizes, preserving journal-before-write.
        let progress = manager
            .identity_ceremony_action(IdentityCeremonyAction::Stamp, None, None)
            .await
            .unwrap();
        assert_eq!(progress.stage, IdentityCeremonyStage::Complete);

        let dpi_writes: Vec<u8> = session
            .writes()
            .into_iter()
            .filter_map(|write| match write {
                ScriptedWrite::Dpi(dpi) => Some(dpi.profile.get()),
                _ => None,
            })
            .collect();
        assert_eq!(
            dpi_writes,
            vec![2, 3, 4, 5],
            "adoption confirmation stamps only profiles that do not carry the token"
        );
        let state = manager.store().load().unwrap();
        assert!(state.identity_setup.is_none());
        assert_eq!(state.devices.len(), 1);
        let adopted = state.devices.values().next().unwrap();
        assert_eq!(
            adopted.identity.physical_id,
            Some(token),
            "the foreign token is adopted, not replaced by a minted one"
        );
    }
    #[tokio::test]
    async fn interrupted_stamp_resumes_without_restamping_completed_profiles() {
        let store = StateStore::memory();
        let existing_token = PhysicalId::from_token_bytes([0x31; 16]);
        let ep_existing = wired_endpoint_named(r"\\?\hid#resume-existing", "EXISTING");
        let existing_id = insert_persistent_device(&store, ep_existing.clone(), existing_token);
        let ep_new = wired_endpoint_named(r"\\?\hid#resume-new", "NEW-MOUSE");
        let session_new = unmarked_session();
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(ep_new.clone(), true), session_new.clone()),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);

        manager
            .identity_ceremony_action(
                IdentityCeremonyAction::Begin(IdentityCeremonyKind::AddMouse),
                None,
                None,
            )
            .await
            .unwrap();
        manager
            .identity_ceremony_action(
                IdentityCeremonyAction::Reconnected,
                Some(ep_new.clone()),
                None,
            )
            .await
            .unwrap();

        // Simulate an interruption after the first profile was stamped: the
        // durable journal records profile 1 as `Stamped` before the process
        // stopped. Resuming must never re-stamp it.
        {
            let mut txn = store.transaction().unwrap();
            let journal = txn.state_mut().identity_setup.as_mut().unwrap();
            let subject = journal.subjects.last_mut().unwrap();
            // The token was minted and reserved before the first profile was
            // stamped; an interrupted resume must keep it and never re-stamp.
            subject.token = Some(PhysicalId::from_token_bytes([0x42; 16]));
            subject
                .stamp_progress
                .insert(ProfileId::new(1).unwrap(), IdentityStampProgress::Stamped);
            txn.commit().unwrap();
        }

        let progress = manager
            .identity_ceremony_action(IdentityCeremonyAction::Stamp, None, None)
            .await
            .unwrap();
        assert_eq!(progress.stage, IdentityCeremonyStage::Complete);

        let dpi_writes: Vec<u8> = session_new
            .writes()
            .into_iter()
            .filter_map(|write| match write {
                ScriptedWrite::Dpi(dpi) => Some(dpi.profile.get()),
                _ => None,
            })
            .collect();
        assert_eq!(
            dpi_writes,
            vec![2, 3, 4, 5],
            "resume stamps only the profiles the journal left pending"
        );

        let state = manager.store().load().unwrap();
        assert!(state.identity_setup.is_none());
        assert_eq!(state.devices.len(), 2);
        let new_device = state
            .devices
            .values()
            .find(|device| device.identity.id != existing_id)
            .expect("the enrolled mouse was finalized");
        assert!(new_device.identity.physical_id.is_some());
        assert!(new_device.identity.has_endpoint(TransportKind::Wired));
    }

    #[tokio::test]
    async fn restore_rotates_token_replaces_capture_and_clears_old_endpoints() {
        let store = StateStore::memory();
        let old_token = PhysicalId::from_token_bytes([0x41; 16]);
        let ep_old = wired_endpoint_named(r"\\?\hid#restore-old", "OLD-UNIT");
        let ep_ble = DeviceEndpoint::ble("ble-restore-old", None).unwrap();
        let mut txn = store.transaction().unwrap();
        let id = txn.state_mut().allocate_device_id().unwrap();
        let mut identity = DeviceIdentity::new(id.clone(), None);
        identity.physical_id = Some(old_token);
        identity.upsert_endpoint(ep_old.clone());
        identity.upsert_endpoint(ep_ble.clone());
        txn.state_mut()
            .devices
            .insert(id.clone(), crate::state::DeviceState::new(identity));
        txn.state_mut().identity_mode = IdentityMode::Persistent;
        txn.state_mut().selected_device = Some(id.clone());
        txn.commit().unwrap();

        // The old unit is gone; only the reconnected new unit is listed.
        let ep_new = wired_endpoint_named(r"\\?\hid#restore-new", "NEW-UNIT");
        let session_new = unmarked_session();
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(ep_new.clone(), true), session_new.clone()),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);

        let progress = manager
            .identity_ceremony_action(
                IdentityCeremonyAction::Begin(IdentityCeremonyKind::Restore),
                None,
                Some(id.clone()),
            )
            .await
            .unwrap();
        assert_eq!(progress.stage, IdentityCeremonyStage::AwaitingReconnect);

        let progress = manager
            .identity_ceremony_action(
                IdentityCeremonyAction::Reconnected,
                Some(ep_new.clone()),
                Some(id.clone()),
            )
            .await
            .unwrap();
        assert_eq!(progress.stage, IdentityCeremonyStage::Stamping);

        let progress = manager
            .identity_ceremony_action(IdentityCeremonyAction::Stamp, None, None)
            .await
            .unwrap();
        assert_eq!(progress.stage, IdentityCeremonyStage::Complete);

        let state = manager.store().load().unwrap();
        assert!(state.identity_setup.is_none());
        let device = &state.devices[&id];
        assert_eq!(
            device.identity.id, id,
            "restore keeps the same logical mouse"
        );
        assert!(
            device.identity.physical_id.is_some() && device.identity.physical_id != Some(old_token),
            "restore rotates in a fresh token"
        );
        assert_eq!(
            device.identity.endpoints.len(),
            1,
            "the old BLE and USB endpoints are cleared"
        );
        assert!(!device.identity.has_endpoint(TransportKind::Ble));
        assert_eq!(
            device
                .identity
                .endpoint(TransportKind::Wired)
                .unwrap()
                .locator,
            DeviceLocator::UsbPath(r"\\?\hid#restore-new".to_string())
        );
        assert!(
            device.profiles.contains_key(&ProfileId::new(1).unwrap()),
            "the restored unit's capture replaces the durable evidence"
        );
        assert!(
            session_new.writes().len() >= 5,
            "every profile was re-stamped"
        );
    }

    #[tokio::test]
    async fn foreign_adoption_preserves_the_observed_token() {
        let store = StateStore::memory();
        {
            let mut txn = store.transaction().unwrap();
            txn.state_mut().identity_mode = IdentityMode::Persistent;
            txn.commit().unwrap();
        }
        let token = PhysicalId::from_token_bytes([0x55; 16]);
        let ep = wired_endpoint_named(r"\\?\hid#foreign", "FOREIGN");
        let session = session_with_watermark(token);
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(ep.clone(), true), session.clone()),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);

        let progress = manager
            .identity_ceremony_action(
                IdentityCeremonyAction::Begin(IdentityCeremonyKind::ForeignAdoption),
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(progress.stage, IdentityCeremonyStage::AwaitingReconnect);

        let progress = manager
            .identity_ceremony_action(IdentityCeremonyAction::Reconnected, Some(ep.clone()), None)
            .await
            .unwrap();
        assert_eq!(
            progress.stage,
            IdentityCeremonyStage::Complete,
            "a fully stamped foreign mouse is adopted without further input"
        );

        assert!(
            session
                .writes()
                .iter()
                .all(|write| !matches!(write, ScriptedWrite::Dpi(_))),
            "adoption of an already-tagged mouse never re-stamps its profiles"
        );
        let state = manager.store().load().unwrap();
        assert!(state.identity_setup.is_none());
        assert_eq!(state.devices.len(), 1);
        let adopted = state.devices.values().next().unwrap();
        assert_eq!(
            adopted.identity.physical_id,
            Some(token),
            "the foreign token is adopted, not replaced by a minted one"
        );
    }

    #[tokio::test]
    async fn ble_association_is_explicit() {
        let store = StateStore::memory();
        let token = PhysicalId::from_token_bytes([0x66; 16]);
        let ep_usb = wired_endpoint_named(r"\\?\hid#ble-mouse", "BLE-MOUSE");
        let id = insert_persistent_device(&store, ep_usb.clone(), token);
        let ep_ble = DeviceEndpoint::ble("ble-platform-9", None).unwrap();
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(
                    make_discovered(ep_usb.clone(), true),
                    session_with_watermark(token),
                )
                .with_endpoint(
                    make_discovered(ep_ble.clone(), true),
                    ScriptedFakeSession::ble(),
                ),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), factory);

        // A BLE connection is never associated implicitly.
        let view = manager.discover(TransportSelection::Auto).await.unwrap();
        let ble_connection = view
            .connections
            .iter()
            .find(|connection| connection.endpoint.transport == TransportKind::Ble)
            .expect("the BLE connection is listed");
        assert_eq!(
            ble_connection.resolution,
            IdentityResolution::Unassociated {
                reason: UnassociatedReason::Absent,
                physical_id: None,
            }
        );

        // Explicit association binds the platform id to the logical mouse.
        let progress = manager
            .identity_ceremony_action(
                IdentityCeremonyAction::Begin(IdentityCeremonyKind::BleAssociation),
                None,
                Some(id.clone()),
            )
            .await
            .unwrap();
        assert_eq!(progress.stage, IdentityCeremonyStage::AwaitingReconnect);
        let progress = manager
            .identity_ceremony_action(
                IdentityCeremonyAction::Associate,
                Some(ep_ble.clone()),
                Some(id.clone()),
            )
            .await
            .unwrap();
        assert_eq!(progress.stage, IdentityCeremonyStage::Complete);

        let state = manager.store().load().unwrap();
        assert!(state.identity_setup.is_none());
        assert_eq!(
            state.devices[&id]
                .identity
                .endpoint(TransportKind::Ble)
                .unwrap()
                .locator,
            ep_ble.locator,
            "the associated BLE endpoint is stored on the logical mouse"
        );

        // The same BLE connection now resolves to the logical mouse.
        let view = manager.discover(TransportSelection::Auto).await.unwrap();
        let ble_connection = view
            .connections
            .iter()
            .find(|connection| connection.endpoint.transport == TransportKind::Ble)
            .expect("the BLE connection is listed");
        assert_eq!(
            ble_connection.resolution,
            IdentityResolution::Resolved {
                identity: id.clone()
            }
        );
    }
}
