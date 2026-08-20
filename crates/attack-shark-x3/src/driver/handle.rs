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
    DpiState, InputEvent, PollingRate, PreferencesState, ProfileId, ProfileMetadata, ProtocolError,
    TransportKind, protocol::buttons::ButtonsState,
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
            // Targeted traffic can perturb deferred profile persistence. The
            // receiver's verified writes need a five-second quiet window.
            write_delay: Duration::from_secs(5),
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

    #[error("the HID worker command queue is full")]
    WorkerBusy,

    #[error(
        "armed read for {section} (profile {profile:?}) failed after {attempts} attempts: {last}"
    )]
    ReadAttemptsExhausted {
        section: &'static str,
        profile: Option<ProfileId>,
        attempts: u8,
        #[source]
        last: ReadFailure,
    },
    #[error("the {section} write did not verify for profile {profile}")]
    WriteVerificationMismatch {
        section: &'static str,
        profile: ProfileId,
    },

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
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
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
    pub(crate) commands: mpsc::SyncSender<Command>,
    pub(crate) input_events: broadcast::Sender<InputEvent>,
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
            handle.input_events.clone(),
            handle.battery_level.clone(),
            handle.input_available.clone(),
            Arc::downgrade(&handle.input_stop),
        )?;
        Ok(handle)
    }

    /// Subscribes to decoded report-`0x03` input events from the auxiliary HID
    /// collection.
    ///
    /// The receiver carries one event stream for profile/DPI, battery,
    /// connection, DPI-index, LED-mode, and profile-sync notifications.
    #[must_use]
    pub fn subscribe_input_events(&self) -> broadcast::Receiver<InputEvent> {
        self.input_events.subscribe()
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
        let mut events = self.input_events.subscribe();
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
                    Ok(InputEvent::BatteryChanged(event)) => break Ok(event.level),
                    Ok(_) => {}
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

    /// Reads the live polling rate through a serialized USB readback armed for
    /// the explicit target profile.
    ///
    /// Report `0x06` skips the profile loader, so the readback reflects the
    /// profile that is currently live on the device. The readback's profile
    /// byte is validated against the requested profile and the armed selector
    /// carries the requested profile.
    ///
    /// # Errors
    ///
    /// Returns transport, retry-exhaustion, protocol-validation, or worker
    /// lifecycle errors.
    pub async fn read_polling_rate(&self, profile: ProfileId) -> Result<PollingRate, DriverError> {
        self.request(|reply| Command::PollingRate { profile, reply })
            .await
    }

    /// Submits the DPI write packet without readback or verification.
    ///
    /// This is a stock-style send-only path: exactly one report is emitted
    /// and nothing is read back or persisted.
    ///
    /// # Errors
    ///
    /// Returns encoding or transport errors.
    pub async fn send_dpi(&self, state: DpiState) -> Result<(), DriverError> {
        self.request(|reply| Command::SendDpi { state, reply })
            .await
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

    /// Submits the preferences write packet without readback or verification.
    ///
    /// This is a stock-style send-only path: exactly one report is emitted
    /// and nothing is read back or persisted.
    ///
    /// # Errors
    ///
    /// Returns encoding or transport errors.
    pub async fn send_preferences(&self, state: PreferencesState) -> Result<(), DriverError> {
        self.request(|reply| Command::SendPreferences { state, reply })
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

    /// Submits the button-table write packet without readback or verification.
    ///
    /// This is a stock-style send-only path: exactly one report is emitted
    /// and nothing is read back or persisted.
    ///
    /// # Errors
    ///
    /// Returns encoding or transport errors.
    pub async fn send_buttons(&self, state: ButtonsState) -> Result<(), DriverError> {
        self.request(|reply| Command::SendButtons { state, reply })
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

    /// Submits the polling-rate report without readback or verification.
    ///
    /// This is a stock-style send-only path: exactly one report is emitted and
    /// nothing is read back or persisted. Report `0x06` skips the profile
    /// loader; byte 2 is a save alias, and the complete live image may be
    /// persisted into that slot by the device's deferred writer. This method
    /// provides no live-image safety precondition.
    ///
    /// # Errors
    ///
    /// Returns encoding or transport errors.
    pub async fn send_polling_rate_unchecked(
        &self,
        profile: ProfileId,
        rate: PollingRate,
    ) -> Result<(), DriverError> {
        self.request(|reply| Command::SendPollingRateUnchecked {
            profile,
            rate,
            reply,
        })
        .await
    }

    /// Writes the polling rate for the explicit target profile and verifies
    /// the immediate rate field with a fresh readback.
    ///
    /// Report `0x06` skips the profile loader; byte 2 is a save alias, and the
    /// complete live image may be persisted into that slot by the device's
    /// deferred writer. This method provides no live-image safety
    /// precondition: the readback verifies the immediate rate field only.
    ///
    /// # Errors
    ///
    /// Returns transport, retry-exhaustion, readback-validation,
    /// write-verification, or worker lifecycle errors.
    pub async fn write_polling_rate_unchecked(
        &self,
        profile: ProfileId,
        rate: PollingRate,
    ) -> Result<PollingRate, DriverError> {
        self.request(|reply| Command::WritePollingRateUnchecked {
            profile,
            rate,
            reply,
        })
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
            .try_send(make_command(reply))
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => DriverError::WorkerBusy,
                mpsc::TrySendError::Disconnected(_) => DriverError::WorkerUnavailable,
            })?;
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
    PollingRate {
        profile: ProfileId,
        reply: Reply<PollingRate>,
    },
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
    SendDpi {
        state: DpiState,
        reply: Reply<()>,
    },
    SendPreferences {
        state: PreferencesState,
        reply: Reply<()>,
    },
    SendButtons {
        state: ButtonsState,
        reply: Reply<()>,
    },
    SendPollingRateUnchecked {
        profile: ProfileId,
        rate: PollingRate,
        reply: Reply<()>,
    },
    WritePollingRateUnchecked {
        profile: ProfileId,
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

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex, atomic::AtomicBool},
        time::Duration,
    };

    use tokio::sync::broadcast;

    use crate::TransportKind;

    use super::{Command, DriverError, MouseHandle, ReadPolicy};

    #[test]
    fn receiver_policy_remains_bounded() {
        let policy = ReadPolicy::receiver_default();

        assert_eq!(policy.readiness_timeout, Duration::from_secs(2));
        assert_eq!(policy.max_attempts.get(), 4);
        assert_eq!(policy.poll_interval, Duration::from_millis(5));
        assert_eq!(policy.write_delay, Duration::from_secs(5));
    }

    #[tokio::test]
    async fn bounded_command_queue_returns_busy_when_full() {
        let (commands, _receiver) = std::sync::mpsc::sync_channel::<Command>(1);
        // Fill the single slot.
        let (reply, _) = tokio::sync::oneshot::channel::<Result<(), DriverError>>();
        commands
            .try_send(Command::SendRawFeatureReport {
                bytes: vec![0x00],
                reply,
            })
            .expect("first send must succeed");
        let (input_events, _) = broadcast::channel(16);
        let handle = MouseHandle {
            commands,
            input_events,
            battery_level: Arc::new(Mutex::new(None)),
            input_available: Arc::new(AtomicBool::new(false)),
            input_stop: Arc::new(AtomicBool::new(false)),
            transport_kind: TransportKind::Wired,
        };
        let error = handle
            .send_raw_feature_report(vec![0x01])
            .await
            .expect_err("saturated queue must be reported as busy");
        assert!(matches!(error, DriverError::WorkerBusy));
    }

    #[tokio::test]
    async fn disconnected_command_queue_reports_unavailable() {
        let (commands, receiver) = std::sync::mpsc::sync_channel::<Command>(1);
        drop(receiver);
        let (input_events, _) = broadcast::channel(16);
        let handle = MouseHandle {
            commands,
            input_events,
            battery_level: Arc::new(Mutex::new(None)),
            input_available: Arc::new(AtomicBool::new(false)),
            input_stop: Arc::new(AtomicBool::new(false)),
            transport_kind: TransportKind::Wired,
        };
        let error = handle
            .send_raw_feature_report(vec![0x01])
            .await
            .expect_err("disconnected queue must be unavailable");
        assert!(matches!(error, DriverError::WorkerUnavailable));
    }
}
