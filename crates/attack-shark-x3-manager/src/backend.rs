use std::time::Duration;
#[cfg(test)]
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
#[cfg(feature = "usb")]
use attack_shark_x3::MouseHandle;
#[cfg(any(feature = "usb", feature = "ble"))]
use attack_shark_x3::driver::ProfileSnapshot;
use attack_shark_x3::{
    BatteryEvent, ButtonsState, DpiButtonEvent, DpiState, PollingRate, PreferencesState, ProfileId,
    ProfileMetadata, TransportKind,
};
#[cfg(feature = "ble")]
use attack_shark_x3::{BleDeviceId, BleHandle, BleSelector};
#[cfg(feature = "usb")]
use attack_shark_x3::{DeviceSelector, UsbDeviceKind, list_devices_for};
use tokio::sync::broadcast;

#[cfg(any(feature = "usb", feature = "ble"))]
use crate::device::DeviceLocator;
#[cfg(feature = "ble")]
use crate::error::StateError;
use crate::{
    device::{DeviceIdentity, TransportSelection},
    error::ManagerError,
    operation::DiscoveredDevice,
};

/// The result shape of a typed session write.
///
/// USB writes are only successful after the low-level worker has performed its
/// fresh readback check. BLE has no read path, so an accepted application ACK is
/// the strongest evidence available to the manager.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SessionWrite<T> {
    ReadbackVerified(T),
    Acknowledged,
}

/// Optional input streams owned by one open session.
///
/// BLE configuration sessions do not expose either stream. USB sessions expose
/// the low-level HID event receivers directly and preserve their transport
/// semantics for the resource layer.
pub(crate) struct SessionEvents {
    pub(crate) dpi_button: Option<broadcast::Receiver<DpiButtonEvent>>,
    pub(crate) battery: Option<broadcast::Receiver<BatteryEvent>>,
}

#[async_trait(?Send)]
pub(crate) trait SessionFactory: Send + Sync {
    async fn list(
        &self,
        selection: TransportSelection,
    ) -> Result<Vec<DiscoveredDevice>, ManagerError>;

    async fn open(&self, identity: &DeviceIdentity)
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
    async fn read_polling_rate(&self) -> Result<PollingRate, ManagerError>;

