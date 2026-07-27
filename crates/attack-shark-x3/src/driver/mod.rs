#![cfg_attr(not(feature = "usb"), allow(dead_code, unused_imports))]

use std::{
    num::NonZeroU8,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use thiserror::Error;
use tokio::sync::{broadcast, oneshot};

use crate::{
    BatteryEvent, DpiButtonEvent, DpiReport, DpiState, PollingRate, PollingRateReport,
    PreferencesReport, PreferencesState, ProfileControlFraming, ProfileControlReport, ProfileId,
    ProfileMetadata, ProfileMetadataReport, ProtocolError, ReadSelector, ReadbackRequest,
    ReadinessStatus, TransportKind, decode_battery_report, decode_dpi_button_report,
    protocol::buttons::{ButtonsReport, ButtonsState},
};

#[cfg(feature = "usb")]
mod usb;

#[cfg(feature = "ble")]
pub mod ble;

#[cfg(feature = "usb")]
pub use usb::{DeviceInfo, DeviceSelector, UsbDeviceKind, list_devices, list_devices_for};

const MAX_FEATURE_REPORT_LENGTH: usize = 128;
const PROFILE_ACTIVATION_QUIET_PERIOD: Duration = Duration::from_millis(500);
const RECEIVER_READINESS_DELAY: Duration = Duration::from_millis(500);

/// Bounded policy for one-shot X3 configuration reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadPolicy {
    pub max_attempts: NonZeroU8,
    pub readiness_timeout: Duration,
    pub poll_interval: Duration,
    /// Delay between a configuration write and its fresh readback.
    pub write_delay: Duration,
}

impl Default for ReadPolicy {
    fn default() -> Self {
        Self {
            // The live benchmark used one initial attempt plus three retries.
            max_attempts: NonZeroU8::new(4).expect("four is nonzero"),
            readiness_timeout: Duration::from_millis(250),
            poll_interval: Duration::from_millis(1),
            write_delay: Duration::from_millis(500),
        }
    }
}
impl ReadPolicy {
    /// Returns a conservative policy for the FA60 receiver's RF round trip.
    #[must_use]
    pub fn receiver_default() -> Self {
        Self {
            readiness_timeout: Duration::from_secs(2),
            poll_interval: Duration::from_millis(5),
            ..Self::default()
        }
    }
}

/// The final retryable failure observed by an armed-read transaction.
#[derive(Debug, Eq, Error, PartialEq)]
pub enum ReadFailure {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),

    #[error("the A0 readiness mailbox did not become ready before the deadline")]
    ReadinessTimeout,
}

/// Errors produced by device discovery, the worker, or a serialized transaction.
#[derive(Debug, Error)]
pub enum DriverError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),

    #[error("HID transport error: {0}")]
    Transport(String),

    #[error("battery telemetry is available only from the 2.4 GHz receiver")]
    BatteryUnavailable,

    #[error("timed out waiting for a receiver battery report")]
    BatteryTimeout,

    #[error("the receiver input-report worker is unavailable")]
    InputUnavailable,

    #[error("no matching X3 USB configuration collection was found")]
    DeviceNotFound,

    #[error("{count} matching X3 USB configuration collections were found; select an exact path")]
    AmbiguousDevice { count: usize },

    #[error("could not start the HID worker: {0}")]
    WorkerStart(String),

    #[error("the HID worker is no longer available")]
    WorkerUnavailable,

    #[error("armed read failed after {attempts} attempts: {last}")]
    ReadAttemptsExhausted {
        attempts: u8,
        #[source]
        last: ReadFailure,
    },
    #[error("the {section} write did not verify for profile {profile}")]
    WriteVerificationMismatch {
        section: &'static str,
        profile: ProfileId,
    },
    #[error("the global {section} write did not verify")]
    GlobalWriteVerificationMismatch { section: &'static str },

    #[error("profile {profile} is already active")]
    ProfileAlreadyActive { profile: ProfileId },
    #[error("profile {profile} is not enabled (maximum enabled profile is {maximum})")]
    ProfileNotEnabled {
        profile: ProfileId,
        maximum: ProfileId,
    },
    #[error("maximum profile {maximum} is below the current profile {current}")]
    MaximumProfileBelowCurrent {
        maximum: ProfileId,
        current: ProfileId,
    },
}

/// A complete profile observation from one uninterrupted worker command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileSnapshot {
    pub persistent_metadata: ProfileMetadata,
    pub target_profile: ProfileId,
    pub dpi: DpiState,
    pub preferences: PreferencesState,
    pub buttons: ButtonsState,
}

/// Cloneable handle for serialized feature-report operations and auxiliary
/// input-event subscription.
#[derive(Clone, Debug)]
pub struct MouseHandle {
    commands: mpsc::Sender<Command>,
    dpi_button_events: broadcast::Sender<DpiButtonEvent>,
    battery_events: broadcast::Sender<BatteryEvent>,
    battery_level: Arc<Mutex<Option<u8>>>,
    input_available: Arc<AtomicBool>,
    input_stop: Arc<AtomicBool>,
    transport_kind: TransportKind,
}

impl MouseHandle {
    /// Opens one uniquely selected FA61 configuration collection and starts its
    /// exclusive worker thread.
    ///
    /// # Errors
    ///
    /// Returns an error when discovery, collection selection, device opening,
    /// or worker startup fails.
    #[cfg(feature = "usb")]
    pub fn open(selector: DeviceSelector) -> Result<Self, DriverError> {
        Self::open_for_kind_with_policy(selector, usb::UsbDeviceKind::Wired, ReadPolicy::default())
    }

    /// Opens the wired device with an explicit bounded read policy.
    ///
    /// # Errors
    ///
    /// Returns an error when discovery, collection selection, device opening,
    /// or worker startup fails.
    #[cfg(feature = "usb")]
    pub fn open_with_policy(
        selector: DeviceSelector,
        policy: ReadPolicy,
    ) -> Result<Self, DriverError> {
        Self::open_for_kind_with_policy(selector, usb::UsbDeviceKind::Wired, policy)
    }

    /// Opens one uniquely selected FA60 receiver configuration collection.
    ///
    /// # Errors
    ///
    /// Returns an error when discovery, collection selection, device opening,
    /// or worker startup fails.
    #[cfg(feature = "usb")]
    pub fn open_receiver(selector: DeviceSelector) -> Result<Self, DriverError> {
        Self::open_for_kind_with_policy(
            selector,
            usb::UsbDeviceKind::Receiver,
            ReadPolicy::receiver_default(),
        )
    }

