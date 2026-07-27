use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

#[cfg(not(windows))]
use bluest::Characteristic;
use bluest::{Adapter, BluetoothUuidExt, Device, DeviceId, Uuid};
use futures_lite::{Stream, StreamExt};
use thiserror::Error;
#[cfg(not(windows))]
use tokio::{sync::oneshot, task::JoinHandle};
use tokio::{
    sync::{Mutex, mpsc},
    time::{Instant, timeout},
};

use crate::{
    ButtonsReport, ButtonsState, DpiReport, DpiState, PollingRate, PollingRateReport,
    PreferencesReport, PreferencesState, ProfileControlFraming, ProfileControlReport,
    ProfileMetadata, ProtocolError, TransportKind,
};
#[cfg(windows)]
mod windows;

const NOTIFICATION_QUEUE_CAPACITY: usize = 16;
/// Returns the custom X3/M600 configuration service UUID.
#[must_use]
pub fn fee0_service() -> Uuid {
    Uuid::from_u16(0xfee0)
}

/// Returns the application-report write characteristic UUID.
#[must_use]
pub fn fee3_write() -> Uuid {
    Uuid::from_u16(0xfee3)
}

/// Returns the application ACK notification characteristic UUID.
#[must_use]
pub fn fee4_ack() -> Uuid {
    Uuid::from_u16(0xfee4)
}
/// A platform-specific identifier for an already-known BLE device.
pub type BleDeviceId = DeviceId;

/// Errors returned by BLE discovery, validation, writes, and ACK handling.
#[derive(Debug, Error)]
pub enum BleError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("Bluetooth adapter was not found or is disabled")]
    AdapterUnavailable,
    #[error("Bluetooth backend operation failed: {0}")]
    Backend(String),
    #[error("Bluetooth operation `{operation}` failed: {details}")]
    Operation {
        operation: &'static str,
        details: String,
    },
    #[error("no connected BLE device exposing FEE0 was found")]
    DeviceNotFound,
    #[error("{count} connected BLE devices exposing FEE0 were found; select an exact device")]
    AmbiguousDevice { count: usize },
    #[error("BLE device {id} could not be opened")]
    DeviceOpen { id: String },
    #[error("BLE device is disconnected")]
    Disconnected,
    #[error("BLE service {uuid} was not found")]
    MissingService { uuid: Uuid },
    #[error("multiple BLE services matched {uuid}")]
    AmbiguousService { uuid: Uuid },
    #[error("BLE characteristic {uuid} was not found")]
    MissingCharacteristic { uuid: Uuid },
    #[error("multiple BLE characteristics matched {uuid}")]
    AmbiguousCharacteristic { uuid: Uuid },
    #[error("BLE characteristic {uuid} does not support the required operation")]
    InvalidCharacteristicProperties { uuid: Uuid },
    #[error("BLE report 0x{report_id:02x} has no supported ACK path")]
    UnsupportedReport { report_id: u8 },
    #[error("BLE report 0x{report_id:02x} was rejected by the mouse")]
    AckRejected { report_id: u8 },
    #[error("timed out waiting for the BLE ACK for report 0x{report_id:02x}")]
    AckTimeout { report_id: u8 },
    #[error("malformed BLE ACK notification")]
    MalformedAck,
    #[error("BLE notification queue overflowed")]
    NotificationOverflow,
    #[error("BLE notification stream failed: {0}")]
    Notification(String),
}

/// Bounded policy for BLE application-report transactions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlePolicy {
    /// Maximum time to wait for the matching FEE4 ACK.
    pub ack_timeout: Duration,
    /// Delay after an accepted write before another write is allowed.
    pub write_quiet_period: Duration,
}

impl Default for BlePolicy {
    fn default() -> Self {
        Self {
            ack_timeout: Duration::from_secs(2),
            write_quiet_period: Duration::from_millis(500),
        }
    }
}

/// A complete typed report that is safe to send through the production BLE API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BleReport {
    Dpi(DpiState),
    Preferences(PreferencesState),
    PollingRate(PollingRate),
    Buttons(ButtonsState),
    ProfileControl(ProfileMetadata),
}

