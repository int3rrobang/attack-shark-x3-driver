#![cfg_attr(not(feature = "usb"), allow(dead_code, unused_imports))]

use std::{
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use tokio::sync::broadcast;

use crate::{
    DpiReport, DpiState, InputEvent, PollingRate, PollingRateReport, PreferencesReport,
    PreferencesState, ProfileControlFraming, ProfileControlReport, ProfileId, ProfileMetadata,
    ProfileMetadataReport, ProtocolError, ReadSelector, ReadbackRequest, ReadinessStatus,
    TransportKind, decode_input_report,
    protocol::buttons::{ButtonsReport, ButtonsState},
};

use super::handle::{Command, DriverError, MouseHandle, ProfileSnapshot, ReadFailure, ReadPolicy};

#[cfg(feature = "usb")]
use super::usb::{self, DeviceSelector};

const MAX_FEATURE_REPORT_LENGTH: usize = 128;
const RECEIVER_READINESS_DELAY: Duration = Duration::from_millis(500);

pub(crate) trait FeatureTransport: Send + 'static {
    fn send_feature_report(&mut self, report: &[u8]) -> Result<(), String>;
    fn get_feature_report(&mut self, report_id: u8, buffer: &mut [u8]) -> Result<usize, String>;
}

pub(crate) trait InputTransport: Send + 'static {
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
                Command::LivePollingRate { alias, reply } => {
                    let _ = reply.send(self.read_live_polling_rate(alias));
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
                Command::SendDpi { state, reply } => {
                    let _ = reply.send(self.send_dpi(&state));
                }
                Command::SendPreferences { state, reply } => {
                    let _ = reply.send(self.send_preferences(&state));
                }
                Command::SendButtons { state, reply } => {
                    let _ = reply.send(self.send_buttons(&state));
                }
                Command::SendPollingRateUnchecked {
                    profile,
                    rate,
                    reply,
                } => {
                    let _ = reply.send(self.send_polling_rate_unchecked(profile, rate));
                }
                Command::WritePollingRateUnchecked {
                    profile,
                    rate,
                    reply,
                } => {
                    let _ = reply.send(self.write_polling_rate_unchecked(profile, rate));
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
    fn read_live_polling_rate(&mut self, alias: ProfileId) -> Result<PollingRate, DriverError> {
        let transport_kind = self.transport_kind;
        self.armed_read(ReadbackRequest::PollingRate(alias), move |packet| {
            PollingRateReport::decode_for_transport(packet, transport_kind, alias)
                .map(|report| report.rate)
        })
    }

    fn send_dpi(&mut self, state: &DpiState) -> Result<(), DriverError> {
        let report = DpiReport::encode(state, self.transport_kind)?;
        self.transport
            .send_feature_report(report.as_bytes())
            .map_err(DriverError::Transport)
    }

    fn write_dpi(&mut self, state: &DpiState) -> Result<DpiState, DriverError> {
        self.send_dpi(state)?;
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

    fn send_preferences(&mut self, state: &PreferencesState) -> Result<(), DriverError> {
        let report = PreferencesReport::encode_framed(state, crate::PreferencesFraming::Compact);
        self.transport
            .send_feature_report(report.as_bytes())
            .map_err(DriverError::Transport)
    }

    fn write_preferences(
        &mut self,
        state: PreferencesState,
    ) -> Result<PreferencesState, DriverError> {
        self.send_preferences(&state)?;
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

    fn send_buttons(&mut self, state: &ButtonsState) -> Result<(), DriverError> {
        let report = ButtonsReport::encode(state);
        self.transport
            .send_feature_report(report.as_bytes())
            .map_err(DriverError::Transport)
    }

    fn write_buttons(&mut self, state: ButtonsState) -> Result<ButtonsState, DriverError> {
        self.send_buttons(&state)?;
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

    fn send_polling_rate_unchecked(
        &mut self,
        profile: ProfileId,
        rate: PollingRate,
    ) -> Result<(), DriverError> {
        let report = PollingRateReport::encode(profile, rate);
        self.transport
            .send_feature_report(report.as_bytes())
            .map_err(DriverError::Transport)
    }

    fn write_polling_rate_unchecked(
        &mut self,
        profile: ProfileId,
        rate: PollingRate,
    ) -> Result<PollingRate, DriverError> {
        self.send_polling_rate_unchecked(profile, rate)?;
        self.sleep_after_write();
        let actual = self.read_live_polling_rate(profile)?;
        if actual != rate {
            return Err(DriverError::WriteVerificationMismatch {
                section: "polling rate",
                profile,
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
        self.sleep_after_write();
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
        self.sleep_after_write();
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
        let section = match request {
            ReadbackRequest::Version => "version",
            ReadbackRequest::ProfileMetadata => "profile metadata",
            ReadbackRequest::PollingRate(_) => "polling rate",
            ReadbackRequest::Dpi(_) => "dpi",
            ReadbackRequest::Preferences(_) => "preferences",
            ReadbackRequest::Buttons(_) => "buttons",
        };
        Err(DriverError::ReadAttemptsExhausted {
            section,
            profile: request.target_profile(),
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

pub(crate) fn spawn_worker(
    policy: ReadPolicy,
    transport_kind: TransportKind,
    open_transport: impl FnOnce() -> Result<Box<dyn FeatureTransport>, DriverError> + Send + 'static,
) -> Result<MouseHandle, DriverError> {
    let (commands, requests) = mpsc::sync_channel(32);
    let (opened, result) = mpsc::sync_channel(1);
    let (input_events, _) = broadcast::channel(16);
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
    Ok(MouseHandle {
        commands,
        input_events,
        battery_level: Arc::new(Mutex::new(None)),
        input_available: Arc::new(AtomicBool::new(false)),
        input_stop: Arc::new(AtomicBool::new(false)),
        transport_kind,
    })
}

#[cfg(feature = "usb")]
pub(crate) fn spawn_input_worker(
    selector: DeviceSelector,
    kind: usb::UsbDeviceKind,
    input_events: broadcast::Sender<InputEvent>,
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
                        if let Some(event) = decode_input_report(packet) {
                            if let InputEvent::BatteryChanged(battery) = event
                                && let Ok(mut cached) = battery_level.lock()
                            {
                                *cached = Some(battery.level);
                            }
                            let _ = input_events.send(event);
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
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            eprintln!("attack-shark-x3: input worker unavailable: {error}");
        }
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
        time::{Duration, Instant},
    };

    use super::{
        DriverError, FeatureTransport, InputEvent, MouseHandle, ReadFailure, ReadPolicy,
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
    // Nine-byte report-`0x06` readbacks: byte 2 mirrors the working alias.
    const POLLING_RATE_1: &str = "06090101fe00000000";
    const POLLING_RATE_2: &str = "06090201fe00000000";
    const POLLING_RATE_2_500: &str = "06090202fd00000000";

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
    async fn input_subscription_preserves_the_raw_event() {
        let (handle, remaining) = handle(Vec::new(), test_policy(1));
        let mut events = handle.subscribe_input_events();
        let raw_report = [0x03, 0x00, 0x10, 0x03, 0x00];
        let expected = InputEvent::ActiveDpiStageChanged(crate::DpiButtonEvent {
            raw_report,
            active_stage: StageIndex::try_from(3).expect("stage 3 is valid"),
        });

        handle
            .input_events
            .send(expected)
            .expect("test subscriber must receive the event");

        assert_eq!(
            events.recv().await.expect("event must be available"),
            expected
        );
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn polling_rate_read_targets_requested_profile_and_uses_the_armed_mailbox() {
        let target = profile(2);
        let mut steps = Vec::new();
        successful_read(
            &mut steps,
            ReadbackRequest::PollingRate(target),
            POLLING_RATE_2,
        );
        let (handle, remaining) = handle(steps, test_policy(2));

        let actual = handle
            .read_live_polling_rate(target)
            .await
            .expect("polling-rate readback must decode");
        assert_eq!(actual, PollingRate::Hz1000);
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn polling_rate_read_validates_the_readback_profile_byte() {
        // Wire-shape contract only: the readback's profile byte must match the
        // armed selector; the read path does not claim any content proof.
        let target = profile(2);
        let mut steps = Vec::new();
        successful_read(
            &mut steps,
            ReadbackRequest::PollingRate(target),
            POLLING_RATE_1,
        );
        let (handle, remaining) = handle(steps, test_policy(1));
        let error = handle
            .read_live_polling_rate(target)
            .await
            .expect_err("readback profile byte mismatch must fail");

        assert!(matches!(
            error,
            DriverError::ReadAttemptsExhausted {
                section: "polling rate",
                profile: Some(p),
                attempts: 1,
                last: ReadFailure::Protocol(ProtocolError::ProfileMismatch {
                    expected: 2,
                    actual: 1,
                }),
            } if p == target
        ));
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn polling_rate_write_sends_target_packet_and_verifies_readback() {
        // Verified write without any profile snapshot: the worker emits the
        // target-correct `0x06` packet (byte 2 = `02`) and validates the
        // fresh profile-scoped readback, exactly as the stock path does.
        let target = profile(2);
        let rate = PollingRate::Hz1000;
        let report = PollingRateReport::encode(target, rate);
        let mut steps = vec![Step::Send(report.as_bytes().to_vec())];
        successful_read(
            &mut steps,
            ReadbackRequest::PollingRate(target),
            POLLING_RATE_2,
        );
        let (handle, remaining) = handle(steps, test_policy(2));

        let actual = handle
            .write_polling_rate_unchecked(target, rate)
            .await
            .expect("polling-rate write for profile 2 must verify");
        assert_eq!(actual, rate);
        assert_eq!(report.as_bytes()[2], 0x02);
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn polling_rate_write_rejects_mismatched_rate_readback() {
        let target = profile(2);
        let rate = PollingRate::Hz1000;
        let report = PollingRateReport::encode(target, rate);
        let mut steps = vec![Step::Send(report.as_bytes().to_vec())];
        successful_read(
            &mut steps,
            ReadbackRequest::PollingRate(target),
            POLLING_RATE_2_500,
        );
        let (handle, remaining) = handle(steps, test_policy(2));

        let error = handle
            .write_polling_rate_unchecked(target, rate)
            .await
            .expect_err("different rate readback must fail verification");
        assert!(matches!(
            error,
            DriverError::WriteVerificationMismatch {
                section: "polling rate",
                profile,
            } if profile == target
        ));
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn send_dpi_submits_one_report_without_readback() {
        // The script contains only the outgoing packet: any readback or
        // selector send would consume a missing step and fail the call, so
        // success plus an exhausted script proves exactly one report is sent.
        let target = profile(2);
        let state = DpiReport::decode(&bytes(PROFILE_2_DPI), TransportKind::Wired, target)
            .expect("fixture DPI must decode")
            .state;
        let encoded =
            DpiReport::encode(&state, TransportKind::Wired).expect("fixture DPI must encode");
        let (handle, remaining) = handle(
            vec![Step::Send(encoded.as_bytes().to_vec())],
            test_policy(2),
        );

        handle
            .send_dpi(state)
            .await
            .expect("send-only DPI write must submit exactly one report");
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn send_preferences_submits_one_report_without_readback() {
        let target = profile(2);
        let state = PreferencesReport::decode(&bytes(PROFILE_2_PREFERENCES), target)
            .expect("fixture preferences must decode")
            .state;
        let encoded = PreferencesReport::encode_framed(&state, PreferencesFraming::Compact);
        let (handle, remaining) = handle(
            vec![Step::Send(encoded.as_bytes().to_vec())],
            test_policy(2),
        );

        handle
            .send_preferences(state)
            .await
            .expect("send-only preferences write must submit exactly one report");
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn send_buttons_submits_one_report_without_readback() {
        let target = profile(2);
        let state = ButtonsReport::decode(&bytes(PROFILE_2_BUTTONS), target)
            .expect("fixture buttons must decode")
            .state;
        let encoded = ButtonsReport::encode(&state);
        let (handle, remaining) = handle(
            vec![Step::Send(encoded.as_bytes().to_vec())],
            test_policy(2),
        );

        handle
            .send_buttons(state)
            .await
            .expect("send-only buttons write must submit exactly one report");
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn send_polling_rate_unchecked_submits_one_target_report_without_readback() {
        let target = profile(2);
        let rate = PollingRate::Hz1000;
        let report = PollingRateReport::encode(target, rate);
        assert_eq!(report.as_bytes()[2], 0x02);
        let (handle, remaining) =
            handle(vec![Step::Send(report.as_bytes().to_vec())], test_policy(2));

        handle
            .send_polling_rate_unchecked(target, rate)
            .await
            .expect("send-only polling-rate write must submit exactly one report");
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn send_transport_error_propagates_without_readback() {
        let target = profile(2);
        let state = DpiReport::decode(&bytes(PROFILE_2_DPI), TransportKind::Wired, target)
            .expect("fixture DPI must decode")
            .state;
        let (handle, remaining) = handle(vec![Step::SendError], test_policy(2));

        let error = handle
            .send_dpi(state)
            .await
            .expect_err("send-only write must surface the transport failure");
        assert!(matches!(error, DriverError::Transport(_)));
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
    async fn profile_activation_holds_the_configured_quiet_period() {
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
        let quiet_period = Duration::from_millis(25);
        let mut policy = test_policy(2);
        policy.write_delay = quiet_period;
        let (handle, remaining) = handle(steps, policy);

        let started = Instant::now();
        handle
            .activate_profile(target)
            .await
            .expect("profile activation metadata must verify");

        assert!(started.elapsed() >= quiet_period);
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
                section: "profile metadata",
                profile: None,
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
                section: "profile metadata",
                profile: None,
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

    #[tokio::test]
    async fn read_exhaustion_carries_section_and_profile_context() {
        let target = profile(2);
        let mut steps = Vec::new();
        // Force DPI read to exhaust with readiness timeouts; target profile is 2.
        for _ in 0..2 {
            steps.push(Step::Send(selector(ReadbackRequest::Dpi(target))));
            steps.push(Step::Get {
                report_id: 0xa0,
                capacity: 8,
                response: not_ready(),
            });
        }
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
            .read_dpi(target)
            .await
            .expect_err("DPI exhaustion must be reported");
        match error {
            DriverError::ReadAttemptsExhausted {
                section,
                profile,
                attempts: 2,
                last: ReadFailure::ReadinessTimeout,
            } => {
                assert_eq!(section, "dpi");
                assert_eq!(profile, Some(target));
                assert!(format!("{error}").contains("dpi"));
            }
            other => panic!("unexpected error shape: {other:?}"),
        }
        assert_eq!(remaining.load(Ordering::SeqCst), 0);
    }
}