    /// Opens a selected USB device with an explicit read policy.
    ///
    /// # Errors
    ///
    /// Returns an error when discovery, collection selection, device opening,
    /// or worker startup fails.
    #[cfg(feature = "usb")]
    pub fn open_for_kind(
        selector: DeviceSelector,
        kind: usb::UsbDeviceKind,
    ) -> Result<Self, DriverError> {
        let policy = match kind {
            usb::UsbDeviceKind::Wired => ReadPolicy::default(),
            usb::UsbDeviceKind::Receiver => ReadPolicy::receiver_default(),
        };
        Self::open_for_kind_with_policy(selector, kind, policy)
    }

    /// Opens a selected USB device with an explicit kind and read policy.
    ///
    /// # Errors
    ///
    /// Returns an error when discovery, collection selection, device opening,
    /// or worker startup fails.
    #[cfg(feature = "usb")]
    pub fn open_for_kind_with_policy(
        selector: DeviceSelector,
        kind: usb::UsbDeviceKind,
        policy: ReadPolicy,
    ) -> Result<Self, DriverError> {
        let input_selector = selector.clone();
        let handle = spawn_worker(policy, kind.transport_kind(), move || {
            usb::open_transport(&selector, kind)
                .map(|transport| Box::new(transport) as Box<dyn FeatureTransport>)
        })?;
        spawn_input_worker(
            input_selector,
            kind,
            handle.dpi_button_events.clone(),
            handle.battery_events.clone(),
            handle.battery_level.clone(),
            handle.input_available.clone(),
            Arc::downgrade(&handle.input_stop),
        )?;
        Ok(handle)
    }

    /// Subscribes to physical DPI-button pulses from the auxiliary HID input path.
    ///
    /// Works on both wired (FA61) and receiver (FA60) transports. Byte 3 of the
    /// report is the resulting one-based active DPI-stage index.
    #[must_use]
    pub fn subscribe_dpi_button_events(&self) -> broadcast::Receiver<DpiButtonEvent> {
        self.dpi_button_events.subscribe()
    }

    /// Subscribes to battery reports emitted by the FA60 receiver.
    ///
    /// Confirmed: the receiver pushes `03 10 40 01 <level>` (level 1–10, ×10 =
    /// percentage) on the auxiliary HID collection. Wired mode does not emit
    /// battery reports; the stream will be empty on FA61.
    #[must_use]
    pub fn subscribe_battery_events(&self) -> broadcast::Receiver<BatteryEvent> {
        self.battery_events.subscribe()
    }

    /// Waits for a receiver battery report, returning a cached value when one
    /// has already been observed.
    ///
    /// Battery telemetry is receiver-only (FA60). The firmware does not emit
    /// battery reports in wired mode (FA61).
    ///
    /// # Errors
    ///
    /// Returns an error for wired devices, a closed input worker, or timeout.
    pub async fn read_battery(&self, timeout: Duration) -> Result<u8, DriverError> {
        if self.transport_kind != TransportKind::Receiver {
            return Err(DriverError::BatteryUnavailable);
        }
        let mut events = self.battery_events.subscribe();
        if let Some(level) = self
            .battery_level
            .lock()
            .map_err(|_| DriverError::InputUnavailable)?
            .as_ref()
            .copied()
        {
            return Ok(level);
        }
        if !self.input_available.load(Ordering::Acquire) {
            return Err(DriverError::InputUnavailable);
        }

        let level = tokio::time::timeout(timeout, async {
            loop {
                match events.recv().await {
                    Ok(event) => break Ok(event.level),
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => {
                        break Err(DriverError::InputUnavailable);
                    }
                }
            }
        })
        .await
        .map_err(|_| DriverError::BatteryTimeout)??;
        Ok(level)
    }

    /// Reads persistent report-`0x0c` metadata.
    ///
    /// # Errors
    ///
    /// Returns transport, retry-exhaustion, protocol-validation, or worker
    /// lifecycle errors.
    pub async fn read_profile_metadata(&self) -> Result<ProfileMetadata, DriverError> {
        self.request(Command::ProfileMetadata).await
    }

    /// Reads and validates DPI state for the explicit working-profile target.
    ///
    /// # Errors
    ///
    /// Returns transport, retry-exhaustion, protocol-validation, or worker
    /// lifecycle errors.
    pub async fn read_dpi(&self, profile: ProfileId) -> Result<DpiState, DriverError> {
        self.request(|reply| Command::Dpi { profile, reply }).await
    }

    /// Reads and validates preferences for the explicit working-profile target.
    ///
    /// # Errors
    ///
    /// Returns transport, retry-exhaustion, protocol-validation, or worker
    /// lifecycle errors.
    pub async fn read_preferences(
        &self,
        profile: ProfileId,
    ) -> Result<PreferencesState, DriverError> {
        self.request(|reply| Command::Preferences { profile, reply })
            .await
    }

    /// Reads and validates all button slots for the explicit working-profile target.
    ///
    /// # Errors
    ///
    /// Returns transport, retry-exhaustion, protocol-validation, or worker
    /// lifecycle errors.
    pub async fn read_buttons(&self, profile: ProfileId) -> Result<ButtonsState, DriverError> {
        self.request(|reply| Command::Buttons { profile, reply })
            .await
    }

    /// Reads the global polling rate through a serialized USB readback.
    ///
    /// # Errors
    ///
    /// Returns transport, retry-exhaustion, protocol-validation, or worker
    /// lifecycle errors.
    pub async fn read_polling_rate(&self) -> Result<PollingRate, DriverError> {
        self.request(Command::PollingRate).await
    }

    /// Writes DPI and returns the state confirmed by a fresh readback.
    ///
    /// # Errors
    ///
    /// Returns encoding, transport, retry-exhaustion, readback-validation,
    /// write-verification, or worker lifecycle errors.
    pub async fn write_dpi(&self, state: DpiState) -> Result<DpiState, DriverError> {
        self.request(|reply| Command::WriteDpi { state, reply })
            .await
    }

    /// Writes preferences and returns the state confirmed by a fresh readback.
    ///
    /// # Errors
    ///
    /// Returns transport, retry-exhaustion, readback-validation,
    /// write-verification, or worker lifecycle errors.
    pub async fn write_preferences(
        &self,
        state: PreferencesState,
    ) -> Result<PreferencesState, DriverError> {
        self.request(|reply| Command::WritePreferences { state, reply })
            .await
    }

    /// Writes the complete button table and returns the state confirmed by a
    /// fresh readback.
    ///
    /// # Errors
    ///
    /// Returns transport, retry-exhaustion, readback-validation,
    /// write-verification, or worker lifecycle errors.
    pub async fn write_buttons(&self, state: ButtonsState) -> Result<ButtonsState, DriverError> {
        self.request(|reply| Command::WriteButtons { state, reply })
            .await
    }

