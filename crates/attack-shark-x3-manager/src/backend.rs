use std::time::Duration;
#[cfg(test)]
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
#[cfg(feature = "usb")]
use attack_shark_x3::MouseHandle;
#[cfg(any(feature = "usb", feature = "ble"))]
use attack_shark_x3::driver::ProfileSnapshot;
#[cfg(feature = "ble")]
use attack_shark_x3::{BleDeviceId, BleHandle, BleSelector};
use attack_shark_x3::{
    ButtonsState, DpiState, InputEvent, PollingRate, PreferencesState, ProfileId, ProfileMetadata,
    TransportKind,
};
#[cfg(feature = "usb")]
use attack_shark_x3::{DeviceSelector, UsbDeviceKind, list_devices_for};
use tokio::sync::broadcast;

use crate::device::DeviceLocator;
#[cfg(feature = "ble")]
use crate::error::StateError;
use crate::{
    device::{DeviceEndpoint, TransportSelection},
    error::ManagerError,
    operation::{DiscoveredEndpoint, VerificationMethod},
};

/// The result shape of a typed session write.
///
/// A transport-verified write is acknowledged at acceptance and carries no
/// readback payload. A readback-verified write carries the state confirmed by
/// a fresh post-write read; USB can provide this and BLE cannot, so BLE
/// readback requests are rejected up front.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SessionWrite<T> {
    ReadbackVerified(T),
    Acknowledged,
}

/// Optional decoded report-`0x03` input stream owned by one open session.
///
/// BLE configuration sessions do not expose this stream. USB sessions expose
/// the low-level HID event receiver directly and preserve its transport
/// semantics for the manager event bridge.
pub(crate) struct SessionEvents {
    pub(crate) input: Option<broadcast::Receiver<InputEvent>>,
}

#[async_trait(?Send)]
pub(crate) trait SessionFactory: Send + Sync {
    async fn list(
        &self,
        selection: TransportSelection,
    ) -> Result<Vec<DiscoveredEndpoint>, ManagerError>;

    async fn open(&self, endpoint: &DeviceEndpoint)
    -> Result<Box<dyn DeviceSession>, ManagerError>;
}

#[async_trait(?Send)]
pub(crate) trait DeviceSession: Send + Sync {
    fn transport(&self) -> TransportKind;

    async fn read_profile_metadata(&self) -> Result<ProfileMetadata, ManagerError>;
    async fn read_profile(&self, profile: ProfileId) -> Result<ProfileSnapshot, ManagerError>;
    async fn read_dpi(&self, profile: ProfileId) -> Result<DpiState, ManagerError>;
    async fn read_preferences(&self, profile: ProfileId) -> Result<PreferencesState, ManagerError>;
    async fn read_buttons(&self, profile: ProfileId) -> Result<ButtonsState, ManagerError>;
    async fn read_polling_rate(&self, profile: ProfileId) -> Result<PollingRate, ManagerError>;