impl BleReport {
    /// Returns the application report identifier.
    #[must_use]
    pub fn report_id(&self) -> u8 {
        match self {
            Self::Dpi(_) => 0x04,
            Self::Preferences(_) => 0x05,
            Self::PollingRate(_) => 0x06,
            Self::Buttons(_) => 0x08,
            Self::ProfileControl(_) => 0x0c,
        }
    }

    fn encode(self) -> Result<Vec<u8>, BleError> {
        match self {
            Self::Dpi(state) => {
                let report = DpiReport::encode(&state, TransportKind::Ble)?;
                DpiReport::decode(report.as_bytes(), TransportKind::Ble, state.profile)?;
                Ok(report.as_bytes().to_vec())
            }
            Self::Preferences(state) => {
                let report =
                    PreferencesReport::encode_framed(&state, crate::PreferencesFraming::Compact);
                PreferencesReport::decode(report.as_bytes(), state.profile)?;
                Ok(report.as_bytes().to_vec())
            }
            Self::PollingRate(rate) => {
                let report = PollingRateReport::encode(rate);
                PollingRateReport::decode(report.as_bytes())?;
                Ok(report.as_bytes().to_vec())
            }
            Self::Buttons(state) => {
                let report = ButtonsReport::encode(&state);
                ButtonsReport::decode(report.as_bytes(), state.profile)?;
                Ok(report.as_bytes().to_vec())
            }
            Self::ProfileControl(metadata) => {
                let report = ProfileControlReport::encode(metadata, ProfileControlFraming::Compact);
                Ok(report.as_bytes().to_vec())
            }
        }
    }
}

/// The result of a write accepted by the mouse's BLE application parser.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BleWriteReceipt {
    pub report_id: u8,
    pub ack_status: u8,
}

/// A decoded FEE4 application ACK.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BleAck {
    pub report_id: u8,
    pub status: u8,
}

/// Decodes a FEE4 notification, ignoring known unsolicited notification shapes.
///
/// Returns `Ok(None)` for non-ACK FEE4 notifications. The application ACK format
/// is exactly `10 50 <status> <report_id>`.
///
/// # Errors
///
/// Returns [`BleError::MalformedAck`] when an ACK-shaped notification has an
/// unknown status byte.
pub fn decode_ack(notification: &[u8]) -> Result<Option<BleAck>, BleError> {
    if notification.len() != 4 || notification[0..2] != [0x10, 0x50] {
        return Ok(None);
    }
    match notification[2] {
        0x00 | 0x01 => Ok(Some(BleAck {
            status: notification[2],
            report_id: notification[3],
        })),
        _ => Err(BleError::MalformedAck),
    }
}

#[derive(Clone)]
struct NotificationSender {
    sender: mpsc::Sender<Result<Vec<u8>, BleError>>,
    overflowed: Arc<AtomicBool>,
}

struct NotificationInbox {
    receiver: mpsc::Receiver<Result<Vec<u8>, BleError>>,
    overflowed: Arc<AtomicBool>,
}

fn notification_channel() -> (NotificationSender, NotificationInbox) {
    let (sender, receiver) = mpsc::channel(NOTIFICATION_QUEUE_CAPACITY);
    let overflowed = Arc::new(AtomicBool::new(false));
    (
        NotificationSender {
            sender,
            overflowed: Arc::clone(&overflowed),
        },
        NotificationInbox {
            receiver,
            overflowed,
        },
    )
}

impl NotificationSender {
    fn send(&self, notification: Result<Vec<u8>, BleError>) {
        if self.sender.try_send(notification).is_err() {
            self.overflowed.store(true, Ordering::Release);
        }
    }
}

impl NotificationInbox {
    fn drain_idle(&mut self) -> Result<(), BleError> {
        if self.overflowed.load(Ordering::Acquire) {
            return Err(BleError::NotificationOverflow);
        }
        loop {
            match self.receiver.try_recv() {
                Ok(notification) => {
                    notification?;
                }
                Err(mpsc::error::TryRecvError::Empty) => return Ok(()),
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    return Err(BleError::Disconnected);
                }
            }
        }
    }
}