    /// Writes the global polling rate and returns the state confirmed by a
    /// fresh readback.
    ///
    /// # Errors
    ///
    /// Returns transport, retry-exhaustion, readback-validation,
    /// write-verification, or worker lifecycle errors.
    pub async fn write_polling_rate(&self, rate: PollingRate) -> Result<PollingRate, DriverError> {
        self.request(|reply| Command::WritePollingRate { rate, reply })
            .await
    }

    /// Activates a profile after validating and preserving the current maximum.
    ///
    /// # Errors
    ///
    /// Returns transport, retry-exhaustion, metadata-validation, profile-policy,
    /// write-verification, or worker lifecycle errors.
    pub async fn activate_profile(
        &self,
        profile: ProfileId,
    ) -> Result<ProfileMetadata, DriverError> {
        self.request(|reply| Command::ActivateProfile { profile, reply })
            .await
    }

    /// Changes the enabled maximum profile while preserving the current profile.
    ///
    /// This is a bounded metadata operation, not a reset. The current profile
    /// must remain enabled; the worker serializes the compact `0x0c` write,
    /// enforces the profile quiet period, and validates metadata readback.
    ///
    /// # Errors
    ///
    /// Returns transport, retry-exhaustion, metadata-validation, profile-policy,
    /// write-verification, or worker lifecycle errors.
    pub async fn set_maximum_profile(
        &self,
        maximum: ProfileId,
    ) -> Result<ProfileMetadata, DriverError> {
        self.request(|reply| Command::SetMaximumProfile { maximum, reply })
            .await
    }

    /// Reads metadata and all confirmed profile-backed sections as one queue item.
    ///
    /// Persistent metadata is reported separately and is never treated as proof
    /// that the target profile is the currently loaded working image.
    ///
    /// # Errors
    ///
    /// Returns the first transport, retry-exhaustion, protocol-validation, or
    /// worker lifecycle error encountered by the uninterrupted sequence.
    pub async fn read_profile(
        &self,
        target_profile: ProfileId,
    ) -> Result<ProfileSnapshot, DriverError> {
        self.request(|reply| Command::Profile {
            target_profile,
            reply,
        })
        .await
    }

    /// Sends raw feature-report bytes with no readback or verification.
    ///
    /// This is a low-level escape hatch for firmware probing. Production
    /// code should use the typed write methods instead.
    ///
    /// # Errors
    ///
    /// Returns transport or worker lifecycle errors.
    pub async fn send_raw_feature_report(&self, bytes: Vec<u8>) -> Result<(), DriverError> {
        self.request(|reply| Command::SendRawFeatureReport { bytes, reply })
            .await
    }

    async fn request<T>(
        &self,
        make_command: impl FnOnce(oneshot::Sender<Result<T, DriverError>>) -> Command,
    ) -> Result<T, DriverError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(make_command(reply))
            .map_err(|_| DriverError::WorkerUnavailable)?;
        response.await.map_err(|_| DriverError::WorkerUnavailable)?
    }
}

type Reply<T> = oneshot::Sender<Result<T, DriverError>>;

enum Command {
    ProfileMetadata(Reply<ProfileMetadata>),
    Dpi {
        profile: ProfileId,
        reply: Reply<DpiState>,
    },
    Preferences {
        profile: ProfileId,
        reply: Reply<PreferencesState>,
    },
    Buttons {
        profile: ProfileId,
        reply: Reply<ButtonsState>,
    },
    PollingRate(Reply<PollingRate>),
    WriteDpi {
        state: DpiState,
        reply: Reply<DpiState>,
    },
    WritePreferences {
        state: PreferencesState,
        reply: Reply<PreferencesState>,
    },
    WriteButtons {
        state: ButtonsState,
        reply: Reply<ButtonsState>,
    },
    WritePollingRate {
        rate: PollingRate,
        reply: Reply<PollingRate>,
    },
    SetMaximumProfile {
        maximum: ProfileId,
        reply: Reply<ProfileMetadata>,
    },
    ActivateProfile {
        profile: ProfileId,
        reply: Reply<ProfileMetadata>,
    },
    Profile {
        target_profile: ProfileId,
        reply: Reply<ProfileSnapshot>,
    },
    SendRawFeatureReport {
        bytes: Vec<u8>,
        reply: Reply<()>,
    },
}

trait FeatureTransport: Send + 'static {
    fn send_feature_report(&mut self, report: &[u8]) -> Result<(), String>;
    fn get_feature_report(&mut self, report_id: u8, buffer: &mut [u8]) -> Result<usize, String>;
}

trait InputTransport: Send + 'static {
    fn read_input_report(&mut self, buffer: &mut [u8], timeout_ms: i32) -> Result<usize, String>;
}

struct Worker {
    transport: Box<dyn FeatureTransport>,
    policy: ReadPolicy,
    transport_kind: TransportKind,
}

impl Worker {
    fn run(&mut self, commands: &mpsc::Receiver<Command>) {
        while let Ok(command) = commands.recv() {
            match command {
                Command::ProfileMetadata(reply) => {
                    let _ = reply.send(self.read_profile_metadata());
                }
                Command::Dpi { profile, reply } => {
                    let _ = reply.send(self.read_dpi(profile));
                }
                Command::Preferences { profile, reply } => {
                    let _ = reply.send(self.read_preferences(profile));
                }
                Command::Buttons { profile, reply } => {
                    let _ = reply.send(self.read_buttons(profile));
                }
                Command::PollingRate(reply) => {
                    let _ = reply.send(self.read_polling_rate());
                }
                Command::WriteDpi { state, reply } => {
                    let _ = reply.send(self.write_dpi(&state));
                }
                Command::WritePreferences { state, reply } => {
                    let _ = reply.send(self.write_preferences(state));
                }
                Command::WriteButtons { state, reply } => {
                    let _ = reply.send(self.write_buttons(state));
                }
                Command::WritePollingRate { rate, reply } => {
                    let _ = reply.send(self.write_polling_rate(rate));
                }
                Command::SetMaximumProfile { maximum, reply } => {
                    let _ = reply.send(self.set_maximum_profile(maximum));
                }
                Command::ActivateProfile { profile, reply } => {
                    let _ = reply.send(self.activate_profile(profile));
                }
                Command::Profile {
                    target_profile,
                    reply,
                } => {
                    let _ = reply.send(self.read_profile(target_profile));
                }
                Command::SendRawFeatureReport { bytes, reply } => {
                    let _ = reply.send(
                        self.transport
                            .send_feature_report(&bytes)
                            .map_err(DriverError::Transport),
                    );
                }
            }
        }
    }

