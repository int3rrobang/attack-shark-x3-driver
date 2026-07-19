use std::{
    num::NonZeroU8,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use thiserror::Error;
use tokio::sync::oneshot;

use crate::{
    DpiReport, DpiState, PreferencesReport, PreferencesState, ProfileId, ProfileMetadata,
    ProfileMetadataReport, ProtocolError, ReadSelector, ReadbackRequest, ReadinessStatus,
    TransportKind,
    protocol::buttons::{ButtonsReport, ButtonsState},
};

#[cfg(feature = "usb")]
mod usb;

#[cfg(feature = "usb")]
pub use usb::{DeviceInfo, DeviceSelector, list_devices};

const MAX_FEATURE_REPORT_LENGTH: usize = 128;

/// Bounded policy for one-shot FA61 configuration reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadPolicy {
    pub max_attempts: NonZeroU8,
    pub readiness_timeout: Duration,
    pub poll_interval: Duration,
}

impl Default for ReadPolicy {
    fn default() -> Self {
        Self {
            // The live benchmark used one initial attempt plus three retries.
            max_attempts: NonZeroU8::new(4).expect("four is nonzero"),
            readiness_timeout: Duration::from_millis(250),
            poll_interval: Duration::from_millis(1),
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

    #[error("no matching FA61 configuration collection was found")]
    DeviceNotFound,

    #[error("{count} matching FA61 configuration collections were found; select an exact path")]
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

/// Cloneable request handle for the one thread that owns the HID device.
#[derive(Clone, Debug)]
pub struct MouseHandle {
    commands: mpsc::Sender<Command>,
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
        Self::open_with_policy(selector, ReadPolicy::default())
    }

    /// Opens a device with an explicit bounded read policy.
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
        spawn_worker(policy, move || {
            usb::open_transport(&selector)
                .map(|transport| Box::new(transport) as Box<dyn FeatureTransport>)
        })
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
    Profile {
        target_profile: ProfileId,
        reply: Reply<ProfileSnapshot>,
    },
}

trait FeatureTransport: Send + 'static {
    fn send_feature_report(&mut self, report: &[u8]) -> Result<(), String>;
    fn get_feature_report(&mut self, report_id: u8, buffer: &mut [u8]) -> Result<usize, String>;
}

struct Worker {
    transport: Box<dyn FeatureTransport>,
    policy: ReadPolicy,
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
                Command::Profile {
                    target_profile,
                    reply,
                } => {
                    let _ = reply.send(self.read_profile(target_profile));
                }
            }
        }
    }

    fn read_profile_metadata(&mut self) -> Result<ProfileMetadata, DriverError> {
        self.armed_read(ReadbackRequest::ProfileMetadata, |packet| {
            ProfileMetadataReport::decode(packet).map(|report| report.metadata)
        })
    }

    fn read_dpi(&mut self, profile: ProfileId) -> Result<DpiState, DriverError> {
        self.armed_read(ReadbackRequest::Dpi(profile), |packet| {
            DpiReport::decode(packet, TransportKind::Wired, profile).map(|report| report.state)
        })
    }

    fn read_preferences(&mut self, profile: ProfileId) -> Result<PreferencesState, DriverError> {
        self.armed_read(ReadbackRequest::Preferences(profile), |packet| {
            PreferencesReport::decode(packet, profile).map(|report| report.state)
        })
    }

    fn read_buttons(&mut self, profile: ProfileId) -> Result<ButtonsState, DriverError> {
        self.armed_read(ReadbackRequest::Buttons(profile), |packet| {
            ButtonsReport::decode(packet, profile).map(|report| report.state)
        })
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

        let started = Instant::now();
        loop {
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
            match ReadinessStatus::decode(&status[..status_length]) {
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
                Err(error) => return Ok(Err(error.into())),
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
    open_transport: impl FnOnce() -> Result<Box<dyn FeatureTransport>, DriverError> + Send + 'static,
) -> Result<MouseHandle, DriverError> {
    let (commands, requests) = mpsc::channel();
    let (opened, result) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("attack-shark-x3-hid".to_owned())
        .spawn(move || match open_transport() {
            Ok(transport) => {
                if opened.send(Ok(())).is_ok() {
                    Worker { transport, policy }.run(&requests);
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
    Ok(MouseHandle { commands })
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
        DriverError, FeatureTransport, MouseHandle, ReadFailure, ReadPolicy, ReadSelector,
        ReadbackRequest, spawn_worker,
    };
    use crate::{ProfileId, ProtocolError};

    const PROFILE_2_METADATA: &str = "0c0a0102fd05fa000000";
    const PROFILE_1_DPI: &str = "04380100003f00000f1f2f3f63070000000000000002000001ff000000ff000000ffffff0000ffffff00ffff4000ffffff010e7c00000000";
    const PROFILE_2_DPI: &str = "04380200003f00000f1f2f3f63070000000000000002000002ff000000ff000000ffffff0000ffffff00ffff4000ffffff030e7f00000000";
    const PROFILE_2_PREFERENCES: &str = "050f0270030800ff000104017f0000";
    const PROFILE_2_BUTTONS: &str = "083b020200000300000400000d00003c00000f00000600000500003c00000000000000000000000000000000000000000000000a000009000000bb";

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
        steps.push(Step::Send(selector(request)));
        steps.push(Step::Get {
            report_id: 0xa0,
            capacity: 8,
            response: ready(),
        });
        steps.push(Step::Get {
            report_id: request.report_id(),
            capacity: usize::from(request.report_length()),
            response: bytes(response),
        });
    }

    fn test_policy(max_attempts: u8) -> ReadPolicy {
        ReadPolicy {
            max_attempts: NonZeroU8::new(max_attempts).expect("test attempts must be nonzero"),
            readiness_timeout: Duration::from_millis(100),
            poll_interval: Duration::ZERO,
        }
    }

    fn handle(steps: Vec<Step>, policy: ReadPolicy) -> (MouseHandle, Arc<AtomicUsize>) {
        let (transport, remaining) = ScriptedTransport::new(steps);
        let handle = spawn_worker(policy, || {
            Ok(Box::new(transport) as Box<dyn FeatureTransport>)
        })
        .expect("scripted worker must start");
        (handle, remaining)
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