impl Stream for NotificationInbox {
    type Item = Result<Vec<u8>, BleError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let inbox = self.get_mut();
        if inbox.overflowed.load(Ordering::Acquire) {
            return Poll::Ready(Some(Err(BleError::NotificationOverflow)));
        }
        inbox.receiver.poll_recv(context)
    }
}

async fn complete_ble_write<S, F>(
    report_id: u8,
    ack_timeout: Duration,
    notifications: &mut S,
    write: F,
) -> Result<BleWriteReceipt, BleError>
where
    S: Stream<Item = Result<Vec<u8>, BleError>> + Unpin,
    F: Future<Output = Result<(), BleError>>,
{
    write.await?;
    let deadline = Instant::now() + ack_timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(BleError::AckTimeout { report_id });
        }
        let notification = match timeout(remaining, notifications.next()).await {
            Ok(Some(Ok(value))) => value,
            Ok(Some(Err(error))) => return Err(error),
            Ok(None) => return Err(BleError::Disconnected),
            Err(_) => return Err(BleError::AckTimeout { report_id }),
        };
        let Some(ack) = decode_ack(&notification)? else {
            continue;
        };
        if ack.report_id != report_id {
            continue;
        }
        if ack.status != 0 {
            return Err(BleError::AckRejected { report_id });
        }
        return Ok(BleWriteReceipt {
            report_id,
            ack_status: ack.status,
        });
    }
}

/// Selects an already-connected or previously remembered device.
#[derive(Clone, Debug)]
pub enum BleSelector {
    /// Select the sole connected FEE0 device.
    UniqueConnected,
    /// Reopen a platform-specific device ID returned by `list_connected`.
    Device(BleDeviceId),
    /// Select a connected FEE0 device by its advertised/OS-visible name.
    Name(String),
}

/// Public information about an already-connected BLE device.
#[derive(Clone, Debug)]
pub struct BleDeviceInfo {
    pub id: BleDeviceId,
    pub name: Option<String>,
    pub connected: bool,
}

struct BleSession {
    adapter: Adapter,
    device: Device,
    #[cfg(windows)]
    windows_session: windows::WindowsGattSession,
    #[cfg(not(windows))]
    write_characteristic: Characteristic,
    #[cfg(not(windows))]
    notifications: NotificationInbox,
    #[cfg(not(windows))]
    notification_shutdown: Option<oneshot::Sender<()>>,
    #[cfg(not(windows))]
    notification_task: Option<JoinHandle<()>>,
    policy: BlePolicy,
    closed: bool,
    shutdown_complete: bool,
}

/// Cloneable handle for serialized, ACK-confirmed BLE writes.
#[derive(Clone)]
pub struct BleHandle {
    session: Arc<Mutex<BleSession>>,
}

impl BleHandle {
    /// Lists devices currently connected by the operating system that expose FEE0.
    ///
    /// # Errors
    ///
    /// Returns a backend error when the adapter or connected-device query
    /// fails.
    pub async fn list_connected() -> Result<Vec<BleDeviceInfo>, BleError> {
        let adapter = Adapter::default()
            .await
            .ok_or(BleError::AdapterUnavailable)?;
        adapter
            .wait_available()
            .await
            .map_err(|error| BleError::Backend(error.to_string()))?;
        let devices = adapter
            .connected_devices_with_services(&[fee0_service()])
            .await
            .map_err(|error| BleError::Backend(error.to_string()))?;
        let mut result = Vec::with_capacity(devices.len());
        for device in devices {
            result.push(BleDeviceInfo {
                id: device.id(),
                name: device.name_async().await.ok(),
                connected: device.is_connected().await,
            });
        }
        Ok(result)
    }

    /// Opens an already-known device without invoking pairing or unpairing APIs.
    ///
    /// # Errors
    ///
    /// Returns a discovery, service, or backend error when no suitable
    /// already-known device can be opened.
    pub async fn open(selector: BleSelector) -> Result<Self, BleError> {
        Self::open_with_policy(selector, BlePolicy::default()).await
    }

