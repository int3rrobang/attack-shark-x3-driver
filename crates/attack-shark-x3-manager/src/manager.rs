use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use attack_shark_x3::driver::ProfileSnapshot;
use attack_shark_x3::{ProfileId, ProfileMetadata, TransportKind};

use crate::backend::{DeviceSession, RealSessionFactory, SessionFactory, SessionWrite};
use crate::device::{DeviceEndpoint, DeviceId, DeviceIdentity, TransportSelection};
use crate::error::ManagerError;
use crate::operation::{DeviceStatus, DiscoveredDevice, DiscoveredEndpoint, ResourceSnapshot};
use crate::state::{DeviceState, ProfileState, ResourceState, StateStore, Timestamp};

const BATTERY_READ_TIMEOUT: Duration = Duration::from_secs(20);
const OPERATION_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

pub struct DeviceManager {
    store: StateStore,
    pub(crate) factory: Arc<dyn SessionFactory>,
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

    async fn associate_endpoints_async(
        &self,
        discovered: &[DiscoveredEndpoint],
    ) -> Result<(), ManagerError> {
        if discovered.is_empty() {
            return Ok(());
        }
        let discovered = discovered.to_vec();
        self.store
            .try_mutate_async(move |state| {
                for disc in &discovered {
                    let mut found: Option<DeviceId> = None;
                    for (id, dev_state) in state.devices.iter() {
                        if let Some(ep) = dev_state.identity.endpoint(disc.endpoint.transport)
                            && ep.locator == disc.endpoint.locator
                        {
                            found = Some(id.clone());
                            break;
                        }
                    }
                    if let Some(id) = found {
                        let dev_state = state.devices.get_mut(&id).unwrap();
                        dev_state.identity.upsert_endpoint(disc.endpoint.clone());
                        if dev_state.identity.display_name.is_none() {
                            dev_state.identity.display_name = disc.endpoint.display_name.clone();
                        }
                    } else {
                        let new_id = state.allocate_device_id()?;
                        let mut identity =
                            DeviceIdentity::new(new_id.clone(), disc.endpoint.display_name.clone());
                        identity.upsert_endpoint(disc.endpoint.clone());
                        state.devices.insert(new_id, DeviceState::new(identity));
                    }
                }
                Ok(())
            })
            .await?;
        Ok(())
    }

    async fn discovered_to_logical_async(
        &self,
        discovered: &[DiscoveredEndpoint],
    ) -> Result<Vec<DiscoveredDevice>, ManagerError> {
        let state = self.store.load_async().await?;
        let mut result = Vec::new();
        for disc in discovered {
            let mut found_identity: Option<DeviceIdentity> = None;
            for dev_state in state.devices.values() {
                if let Some(ep) = dev_state.identity.endpoint(disc.endpoint.transport)
                    && ep.locator == disc.endpoint.locator
                {
                    found_identity = Some(dev_state.identity.clone());
                    break;
                }
            }
            if let Some(identity) = found_identity {
                result.push(DiscoveredDevice {
                    identity,
                    connected: disc.connected,
                });
            }
        }
        Ok(result)
    }