    async fn write_dpi(&self, state: DpiState) -> Result<SessionWrite<DpiState>, ManagerError>;
    async fn write_preferences(
        &self,
        state: PreferencesState,
    ) -> Result<SessionWrite<PreferencesState>, ManagerError>;
    async fn write_buttons(
        &self,
        state: ButtonsState,
    ) -> Result<SessionWrite<ButtonsState>, ManagerError>;
    async fn write_polling_rate(
        &self,
        rate: PollingRate,
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
fn list_usb_devices(kind: UsbDeviceKind) -> Result<Vec<DiscoveredDevice>, ManagerError> {
    list_devices_for(kind)?
        .into_iter()
        .map(|info| {
            Ok(DiscoveredDevice {
                identity: DeviceIdentity::usb(
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
async fn list_ble_devices() -> Result<Vec<DiscoveredDevice>, ManagerError> {
    let devices = BleHandle::list_connected().await?;
    devices
        .into_iter()
        .map(|info| {
            let stable_id = serde_json::to_string(&info.id)
                .map_err(|error| ManagerError::State(StateError::Serde(error)))?;
            Ok(DiscoveredDevice {
                identity: DeviceIdentity::ble(&stable_id, info.name.as_deref())?,
                connected: info.connected,
            })
        })
        .collect()
}

#[cfg(feature = "usb")]
async fn open_usb_session(
    identity: &DeviceIdentity,
    kind: UsbDeviceKind,
) -> Result<Box<dyn DeviceSession>, ManagerError> {
    let DeviceLocator::UsbPath(path) = &identity.locator else {
        return Err(ManagerError::DeviceNotFound(identity.id.clone()));
    };
    let handle = MouseHandle::open_for_kind(DeviceSelector::path(path.clone()), kind)?;
    Ok(Box::new(UsbSession {
        handle,
        transport: kind.transport_kind(),
    }))
}

#[cfg(feature = "ble")]
async fn open_ble_session(
    identity: &DeviceIdentity,
) -> Result<Box<dyn DeviceSession>, ManagerError> {
    let DeviceLocator::BlePlatformId(serialized_id) = &identity.locator else {
        return Err(ManagerError::DeviceNotFound(identity.id.clone()));
    };
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
    ) -> Result<Vec<DiscoveredDevice>, ManagerError> {
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
        identity: &DeviceIdentity,
    ) -> Result<Box<dyn DeviceSession>, ManagerError> {
        match identity.transport {
            TransportKind::Wired => {
                #[cfg(feature = "usb")]
                return open_usb_session(identity, UsbDeviceKind::Wired).await;
                #[cfg(not(feature = "usb"))]
                Err(ManagerError::UnsupportedOperation {
                    operation: "open",
                    transport: TransportKind::Wired,
                })
            }
            TransportKind::Receiver => {
                #[cfg(feature = "usb")]
                return open_usb_session(identity, UsbDeviceKind::Receiver).await;
                #[cfg(not(feature = "usb"))]
                Err(ManagerError::UnsupportedOperation {
                    operation: "open",
                    transport: TransportKind::Receiver,
                })
            }
            TransportKind::Ble => {
                #[cfg(feature = "ble")]
                return open_ble_session(identity).await;
                #[cfg(not(feature = "ble"))]
                Err(ManagerError::UnsupportedOperation {
                    operation: "open",
                    transport: TransportKind::Ble,
                })
            }
        }
    }
}

#[cfg(feature = "usb")]
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

    async fn read_polling_rate(&self) -> Result<PollingRate, ManagerError> {
        Ok(self.handle.read_polling_rate().await?)
    }

    async fn write_dpi(&self, state: DpiState) -> Result<SessionWrite<DpiState>, ManagerError> {
        Ok(SessionWrite::ReadbackVerified(
            self.handle.write_dpi(state).await?,
        ))
    }

    async fn write_preferences(
        &self,
        state: PreferencesState,
    ) -> Result<SessionWrite<PreferencesState>, ManagerError> {
        Ok(SessionWrite::ReadbackVerified(
            self.handle.write_preferences(state).await?,
        ))
    }

    async fn write_buttons(
        &self,
        state: ButtonsState,
    ) -> Result<SessionWrite<ButtonsState>, ManagerError> {
        Ok(SessionWrite::ReadbackVerified(
            self.handle.write_buttons(state).await?,
        ))
    }

    async fn write_polling_rate(
        &self,
        rate: PollingRate,
    ) -> Result<SessionWrite<PollingRate>, ManagerError> {
        Ok(SessionWrite::ReadbackVerified(
            self.handle.write_polling_rate(rate).await?,
        ))
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
            dpi_button: Some(self.handle.subscribe_dpi_button_events()),
            battery: Some(self.handle.subscribe_battery_events()),
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

    async fn read_polling_rate(&self) -> Result<PollingRate, ManagerError> {
        Self::unsupported("read_polling_rate")
    }

    async fn write_dpi(&self, state: DpiState) -> Result<SessionWrite<DpiState>, ManagerError> {
        self.handle.write_dpi(state).await?;
        Ok(SessionWrite::Acknowledged)
    }

    async fn write_preferences(
        &self,
        state: PreferencesState,
    ) -> Result<SessionWrite<PreferencesState>, ManagerError> {
        self.handle.write_preferences(state).await?;
        Ok(SessionWrite::Acknowledged)
    }

    async fn write_buttons(
        &self,
        state: ButtonsState,
    ) -> Result<SessionWrite<ButtonsState>, ManagerError> {
        self.handle.write_buttons(state).await?;
        Ok(SessionWrite::Acknowledged)
    }

    async fn write_polling_rate(
        &self,
        rate: PollingRate,
    ) -> Result<SessionWrite<PollingRate>, ManagerError> {
        self.handle.write_polling_rate(rate).await?;
        Ok(SessionWrite::Acknowledged)
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
        SessionEvents {
            dpi_button: None,
            battery: None,
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FakeWriteMode {
    Readback,
    Ack,
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ScriptedWrite {
    Dpi(DpiState),
    Preferences(PreferencesState),
    Buttons(ButtonsState),
    PollingRate(PollingRate),
    ProfileMetadata(ProfileMetadata),
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct ScriptedFakeSession {
    transport: TransportKind,
    write_mode: FakeWriteMode,
    metadata: Arc<Mutex<Option<ProfileMetadata>>>,
    profiles: Arc<Mutex<BTreeMap<ProfileId, ProfileSnapshot>>>,
    polling_rate: Arc<Mutex<Option<PollingRate>>>,
    battery: Arc<Mutex<Option<u8>>>,
    writes: Arc<Mutex<Vec<ScriptedWrite>>>,
    dpi_button_events: broadcast::Sender<DpiButtonEvent>,
    battery_events: broadcast::Sender<BatteryEvent>,
}

#[cfg(test)]
fn lock_scripted<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value.lock().expect("scripted fake state lock poisoned")
}

#[cfg(test)]
impl ScriptedFakeSession {
    fn new(transport: TransportKind, write_mode: FakeWriteMode) -> Self {
        let (dpi_button_events, _) = broadcast::channel(16);
        let (battery_events, _) = broadcast::channel(16);
        Self {
            transport,
            write_mode,
            metadata: Arc::new(Mutex::new(None)),
            profiles: Arc::new(Mutex::new(BTreeMap::new())),
            polling_rate: Arc::new(Mutex::new(None)),
            battery: Arc::new(Mutex::new(None)),
            writes: Arc::new(Mutex::new(Vec::new())),
            dpi_button_events,
            battery_events,
        }
    }

    pub(crate) fn usb() -> Self {
        Self::new(TransportKind::Wired, FakeWriteMode::Readback)
    }

    pub(crate) fn ble() -> Self {
        Self::new(TransportKind::Ble, FakeWriteMode::Ack)
    }

    pub(crate) fn with_metadata(self, metadata: ProfileMetadata) -> Self {
        *lock_scripted(&self.metadata) = Some(metadata);
        self
    }

    pub(crate) fn with_profile(self, snapshot: ProfileSnapshot) -> Self {
        lock_scripted(&self.profiles).insert(snapshot.target_profile, snapshot);
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

    fn unsupported<T>(&self, operation: &'static str) -> Result<T, ManagerError> {
        Err(ManagerError::UnsupportedOperation {
            operation,
            transport: self.transport,
        })
    }

    fn write_outcome<T>(&self, value: T) -> SessionWrite<T> {
        match self.write_mode {
            FakeWriteMode::Readback => SessionWrite::ReadbackVerified(value),
            FakeWriteMode::Ack => SessionWrite::Acknowledged,
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
        let mut snapshot = lock_scripted(&self.profiles).get(&profile).cloned().ok_or(
            ManagerError::MissingBaseline {
                resource: "profile",
                profile: Some(profile),
            },
        )?;
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

    async fn read_polling_rate(&self) -> Result<PollingRate, ManagerError> {
        if self.is_ble() {
            return self.unsupported("read_polling_rate");
        }
        lock_scripted(&self.polling_rate)
            .as_ref()
            .copied()
            .ok_or(ManagerError::MissingBaseline {
                resource: "polling rate",
                profile: None,
            })
    }

    async fn write_dpi(&self, state: DpiState) -> Result<SessionWrite<DpiState>, ManagerError> {
        lock_scripted(&self.writes).push(ScriptedWrite::Dpi(state.clone()));
        if !self.is_ble() {
            if let Some(snapshot) = lock_scripted(&self.profiles).get_mut(&state.profile) {
                snapshot.dpi = state.clone();
            }
        }
        Ok(self.write_outcome(state))
    }

    async fn write_preferences(
        &self,
        state: PreferencesState,
    ) -> Result<SessionWrite<PreferencesState>, ManagerError> {
        lock_scripted(&self.writes).push(ScriptedWrite::Preferences(state));
        if !self.is_ble() {
            if let Some(snapshot) = lock_scripted(&self.profiles).get_mut(&state.profile) {
                snapshot.preferences = state;
            }
        }
        Ok(self.write_outcome(state))
    }

    async fn write_buttons(
        &self,
        state: ButtonsState,
    ) -> Result<SessionWrite<ButtonsState>, ManagerError> {
        lock_scripted(&self.writes).push(ScriptedWrite::Buttons(state));
        if !self.is_ble() {
            if let Some(snapshot) = lock_scripted(&self.profiles).get_mut(&state.profile) {
                snapshot.buttons = state;
            }
        }
        Ok(self.write_outcome(state))
    }

    async fn write_polling_rate(
        &self,
        rate: PollingRate,
    ) -> Result<SessionWrite<PollingRate>, ManagerError> {
        lock_scripted(&self.writes).push(ScriptedWrite::PollingRate(rate));
        if !self.is_ble() {
            *lock_scripted(&self.polling_rate) = Some(rate);
        }
        Ok(self.write_outcome(rate))
    }

    async fn write_profile_metadata(
        &self,
        metadata: ProfileMetadata,
    ) -> Result<SessionWrite<ProfileMetadata>, ManagerError> {
        lock_scripted(&self.writes).push(ScriptedWrite::ProfileMetadata(metadata));
        *lock_scripted(&self.metadata) = Some(metadata);
        Ok(self.write_outcome(metadata))
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
            SessionEvents {
                dpi_button: None,
                battery: None,
            }
        } else {
            SessionEvents {
                dpi_button: Some(self.dpi_button_events.subscribe()),
                battery: Some(self.battery_events.subscribe()),
            }
        }
    }
}

#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct ScriptedFakeFactory {
    devices: Arc<Mutex<BTreeMap<crate::device::DeviceId, DiscoveredDevice>>>,
    sessions: Arc<Mutex<BTreeMap<crate::device::DeviceId, ScriptedFakeSession>>>,
}

#[cfg(test)]
impl ScriptedFakeFactory {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with_device(
        self,
        device: DiscoveredDevice,
        session: ScriptedFakeSession,
    ) -> Self {
        self.add_device(device, session);
        self
    }

    pub(crate) fn with_identity(
        self,
        identity: DeviceIdentity,
        connected: bool,
        session: ScriptedFakeSession,
    ) -> Self {
        self.with_device(
            DiscoveredDevice {
                identity,
                connected,
            },
            session,
        )
    }

    pub(crate) fn add_device(&self, device: DiscoveredDevice, session: ScriptedFakeSession) {
        let id = device.identity.id.clone();
        lock_scripted(&self.devices).insert(id.clone(), device);
        lock_scripted(&self.sessions).insert(id, session);
    }
}

#[cfg(test)]
#[async_trait(?Send)]
impl SessionFactory for ScriptedFakeFactory {
    async fn list(
        &self,
        selection: TransportSelection,
    ) -> Result<Vec<DiscoveredDevice>, ManagerError> {
        let devices = lock_scripted(&self.devices);
        Ok(devices
            .values()
            .filter(|device| match selection {
                TransportSelection::Auto => true,
                TransportSelection::Exact(transport) => device.identity.transport == transport,
            })
            .cloned()
            .collect())
    }

    async fn open(
        &self,
        identity: &DeviceIdentity,
    ) -> Result<Box<dyn DeviceSession>, ManagerError> {
        let session = lock_scripted(&self.sessions)
            .get(&identity.id)
            .cloned()
            .ok_or_else(|| ManagerError::DeviceNotFound(identity.id.clone()))?;
        Ok(Box::new(session))
    }
}

#[cfg(test)]
mod tests {
    use super::{DeviceSession, ScriptedFakeSession, SessionWrite};
    use attack_shark_x3::PollingRate;

    #[tokio::test]
    async fn fake_sessions_preserve_transport_write_evidence() {
        let usb = ScriptedFakeSession::usb();
        assert_eq!(
            usb.write_polling_rate(PollingRate::Hz1000)
                .await
                .expect("scripted USB write succeeds"),
            SessionWrite::ReadbackVerified(PollingRate::Hz1000)
        );

        let ble = ScriptedFakeSession::ble();
        assert_eq!(
            ble.write_polling_rate(PollingRate::Hz1000)
                .await
                .expect("scripted BLE ACK succeeds"),
            SessionWrite::Acknowledged
        );
    }
}