    /// Opens a device with an explicit bounded ACK and write pacing policy.
    ///
    /// # Errors
    ///
    /// Returns a discovery, service, characteristic, or backend error when
    /// the selected device cannot provide the X3 configuration GATT contract.
    pub async fn open_with_policy(
        selector: BleSelector,
        policy: BlePolicy,
    ) -> Result<Self, BleError> {
        let adapter = Adapter::default()
            .await
            .ok_or(BleError::AdapterUnavailable)?;
        adapter
            .wait_available()
            .await
            .map_err(|error| BleError::Backend(error.to_string()))?;
        let device = match selector {
            BleSelector::UniqueConnected => {
                let devices = adapter
                    .connected_devices_with_services(&[fee0_service()])
                    .await
                    .map_err(|error| BleError::Backend(error.to_string()))?;
                match devices.len() {
                    0 => return Err(BleError::DeviceNotFound),
                    1 => devices.into_iter().next().ok_or(BleError::DeviceNotFound)?,
                    count => return Err(BleError::AmbiguousDevice { count }),
                }
            }
            BleSelector::Name(name) => {
                let devices = adapter
                    .connected_devices_with_services(&[fee0_service()])
                    .await
                    .map_err(|error| BleError::Backend(error.to_string()))?;
                let mut matches = Vec::new();
                for device in devices {
                    if device.name_async().await.ok().as_deref() == Some(name.as_str()) {
                        matches.push(device);
                    }
                }
                match matches.len() {
                    0 => return Err(BleError::DeviceNotFound),
                    1 => matches.into_iter().next().ok_or(BleError::DeviceNotFound)?,
                    count => return Err(BleError::AmbiguousDevice { count }),
                }
            }
            BleSelector::Device(id) => adapter
                .open_device(&id)
                .await
                .map_err(|_| BleError::DeviceOpen { id: id.to_string() })?,
        };
        #[cfg(not(windows))]
        if !device.is_connected().await {
            adapter
                .connect_device(&device)
                .await
                .map_err(|error| BleError::Operation {
                    operation: "connect BLE device",
                    details: error.to_string(),
                })?;
        }
        #[cfg(windows)]
        let windows_session = windows::WindowsGattSession::open(
            &device.id().to_string(),
            fee0_service().as_u128(),
            fee3_write().as_u128(),
            fee4_ack().as_u128(),
        )
        .await?;
        #[cfg(not(windows))]
        let (write_characteristic, ack_characteristic) =
            discover_bluest_characteristics(&device).await?;
        #[cfg(not(windows))]
        let (notifications, notification_shutdown, notification_task) =
            start_bluest_notifications(ack_characteristic).await?;

        Ok(Self {
            session: Arc::new(Mutex::new(BleSession {
                adapter,
                device,
                #[cfg(windows)]
                windows_session,
                #[cfg(not(windows))]
                write_characteristic,
                #[cfg(not(windows))]
                notifications,
                #[cfg(not(windows))]
                notification_shutdown: Some(notification_shutdown),
                #[cfg(not(windows))]
                notification_task: Some(notification_task),
                shutdown_complete: false,
                policy,
                closed: false,
            })),
        })
    }

    /// Writes one complete typed report and waits for its matching FEE4 ACK.
    ///
    /// # Errors
    ///
    /// Returns protocol-validation, transport, rejection, disconnection, or
    /// ACK-timeout errors.
    pub async fn write(&self, report: BleReport) -> Result<BleWriteReceipt, BleError> {
        let packet = report.encode()?;
        let mut session = self.session.lock().await;
        session.write(&packet).await
    }
    /// Encodes and writes a complete DPI state.
    ///
    /// # Errors
    ///
    /// Returns the same validation, transport, rejection, and ACK errors as
    /// [`Self::write`].
    pub async fn write_dpi(&self, state: DpiState) -> Result<BleWriteReceipt, BleError> {
        self.write(BleReport::Dpi(state)).await
    }

    /// Encodes and writes a complete preferences state.
    ///
    /// # Errors
    ///
    /// Returns the same validation, transport, rejection, and ACK errors as
    /// [`Self::write`].
    pub async fn write_preferences(
        &self,
        state: PreferencesState,
    ) -> Result<BleWriteReceipt, BleError> {
        self.write(BleReport::Preferences(state)).await
    }