    async fn write_dpi(
        &self,
        state: DpiState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<DpiState>, ManagerError>;
    async fn write_preferences(
        &self,
        state: PreferencesState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<PreferencesState>, ManagerError>;
    async fn write_buttons(
        &self,
        state: ButtonsState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<ButtonsState>, ManagerError>;
    /// Submits a polling-rate report through an `unchecked` driver primitive.
    ///
    /// Report `0x06` skips the profile loader and its deferred writer may
    /// persist the complete live image into the target alias, so this method
    /// provides no live-image safety precondition. Callers must resolve the
    /// safe target image themselves before reaching it.
    async fn write_polling_rate_unchecked(
        &self,
        profile: ProfileId,
        rate: PollingRate,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<PollingRate>, ManagerError>;
    async fn write_profile_metadata(
        &self,
        metadata: ProfileMetadata,
    ) -> Result<SessionWrite<ProfileMetadata>, ManagerError>;

    async fn read_battery(&self, timeout: Duration) -> Result<u8, ManagerError>;
    fn subscribe_events(&self) -> SessionEvents;
}

/// The production transport factory. It is crate-visible only so resource
/// operations can inject it without exposing low-level handles publicly.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RealSessionFactory;

#[cfg(feature = "usb")]
fn list_usb_devices(kind: UsbDeviceKind) -> Result<Vec<DiscoveredEndpoint>, ManagerError> {
    list_devices_for(kind)?
        .into_iter()
        .map(|info| {
            Ok(DiscoveredEndpoint {
                endpoint: DeviceEndpoint::usb(
                    kind.transport_kind(),
                    info.vendor_id,
                    info.product_id,
                    info.serial_number.as_deref(),
                    &info.path,
                    info.product.as_deref(),
                )?,
                connected: true,
            })
        })
        .collect()
}

#[cfg(feature = "ble")]
async fn list_ble_devices() -> Result<Vec<DiscoveredEndpoint>, ManagerError> {
    let devices = BleHandle::list_connected().await?;
    devices
        .into_iter()
        .map(|info| {
            let stable_id = serde_json::to_string(&info.id)
                .map_err(|error| ManagerError::State(StateError::Serde(error)))?;
            Ok(DiscoveredEndpoint {
                endpoint: DeviceEndpoint::ble(&stable_id, info.name.as_deref())?,
                connected: info.connected,
            })
        })
        .collect()
}

#[cfg(feature = "usb")]
async fn open_usb_session(
    endpoint: &DeviceEndpoint,
    kind: UsbDeviceKind,
) -> Result<Box<dyn DeviceSession>, ManagerError> {
    let DeviceLocator::UsbPath(path) = &endpoint.locator else {
        return Err(ManagerError::InvalidUpdate(format!(
            "USB open expected UsbPath locator for transport {:?}, got {:?}",
            endpoint.transport, endpoint.locator
        )));
    };
    if endpoint.transport != kind.transport_kind() {
        return Err(ManagerError::InvalidUpdate(format!(
            "USB open expected transport {:?}, got {:?} for locator {path}",
            kind.transport_kind(),
            endpoint.transport
        )));
    }
    let handle = MouseHandle::open_for_kind(DeviceSelector::path(path.clone()), kind)?;
    Ok(Box::new(UsbSession {
        handle,
        transport: kind.transport_kind(),
    }))
}

#[cfg(feature = "ble")]
async fn open_ble_session(
    endpoint: &DeviceEndpoint,
) -> Result<Box<dyn DeviceSession>, ManagerError> {
    let DeviceLocator::BlePlatformId(serialized_id) = &endpoint.locator else {
        return Err(ManagerError::InvalidUpdate(format!(
            "BLE open expected BlePlatformId locator for transport {:?}, got {:?}",
            endpoint.transport, endpoint.locator
        )));
    };
    if endpoint.transport != TransportKind::Ble {
        return Err(ManagerError::InvalidUpdate(format!(
            "BLE open expected transport Ble, got {:?} for id {serialized_id}",
            endpoint.transport
        )));
    }
    let id: BleDeviceId = serde_json::from_str(serialized_id)
        .map_err(|error| ManagerError::State(StateError::Serde(error)))?;
    let handle = BleHandle::open(BleSelector::Device(id)).await?;
    Ok(Box::new(BleSession { handle }))
}

#[async_trait(?Send)]
impl SessionFactory for RealSessionFactory {
    async fn list(
        &self,
        selection: TransportSelection,
    ) -> Result<Vec<DiscoveredEndpoint>, ManagerError> {
        match selection {
            TransportSelection::Auto => {
                #[cfg(any(feature = "usb", feature = "ble"))]
                {
                    let mut devices = Vec::new();
                    #[cfg(feature = "usb")]
                    {
                        devices.extend(list_usb_devices(UsbDeviceKind::Wired)?);
                        devices.extend(list_usb_devices(UsbDeviceKind::Receiver)?);
                    }
                    #[cfg(feature = "ble")]
                    devices.extend(list_ble_devices().await?);
                    Ok(devices)
                }
                #[cfg(not(any(feature = "usb", feature = "ble")))]
                {
                    Ok(Vec::new())
                }
            }
            TransportSelection::Exact(TransportKind::Wired) => {
                #[cfg(feature = "usb")]
                {
                    list_usb_devices(UsbDeviceKind::Wired)
                }
                #[cfg(not(feature = "usb"))]
                {
                    Err(ManagerError::UnsupportedOperation {
                        operation: "list",
                        transport: TransportKind::Wired,
                    })
                }
            }
            TransportSelection::Exact(TransportKind::Receiver) => {
                #[cfg(feature = "usb")]
                {
                    list_usb_devices(UsbDeviceKind::Receiver)
                }
                #[cfg(not(feature = "usb"))]
                {
                    Err(ManagerError::UnsupportedOperation {
                        operation: "list",
                        transport: TransportKind::Receiver,
                    })
                }
            }
            TransportSelection::Exact(TransportKind::Ble) => {
                #[cfg(feature = "ble")]
                {
                    list_ble_devices().await
                }
                #[cfg(not(feature = "ble"))]
                {
                    Err(ManagerError::UnsupportedOperation {
                        operation: "list",
                        transport: TransportKind::Ble,
                    })
                }
            }
        }
    }

    async fn open(
        &self,
        endpoint: &DeviceEndpoint,
    ) -> Result<Box<dyn DeviceSession>, ManagerError> {
        match endpoint.transport {
            TransportKind::Wired => {
                #[cfg(feature = "usb")]
                return open_usb_session(endpoint, UsbDeviceKind::Wired).await;
                #[cfg(not(feature = "usb"))]
                Err(ManagerError::UnsupportedOperation {
                    operation: "open",
                    transport: TransportKind::Wired,
                })
            }
            TransportKind::Receiver => {
                #[cfg(feature = "usb")]
                return open_usb_session(endpoint, UsbDeviceKind::Receiver).await;
                #[cfg(not(feature = "usb"))]
                Err(ManagerError::UnsupportedOperation {
                    operation: "open",
                    transport: TransportKind::Receiver,
                })
            }
            TransportKind::Ble => {
                #[cfg(feature = "ble")]
                return open_ble_session(endpoint).await;
                #[cfg(not(feature = "ble"))]
                Err(ManagerError::UnsupportedOperation {
                    operation: "open",
                    transport: TransportKind::Ble,
                })
            }
        }
    }
}

struct UsbSession {
    handle: MouseHandle,
    transport: TransportKind,
}

#[cfg(feature = "usb")]
#[async_trait(?Send)]
impl DeviceSession for UsbSession {
    fn transport(&self) -> TransportKind {
        self.transport
    }

    async fn read_profile_metadata(&self) -> Result<ProfileMetadata, ManagerError> {
        Ok(self.handle.read_profile_metadata().await?)
    }

    async fn read_profile(&self, profile: ProfileId) -> Result<ProfileSnapshot, ManagerError> {
        Ok(self.handle.read_profile(profile).await?)
    }

    async fn read_dpi(&self, profile: ProfileId) -> Result<DpiState, ManagerError> {
        Ok(self.handle.read_dpi(profile).await?)
    }

    async fn read_preferences(&self, profile: ProfileId) -> Result<PreferencesState, ManagerError> {
        Ok(self.handle.read_preferences(profile).await?)
    }

    async fn read_buttons(&self, profile: ProfileId) -> Result<ButtonsState, ManagerError> {
        Ok(self.handle.read_buttons(profile).await?)
    }

    async fn read_polling_rate(&self, profile: ProfileId) -> Result<PollingRate, ManagerError> {
        Ok(self.handle.read_polling_rate(profile).await?)
    }

    async fn write_dpi(
        &self,
        state: DpiState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<DpiState>, ManagerError> {
        match verification {
            VerificationMethod::Transport => {
                self.handle.send_dpi(state).await?;
                Ok(SessionWrite::Acknowledged)
            }
            VerificationMethod::Readback => Ok(SessionWrite::ReadbackVerified(
                self.handle.write_dpi(state).await?,
            )),
        }
    }

    async fn write_preferences(
        &self,
        state: PreferencesState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<PreferencesState>, ManagerError> {
        match verification {
            VerificationMethod::Transport => {
                self.handle.send_preferences(state).await?;
                Ok(SessionWrite::Acknowledged)
            }
            VerificationMethod::Readback => Ok(SessionWrite::ReadbackVerified(
                self.handle.write_preferences(state).await?,
            )),
        }
    }

    async fn write_buttons(
        &self,
        state: ButtonsState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<ButtonsState>, ManagerError> {
        match verification {
            VerificationMethod::Transport => {
                self.handle.send_buttons(state).await?;
                Ok(SessionWrite::Acknowledged)
            }
            VerificationMethod::Readback => Ok(SessionWrite::ReadbackVerified(
                self.handle.write_buttons(state).await?,
            )),
        }
    }

    async fn write_polling_rate_unchecked(
        &self,
        profile: ProfileId,
        rate: PollingRate,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<PollingRate>, ManagerError> {
        match verification {
            VerificationMethod::Transport => {
                self.handle
                    .send_polling_rate_unchecked(profile, rate)
                    .await?;
                Ok(SessionWrite::Acknowledged)
            }
            VerificationMethod::Readback => Ok(SessionWrite::ReadbackVerified(
                self.handle
                    .write_polling_rate_unchecked(profile, rate)
                    .await?,
            )),
        }
    }

    async fn write_profile_metadata(
        &self,
        metadata: ProfileMetadata,
    ) -> Result<SessionWrite<ProfileMetadata>, ManagerError> {
        let mut actual = self.handle.read_profile_metadata().await?;

        // Raise the maximum before activating a profile that is not currently
        // enabled. Lowering it waits until the target profile is active so the
        // low-level driver's current-profile invariant is never violated.
        if metadata.maximum().get() > actual.maximum().get() {
            actual = self.handle.set_maximum_profile(metadata.maximum()).await?;
        }
        if metadata.current() != actual.current() {
            actual = self.handle.activate_profile(metadata.current()).await?;
        }
        if metadata.maximum() != actual.maximum() {
            actual = self.handle.set_maximum_profile(metadata.maximum()).await?;
        }

        Ok(SessionWrite::ReadbackVerified(actual))
    }

    async fn read_battery(&self, timeout: Duration) -> Result<u8, ManagerError> {
        Ok(self.handle.read_battery(timeout).await?)
    }

    fn subscribe_events(&self) -> SessionEvents {
        SessionEvents {
            input: Some(self.handle.subscribe_input_events()),
        }
    }
}

#[cfg(feature = "ble")]
struct BleSession {
    handle: BleHandle,
}

#[cfg(feature = "ble")]
impl BleSession {
    fn unsupported<T>(operation: &'static str) -> Result<T, ManagerError> {
        Err(ManagerError::UnsupportedOperation {
            operation,
            transport: TransportKind::Ble,
        })
    }
}

#[cfg(feature = "ble")]
#[async_trait(?Send)]
impl DeviceSession for BleSession {
    fn transport(&self) -> TransportKind {
        TransportKind::Ble
    }

    async fn read_profile_metadata(&self) -> Result<ProfileMetadata, ManagerError> {
        Self::unsupported("read_profile_metadata")
    }

    async fn read_profile(&self, _profile: ProfileId) -> Result<ProfileSnapshot, ManagerError> {
        Self::unsupported("read_profile")
    }

    async fn read_dpi(&self, _profile: ProfileId) -> Result<DpiState, ManagerError> {
        Self::unsupported("read_dpi")
    }

    async fn read_preferences(
        &self,
        _profile: ProfileId,
    ) -> Result<PreferencesState, ManagerError> {
        Self::unsupported("read_preferences")
    }

    async fn read_buttons(&self, _profile: ProfileId) -> Result<ButtonsState, ManagerError> {
        Self::unsupported("read_buttons")
    }

    async fn read_polling_rate(&self, _profile: ProfileId) -> Result<PollingRate, ManagerError> {
        Self::unsupported("read_polling_rate")
    }

    async fn write_dpi(
        &self,
        state: DpiState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<DpiState>, ManagerError> {
        match verification {
            VerificationMethod::Transport => {
                self.handle.write_dpi(state).await?;
                Ok(SessionWrite::Acknowledged)
            }
            VerificationMethod::Readback => Self::unsupported("write_dpi"),
        }
    }

    async fn write_preferences(
        &self,
        state: PreferencesState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<PreferencesState>, ManagerError> {
        match verification {
            VerificationMethod::Transport => {
                self.handle.write_preferences(state).await?;
                Ok(SessionWrite::Acknowledged)
            }
            VerificationMethod::Readback => Self::unsupported("write_preferences"),
        }
    }

    async fn write_buttons(
        &self,
        state: ButtonsState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<ButtonsState>, ManagerError> {
        match verification {
            VerificationMethod::Transport => {
                self.handle.write_buttons(state).await?;
                Ok(SessionWrite::Acknowledged)
            }
            VerificationMethod::Readback => Self::unsupported("write_buttons"),
        }
    }

    async fn write_polling_rate_unchecked(
        &self,
        profile: ProfileId,
        rate: PollingRate,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<PollingRate>, ManagerError> {
        match verification {
            VerificationMethod::Transport => {
                // BLE has no readback path; an accepted application ACK is
                // the strongest evidence available to the manager.
                self.handle
                    .write_polling_rate_unchecked(profile, rate)
                    .await?;
                Ok(SessionWrite::Acknowledged)
            }
            VerificationMethod::Readback => Self::unsupported("write_polling_rate_unchecked"),
        }
    }

    async fn write_profile_metadata(
        &self,
        metadata: ProfileMetadata,
    ) -> Result<SessionWrite<ProfileMetadata>, ManagerError> {
        self.handle.write_profile_control(metadata).await?;
        Ok(SessionWrite::Acknowledged)
    }

    async fn read_battery(&self, _timeout: Duration) -> Result<u8, ManagerError> {
        Self::unsupported("read_battery")
    }

    fn subscribe_events(&self) -> SessionEvents {
        SessionEvents { input: None }
    }
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ScriptedWrite {
    Dpi(DpiState),
    Preferences(PreferencesState),
    Buttons(ButtonsState),
    PollingRate(ProfileId, PollingRate),
    ProfileMetadata(ProfileMetadata),
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct ScriptedFakeSession {
    transport: TransportKind,
    /// Default evidence for session writes that carry no per-call method
    /// (profile metadata): USB readback evidence, BLE transport ACK. The four
    /// resource writes take their `VerificationMethod` per call instead.
    verification: VerificationMethod,
    metadata: Arc<Mutex<Option<ProfileMetadata>>>,
    profiles: Arc<Mutex<BTreeMap<ProfileId, ProfileSnapshot>>>,
    profile_sequences: Arc<Mutex<BTreeMap<ProfileId, VecDeque<ProfileSnapshot>>>>,
    last_profiles: Arc<Mutex<BTreeMap<ProfileId, ProfileSnapshot>>>,
    polling_rate: Arc<Mutex<Option<PollingRate>>>,
    battery: Arc<Mutex<Option<u8>>>,
    writes: Arc<Mutex<Vec<ScriptedWrite>>>,
    metadata_write_failure: Arc<Mutex<Option<ProfileMetadata>>>,
    input_events: broadcast::Sender<InputEvent>,
}

#[cfg(test)]
fn lock_scripted<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value.lock().expect("scripted fake state lock poisoned")
}

#[cfg(test)]
impl ScriptedFakeSession {
    fn new(transport: TransportKind, verification: VerificationMethod) -> Self {
        let (input_events, _) = broadcast::channel(16);
        Self {
            transport,
            verification,
            metadata: Arc::new(Mutex::new(None)),
            profiles: Arc::new(Mutex::new(BTreeMap::new())),
            profile_sequences: Arc::new(Mutex::new(BTreeMap::new())),
            last_profiles: Arc::new(Mutex::new(BTreeMap::new())),
            polling_rate: Arc::new(Mutex::new(None)),
            battery: Arc::new(Mutex::new(None)),
            writes: Arc::new(Mutex::new(Vec::new())),
            metadata_write_failure: Arc::new(Mutex::new(None)),
            input_events,
        }
    }

    pub(crate) fn usb() -> Self {
        Self::new(TransportKind::Wired, VerificationMethod::Readback)
    }

    pub(crate) fn ble() -> Self {
        Self::new(TransportKind::Ble, VerificationMethod::Transport)
    }

    pub(crate) fn with_metadata(self, metadata: ProfileMetadata) -> Self {
        *lock_scripted(&self.metadata) = Some(metadata);
        self
    }

    pub(crate) fn with_profile(self, snapshot: ProfileSnapshot) -> Self {
        lock_scripted(&self.profiles).insert(snapshot.target_profile, snapshot);
        self
    }
    pub(crate) fn with_profile_sequence(self, sequence: Vec<ProfileSnapshot>) -> Self {
        {
            let mut queues = lock_scripted(&self.profile_sequences);
            let mut last_profiles = lock_scripted(&self.last_profiles);
            for snapshot in sequence {
                let profile = snapshot.target_profile;
                queues.entry(profile).or_default().push_back(snapshot);
                last_profiles.remove(&profile);
            }
        }
        self
    }

    pub(crate) fn fail_metadata_write_for(self, metadata: ProfileMetadata) -> Self {
        *lock_scripted(&self.metadata_write_failure) = Some(metadata);
        self
    }

    pub(crate) fn with_polling_rate(self, rate: PollingRate) -> Self {
        *lock_scripted(&self.polling_rate) = Some(rate);
        self
    }

    pub(crate) fn with_battery(self, level: u8) -> Self {
        *lock_scripted(&self.battery) = Some(level);
        self
    }

    /// Snapshot of every hardware write the fake has performed, in order.
    pub(crate) fn writes(&self) -> Vec<ScriptedWrite> {
        lock_scripted(&self.writes).clone()
    }

    fn unsupported<T>(&self, operation: &'static str) -> Result<T, ManagerError> {
        Err(ManagerError::UnsupportedOperation {
            operation,
            transport: self.transport,
        })
    }

    fn write_outcome<T>(&self, value: T, verification: VerificationMethod) -> SessionWrite<T> {
        match verification {
            VerificationMethod::Transport => SessionWrite::Acknowledged,
            VerificationMethod::Readback => SessionWrite::ReadbackVerified(value),
        }
    }

    fn readback_unsupported(&self, operation: &'static str) -> ManagerError {
        ManagerError::UnsupportedOperation {
            operation,
            transport: self.transport,
        }
    }

    fn is_ble(&self) -> bool {
        self.transport == TransportKind::Ble
    }
}

#[cfg(test)]
#[async_trait(?Send)]
impl DeviceSession for ScriptedFakeSession {
    fn transport(&self) -> TransportKind {
        self.transport
    }

    async fn read_profile_metadata(&self) -> Result<ProfileMetadata, ManagerError> {
        if self.is_ble() {
            return self.unsupported("read_profile_metadata");
        }
        lock_scripted(&self.metadata)
            .as_ref()
            .copied()
            .ok_or(ManagerError::MissingBaseline {
                resource: "profile metadata",
                profile: None,
            })
    }

    async fn read_profile(&self, profile: ProfileId) -> Result<ProfileSnapshot, ManagerError> {
        if self.is_ble() {
            return self.unsupported("read_profile");
        }
        let queued = lock_scripted(&self.profile_sequences)
            .get_mut(&profile)
            .and_then(VecDeque::pop_front);
        let mut snapshot = if let Some(snapshot) = queued {
            lock_scripted(&self.last_profiles).insert(profile, snapshot.clone());
            snapshot
        } else if let Some(snapshot) = lock_scripted(&self.last_profiles).get(&profile).cloned() {
            snapshot
        } else {
            lock_scripted(&self.profiles).get(&profile).cloned().ok_or(
                ManagerError::MissingBaseline {
                    resource: "profile",
                    profile: Some(profile),
                },
            )?
        };
        if let Some(metadata) = lock_scripted(&self.metadata).as_ref().copied() {
            snapshot.persistent_metadata = metadata;
        }
        Ok(snapshot)
    }

    async fn read_dpi(&self, profile: ProfileId) -> Result<DpiState, ManagerError> {
        if self.is_ble() {
            return self.unsupported("read_dpi");
        }
        lock_scripted(&self.profiles)
            .get(&profile)
            .map(|snapshot| snapshot.dpi.clone())
            .ok_or(ManagerError::MissingBaseline {
                resource: "DPI",
                profile: Some(profile),
            })
    }

    async fn read_preferences(&self, profile: ProfileId) -> Result<PreferencesState, ManagerError> {
        if self.is_ble() {
            return self.unsupported("read_preferences");
        }
        lock_scripted(&self.profiles)
            .get(&profile)
            .map(|snapshot| snapshot.preferences)
            .ok_or(ManagerError::MissingBaseline {
                resource: "preferences",
                profile: Some(profile),
            })
    }

    async fn read_buttons(&self, profile: ProfileId) -> Result<ButtonsState, ManagerError> {
        if self.is_ble() {
            return self.unsupported("read_buttons");
        }
        lock_scripted(&self.profiles)
            .get(&profile)
            .map(|snapshot| snapshot.buttons)
            .ok_or(ManagerError::MissingBaseline {
                resource: "buttons",
                profile: Some(profile),
            })
    }

    async fn read_polling_rate(&self, profile: ProfileId) -> Result<PollingRate, ManagerError> {
        if self.is_ble() {
            return self.unsupported("read_polling_rate");
        }
        lock_scripted(&self.polling_rate)
            .as_ref()
            .copied()
            .ok_or(ManagerError::MissingBaseline {
                resource: "polling rate",
                profile: Some(profile),
            })
    }

    async fn write_dpi(
        &self,
        state: DpiState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<DpiState>, ManagerError> {
        if self.is_ble() && verification == VerificationMethod::Readback {
            return Err(self.readback_unsupported("write_dpi"));
        }
        lock_scripted(&self.writes).push(ScriptedWrite::Dpi(state.clone()));
        if !self.is_ble()
            && let Some(snapshot) = lock_scripted(&self.profiles).get_mut(&state.profile)
        {
            snapshot.dpi = state.clone();
        }
        Ok(self.write_outcome(state, verification))
    }

    async fn write_preferences(
        &self,
        state: PreferencesState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<PreferencesState>, ManagerError> {
        if self.is_ble() && verification == VerificationMethod::Readback {
            return Err(self.readback_unsupported("write_preferences"));
        }
        lock_scripted(&self.writes).push(ScriptedWrite::Preferences(state));
        if !self.is_ble()
            && let Some(snapshot) = lock_scripted(&self.profiles).get_mut(&state.profile)
        {
            snapshot.preferences = state;
        }
        Ok(self.write_outcome(state, verification))
    }

    async fn write_buttons(
        &self,
        state: ButtonsState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<ButtonsState>, ManagerError> {
        if self.is_ble() && verification == VerificationMethod::Readback {
            return Err(self.readback_unsupported("write_buttons"));
        }
        lock_scripted(&self.writes).push(ScriptedWrite::Buttons(state));
        if !self.is_ble()
            && let Some(snapshot) = lock_scripted(&self.profiles).get_mut(&state.profile)
        {
            snapshot.buttons = state;
        }
        Ok(self.write_outcome(state, verification))
    }

    async fn write_polling_rate_unchecked(
        &self,
        profile: ProfileId,
        rate: PollingRate,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<PollingRate>, ManagerError> {
        if self.is_ble() && verification == VerificationMethod::Readback {
            return Err(self.readback_unsupported("write_polling_rate_unchecked"));
        }
        lock_scripted(&self.writes).push(ScriptedWrite::PollingRate(profile, rate));
        *lock_scripted(&self.polling_rate) = Some(rate);
        Ok(self.write_outcome(rate, verification))
    }

    async fn write_profile_metadata(
        &self,
        metadata: ProfileMetadata,
    ) -> Result<SessionWrite<ProfileMetadata>, ManagerError> {
        if lock_scripted(&self.metadata_write_failure).as_ref() == Some(&metadata) {
            return Err(ManagerError::InvalidUpdate(
                "scripted profile metadata write failure".to_owned(),
            ));
        }
        lock_scripted(&self.writes).push(ScriptedWrite::ProfileMetadata(metadata));
        *lock_scripted(&self.metadata) = Some(metadata);
        Ok(self.write_outcome(metadata, self.verification))
    }

    async fn read_battery(&self, _timeout: Duration) -> Result<u8, ManagerError> {
        if self.is_ble() {
            return self.unsupported("read_battery");
        }
        lock_scripted(&self.battery)
            .as_ref()
            .copied()
            .ok_or(ManagerError::MissingBaseline {
                resource: "battery",
                profile: None,
            })
    }

    fn subscribe_events(&self) -> SessionEvents {
        if self.is_ble() {
            SessionEvents { input: None }
        } else {
            SessionEvents {
                input: Some(self.input_events.subscribe()),
            }
        }
    }
}

#[cfg(test)]
fn endpoint_key(endpoint: &DeviceEndpoint) -> String {
    match &endpoint.locator {
        DeviceLocator::UsbPath(path) => format!("{:?}:{}", endpoint.transport, path),
        DeviceLocator::BlePlatformId(id) => format!("{:?}:{}", endpoint.transport, id),
    }
}

#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct ScriptedFakeFactory {
    endpoints: Arc<Mutex<BTreeMap<String, DiscoveredEndpoint>>>,
    sessions: Arc<Mutex<BTreeMap<String, ScriptedFakeSession>>>,
    discovery_queue: Arc<Mutex<VecDeque<Vec<DiscoveredEndpoint>>>>,
    last_discovery: Arc<Mutex<Option<Vec<DiscoveredEndpoint>>>>,
    list_calls: Arc<Mutex<Vec<TransportSelection>>>,
}

#[cfg(test)]
impl ScriptedFakeFactory {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with_discovery_sequence(self, sequence: Vec<Vec<DiscoveredEndpoint>>) -> Self {
        *lock_scripted(&self.discovery_queue) = VecDeque::from(sequence);
        *lock_scripted(&self.last_discovery) = None;
        self
    }

    pub(crate) fn list_calls(&self) -> Vec<TransportSelection> {
        lock_scripted(&self.list_calls).clone()
    }

    pub(crate) fn with_endpoint(
        self,
        discovered: DiscoveredEndpoint,
        session: ScriptedFakeSession,
    ) -> Self {
        self.add_endpoint(discovered, session);
        self
    }

    /// Legacy helper: accepts a DeviceIdentity and converts its first endpoint.
    pub(crate) fn with_identity(
        self,
        identity: crate::device::DeviceIdentity,
        connected: bool,
        session: ScriptedFakeSession,
    ) -> Self {
        let endpoint = identity
            .endpoints
            .values()
            .next()
            .cloned()
            .expect("DeviceIdentity must contain at least one endpoint");
        self.add_endpoint(
            DiscoveredEndpoint {
                endpoint,
                connected,
            },
            session,
        );
        self
    }

    pub(crate) fn add_endpoint(
        &self,
        discovered: DiscoveredEndpoint,
        session: ScriptedFakeSession,
    ) {
        let key = endpoint_key(&discovered.endpoint);
        lock_scripted(&self.endpoints).insert(key.clone(), discovered);
        lock_scripted(&self.sessions).insert(key, session);
    }
}

#[cfg(test)]
#[async_trait(?Send)]
impl SessionFactory for ScriptedFakeFactory {
    async fn list(
        &self,
        selection: TransportSelection,
    ) -> Result<Vec<DiscoveredEndpoint>, ManagerError> {
        lock_scripted(&self.list_calls).push(selection);
        let candidates = if let Some(next) = lock_scripted(&self.discovery_queue).pop_front() {
            *lock_scripted(&self.last_discovery) = Some(next.clone());
            next
        } else if let Some(last) = lock_scripted(&self.last_discovery).clone() {
            last
        } else {
            lock_scripted(&self.endpoints).values().cloned().collect()
        };
        Ok(candidates
            .into_iter()
            .filter(|discovered| match selection {
                TransportSelection::Auto => true,
                TransportSelection::Exact(transport) => discovered.endpoint.transport == transport,
            })
            .collect())
    }

    async fn open(
        &self,
        endpoint: &DeviceEndpoint,
    ) -> Result<Box<dyn DeviceSession>, ManagerError> {
        let key = endpoint_key(endpoint);
        let session = lock_scripted(&self.sessions)
            .get(&key)
            .cloned()
            .ok_or_else(|| {
                ManagerError::InvalidUpdate(format!(
                    "no scripted session for endpoint {endpoint:?} (key {key})"
                ))
            })?;
        Ok(Box::new(session))
    }
}

#[cfg(test)]
mod tests {
    use super::{DeviceSession, ScriptedFakeSession, SessionWrite};
    use crate::error::ManagerError;
    use crate::operation::VerificationMethod;
    use attack_shark_x3::{
        ButtonAssignment, ButtonsState, DpiState, DpiValue, PollingRate, PreferencesState,
        ProfileId, StageIndex,
    };

    fn profile(value: u8) -> ProfileId {
        ProfileId::try_from(value).expect("test profile must be valid")
    }

    fn snapshot() -> attack_shark_x3::driver::ProfileSnapshot {
        use attack_shark_x3::ProfileMetadata;
        let dpi = DpiState::new(
            profile(2),
            vec![DpiValue::new(800).expect("valid dpi")],
            StageIndex::new(1).expect("valid stage"),
            [0; 25],
        )
        .expect("valid dpi state");
        let slots = [ButtonAssignment::default(); 18];
        attack_shark_x3::driver::ProfileSnapshot {
            persistent_metadata: ProfileMetadata::new(profile(2), profile(5))
                .expect("valid metadata"),
            target_profile: profile(2),
            dpi,
            preferences: PreferencesState::new(profile(2), 0, 0, 0, [0, 0, 0], 0, 0),
            buttons: ButtonsState::new(profile(2), slots),
        }
    }

    /// Values distinct from the baseline snapshot so state-update assertions
    /// prove the write landed rather than echoing the seeded baseline.
    fn written_resources() -> (DpiState, PreferencesState, ButtonsState) {
        let dpi = DpiState::new(
            profile(2),
            vec![DpiValue::new(1600).expect("valid dpi")],
            StageIndex::new(1).expect("valid stage"),
            [0; 25],
        )
        .expect("valid dpi state");
        let preferences = PreferencesState::new(profile(2), 1, 2, 3, [4, 5, 6], 7, 8);
        let mut slots = [ButtonAssignment::default(); 18];
        slots[0] = ButtonAssignment::new(0x01, 0x02, 0x03);
        (dpi, preferences, ButtonsState::new(profile(2), slots))
    }

    #[tokio::test]
    async fn usb_fake_routes_transport_to_acknowledged() {
        let usb = ScriptedFakeSession::usb().with_profile(snapshot());
        let (dpi, preferences, buttons) = written_resources();

        assert_eq!(
            usb.write_dpi(dpi, VerificationMethod::Transport)
                .await
                .expect("USB transport DPI write succeeds"),
            SessionWrite::Acknowledged
        );
        assert_eq!(
            usb.write_preferences(preferences, VerificationMethod::Transport)
                .await
                .expect("USB transport preferences write succeeds"),
            SessionWrite::Acknowledged
        );
        assert_eq!(
            usb.write_buttons(buttons, VerificationMethod::Transport)
                .await
                .expect("USB transport buttons write succeeds"),
            SessionWrite::Acknowledged
        );
        assert_eq!(
            usb.write_polling_rate_unchecked(
                profile(2),
                PollingRate::Hz1000,
                VerificationMethod::Transport
            )
            .await
            .expect("USB transport polling rate write succeeds"),
            SessionWrite::Acknowledged
        );
    }

    #[tokio::test]
    async fn usb_fake_routes_readback_to_readback_verified_and_updates_state() {
        let usb = ScriptedFakeSession::usb().with_profile(snapshot());
        let (dpi, preferences, buttons) = written_resources();

        assert_eq!(
            usb.write_dpi(dpi.clone(), VerificationMethod::Readback)
                .await
                .expect("USB readback DPI write succeeds"),
            SessionWrite::ReadbackVerified(dpi.clone())
        );
        assert_eq!(
            usb.write_preferences(preferences, VerificationMethod::Readback)
                .await
                .expect("USB readback preferences write succeeds"),
            SessionWrite::ReadbackVerified(preferences)
        );
        assert_eq!(
            usb.write_buttons(buttons, VerificationMethod::Readback)
                .await
                .expect("USB readback buttons write succeeds"),
            SessionWrite::ReadbackVerified(buttons)
        );
        assert_eq!(
            usb.write_polling_rate_unchecked(
                profile(2),
                PollingRate::Hz1000,
                VerificationMethod::Readback
            )
            .await
            .expect("USB readback polling rate write succeeds"),
            SessionWrite::ReadbackVerified(PollingRate::Hz1000)
        );

        // Successful writes land in the fake device state.
        assert_eq!(usb.read_dpi(profile(2)).await.expect("read back DPI"), dpi);
        assert_eq!(
            usb.read_preferences(profile(2))
                .await
                .expect("read back preferences"),
            preferences
        );
        assert_eq!(
            usb.read_buttons(profile(2))
                .await
                .expect("read back buttons"),
            buttons
        );
        assert_eq!(
            usb.read_polling_rate(profile(2))
                .await
                .expect("read back polling rate"),
            PollingRate::Hz1000
        );
    }

    #[tokio::test]
    async fn ble_fake_accepts_transport_but_rejects_readback() {
        let ble = ScriptedFakeSession::ble();
        let (dpi, preferences, buttons) = written_resources();

        assert_eq!(
            ble.write_dpi(dpi, VerificationMethod::Transport)
                .await
                .expect("BLE transport DPI write succeeds"),
            SessionWrite::Acknowledged
        );
        assert_eq!(
            ble.write_preferences(preferences, VerificationMethod::Transport)
                .await
                .expect("BLE transport preferences write succeeds"),
            SessionWrite::Acknowledged
        );
        assert_eq!(
            ble.write_buttons(buttons, VerificationMethod::Transport)
                .await
                .expect("BLE transport buttons write succeeds"),
            SessionWrite::Acknowledged
        );
        assert_eq!(
            ble.write_polling_rate_unchecked(
                profile(2),
                PollingRate::Hz1000,
                VerificationMethod::Transport
            )
            .await
            .expect("BLE transport polling rate write succeeds"),
            SessionWrite::Acknowledged
        );

        let (dpi, preferences, buttons) = written_resources();
        assert!(matches!(
            ble.write_dpi(dpi, VerificationMethod::Readback).await,
            Err(ManagerError::UnsupportedOperation { .. })
        ));
        assert!(matches!(
            ble.write_preferences(preferences, VerificationMethod::Readback)
                .await,
            Err(ManagerError::UnsupportedOperation { .. })
        ));
        assert!(matches!(
            ble.write_buttons(buttons, VerificationMethod::Readback)
                .await,
            Err(ManagerError::UnsupportedOperation { .. })
        ));
        assert!(matches!(
            ble.write_polling_rate_unchecked(
                profile(2),
                PollingRate::Hz1000,
                VerificationMethod::Readback
            )
            .await,
            Err(ManagerError::UnsupportedOperation { .. })
        ));
    }
}
