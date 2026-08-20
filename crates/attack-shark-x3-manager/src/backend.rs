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
    /// Reads the live polling rate; the supplied alias is a wire side effect and never identifies rate content.
    ///
    /// Report `0x06` skips the profile loader: the selector byte is an alias
    /// whose value is recorded as a side effect while the returned rate is
    /// from the current live image.
    async fn read_live_polling_rate(&self, alias: ProfileId) -> Result<PollingRate, ManagerError>;
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
        debug_assert!(false, "USB open expected UsbPath locator");
        return Err(ManagerError::InvalidUpdate(format!(
            "USB open expected UsbPath locator for transport {:?}, got {:?}",
            endpoint.transport, endpoint.locator
        )));
    };
    debug_assert_eq!(
        endpoint.transport,
        kind.transport_kind(),
        "USB transport mismatch"
    );
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
        debug_assert!(false, "BLE open expected BlePlatformId locator");
        return Err(ManagerError::InvalidUpdate(format!(
            "BLE open expected BlePlatformId locator for transport {:?}, got {:?}",
            endpoint.transport, endpoint.locator
        )));
    };
    debug_assert_eq!(
        endpoint.transport,
        TransportKind::Ble,
        "BLE transport mismatch"
    );
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

    async fn read_live_polling_rate(&self, alias: ProfileId) -> Result<PollingRate, ManagerError> {
        Ok(self.handle.read_live_polling_rate(alias).await?)
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

    async fn read_live_polling_rate(&self, _alias: ProfileId) -> Result<PollingRate, ManagerError> {
        Self::unsupported("read_live_polling_rate")
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
    /// Per-profile polling rates; the live rate is `polling_rates[live_profile]`.
    polling_rates: Arc<Mutex<BTreeMap<ProfileId, PollingRate>>>,
    /// The currently live/working profile image; report `0x06` skips the loader
    /// so polling reads return `polling_rates[live_profile]` regardless of alias.
    live_profile: Arc<Mutex<ProfileId>>,
    /// Last alias supplied to `read_live_polling_rate`; wire side effect, never content.
    last_polling_alias: Arc<Mutex<Option<ProfileId>>>,
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
            polling_rates: Arc::new(Mutex::new(BTreeMap::new())),
            live_profile: Arc::new(Mutex::new(ProfileId::MIN_ID)),
            last_polling_alias: Arc::new(Mutex::new(None)),
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
        *lock_scripted(&self.live_profile) = metadata.current();
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

    /// Sets the same polling rate for every profile (1..=5); convenience for legacy single-rate tests.
    pub(crate) fn with_polling_rate(self, rate: PollingRate) -> Self {
        {
            let mut rates = lock_scripted(&self.polling_rates);
            for id in 1..=5 {
                if let Ok(profile) = ProfileId::try_from(id) {
                    rates.insert(profile, rate);
                }
            }
        }
        self
    }

    /// Sets a per-profile polling rate.
    pub(crate) fn with_polling_rate_for(self, profile: ProfileId, rate: PollingRate) -> Self {
        lock_scripted(&self.polling_rates).insert(profile, rate);
        self
    }

    /// Forces the live/working profile; subsequent `read_live_polling_rate` returns this profile's rate.
    pub(crate) fn with_live_profile(self, profile: ProfileId) -> Self {
        *lock_scripted(&self.live_profile) = profile;
        self
    }

    /// Returns the last alias supplied to `read_live_polling_rate`, if any.
    pub(crate) fn last_polling_alias(&self) -> Option<ProfileId> {
        *lock_scripted(&self.last_polling_alias)
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
        *lock_scripted(&self.live_profile) = profile;
        Ok(snapshot)
    }

    async fn read_dpi(&self, profile: ProfileId) -> Result<DpiState, ManagerError> {
        if self.is_ble() {
            return self.unsupported("read_dpi");
        }
        let result = lock_scripted(&self.profiles)
            .get(&profile)
            .map(|snapshot| snapshot.dpi.clone())
            .ok_or(ManagerError::MissingBaseline {
                resource: "DPI",
                profile: Some(profile),
            })?;
        *lock_scripted(&self.live_profile) = profile;
        Ok(result)
    }

    async fn read_preferences(&self, profile: ProfileId) -> Result<PreferencesState, ManagerError> {
        if self.is_ble() {
            return self.unsupported("read_preferences");
        }
        let result = lock_scripted(&self.profiles)
            .get(&profile)
            .map(|snapshot| snapshot.preferences)
            .ok_or(ManagerError::MissingBaseline {
                resource: "preferences",
                profile: Some(profile),
            })?;
        *lock_scripted(&self.live_profile) = profile;
        Ok(result)
    }

    async fn read_buttons(&self, profile: ProfileId) -> Result<ButtonsState, ManagerError> {
        if self.is_ble() {
            return self.unsupported("read_buttons");
        }
        let result = lock_scripted(&self.profiles)
            .get(&profile)
            .map(|snapshot| snapshot.buttons)
            .ok_or(ManagerError::MissingBaseline {
                resource: "buttons",
                profile: Some(profile),
            })?;
        *lock_scripted(&self.live_profile) = profile;
        Ok(result)
    }

    async fn read_live_polling_rate(&self, alias: ProfileId) -> Result<PollingRate, ManagerError> {
        if self.is_ble() {
            return self.unsupported("read_live_polling_rate");
        }
        *lock_scripted(&self.last_polling_alias) = Some(alias);
        let live = *lock_scripted(&self.live_profile);
        lock_scripted(&self.polling_rates)
            .get(&live)
            .copied()
            .ok_or(ManagerError::MissingBaseline {
                resource: "polling rate",
                profile: Some(alias),
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
        // Report `0x06` is a save alias: the deferred writer serializes the live
        // image into `profile`, so the target slot's rate becomes `rate`. The live
        // image's rate is also the new rate when the session's live profile is
        // the target; otherwise the live rate stays until the target is loaded.
        lock_scripted(&self.polling_rates).insert(profile, rate);
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
        // Real activation establishes the new live profile; mirror it.
        *lock_scripted(&self.live_profile) = metadata.current();
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

    /// Adds the first endpoint from a test identity.
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
        // Polling is live: after the targeted reads above, live is profile 2.
        assert_eq!(
            usb.read_live_polling_rate(profile(2))
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

    #[tokio::test]
    async fn live_polling_alias_is_side_effect_not_content() {
        // Per-profile rates with live profile 1.
        let session = ScriptedFakeSession::usb()
            .with_profile(snapshot())
            .with_profile(attack_shark_x3::driver::ProfileSnapshot {
                target_profile: profile(1),
                persistent_metadata: attack_shark_x3::ProfileMetadata::new(profile(1), profile(5))
                    .expect("valid metadata"),
                dpi: snapshot().dpi.clone(),
                preferences: snapshot().preferences,
                buttons: snapshot().buttons,
            })
            .with_polling_rate_for(profile(1), PollingRate::Hz500)
            .with_polling_rate_for(profile(2), PollingRate::Hz1000)
            .with_live_profile(profile(1));
        // Standalone alias 2 returns live profile 1 rate and records alias.
        let rate_via_alias2 = session
            .read_live_polling_rate(profile(2))
            .await
            .expect("live polling rate via alias 2 must succeed");
        assert_eq!(
            rate_via_alias2,
            PollingRate::Hz500,
            "alias 2 must return live 1 rate, exposing cross-profile contamination"
        );
        assert_eq!(session.last_polling_alias(), Some(profile(2)));
        // Loading target 2 establishes it as live; alias 2 now returns profile 2 rate.
        session
            .read_profile(profile(2))
            .await
            .expect("loading profile 2 must succeed");
        let rate_after_load = session
            .read_live_polling_rate(profile(2))
            .await
            .expect("live polling after load must succeed");
        assert_eq!(rate_after_load, PollingRate::Hz1000);
        // Even alias 1 now returns live 2 rate.
        let rate_via_alias1 = session
            .read_live_polling_rate(profile(1))
            .await
            .expect("alias 1 must now return live 2 rate");
        assert_eq!(rate_via_alias1, PollingRate::Hz1000);
        assert_eq!(session.last_polling_alias(), Some(profile(1)));
    }

    #[tokio::test]
    async fn polling_write_updates_target_slot_and_live_visibility() {
        let session = ScriptedFakeSession::usb()
            .with_profile(snapshot())
            .with_profile(attack_shark_x3::driver::ProfileSnapshot {
                target_profile: profile(1),
                persistent_metadata: attack_shark_x3::ProfileMetadata::new(profile(1), profile(5))
                    .expect("valid metadata"),
                dpi: snapshot().dpi.clone(),
                preferences: snapshot().preferences,
                buttons: snapshot().buttons,
            })
            .with_polling_rate_for(profile(1), PollingRate::Hz1000)
            .with_polling_rate_for(profile(2), PollingRate::Hz500)
            .with_live_profile(profile(1));
        // Write alias 2 while live is 1: target slot updated, live unchanged.
        session
            .write_polling_rate_unchecked(
                profile(2),
                PollingRate::Hz250,
                VerificationMethod::Readback,
            )
            .await
            .expect("write alias 2 must succeed");
        let still_live1 = session
            .read_live_polling_rate(profile(2))
            .await
            .expect("alias 2 while live 1 must still return live 1 rate");
        assert_eq!(still_live1, PollingRate::Hz1000);
        // Load target 2: now live 2 returns the newly written rate.
        session.read_profile(profile(2)).await.expect("load 2");
        let now_live2 = session
            .read_live_polling_rate(profile(1))
            .await
            .expect("alias 1 while live 2 must return live 2 rate");
        assert_eq!(now_live2, PollingRate::Hz250);
        // Write via live 2 alias: immediate visibility.
        session
            .write_polling_rate_unchecked(
                profile(2),
                PollingRate::Hz500,
                VerificationMethod::Readback,
            )
            .await
            .expect("write alias 2 while live 2");
        let immediate = session
            .read_live_polling_rate(profile(2))
            .await
            .expect("must see new live rate");
        assert_eq!(immediate, PollingRate::Hz500);
        // Metadata activation updates live profile.
        let meta = attack_shark_x3::ProfileMetadata::new(profile(2), profile(5)).expect("metadata");
        session
            .write_profile_metadata(meta)
            .await
            .expect("metadata write");
        let after_meta = session
            .read_live_polling_rate(profile(1))
            .await
            .expect("after metadata activation live is 2");
        assert_eq!(after_meta, PollingRate::Hz500);
    }
}