    fn read_profile_metadata(&mut self) -> Result<ProfileMetadata, DriverError> {
        let transport_kind = self.transport_kind;
        self.armed_read(ReadbackRequest::ProfileMetadata, move |packet| {
            ProfileMetadataReport::decode_for_transport(packet, transport_kind)
                .map(|report| report.metadata)
        })
    }

    fn read_dpi(&mut self, profile: ProfileId) -> Result<DpiState, DriverError> {
        let transport_kind = self.transport_kind;
        self.armed_read(ReadbackRequest::Dpi(profile), move |packet| {
            DpiReport::decode(packet, transport_kind, profile).map(|report| report.state)
        })
    }

    fn read_preferences(&mut self, profile: ProfileId) -> Result<PreferencesState, DriverError> {
        let transport_kind = self.transport_kind;
        self.armed_read(ReadbackRequest::Preferences(profile), move |packet| {
            PreferencesReport::decode_for_transport(packet, transport_kind, profile)
                .map(|report| report.state)
        })
    }

    fn read_buttons(&mut self, profile: ProfileId) -> Result<ButtonsState, DriverError> {
        let transport_kind = self.transport_kind;
        self.armed_read(ReadbackRequest::Buttons(profile), move |packet| {
            ButtonsReport::decode_for_transport(packet, transport_kind, profile)
                .map(|report| report.state)
        })
    }

    fn read_polling_rate(&mut self) -> Result<PollingRate, DriverError> {
        let transport_kind = self.transport_kind;
        self.armed_read(ReadbackRequest::PollingRate, move |packet| {
            PollingRateReport::decode_for_transport(packet, transport_kind)
                .map(|report| report.rate)
        })
    }

    fn write_dpi(&mut self, state: &DpiState) -> Result<DpiState, DriverError> {
        let report = DpiReport::encode(state, self.transport_kind)?;
        self.transport
            .send_feature_report(report.as_bytes())
            .map_err(DriverError::Transport)?;
        self.sleep_after_write();
        let actual = self.read_dpi(state.profile)?;
        if actual != *state {
            return Err(DriverError::WriteVerificationMismatch {
                section: "dpi",
                profile: state.profile,
            });
        }
        Ok(actual)
    }

    fn write_preferences(
        &mut self,
        state: PreferencesState,
    ) -> Result<PreferencesState, DriverError> {
        let framing = match self.transport_kind {
            TransportKind::Receiver | TransportKind::Wired | TransportKind::Ble => {
                crate::PreferencesFraming::Compact
            }
        };
        let report = PreferencesReport::encode_framed(&state, framing);
        self.transport
            .send_feature_report(report.as_bytes())
            .map_err(DriverError::Transport)?;
        self.sleep_after_write();
        let actual = self.read_preferences(state.profile)?;
        if actual != state {
            return Err(DriverError::WriteVerificationMismatch {
                section: "preferences",
                profile: state.profile,
            });
        }
        Ok(actual)
    }

    fn write_buttons(&mut self, state: ButtonsState) -> Result<ButtonsState, DriverError> {
        let report = ButtonsReport::encode(&state);
        self.transport
            .send_feature_report(report.as_bytes())
            .map_err(DriverError::Transport)?;
        self.sleep_after_write();
        let actual = self.read_buttons(state.profile)?;
        if actual != state {
            return Err(DriverError::WriteVerificationMismatch {
                section: "buttons",
                profile: state.profile,
            });
        }
        Ok(actual)
    }

    fn write_polling_rate(&mut self, rate: PollingRate) -> Result<PollingRate, DriverError> {
        let report = PollingRateReport::encode(rate);
        self.transport
            .send_feature_report(report.as_bytes())
            .map_err(DriverError::Transport)?;
        self.sleep_after_write();
        let actual = self.read_polling_rate()?;
        if actual != rate {
            return Err(DriverError::GlobalWriteVerificationMismatch {
                section: "polling rate",
            });
        }
        Ok(actual)
    }

    fn set_maximum_profile(&mut self, maximum: ProfileId) -> Result<ProfileMetadata, DriverError> {
        let current = self.read_profile_metadata()?;
        if maximum.get() < current.current().get() {
            return Err(DriverError::MaximumProfileBelowCurrent {
                maximum,
                current: current.current(),
            });
        }
        if maximum == current.maximum() {
            return Ok(current);
        }
        let expected = ProfileMetadata::new(current.current(), maximum)?;
        let framing = match self.transport_kind {
            TransportKind::Receiver => ProfileControlFraming::Full,
            TransportKind::Wired | TransportKind::Ble => ProfileControlFraming::Compact,
        };
        let report = ProfileControlReport::encode(expected, framing);
        self.transport
            .send_feature_report(report.as_bytes())
            .map_err(DriverError::Transport)?;
        thread::sleep(PROFILE_ACTIVATION_QUIET_PERIOD);
        let actual = self.read_profile_metadata()?;
        if actual != expected {
            return Err(DriverError::WriteVerificationMismatch {
                section: "profile metadata",
                profile: expected.current(),
            });
        }
        Ok(actual)
    }

    fn activate_profile(&mut self, profile: ProfileId) -> Result<ProfileMetadata, DriverError> {
        let current = self.read_profile_metadata()?;
        if current.current() == profile {
            return Err(DriverError::ProfileAlreadyActive { profile });
        }
        if profile.get() > current.maximum().get() {
            return Err(DriverError::ProfileNotEnabled {
                profile,
                maximum: current.maximum(),
            });
        }
        let expected = ProfileMetadata::new(profile, current.maximum())?;
        let framing = match self.transport_kind {
            TransportKind::Receiver => ProfileControlFraming::Full,
            TransportKind::Wired | TransportKind::Ble => ProfileControlFraming::Compact,
        };
        let report = ProfileControlReport::encode(expected, framing);
        self.transport
            .send_feature_report(report.as_bytes())
            .map_err(DriverError::Transport)?;
        thread::sleep(PROFILE_ACTIVATION_QUIET_PERIOD);
        let actual = self.read_profile_metadata()?;
        if actual != expected {
            return Err(DriverError::WriteVerificationMismatch {
                section: "profile metadata",
                profile,
            });
        }
        Ok(actual)
    }

    fn sleep_after_write(&self) {
        if !self.policy.write_delay.is_zero() {
            thread::sleep(self.policy.write_delay);
        }
    }

    fn read_profile(&mut self, target_profile: ProfileId) -> Result<ProfileSnapshot, DriverError> {
        let persistent_metadata = self.read_profile_metadata()?;
        let dpi = self.read_dpi(target_profile)?;
        let preferences = self.read_preferences(target_profile)?;
        let buttons = self.read_buttons(target_profile)?;
        Ok(ProfileSnapshot {
            persistent_metadata,
            target_profile,
            dpi,
            preferences,
            buttons,
        })
    }