    pub async fn list_devices(
        &self,
        selection: TransportSelection,
    ) -> Result<Vec<DiscoveredDevice>, ManagerError> {
        let discovered = self.factory.list(selection).await?;
        self.associate_endpoints_async(&discovered).await?;
        self.discovered_to_logical_async(&discovered).await
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
        let discovered_endpoints = self.factory.list(selection).await?;
        self.associate_endpoints_async(&discovered_endpoints)
            .await?;
        let discovered = self
            .discovered_to_logical_async(&discovered_endpoints)
            .await?;

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
            _ => Err(ManagerError::AmbiguousDevice {
                selection,
                candidates,
            }),
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
            state.next_device_number = num + 1;
            if state.next_device_number == 0 {
                return Err(ManagerError::State(
                    crate::error::StateError::invalid_state("nextDeviceNumber overflow"),
                ));
            }
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

    pub fn link_devices(&self, source: &DeviceId, target: &DeviceId) -> Result<(), ManagerError> {
        if source == target {
            return Err(ManagerError::InvalidUpdate(
                "cannot link device to itself".to_string(),
            ));
        }
        let mut txn = self.store.transaction()?;
        let source_state = txn
            .state()
            .devices
            .get(source)
            .cloned()
            .ok_or_else(|| ManagerError::DeviceNotFound(source.clone()))?;
        let target_state = txn
            .state()
            .devices
            .get(target)
            .cloned()
            .ok_or_else(|| ManagerError::DeviceNotFound(target.clone()))?;

        for transport in source_state.identity.endpoints.keys() {
            if target_state.identity.endpoints.contains_key(transport) {
                return Err(ManagerError::InvalidUpdate(format!(
                    "endpoint conflict: transport {transport:?} already present in target {target}"
                )));
            }
        }

        let source_has_evidence = device_has_evidence(&source_state);
        let target_has_evidence = device_has_evidence(&target_state);
        if source_has_evidence && target_has_evidence {
            return Err(ManagerError::InvalidUpdate(format!(
                "both devices have configuration evidence: cannot merge {source} into {target} without precedence"
            )));
        }
        if source_has_evidence {
            return Err(ManagerError::InvalidUpdate(format!(
                "source device {source} has configuration evidence; explicit link would discard profile/resource state"
            )));
        }

        let source_endpoints = source_state.identity.endpoints.clone();
        let target_entry = txn.state_mut().devices.get_mut(target).unwrap();
        for (transport, endpoint) in source_endpoints {
            target_entry.identity.endpoints.insert(transport, endpoint);
        }
        if target_entry.identity.display_name.is_none() {
            target_entry.identity.display_name = source_state.identity.display_name.clone();
        }

        txn.state_mut().devices.remove(source);
        if txn.state().selected_device.as_ref() == Some(source) {
            txn.state_mut().selected_device = Some(target.clone());
        }
        txn.commit()?;
        Ok(())
    }

    pub async fn rebind_missing_endpoint(
        &self,
        device: &DeviceId,
        transport: TransportKind,
    ) -> Result<(), ManagerError> {
        let device_owned = device.clone();
        let state = self.store.load_async().await?;
        let identity = state
            .devices
            .get(&device_owned)
            .ok_or_else(|| ManagerError::DeviceNotFound(device_owned.clone()))?
            .identity
            .clone();
        let stored_endpoint = identity.endpoint(transport).cloned().ok_or_else(|| {
            ManagerError::InvalidUpdate(format!(
                "device {device} has no endpoint for transport {transport:?}"
            ))
        })?;
        let discovered = self
            .factory
            .list(TransportSelection::Exact(transport))
            .await?;
        if discovered
            .iter()
            .any(|d| d.endpoint.locator == stored_endpoint.locator && d.connected)
        {
            return Ok(());
        }
        let mut candidates: Vec<DeviceEndpoint> = discovered
            .into_iter()
            .filter(|d| d.connected && d.endpoint.transport == transport)
            .filter(|d| {
                if is_usb_transport(transport) {
                    d.endpoint.vendor_id == stored_endpoint.vendor_id
                        && d.endpoint.product_id == stored_endpoint.product_id
                } else {
                    true
                }
            })
            .map(|d| d.endpoint)
            .collect();
        candidates.sort_by(|a, b| a.locator.cmp(&b.locator));
        candidates.dedup_by(|a, b| a.locator == b.locator);
        match candidates.len() {
            0 => Err(ManagerError::InvalidUpdate(format!(
                "no candidate for rebind of device {device} transport {transport:?}"
            ))),
            1 => {
                let new_endpoint = candidates.pop().expect("length checked above");
                let device = device.clone();
                self.store
                    .mutate_async(move |state| {
                        state
                            .devices
                            .get_mut(&device)
                            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?
                            .identity
                            .upsert_endpoint(new_endpoint);
                        Ok::<_, ManagerError>(())
                    })
                    .await?
            }
            _ => Err(ManagerError::InvalidUpdate(format!(
                "ambiguous candidates for rebind of device {device} transport {transport:?}: {} candidates with same VID/PID",
                candidates.len()
            ))),
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
            .acquire_operation_lock(device, OPERATION_LOCK_TIMEOUT, operation)?;
        let device_owned = device.clone();
        let state = self.store.load_async().await?;
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
        Ok((identity, session, guard))
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
        if update.is_empty() {
            return Err(ManagerError::InvalidUpdate(
                "profile update must provide at least one field".to_owned(),
            ));
        }
        if update.has_polling() && update.has_non_rate() {
            return Err(ManagerError::InvalidUpdate(
                "polling rate cannot be combined with DPI/preferences/buttons; report 0x06 must remain isolated".to_owned(),
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

        let (_identity, session, _guard) = self.open_locked(device, "apply_profile_update").await?;
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
                    crate::resources::settings::persist_polling_write(
                        state,
                        &device_id,
                        profile,
                        desired_rate,
                        write,
                        now,
                    )
                })
                .await??;
            let finished = crate::resources::settings::finish_polling_write(outcome, profile)?;
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
                    crate::operation::BaselineSource::Live => session_ref.read_dpi(profile).await?,
                    crate::operation::BaselineSource::Stored => {
                        self.load_stored_dpi_baseline(device, profile, false)
                            .await?
                    }
                },
            };
            let desired = crate::resources::dpi::merge_dpi_delta(baseline, &delta)?;
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
                        session_ref.read_preferences(profile).await?
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
                        session_ref.read_buttons(profile).await?
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
                    let out = crate::resources::dpi::persist_dpi_write(
                        state, &device_id, profile, desired, write, now,
                    )?;
                    dpi_out = Some(out);
                }
                if let Some((desired, write)) = prefs_pair {
                    let out = crate::resources::settings::persist_preferences_write(
                        state, &device_id, profile, desired, write, now,
                    )?;
                    prefs_out = Some(out);
                }
                if let Some((desired, write)) = buttons_pair {
                    let out = crate::resources::buttons::persist_buttons_write(
                        state, &device_id, desired, write, now,
                    )?;
                    btn_out = Some(out);
                }
                Ok::<_, ManagerError>((dpi_out, prefs_out, btn_out))
            })
            .await??;

        let dpi_final = if let Some(out) = dpi_outcome {
            Some(crate::resources::dpi::finish_dpi_write(out, profile)?)
        } else {
            None
        };
        let prefs_final = if let Some(out) = prefs_outcome {
            Some(crate::resources::settings::finish_preferences_write(
                out, profile,
            )?)
        } else {
            None
        };
        let buttons_final = if let Some(out) = buttons_outcome {
            if out.verification.application == crate::state::ApplicationVerification::Mismatch {
                return Err(ManagerError::VerificationMismatch {
                    resource: "buttons",
                    profile: Some(profile),
                });
            }
            Some(out)
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
#[cfg(test)]
mod tests {
    use super::DeviceManager;
    use crate::backend::{DeviceSession, ScriptedFakeFactory, ScriptedFakeSession, SessionWrite};
    use crate::device::{
        DeviceEndpoint, DeviceId, DeviceIdentity, DeviceLocator, TransportSelection,
    };
    use crate::error::ManagerError;
    use crate::operation::DiscoveredEndpoint;
    use crate::state::{
        ApplicationVerification, DesiredSource, DesiredState, StatePaths, StateStore, Verification,
    };
    use attack_shark_x3::{
        ButtonAssignment, ButtonsState, DpiState, DpiValue, PollingRate, PreferencesState,
        ProfileId, ProfileMetadata, StageIndex, TransportKind,
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
        let store = StateStore::memory();
        let ep_a = wired_endpoint_named(r"\\?\hid#explicit-a", "EXPLICIT-A");
        let ep_b = wired_endpoint_named(r"\\?\hid#explicit-b", "EXPLICIT-B");
        let id_a = insert_device_with_endpoint(&store, ep_a.clone());
        let id_b = insert_device_with_endpoint(&store, ep_b.clone());

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
            .resolve_device(Some(&id_b), TransportSelection::Auto)
            .await
            .unwrap();

        assert_eq!(selected, id_b);
        let state = manager.store().load().unwrap();
        assert!(state.devices.contains_key(&id_a));
        assert!(state.devices.contains_key(&id_b));
    }

    #[tokio::test]
    async fn resolve_device_honors_connected_stored_selection() {
        let store = StateStore::memory();
        let ep_a = wired_endpoint_named(r"\\?\hid#stored-a", "STORED-A");
        let ep_b = wired_endpoint_named(r"\\?\hid#stored-b", "STORED-B");
        let id_a = insert_device_with_endpoint(&store, ep_a.clone());
        let id_b = insert_device_with_endpoint(&store, ep_b.clone());
        {
            let mut txn = store.transaction().unwrap();
            txn.state_mut().selected_device = Some(id_b.clone());
            txn.commit().unwrap();
        }
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(ep_a, true), ScriptedFakeSession::usb())
                .with_endpoint(make_discovered(ep_b, true), ScriptedFakeSession::usb()),
        );
        let manager = DeviceManager::with_store_and_factory(store, factory);

        assert_eq!(
            manager
                .resolve_device(None, TransportSelection::Auto)
                .await
                .unwrap(),
            id_b
        );
        assert!(manager.store().load().unwrap().devices.contains_key(&id_a));
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
        let ep_a = wired_endpoint_named(r"\\?\hid#ambig-a", "AMBIGUOUS-A");
        let ep_b = wired_endpoint_named(r"\\?\hid#ambig-b", "AMBIGUOUS-B");
        let factory = Arc::new(
            ScriptedFakeFactory::new()
                .with_endpoint(make_discovered(ep_a, true), ScriptedFakeSession::usb())
                .with_endpoint(make_discovered(ep_b, true), ScriptedFakeSession::usb()),
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
                assert_eq!(candidates.len(), 2);
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
        let first_id = txn.state_mut().allocate_device_id().unwrap();
        txn.commit().unwrap();
        let first = DeviceIdentity::new(first_id.clone(), None).with_endpoint(endpoint);
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
        let second = DeviceIdentity::new(second_id.clone(), Some("Second".to_string()))
            .with_endpoint(second_endpoint);
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
    async fn port_change_rebinding_updates_locator_when_unique() {
        let store = StateStore::memory();
        let original = DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa60,
            None,
            "/dev/hidraw0",
            None,
        )
        .unwrap();
        let id = insert_device_with_endpoint(&store, original.clone());
        let new_candidate = DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa60,
            None,
            "/dev/hidraw1",
            None,
        )
        .unwrap();
        let factory = ScriptedFakeFactory::new().with_endpoint(
            make_discovered(new_candidate.clone(), true),
            ScriptedFakeSession::usb(),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), Arc::new(factory));
        manager
            .rebind_missing_endpoint(&id, TransportKind::Wired)
            .await
            .unwrap();
        let updated = manager.device_identity(&id).unwrap();
        assert_eq!(
            updated.endpoint(TransportKind::Wired).unwrap().locator,
            DeviceLocator::UsbPath("/dev/hidraw1".to_string())
        );
        assert_eq!(updated.id, id);
    }

    #[tokio::test]
    async fn ambiguous_same_model_refusal_does_not_guess() {
        let store = StateStore::memory();
        let original = DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa60,
            None,
            "/dev/hidraw0",
            None,
        )
        .unwrap();
        let id = insert_device_with_endpoint(&store, original.clone());
        let cand1 = DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa60,
            None,
            "/dev/hidraw1",
            None,
        )
        .unwrap();
        let cand2 = DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa60,
            None,
            "/dev/hidraw2",
            None,
        )
        .unwrap();
        let factory = ScriptedFakeFactory::new()
            .with_endpoint(make_discovered(cand1, true), ScriptedFakeSession::usb())
            .with_endpoint(make_discovered(cand2, true), ScriptedFakeSession::usb());
        let manager = DeviceManager::with_store_and_factory(store.clone(), Arc::new(factory));
        let result = manager
            .rebind_missing_endpoint(&id, TransportKind::Wired)
            .await;
        assert!(
            result.is_err(),
            "ambiguous same VID/PID candidates must fail"
        );
        let still = manager.device_identity(&id).unwrap();
        assert_eq!(
            still.endpoint(TransportKind::Wired).unwrap().locator,
            DeviceLocator::UsbPath("/dev/hidraw0".to_string())
        );
    }

    #[tokio::test]
    async fn cross_transport_non_auto_merge_allocates_separate_devices() {
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
                    DiscoveredEndpoint {
                        endpoint: e_wired.clone(),
                        connected: true,
                    },
                    ScriptedFakeSession::usb(),
                )
                .with_endpoint(
                    DiscoveredEndpoint {
                        endpoint: e_receiver.clone(),
                        connected: true,
                    },
                    ScriptedFakeSession::usb(),
                ),
        );
        let manager = DeviceManager::with_store_and_factory(StateStore::memory(), factory);
        let devices = manager
            .list_devices(TransportSelection::Auto)
            .await
            .unwrap();
        assert_eq!(
            devices.len(),
            2,
            "wired and receiver with same VID/PID must not auto-merge"
        );
        let ids: Vec<DeviceId> = devices.iter().map(|d| d.identity.id.clone()).collect();
        assert_ne!(ids[0], ids[1]);
        for dev in devices {
            assert_eq!(dev.identity.endpoints.len(), 1);
        }
    }

    #[tokio::test]
    async fn explicit_link_moves_endpoints_and_removes_source_without_evidence() {
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
        let id_target = insert_device_with_endpoint(&store, wired.clone());
        let id_source = insert_device_with_endpoint(&store, receiver.clone());

        let manager = DeviceManager::with_store_and_factory(
            store.clone(),
            Arc::new(ScriptedFakeFactory::new()),
        );
        manager.link_devices(&id_source, &id_target).unwrap();

        let state = store.load().unwrap();
        assert!(
            !state.devices.contains_key(&id_source),
            "source without evidence should be removed"
        );
        let target = state.devices.get(&id_target).unwrap();
        assert!(target.identity.has_endpoint(TransportKind::Wired));
        assert!(target.identity.has_endpoint(TransportKind::Receiver));
        assert_eq!(target.identity.id, id_target);
    }

    #[tokio::test]
    async fn explicit_link_rejects_when_source_has_evidence() {
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
        let ble = DeviceEndpoint::ble("ble-1", None).unwrap();
        let id_target = insert_device_with_endpoint(&store, wired.clone());
        let id_source = insert_device_with_endpoint(&store, ble.clone());
        {
            let mut txn = store.transaction().unwrap();
            let dev = txn.state_mut().devices.get_mut(&id_source).unwrap();
            dev.profiles
                .insert(attack_shark_x3::ProfileId::new(1).unwrap(), {
                    let mut ps = crate::state::ProfileState::empty();
                    ps.dpi.desired = Some(DesiredState {
                        value: attack_shark_x3::DpiState::captured_empty_profile_one(
                            vec![attack_shark_x3::DpiValue::new(800).unwrap()],
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
        let manager = DeviceManager::with_store_and_factory(
            store.clone(),
            Arc::new(ScriptedFakeFactory::new()),
        );
        let err = manager.link_devices(&id_source, &id_target).unwrap_err();
        assert!(matches!(err, ManagerError::InvalidUpdate(_)));
        let state = store.load().unwrap();
        assert!(state.devices.contains_key(&id_source));
        assert!(state.devices.contains_key(&id_target));
    }

    #[tokio::test]
    async fn link_rejects_endpoint_conflict() {
        let store = StateStore::memory();
        let wired1 = DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa61,
            None,
            "/dev/hidraw0",
            None,
        )
        .unwrap();
        let wired2 = DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa61,
            None,
            "/dev/hidraw1",
            None,
        )
        .unwrap();
        let id_a = insert_device_with_endpoint(&store, wired1);
        let id_b = insert_device_with_endpoint(&store, wired2);
        let manager = DeviceManager::with_store_and_factory(
            store.clone(),
            Arc::new(ScriptedFakeFactory::new()),
        );
        let err = manager.link_devices(&id_a, &id_b).unwrap_err();
        assert!(matches!(err, ManagerError::InvalidUpdate(_)));
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
    async fn logical_id_persists_across_explicit_link_and_rebind() {
        let store = StateStore::memory();
        let wired0 = DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa61,
            None,
            "/dev/hidraw0",
            None,
        )
        .unwrap();
        let id = insert_device_with_endpoint(&store, wired0.clone());
        let wired1 = DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa61,
            None,
            "/dev/hidraw1",
            None,
        )
        .unwrap();
        let factory = ScriptedFakeFactory::new().with_endpoint(
            make_discovered(wired1.clone(), true),
            ScriptedFakeSession::usb(),
        );
        let manager = DeviceManager::with_store_and_factory(store.clone(), Arc::new(factory));
        manager
            .rebind_missing_endpoint(&id, TransportKind::Wired)
            .await
            .unwrap();
        let after_rebind = manager.device_identity(&id).unwrap();
        assert_eq!(after_rebind.id, id);
        assert_eq!(
            after_rebind.endpoint(TransportKind::Wired).unwrap().locator,
            DeviceLocator::UsbPath("/dev/hidraw1".to_string())
        );

        let ble = DeviceEndpoint::ble("ble-new", None).unwrap();
        let id_ble = insert_device_with_endpoint(&store, ble.clone());
        manager.link_devices(&id_ble, &id).unwrap();
        let after_link = manager.device_identity(&id).unwrap();
        assert_eq!(after_link.id, id);
        assert!(after_link.has_endpoint(TransportKind::Wired));
        assert!(after_link.has_endpoint(TransportKind::Ble));
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
}