    /// Encodes and writes a polling-rate report.
    ///
    /// # Errors
    ///
    /// Returns the same validation, transport, rejection, and ACK errors as
    /// [`Self::write`].
    pub async fn write_polling_rate(&self, rate: PollingRate) -> Result<BleWriteReceipt, BleError> {
        self.write(BleReport::PollingRate(rate)).await
    }

    /// Encodes and writes a complete button table.
    ///
    /// # Errors
    ///
    /// Returns the same validation, transport, rejection, and ACK errors as
    /// [`Self::write`].
    pub async fn write_buttons(&self, state: ButtonsState) -> Result<BleWriteReceipt, BleError> {
        self.write(BleReport::Buttons(state)).await
    }

    /// Encodes and writes a compact current/maximum profile-control report.
    ///
    /// BLE returns parser acceptance for this edge-triggered operation; it
    /// cannot provide the USB metadata readback used to verify application.
    ///
    /// # Errors
    ///
    /// Returns the same validation, transport, rejection, and ACK errors as
    /// [`Self::write`].
    pub async fn write_profile_control(
        &self,
        metadata: ProfileMetadata,
    ) -> Result<BleWriteReceipt, BleError> {
        self.write(BleReport::ProfileControl(metadata)).await
    }

    /// Closes this GATT session and its persistent FEE4 subscription. It never
    /// changes OS pairing state.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the notification subscription or session
    /// cannot be closed.
    pub async fn disconnect(&self) -> Result<(), BleError> {
        let mut session = self.session.lock().await;
        if session.shutdown_complete {
            return Ok(());
        }
        session.closed = true;
        let subscription_result = session.close_notifications().await;
        let disconnect_result = session
            .adapter
            .disconnect_device(&session.device)
            .await
            .map_err(|error| BleError::Backend(error.to_string()));
        session.shutdown_complete = true;
        subscription_result?;
        disconnect_result
    }
}

#[cfg(not(windows))]
async fn discover_bluest_characteristics(
    device: &Device,
) -> Result<(Characteristic, Characteristic), BleError> {
    let services = device
        .discover_services()
        .await
        .map_err(|error| BleError::Operation {
            operation: "discover GATT services",
            details: error.to_string(),
        })?;
    let mut services = services
        .into_iter()
        .filter(|service| service.uuid() == fee0_service());
    let service = services.next().ok_or(BleError::MissingService {
        uuid: fee0_service(),
    })?;
    if services.next().is_some() {
        return Err(BleError::AmbiguousService {
            uuid: fee0_service(),
        });
    }
    let characteristics =
        service
            .discover_characteristics()
            .await
            .map_err(|error| BleError::Operation {
                operation: "discover FEE0 characteristics",
                details: error.to_string(),
            })?;
    let mut writes = characteristics
        .iter()
        .filter(|characteristic| characteristic.uuid() == fee3_write());
    let write_characteristic = writes
        .next()
        .cloned()
        .ok_or(BleError::MissingCharacteristic { uuid: fee3_write() })?;
    if writes.next().is_some() {
        return Err(BleError::AmbiguousCharacteristic { uuid: fee3_write() });
    }
    let mut acks = characteristics
        .iter()
        .filter(|characteristic| characteristic.uuid() == fee4_ack());
    let ack_characteristic = acks
        .next()
        .cloned()
        .ok_or(BleError::MissingCharacteristic { uuid: fee4_ack() })?;
    if acks.next().is_some() {
        return Err(BleError::AmbiguousCharacteristic { uuid: fee4_ack() });
    }
    if !write_characteristic
        .properties()
        .await
        .map_err(|error| BleError::Operation {
            operation: "read FEE3 properties",
            details: error.to_string(),
        })?
        .write
    {
        return Err(BleError::InvalidCharacteristicProperties { uuid: fee3_write() });
    }
    if !ack_characteristic
        .properties()
        .await
        .map_err(|error| BleError::Operation {
            operation: "read FEE4 properties",
            details: error.to_string(),
        })?
        .notify
    {
        return Err(BleError::InvalidCharacteristicProperties { uuid: fee4_ack() });
    }
    Ok((write_characteristic, ack_characteristic))
}