    fn armed_read<T>(
        &mut self,
        request: ReadbackRequest,
        decode: impl Fn(&[u8]) -> Result<T, ProtocolError>,
    ) -> Result<T, DriverError> {
        let attempts = self.policy.max_attempts.get();
        let mut last_failure = ReadFailure::ReadinessTimeout;
        for _ in 0..attempts {
            match self.read_attempt(request, &decode)? {
                Ok(value) => return Ok(value),
                Err(failure) => last_failure = failure,
            }
        }
        Err(DriverError::ReadAttemptsExhausted {
            attempts,
            last: last_failure,
        })
    }

    fn read_readiness_status(
        &mut self,
    ) -> Result<Result<ReadinessStatus, ReadFailure>, DriverError> {
        let mut status = [0_u8; 8];
        let status_length = self
            .transport
            .get_feature_report(0xa0, &mut status)
            .map_err(DriverError::Transport)?;
        if status_length > status.len() {
            return Ok(Err(ProtocolError::InvalidReportLength {
                expected: status.len(),
                actual: status_length,
            }
            .into()));
        }
        Ok(ReadinessStatus::decode(&status[..status_length]).map_err(ReadFailure::from))
    }

    /// Returns transport errors outside the inner result because they are not
    /// packet mismatches and are not silently retried.
    fn read_attempt<T>(
        &mut self,
        request: ReadbackRequest,
        decode: &impl Fn(&[u8]) -> Result<T, ProtocolError>,
    ) -> Result<Result<T, ReadFailure>, DriverError> {
        let selector = ReadSelector::encode(request);
        self.transport
            .send_feature_report(selector.as_bytes())
            .map_err(DriverError::Transport)?;

        if self.transport_kind == TransportKind::Receiver {
            // The receiver's RF round trip needs an initial delay before
            // the first readiness check.
            thread::sleep(RECEIVER_READINESS_DELAY);
        }
        {
            let started = Instant::now();
            loop {
                match self.read_readiness_status()? {
                    Ok(ReadinessStatus::Ready) => break,
                    Ok(ReadinessStatus::NotReady) => {
                        let Some(remaining) = self
                            .policy
                            .readiness_timeout
                            .checked_sub(started.elapsed())
                            .filter(|remaining| !remaining.is_zero())
                        else {
                            return Ok(Err(ReadFailure::ReadinessTimeout));
                        };
                        if !self.policy.poll_interval.is_zero() {
                            thread::sleep(self.policy.poll_interval.min(remaining));
                        }
                    }
                    Err(failure) => return Ok(Err(failure)),
                }
            }
        }

        let expected_length = usize::from(request.report_length());
        debug_assert!(expected_length <= MAX_FEATURE_REPORT_LENGTH);
        let mut report = [0_u8; MAX_FEATURE_REPORT_LENGTH];
        let actual_length = self
            .transport
            .get_feature_report(request.report_id(), &mut report[..expected_length])
            .map_err(DriverError::Transport)?;
        if actual_length > expected_length {
            return Ok(Err(ProtocolError::InvalidReportLength {
                expected: expected_length,
                actual: actual_length,
            }
            .into()));
        }
        Ok(decode(&report[..actual_length]).map_err(ReadFailure::from))
    }
}

fn spawn_worker(
    policy: ReadPolicy,
    transport_kind: TransportKind,
    open_transport: impl FnOnce() -> Result<Box<dyn FeatureTransport>, DriverError> + Send + 'static,
) -> Result<MouseHandle, DriverError> {
    let (commands, requests) = mpsc::channel();
    let (opened, result) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("attack-shark-x3-hid".to_owned())
        .spawn(move || match open_transport() {
            Ok(transport) => {
                if opened.send(Ok(())).is_ok() {
                    Worker {
                        transport,
                        policy,
                        transport_kind,
                    }
                    .run(&requests);
                }
            }
            Err(error) => {
                let _ = opened.send(Err(error));
            }
        })
        .map_err(|error| DriverError::WorkerStart(error.to_string()))?;
    result
        .recv()
        .map_err(|_| DriverError::WorkerUnavailable)??;
    let (dpi_button_events, _) = broadcast::channel(16);
    let (battery_events, _) = broadcast::channel(16);
    Ok(MouseHandle {
        commands,
        dpi_button_events,
        battery_events,
        battery_level: Arc::new(Mutex::new(None)),
        input_available: Arc::new(AtomicBool::new(false)),
        input_stop: Arc::new(AtomicBool::new(false)),
        transport_kind,
    })
}

