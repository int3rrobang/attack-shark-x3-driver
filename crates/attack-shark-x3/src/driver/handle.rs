#![cfg_attr(not(feature = "usb"), allow(dead_code, unused_imports))]

use std::{
    num::NonZeroU8,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use thiserror::Error;
use tokio::sync::{broadcast, oneshot};

use crate::{
    BatteryEvent, DpiButtonEvent, DpiState, PollingRate, PreferencesState, ProfileId,
    ProfileMetadata, ProtocolError, TransportKind, protocol::buttons::ButtonsState,
};

#[cfg(feature = "usb")]
use super::usb::{self, DeviceSelector};
use super::worker::{self, FeatureTransport};

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
    pub(crate) commands: mpsc::Sender<Command>,
    pub(crate) dpi_button_events: broadcast::Sender<DpiButtonEvent>,
    pub(crate) battery_events: broadcast::Sender<BatteryEvent>,
    pub(crate) battery_level: Arc<Mutex<Option<u8>>>,
    pub(crate) input_available: Arc<AtomicBool>,
    pub(crate) input_stop: Arc<AtomicBool>,
    pub(crate) transport_kind: TransportKind,
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
        let handle = worker::spawn_worker(policy, kind.transport_kind(), move || {
            usb::open_transport(&selector, kind)
                .map(|transport| Box::new(transport) as Box<dyn FeatureTransport>)
        })?;
        worker::spawn_input_worker(
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

pub(crate) type Reply<T> = oneshot::Sender<Result<T, DriverError>>;

pub(crate) enum Command {
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