#[cfg(not(windows))]
async fn start_bluest_notifications(
    ack_characteristic: Characteristic,
) -> Result<(NotificationInbox, oneshot::Sender<()>, JoinHandle<()>), BleError> {
    let (notification_sender, notifications) = notification_channel();
    let (ready_sender, ready_receiver) = oneshot::channel();
    let (shutdown_sender, mut shutdown_receiver) = oneshot::channel();
    let task = tokio::spawn(async move {
        let mut stream = match ack_characteristic.notify().await {
            Ok(stream) => {
                let _ = ready_sender.send(Ok(()));
                stream
            }
            Err(error) => {
                let _ = ready_sender.send(Err(BleError::Operation {
                    operation: "enable FEE4 notifications",
                    details: error.to_string(),
                }));
                return;
            }
        };
        loop {
            tokio::select! {
                _ = &mut shutdown_receiver => return,
                notification = stream.next() => {
                    match notification {
                        Some(Ok(value)) => notification_sender.send(Ok(value)),
                        Some(Err(error)) => {
                            notification_sender.send(Err(BleError::Notification(
                                error.to_string(),
                            )));
                            return;
                        }
                        None => {
                            notification_sender.send(Err(BleError::Disconnected));
                            return;
                        }
                    }
                }
            }
        }
    });
    match ready_receiver.await {
        Ok(Ok(())) => Ok((notifications, shutdown_sender, task)),
        Ok(Err(error)) => {
            let _ = task.await;
            Err(error)
        }
        Err(error) => {
            let task_result = task.await;
            Err(BleError::Operation {
                operation: "start FEE4 notification task",
                details: task_result
                    .err()
                    .map_or_else(|| error.to_string(), |error| error.to_string()),
            })
        }
    }
}

impl BleSession {
    async fn write(&mut self, packet: &[u8]) -> Result<BleWriteReceipt, BleError> {
        if self.closed {
            return Err(BleError::Disconnected);
        }
        let report_id = *packet.first().ok_or(BleError::MalformedAck)?;
        #[cfg(windows)]
        let result = self
            .windows_session
            .write(report_id, packet, self.policy.ack_timeout)
            .await;
        #[cfg(not(windows))]
        let result = self.write_bluest(report_id, packet).await;
        match result {
            Ok(receipt) => {
                if !self.policy.write_quiet_period.is_zero() {
                    tokio::time::sleep(self.policy.write_quiet_period).await;
                }
                Ok(receipt)
            }
            Err(error) => {
                self.closed = true;
                let _ = self.close_notifications().await;
                Err(error)
            }
        }
    }

    async fn close_notifications(&mut self) -> Result<(), BleError> {
        #[cfg(windows)]
        {
            self.windows_session.close().await
        }
        #[cfg(not(windows))]
        {
            self.stop_bluest_notifications().await
        }
    }

    #[cfg(not(windows))]
    async fn write_bluest(
        &mut self,
        report_id: u8,
        packet: &[u8],
    ) -> Result<BleWriteReceipt, BleError> {
        self.notifications.drain_idle()?;
        let write_characteristic = &self.write_characteristic;
        complete_ble_write(
            report_id,
            self.policy.ack_timeout,
            &mut self.notifications,
            async move {
                write_characteristic
                    .write(packet)
                    .await
                    .map_err(|error| BleError::Operation {
                        operation: "write packet to FEE3",
                        details: error.to_string(),
                    })
            },
        )
        .await
    }