#[cfg(feature = "usb")]
fn spawn_input_worker(
    selector: DeviceSelector,
    kind: usb::UsbDeviceKind,
    dpi_events: broadcast::Sender<DpiButtonEvent>,
    battery_events: broadcast::Sender<BatteryEvent>,
    battery_level: Arc<Mutex<Option<u8>>>,
    input_available: Arc<AtomicBool>,
    stop: Weak<AtomicBool>,
) -> Result<(), DriverError> {
    let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("attack-shark-x3-input".to_owned())
        .spawn(move || {
            let mut transport = match usb::open_input_transport(&selector, kind) {
                Ok(transport) => {
                    input_available.store(true, Ordering::Release);
                    let _ = ready_sender.send(Ok(()));
                    transport
                }
                Err(error) => {
                    input_available.store(false, Ordering::Release);
                    let _ = ready_sender.send(Err(error));
                    return;
                }
            };
            let mut buffer = [0_u8; 64];
            while let Some(stop_flag) = stop.upgrade() {
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
                match transport.read_input_report(&mut buffer, 100) {
                    Ok(0) => {}
                    Ok(length) if length <= buffer.len() => {
                        let packet = &buffer[..length];
                        if let Some(event) = decode_dpi_button_report(packet) {
                            let _ = dpi_events.send(event);
                        }
                        if let Some(event) = decode_battery_report(packet) {
                            if let Ok(mut cached) = battery_level.lock() {
                                *cached = Some(event.level);
                            }
                            let _ = battery_events.send(event);
                        }
                    }
                    Ok(_) => {}
                    Err(_) => {
                        input_available.store(false, Ordering::Release);
                        break;
                    }
                }
            }
        })
        .map_err(|error| DriverError::WorkerStart(error.to_string()))?;
    match ready_receiver.recv() {
        Ok(Ok(()) | Err(_)) => {}
        Err(_) => return Err(DriverError::WorkerUnavailable),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        num::NonZeroU8,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use super::{
        DpiButtonEvent, DriverError, FeatureTransport, MouseHandle, ReadFailure, ReadPolicy,
        ReadSelector, ReadbackRequest, spawn_worker,
    };
    use crate::{
        ButtonsReport, DpiReport, PollingRate, PollingRateReport, PreferencesFraming,
        PreferencesReport, ProfileControlFraming, ProfileControlReport, ProfileId, ProfileMetadata,
        ProtocolError, StageIndex, TransportKind,
    };

    const PROFILE_2_METADATA: &str = "0c0a0102fd05fa000000";
    const PROFILE_1_DPI: &str = "04380100003f00000f1f2f3f63070000000000000002000001ff000000ff000000ffffff0000ffffff00ffff4000ffffff010e7c00000000";
    const PROFILE_2_DPI: &str = "04380200003f00000f1f2f3f63070000000000000002000002ff000000ff000000ffffff0000ffffff00ffff4000ffffff030e7f00000000";
    const PROFILE_2_PREFERENCES: &str = "050f0270030800ff000104017f0000";
    const PROFILE_2_BUTTONS: &str = "083b020200000300000400000d00003c00000f00000600000500003c00000000000000000000000000000000000000000000000a000009000000bb";
    const POLLING_RATE: &str = "06090101fe00000000";

    enum Step {
        Send(Vec<u8>),
        SendError,
        Get {
            report_id: u8,
            capacity: usize,
            response: Vec<u8>,
        },
    }

    struct ScriptedTransport {
        steps: VecDeque<Step>,
        remaining: Arc<AtomicUsize>,
    }

    impl ScriptedTransport {
        fn new(steps: Vec<Step>) -> (Self, Arc<AtomicUsize>) {
            let remaining = Arc::new(AtomicUsize::new(steps.len()));
            (
                Self {
                    steps: steps.into(),
                    remaining: Arc::clone(&remaining),
                },
                remaining,
            )
        }

        fn pop(&mut self) -> Result<Step, String> {
            let step = self
                .steps
                .pop_front()
                .ok_or_else(|| "unexpected HID operation after script end".to_owned())?;
            self.remaining.fetch_sub(1, Ordering::SeqCst);
            Ok(step)
        }
    }

    impl FeatureTransport for ScriptedTransport {
        fn send_feature_report(&mut self, report: &[u8]) -> Result<(), String> {
            match self.pop()? {
                Step::Send(expected) if expected == report => Ok(()),
                Step::Send(expected) => Err(format!(
                    "selector mismatch: expected {expected:02x?}, got {report:02x?}"
                )),
                Step::SendError => Err("injected send failure".to_owned()),
                Step::Get { .. } => Err("expected a feature-report read, got a write".to_owned()),
            }
        }

        fn get_feature_report(
            &mut self,
            report_id: u8,
            buffer: &mut [u8],
        ) -> Result<usize, String> {
            match self.pop()? {
                Step::Get {
                    report_id: expected_id,
                    capacity,
                    response,
                } if expected_id == report_id && capacity == buffer.len() => {
                    if response.len() > buffer.len() {
                        return Ok(response.len());
                    }
                    buffer[..response.len()].copy_from_slice(&response);
                    Ok(response.len())
                }
                Step::Get {
                    report_id: expected_id,
                    capacity,
                    ..
                } => Err(format!(
                    "read mismatch: expected ID 0x{expected_id:02x}/capacity {capacity}, \
                     got ID 0x{report_id:02x}/capacity {}",
                    buffer.len()
                )),
                Step::Send(_) | Step::SendError => {
                    Err("expected a feature-report write, got a read".to_owned())
                }
            }
        }
    }

    fn profile(value: u8) -> ProfileId {
        ProfileId::try_from(value).expect("test profile must be valid")
    }

    fn bytes(value: &str) -> Vec<u8> {
        assert_eq!(value.len() % 2, 0);
        value
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| (nibble(pair[0]) << 4) | nibble(pair[1]))
            .collect()
    }

    const fn nibble(value: u8) -> u8 {
        match value {
            b'0'..=b'9' => value - b'0',
            b'a'..=b'f' => value - b'a' + 10,
            _ => panic!("test packet must be lowercase hex"),
        }
    }

    fn ready() -> Vec<u8> {
        bytes("a001000000000000")
    }

    fn not_ready() -> Vec<u8> {
        bytes("a000000000000000")
    }

    fn selector(request: ReadbackRequest) -> Vec<u8> {
        ReadSelector::encode(request).as_bytes().to_vec()
    }

    fn successful_read(steps: &mut Vec<Step>, request: ReadbackRequest, response: &str) {
        successful_read_bytes(steps, request, bytes(response));
    }

    fn successful_read_bytes(steps: &mut Vec<Step>, request: ReadbackRequest, response: Vec<u8>) {
        steps.push(Step::Send(selector(request)));
        steps.push(Step::Get {
            report_id: 0xa0,
            capacity: 8,
            response: ready(),
        });
        steps.push(Step::Get {
            report_id: request.report_id(),
            capacity: usize::from(request.report_length()),
            response,
        });
    }

    fn test_policy(max_attempts: u8) -> ReadPolicy {
        ReadPolicy {
            max_attempts: NonZeroU8::new(max_attempts).expect("test attempts must be nonzero"),
            readiness_timeout: Duration::from_millis(100),
            poll_interval: Duration::ZERO,
            write_delay: Duration::ZERO,
        }
    }
    fn handle_with_transport(
        steps: Vec<Step>,
        policy: ReadPolicy,
        transport_kind: TransportKind,
    ) -> (MouseHandle, Arc<AtomicUsize>) {
        let (transport, remaining) = ScriptedTransport::new(steps);
        let handle = spawn_worker(policy, transport_kind, || {
            Ok(Box::new(transport) as Box<dyn FeatureTransport>)
        })
        .expect("scripted worker must start");
        (handle, remaining)
    }

    fn handle(steps: Vec<Step>, policy: ReadPolicy) -> (MouseHandle, Arc<AtomicUsize>) {
        handle_with_transport(steps, policy, TransportKind::Wired)
    }

    #[tokio::test]
    async fn dpi_button_subscription_preserves_the_raw_event() {
        let (handle, remaining) = handle(Vec::new(), test_policy(1));
        let mut events = handle.subscribe_dpi_button_events();
        let raw_report = [0x03, 0x00, 0x10, 0x03, 0x00];
        let expected = DpiButtonEvent {
            raw_report,
            active_stage: StageIndex::try_from(3).expect("stage 3 is valid"),
        };

        handle
            .dpi_button_events
            .send(expected)
            .expect("test subscriber must receive the event");

        assert_eq!(
            events.recv().await.expect("event must be available"),
            expected
        );
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn polling_rate_read_is_global_and_uses_the_armed_mailbox() {
        let mut steps = Vec::new();
        successful_read(&mut steps, ReadbackRequest::PollingRate, POLLING_RATE);
        let (handle, remaining) = handle(steps, test_policy(2));

        let actual = handle
            .read_polling_rate()
            .await
            .expect("polling-rate readback must decode");
        assert_eq!(actual, PollingRate::Hz1000);
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn polling_rate_write_sends_packet_and_verifies_readback() {
        let rate = PollingRate::Hz500;
        let report = PollingRateReport::encode(rate);
        let mut steps = vec![Step::Send(report.as_bytes().to_vec())];
        successful_read(
            &mut steps,
            ReadbackRequest::PollingRate,
            "06090102fd00000000",
        );
        let (handle, remaining) = handle(steps, test_policy(2));

        let actual = handle
            .write_polling_rate(rate)
            .await
            .expect("polling-rate write must verify");
        assert_eq!(actual, rate);
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn polling_rate_write_rejects_mismatched_readback() {
        let rate = PollingRate::Hz500;
        let report = PollingRateReport::encode(rate);
        let mut steps = vec![Step::Send(report.as_bytes().to_vec())];
        successful_read(
            &mut steps,
            ReadbackRequest::PollingRate,
            "06090101fe00000000",
        );
        let (handle, remaining) = handle(steps, test_policy(2));

        let error = handle
            .write_polling_rate(rate)
            .await
            .expect_err("different rate readback must fail verification");
        assert!(matches!(
            error,
            DriverError::GlobalWriteVerificationMismatch {
                section: "polling rate"
            }
        ));
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn complete_profile_read_is_one_uninterrupted_queue_command() {
        let target = profile(2);
        let mut steps = Vec::new();
        successful_read(
            &mut steps,
            ReadbackRequest::ProfileMetadata,
            PROFILE_2_METADATA,
        );
        successful_read(&mut steps, ReadbackRequest::Dpi(target), PROFILE_2_DPI);
        successful_read(
            &mut steps,
            ReadbackRequest::Preferences(target),
            PROFILE_2_PREFERENCES,
        );
        successful_read(
            &mut steps,
            ReadbackRequest::Buttons(target),
            PROFILE_2_BUTTONS,
        );
        let (handle, remaining) = handle(steps, test_policy(2));

        let snapshot = handle
            .read_profile(target)
            .await
            .expect("live-confirmed packet sequence must decode");
        assert_eq!(snapshot.persistent_metadata.current(), target);
        assert_eq!(snapshot.persistent_metadata.maximum(), profile(5));
        assert_eq!(snapshot.target_profile, target);
        assert_eq!(snapshot.dpi.profile, target);
        assert_eq!(snapshot.preferences.profile, target);
        assert_eq!(snapshot.buttons.profile, target);
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn dpi_write_sends_wired_packet_and_verifies_rearmed_readback() {
        let target = profile(2);
        let desired = DpiReport::decode(&bytes(PROFILE_2_DPI), TransportKind::Wired, target)
            .expect("fixture DPI must decode")
            .state;
        let encoded =
            DpiReport::encode(&desired, TransportKind::Wired).expect("fixture DPI must encode");
        let mut steps = vec![Step::Send(encoded.as_bytes().to_vec())];
        successful_read(&mut steps, ReadbackRequest::Dpi(target), PROFILE_2_DPI);
        let (handle, remaining) = handle(steps, test_policy(2));

        let actual = handle
            .write_dpi(desired.clone())
            .await
            .expect("matching DPI readback must verify");
        assert_eq!(actual, desired);
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn preferences_write_sends_compact_packet_and_verifies_readback() {
        let target = profile(2);
        let desired = PreferencesReport::decode(&bytes(PROFILE_2_PREFERENCES), target)
            .expect("fixture preferences must decode")
            .state;
        let encoded = PreferencesReport::encode_framed(&desired, PreferencesFraming::Compact);
        let mut steps = vec![Step::Send(encoded.as_bytes().to_vec())];
        successful_read(
            &mut steps,
            ReadbackRequest::Preferences(target),
            PROFILE_2_PREFERENCES,
        );
        let (handle, remaining) = handle(steps, test_policy(2));

        let actual = handle
            .write_preferences(desired)
            .await
            .expect("matching preferences readback must verify");
        assert_eq!(actual, desired);
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn receiver_preferences_write_uses_compact_framing() {
        let target = profile(2);
        let desired = PreferencesReport::decode(&bytes(PROFILE_2_PREFERENCES), target)
            .expect("receiver fixture must decode")
            .state;
        let encoded = PreferencesReport::encode_framed(&desired, PreferencesFraming::Compact);
        let mut steps = vec![Step::Send(encoded.as_bytes().to_vec())];
        let mut receiver_readback = bytes(PROFILE_2_PREFERENCES);
        receiver_readback[1] = 0x11;
        successful_read_bytes(
            &mut steps,
            ReadbackRequest::Preferences(target),
            receiver_readback,
        );
        let (handle, remaining) =
            handle_with_transport(steps, test_policy(2), TransportKind::Receiver);

        let actual = handle
            .write_preferences(desired)
            .await
            .expect("receiver preferences readback must verify");
        assert_eq!(actual, desired);
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn maximum_profile_write_preserves_current_and_verifies_metadata() {
        let current = profile(1);
        let maximum = profile(2);
        let expected = ProfileMetadata::new(current, maximum).expect("fixture range is valid");
        let control = ProfileControlReport::encode(expected, ProfileControlFraming::Compact);
        let mut steps = Vec::new();
        successful_read(
            &mut steps,
            ReadbackRequest::ProfileMetadata,
            "0c0a0101fe05fa000000",
        );
        steps.push(Step::Send(control.as_bytes().to_vec()));
        successful_read(
            &mut steps,
            ReadbackRequest::ProfileMetadata,
            "0c0a0101fe02fd000000",
        );
        let (handle, remaining) = handle(steps, test_policy(2));

        let actual = handle
            .set_maximum_profile(maximum)
            .await
            .expect("maximum profile metadata must verify");
        assert_eq!(actual, expected);
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn maximum_profile_below_current_is_rejected_before_control_write() {
        let current = profile(2);
        let maximum = profile(1);
        let mut steps = Vec::new();
        successful_read(
            &mut steps,
            ReadbackRequest::ProfileMetadata,
            "0c0a0102fd05fa000000",
        );
        let (handle, remaining) = handle(steps, test_policy(2));

        let error = handle
            .set_maximum_profile(maximum)
            .await
            .expect_err("maximum below current must fail");
        assert!(matches!(
            error,
            DriverError::MaximumProfileBelowCurrent {
                maximum: actual_maximum,
                current: actual_current,
            } if actual_maximum == maximum && actual_current == current
        ));
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn buttons_write_sends_complete_table_and_verifies_readback() {
        let target = profile(2);
        let desired = ButtonsReport::decode(&bytes(PROFILE_2_BUTTONS), target)
            .expect("fixture buttons must decode")
            .state;
        let encoded = ButtonsReport::encode(&desired);
        let mut steps = vec![Step::Send(encoded.as_bytes().to_vec())];
        successful_read(
            &mut steps,
            ReadbackRequest::Buttons(target),
            PROFILE_2_BUTTONS,
        );
        let (handle, remaining) = handle(steps, test_policy(2));

        let actual = handle
            .write_buttons(desired)
            .await
            .expect("matching buttons readback must verify");
        assert_eq!(actual, desired);
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn write_mismatch_is_rejected_without_copying_large_states() {
        let target = profile(2);
        let desired = PreferencesReport::decode(&bytes(PROFILE_2_PREFERENCES), target)
            .expect("fixture preferences must decode")
            .state;
        let mut actual = desired;
        actual.debounce = actual.debounce.wrapping_add(1);
        let encoded = PreferencesReport::encode_framed(&desired, PreferencesFraming::Compact);
        let readback = PreferencesReport::encode_framed(&actual, PreferencesFraming::Full);
        let mut steps = vec![Step::Send(encoded.as_bytes().to_vec())];
        successful_read_bytes(
            &mut steps,
            ReadbackRequest::Preferences(target),
            readback.as_full_bytes().to_vec(),
        );
        let (handle, remaining) = handle(steps, test_policy(2));

        let error = handle
            .write_preferences(desired)
            .await
            .expect_err("different validated state must fail verification");
        assert!(matches!(
            error,
            DriverError::WriteVerificationMismatch {
                section: "preferences",
                profile,
            } if profile == target
        ));
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn profile_activation_preserves_maximum_and_reads_only_metadata_after_quiet_period() {
        let target = profile(1);
        let maximum = profile(5);
        let expected = ProfileMetadata::new(target, maximum).expect("fixture range is valid");
        let control = ProfileControlReport::encode(expected, ProfileControlFraming::Compact);
        let mut steps = Vec::new();
        successful_read(
            &mut steps,
            ReadbackRequest::ProfileMetadata,
            PROFILE_2_METADATA,
        );
        steps.push(Step::Send(control.as_bytes().to_vec()));
        successful_read(
            &mut steps,
            ReadbackRequest::ProfileMetadata,
            "0c0a0101fe05fa000000",
        );
        let (handle, remaining) = handle(steps, test_policy(2));

        let actual = handle
            .activate_profile(target)
            .await
            .expect("profile activation metadata must verify");
        assert_eq!(actual, expected);
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn idempotent_profile_activation_is_rejected_before_control_write() {
        let target = profile(2);
        let mut steps = Vec::new();
        successful_read(
            &mut steps,
            ReadbackRequest::ProfileMetadata,
            PROFILE_2_METADATA,
        );
        let (handle, remaining) = handle(steps, test_policy(2));

        let error = handle
            .activate_profile(target)
            .await
            .expect_err("activating the current profile must fail");
        assert!(matches!(
            error,
            DriverError::ProfileAlreadyActive { profile } if profile == target
        ));
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn disabled_profile_is_rejected_before_control_write() {
        let target = profile(5);
        let expected_maximum = profile(2);
        let mut steps = Vec::new();
        successful_read(
            &mut steps,
            ReadbackRequest::ProfileMetadata,
            "0c0a0101fe02fd000000",
        );
        let (handle, remaining) = handle(steps, test_policy(2));

        let error = handle
            .activate_profile(target)
            .await
            .expect_err("profile above current maximum must fail");
        assert!(matches!(
            error,
            DriverError::ProfileNotEnabled { profile, maximum }
                if profile == target && maximum == expected_maximum
        ));
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn malformed_target_is_fetched_once_then_rearmed_before_retry() {
        let target = profile(2);
        let request = ReadbackRequest::Dpi(target);
        let mut steps = Vec::new();
        successful_read(&mut steps, request, PROFILE_1_DPI);
        steps.push(Step::Send(selector(request)));
        steps.push(Step::Get {
            report_id: 0xa0,
            capacity: 8,
            response: not_ready(),
        });
        steps.push(Step::Get {
            report_id: 0xa0,
            capacity: 8,
            response: ready(),
        });
        steps.push(Step::Get {
            report_id: 0x04,
            capacity: 56,
            response: bytes(PROFILE_2_DPI),
        });
        let (handle, remaining) = handle(steps, test_policy(2));

        let dpi = handle
            .read_dpi(target)
            .await
            .expect("second armed attempt must succeed");
        assert_eq!(dpi.profile, target);
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn malformed_readiness_rearms_until_bounded_exhaustion() {
        let request = ReadbackRequest::ProfileMetadata;
        let mut steps = Vec::new();
        for _ in 0..3 {
            steps.push(Step::Send(selector(request)));
            steps.push(Step::Get {
                report_id: 0xa0,
                capacity: 8,
                response: bytes("a002000000000000"),
            });
        }
        let (handle, remaining) = handle(steps, test_policy(3));

        let error = handle
            .read_profile_metadata()
            .await
            .expect_err("unknown readiness status must exhaust bounded retries");
        assert!(matches!(
            error,
            DriverError::ReadAttemptsExhausted {
                attempts: 3,
                last: ReadFailure::Protocol(ProtocolError::InvalidReadinessStatus { value: 2 }),
            }
        ));
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn readiness_timeout_rearms_without_fetching_target_report() {
        let request = ReadbackRequest::ProfileMetadata;
        let steps = vec![
            Step::Send(selector(request)),
            Step::Get {
                report_id: 0xa0,
                capacity: 8,
                response: not_ready(),
            },
            Step::Send(selector(request)),
            Step::Get {
                report_id: 0xa0,
                capacity: 8,
                response: not_ready(),
            },
        ];
        let (handle, remaining) = handle(
            steps,
            ReadPolicy {
                max_attempts: NonZeroU8::new(2).expect("two is nonzero"),
                readiness_timeout: Duration::ZERO,
                poll_interval: Duration::ZERO,
                write_delay: Duration::ZERO,
            },
        );

        let error = handle
            .read_profile_metadata()
            .await
            .expect_err("not-ready mailbox must time out");
        assert!(matches!(
            error,
            DriverError::ReadAttemptsExhausted {
                attempts: 2,
                last: ReadFailure::ReadinessTimeout,
            }
        ));
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn transport_errors_are_not_hidden_by_protocol_retries() {
        let (handle, remaining) = handle(vec![Step::SendError], test_policy(4));
        let error = handle
            .read_profile_metadata()
            .await
            .expect_err("transport failure must be returned directly");
        assert!(matches!(error, DriverError::Transport(_)));
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }
}