    #[cfg(not(windows))]
    async fn stop_bluest_notifications(&mut self) -> Result<(), BleError> {
        if let Some(shutdown) = self.notification_shutdown.take() {
            let _ = shutdown.send(());
        }
        let Some(task) = self.notification_task.take() else {
            return Ok(());
        };
        task.await.map_err(|error| BleError::Operation {
            operation: "stop FEE4 notification task",
            details: error.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        BleError, BleReport, NOTIFICATION_QUEUE_CAPACITY, complete_ble_write, decode_ack,
        notification_channel,
    };
    use crate::{
        ButtonAssignment, ButtonsState, DpiState, DpiValue, PollingRate, PreferencesState,
        ProfileId, ProfileMetadata, StageIndex,
    };

    fn profile() -> ProfileId {
        ProfileId::new(1).expect("valid profile")
    }

    #[test]
    fn decodes_only_application_acks() {
        assert_eq!(
            decode_ack(&[0x10, 0x50, 0x00, 0x04]).expect("valid ACK"),
            Some(super::BleAck {
                status: 0,
                report_id: 0x04
            })
        );
        assert_eq!(
            decode_ack(&[0x00, 0x80, 0x00, 0x00]).expect("state event"),
            None
        );
        assert!(matches!(
            decode_ack(&[0x10, 0x50, 0x02, 0x04]),
            Err(BleError::MalformedAck)
        ));
    }

    #[tokio::test]
    async fn shared_transaction_ignores_unrelated_notifications() {
        let mut notifications = futures_lite::stream::iter([
            Ok(vec![0x00, 0x80, 0x00, 0x00]),
            Ok(vec![0x10, 0x50, 0x00, 0x05]),
            Ok(vec![0x10, 0x50, 0x00, 0x06]),
        ]);
        let receipt =
            complete_ble_write(0x06, Duration::from_millis(50), &mut notifications, async {
                Ok(())
            })
            .await
            .expect("matching ACK must complete the shared transaction");
        assert_eq!(receipt.report_id, 0x06);
        assert_eq!(receipt.ack_status, 0x00);
    }

    #[tokio::test]
    async fn persistent_inbox_discards_idle_events_and_supports_repeated_writes() {
        let (sender, mut notifications) = notification_channel();
        sender.send(Ok(vec![0x00, 0x80, 0x00, 0x00]));
        notifications
            .drain_idle()
            .expect("idle notification must be discarded");

        for report_id in [0x06, 0x0c] {
            sender.send(Ok(vec![0x10, 0x50, 0x00, report_id]));
            let receipt = complete_ble_write(
                report_id,
                Duration::from_millis(50),
                &mut notifications,
                async { Ok(()) },
            )
            .await
            .expect("matching ACK must complete each transaction");
            assert_eq!(receipt.report_id, report_id);
        }
    }

    #[test]
    fn persistent_inbox_fails_closed_on_overflow() {
        let (sender, mut notifications) = notification_channel();
        for report_id in 0..=NOTIFICATION_QUEUE_CAPACITY {
            let report_id = u8::try_from(report_id).expect("queue capacity fits in a byte");
            sender.send(Ok(vec![0x10, 0x50, 0x00, report_id]));
        }
        assert!(matches!(
            notifications.drain_idle(),
            Err(BleError::NotificationOverflow)
        ));
    }

    #[test]
    fn validates_all_initial_typed_reports_before_write() {
        let dpi = DpiState::new(
            profile(),
            vec![DpiValue::new(800).expect("valid DPI")],
            StageIndex::new(1).expect("valid stage"),
            [0; 25],
        )
        .expect("valid DPI state");
        let preferences = PreferencesState::new(profile(), 0, 0, 0, [0, 0, 0], 0, 0);
        let slots = [ButtonAssignment::default(); crate::protocol::buttons::BUTTON_SLOT_COUNT];
        for report in [
            BleReport::Dpi(dpi),
            BleReport::Preferences(preferences),
            BleReport::PollingRate(PollingRate::Hz1000),
            BleReport::Buttons(ButtonsState::new(profile(), slots)),
        ] {
            assert!(!report.encode().expect("typed report validates").is_empty());
        }

        let metadata =
            ProfileMetadata::new(profile(), ProfileId::new(5).expect("valid maximum profile"))
                .expect("valid profile metadata");
        let profile_report = BleReport::ProfileControl(metadata);
        assert_eq!(profile_report.report_id(), 0x0c);
        assert_eq!(
            profile_report.encode().expect("profile report validates"),
            vec![0x0c, 0x0a, 0x01, 0xfe, 0x05, 0xfa]
        );
    }
}
