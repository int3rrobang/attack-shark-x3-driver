use std::{
    env,
    error::Error,
    fmt,
    io::{self, Write},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use attack_shark_x3::{
    DeviceSelector, MouseHandle, ProfileControlFraming, ProfileControlReport, ProfileId,
    ProfileMetadata, UsbDeviceKind,
};
use tokio::time::sleep;

const COUNTDOWN: Duration = Duration::from_secs(3);
const BASELINE_WINDOW: Duration = Duration::from_secs(5);
const READ_INTERVAL: Duration = Duration::from_secs(1);
const CONTROL_QUIET_PERIOD: Duration = Duration::from_secs(10);
const MAX_READ_REPETITIONS: u8 = 50;
const MAX_WRITE_REPETITIONS: u8 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProbeCase {
    Baseline,
    MetadataRead,
    PollingRead,
    PrepareProfileOne,
    IdempotentSwitch,
    RawEdge,
    VerifiedEdge,
}

impl ProbeCase {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "baseline" => Ok(Self::Baseline),
            "metadata-read" => Ok(Self::MetadataRead),
            "prepare-profile-one" => Ok(Self::PrepareProfileOne),
            "polling-read" => Ok(Self::PollingRead),
            "idempotent-switch" => Ok(Self::IdempotentSwitch),
            "raw-edge" => Ok(Self::RawEdge),
            "verified-edge" => Ok(Self::VerifiedEdge),
            _ => Err(format!("unsupported probe case {value:?}")),
        }
    }

    const fn sends_profile_control(self) -> bool {
        matches!(
            self,
            Self::PrepareProfileOne | Self::IdempotentSwitch | Self::RawEdge | Self::VerifiedEdge
        )
    }

    const fn changes_profile(self) -> bool {
        matches!(self, Self::RawEdge | Self::VerifiedEdge)
    }

    const fn maximum_repetitions(self) -> u8 {
        if self.sends_profile_control() {
            MAX_WRITE_REPETITIONS
        } else {
            MAX_READ_REPETITIONS
        }
    }
}

impl fmt::Display for ProbeCase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Baseline => "baseline",
            Self::MetadataRead => "metadata-read",
            Self::PollingRead => "polling-read",
            Self::PrepareProfileOne => "prepare-profile-one",
            Self::IdempotentSwitch => "idempotent-switch",
            Self::RawEdge => "raw-edge",
            Self::VerifiedEdge => "verified-edge",
        };
        formatter.write_str(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Transport {
    Wired,
    Receiver,
}

impl Transport {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "wired" => Ok(Self::Wired),
            "receiver" => Ok(Self::Receiver),
            _ => Err(format!("unsupported transport {value:?}")),
        }
    }

    const fn device_kind(self) -> UsbDeviceKind {
        match self {
            Self::Wired => UsbDeviceKind::Wired,
            Self::Receiver => UsbDeviceKind::Receiver,
        }
    }

    const fn framing(self) -> ProfileControlFraming {
        match self {
            Self::Wired => ProfileControlFraming::Compact,
            Self::Receiver => ProfileControlFraming::Full,
        }
    }
}

impl fmt::Display for Transport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wired => formatter.write_str("wired"),
            Self::Receiver => formatter.write_str("receiver"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Config {
    transport: Transport,
    case: ProbeCase,
    repetitions: u8,
    target_profile: u8,
    execute: bool,
    allow_profile_control_writes: bool,
}

impl Config {
    fn parse_from(arguments: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut transport = Transport::Receiver;
        let mut case = ProbeCase::Baseline;
        let mut repetitions = 1_u8;
        let mut target_profile = 2_u8;
        let mut execute = false;
        let mut allow_profile_control_writes = false;
        let mut arguments = arguments.into_iter();

        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--transport" => {
                    transport = Transport::parse(&required_value(&mut arguments, "--transport")?)?;
                }
                "--case" => {
                    case = ProbeCase::parse(&required_value(&mut arguments, "--case")?)?;
                }
                "--repetitions" => {
                    repetitions = required_value(&mut arguments, "--repetitions")?
                        .parse()
                        .map_err(|_| "--repetitions must be an integer".to_owned())?;
                }
                "--target-profile" => {
                    target_profile = required_value(&mut arguments, "--target-profile")?
                        .parse()
                        .map_err(|_| "--target-profile must be an integer".to_owned())?;
                }
                "--execute" => execute = true,
                "--allow-profile-control-writes" => allow_profile_control_writes = true,
                "--help" | "-h" => return Err(usage()),
                _ => return Err(format!("unrecognized argument {argument:?}\n\n{}", usage())),
            }
        }

        let config = Self {
            transport,
            case,
            repetitions,
            target_profile,
            execute,
            allow_profile_control_writes,
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        if self.repetitions == 0 || self.repetitions > self.case.maximum_repetitions() {
            return Err(format!(
                "case {} requires repetitions in 1..={} (got {})",
                self.case,
                self.case.maximum_repetitions(),
                self.repetitions
            ));
        }
        if !(ProfileId::MIN..=ProfileId::MAX).contains(&self.target_profile) {
            return Err(format!(
                "--target-profile must be in {}..={} (got {})",
                ProfileId::MIN,
                ProfileId::MAX,
                self.target_profile
            ));
        }
        if self.case.sends_profile_control() && !self.allow_profile_control_writes && self.execute {
            return Err(format!(
                "case {} sends report 0x0c; add --allow-profile-control-writes after reviewing the dry-run",
                self.case
            ));
        }
        Ok(())
    }
}

fn required_value(
    arguments: &mut impl Iterator<Item = String>,
    option: &str,
) -> Result<String, String> {
    arguments
        .next()
        .ok_or_else(|| format!("{option} requires a value"))
}

fn usage() -> String {
    "usage: profile-switch-transport-probe [--transport wired|receiver] \
     [--case baseline|metadata-read|polling-read|prepare-profile-one|idempotent-switch|raw-edge|verified-edge] \
     [--repetitions N] [--target-profile 2] [--execute] \
     [--allow-profile-control-writes]\n\nWithout --execute the probe is an offline dry-run. Real profile-control traffic also requires --allow-profile-control-writes."
        .to_owned()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let config = match Config::parse_from(env::args().skip(1)) {
        Ok(config) => config,
        Err(message) if message.starts_with("usage:") => {
            println!("{message}");
            return Ok(());
        }
        Err(message) => return Err(io::Error::other(message).into()),
    };

    print_plan(&config)?;
    if !config.execute {
        println!("DRY RUN: no device was opened; add --execute to run this plan");
        return Ok(());
    }

    run(config).await
}

fn print_plan(config: &Config) -> Result<(), Box<dyn Error>> {
    println!("=== Profile-switch transport probe ===");
    println!("transport: {}", config.transport);
    println!("case: {}", config.case);
    println!("repetitions: {}", config.repetitions);
    println!("target profile: {}", config.target_profile);
    println!("countdown: {} ms", COUNTDOWN.as_millis());
    println!(
        "control quiet period: {} ms",
        CONTROL_QUIET_PERIOD.as_millis()
    );
    println!(
        "configuration-section traffic: none (the probe never sends or reads reports 0x04, 0x05, or 0x08)"
    );

    let profile_one = ProfileId::try_from(1)?;
    let maximum = ProfileId::try_from(ProfileId::MAX)?;
    let idempotent = ProfileControlReport::encode(
        ProfileMetadata::new(profile_one, maximum)?,
        config.transport.framing(),
    );
    let profile_one_packet_label = if config.case == ProbeCase::PrepareProfileOne {
        "profile-1 preparation packet"
    } else {
        "profile-1 idempotent packet"
    };
    println!("{profile_one_packet_label}: {}", hex(idempotent.as_bytes()));
    if config.case.changes_profile() {
        let target = ProfileId::try_from(config.target_profile)?;
        let forward = ProfileControlReport::encode(
            ProfileMetadata::new(target, maximum)?,
            config.transport.framing(),
        );
        println!("forward edge packet: {}", hex(forward.as_bytes()));
        println!("return edge packet: {}", hex(idempotent.as_bytes()));
        println!(
            "each repetition returns to profile 1; it can consume two metadata-sector erase cycles"
        );
    }
    Ok(())
}

async fn run(config: Config) -> Result<(), Box<dyn Error>> {
    let started = Instant::now();
    let handle =
        MouseHandle::open_for_kind(DeviceSelector::Unique, config.transport.device_kind())?;

    let initial_metadata = if config.case.sends_profile_control() {
        let metadata = handle.read_profile_metadata().await?;
        let profile_one = ProfileId::try_from(1)?;
        let maximum = ProfileId::try_from(ProfileId::MAX)?;
        if metadata.maximum() != maximum
            || (config.case != ProbeCase::PrepareProfileOne && metadata.current() != profile_one)
        {
            return Err(io::Error::other(format!(
                "profile-control cases require current=1 and maximum=5; found {metadata:?}"
            ))
            .into());
        }
        Some(metadata)
    } else {
        None
    };

    if config.case == ProbeCase::PrepareProfileOne {
        prepare_profile_one(
            &handle,
            &config,
            initial_metadata.expect("preparation has preflight metadata"),
            &started,
        )
        .await?;
        return Ok(());
    }

    println!("Move only the tested mouse continuously in smooth circles until STOP appears.");
    countdown().await?;
    event(&started, &config, 0, "window-start", "none");

    for iteration in 1..=config.repetitions {
        match config.case {
            ProbeCase::Baseline => {
                event(&started, &config, iteration, "baseline-start", "none");
                sleep(BASELINE_WINDOW).await;
                event(&started, &config, iteration, "baseline-end", "none");
            }
            ProbeCase::MetadataRead => {
                event(
                    &started,
                    &config,
                    iteration,
                    "operation-start",
                    "metadata-read",
                );
                let metadata = handle.read_profile_metadata().await?;
                event(
                    &started,
                    &config,
                    iteration,
                    "operation-end",
                    &format!(
                        "current-{}-maximum-{}",
                        metadata.current().get(),
                        metadata.maximum().get()
                    ),
                );
                sleep(READ_INTERVAL).await;
            }
            ProbeCase::PollingRead => {
                event(
                    &started,
                    &config,
                    iteration,
                    "operation-start",
                    "polling-read",
                );
                let rate = handle.read_polling_rate(ProfileId::try_from(1)?).await?;
                event(
                    &started,
                    &config,
                    iteration,
                    "operation-end",
                    &format!("rate-{rate}"),
                );
                sleep(READ_INTERVAL).await;
            }
            ProbeCase::PrepareProfileOne => {
                unreachable!("preparation returns before the measured window");
            }
            ProbeCase::IdempotentSwitch => {
                let metadata = initial_metadata.expect("control cases have preflight metadata");
                send_raw_control(
                    &handle,
                    &config,
                    metadata,
                    iteration,
                    "idempotent",
                    &started,
                )
                .await?;
                verify_metadata(&handle, &config, metadata, iteration, &started).await?;
            }
            ProbeCase::RawEdge => {
                run_raw_edge_pair(&handle, &config, iteration, &started).await?;
            }
            ProbeCase::VerifiedEdge => {
                run_verified_edge_pair(&handle, &config, iteration, &started).await?;
            }
        }
    }

    event(&started, &config, 0, "window-end", "none");
    println!("STOP moving the tested mouse");
    Ok(())
}

async fn prepare_profile_one(
    handle: &MouseHandle,
    config: &Config,
    current: ProfileMetadata,
    started: &Instant,
) -> Result<(), Box<dyn Error>> {
    let expected = ProfileMetadata::new(
        ProfileId::try_from(1)?,
        ProfileId::try_from(ProfileId::MAX)?,
    )?;
    if current == expected {
        event(started, config, 0, "preparation-ready", "already-profile-1");
        println!("Preparation complete: device was already on profile 1.");
        return Ok(());
    }

    println!(
        "Preparing controlled baseline: changing profile {} to profile 1 before capture.",
        current.current().get()
    );
    send_raw_control(handle, config, expected, 0, "prepare-profile-1", started).await?;
    verify_metadata(handle, config, expected, 0, started).await?;
    println!("Preparation complete: metadata verified current=1, maximum=5.");
    Ok(())
}

async fn countdown() -> Result<(), io::Error> {
    for value in (1..=COUNTDOWN.as_secs()).rev() {
        println!("starting in {value}...");
        io::stdout().flush()?;
        sleep(Duration::from_secs(1)).await;
    }
    Ok(())
}

async fn send_raw_control(
    handle: &MouseHandle,
    config: &Config,
    metadata: ProfileMetadata,
    iteration: u8,
    label: &str,
    started: &Instant,
) -> Result<(), Box<dyn Error>> {
    let report = ProfileControlReport::encode(metadata, config.transport.framing());
    event(started, config, iteration, "operation-start", label);
    handle
        .send_raw_feature_report(report.as_bytes().to_vec())
        .await?;
    event(started, config, iteration, "write-returned", label);
    sleep(CONTROL_QUIET_PERIOD).await;
    event(started, config, iteration, "quiet-period-ended", label);
    Ok(())
}

async fn verify_metadata(
    handle: &MouseHandle,
    config: &Config,
    expected: ProfileMetadata,
    iteration: u8,
    started: &Instant,
) -> Result<(), Box<dyn Error>> {
    event(started, config, iteration, "verification-start", "metadata");
    let actual = handle.read_profile_metadata().await?;
    event(
        started,
        config,
        iteration,
        "verification-end",
        &format!(
            "current-{}-maximum-{}",
            actual.current().get(),
            actual.maximum().get()
        ),
    );
    if actual != expected {
        return Err(io::Error::other(format!(
            "metadata mismatch: expected {expected:?}, found {actual:?}; stop and recover deliberately"
        ))
        .into());
    }
    Ok(())
}

async fn run_raw_edge_pair(
    handle: &MouseHandle,
    config: &Config,
    iteration: u8,
    started: &Instant,
) -> Result<(), Box<dyn Error>> {
    let profile_one = ProfileId::try_from(1)?;
    let target = ProfileId::try_from(config.target_profile)?;
    if target == profile_one {
        return Err(io::Error::other("raw-edge target must differ from profile 1").into());
    }
    let maximum = ProfileId::try_from(ProfileId::MAX)?;
    let forward = ProfileMetadata::new(target, maximum)?;
    let original = ProfileMetadata::new(profile_one, maximum)?;

    send_raw_control(
        handle,
        config,
        forward,
        iteration,
        "forward-raw-edge",
        started,
    )
    .await?;
    verify_metadata(handle, config, forward, iteration, started).await?;
    send_raw_control(
        handle,
        config,
        original,
        iteration,
        "return-raw-edge",
        started,
    )
    .await?;
    verify_metadata(handle, config, original, iteration, started).await
}

async fn run_verified_edge_pair(
    handle: &MouseHandle,
    config: &Config,
    iteration: u8,
    started: &Instant,
) -> Result<(), Box<dyn Error>> {
    let profile_one = ProfileId::try_from(1)?;
    let target = ProfileId::try_from(config.target_profile)?;
    if target == profile_one {
        return Err(io::Error::other("verified-edge target must differ from profile 1").into());
    }

    event(
        started,
        config,
        iteration,
        "operation-start",
        "forward-verified-edge",
    );
    let forward = handle.activate_profile(target).await?;
    event(
        started,
        config,
        iteration,
        "operation-end",
        &format!(
            "forward-current-{}-maximum-{}",
            forward.current().get(),
            forward.maximum().get()
        ),
    );
    sleep(READ_INTERVAL).await;

    event(
        started,
        config,
        iteration,
        "operation-start",
        "return-verified-edge",
    );
    let returned = handle.activate_profile(profile_one).await?;
    event(
        started,
        config,
        iteration,
        "operation-end",
        &format!(
            "return-current-{}-maximum-{}",
            returned.current().get(),
            returned.maximum().get()
        ),
    );
    sleep(READ_INTERVAL).await;
    Ok(())
}

fn event(started: &Instant, config: &Config, iteration: u8, name: &str, detail: &str) {
    let unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    println!(
        "PROBE_EVENT unix_ms={unix_ms} elapsed_us={} transport={} case={} iteration={iteration} event={name} detail={detail}",
        started.elapsed().as_micros(),
        config.transport,
        config.case,
    );
    let _ = io::stdout().flush();
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::{Config, ProbeCase, Transport};

    fn parse(arguments: &[&str]) -> Result<Config, String> {
        Config::parse_from(arguments.iter().map(|value| (*value).to_owned()))
    }

    #[test]
    fn defaults_to_offline_receiver_baseline() {
        let config = parse(&[]).unwrap();
        assert_eq!(config.transport, Transport::Receiver);
        assert_eq!(config.case, ProbeCase::Baseline);
        assert_eq!(config.repetitions, 1);
        assert!(!config.execute);
    }

    #[test]
    fn executing_control_case_requires_explicit_write_gate() {
        let error = parse(&["--case", "raw-edge", "--execute"]).unwrap_err();
        assert!(error.contains("--allow-profile-control-writes"));
    }

    #[test]
    fn preparation_case_uses_the_write_gate() {
        let error = parse(&["--case", "prepare-profile-one", "--execute"]).unwrap_err();
        assert!(error.contains("--allow-profile-control-writes"));

        let config = parse(&[
            "--case",
            "prepare-profile-one",
            "--execute",
            "--allow-profile-control-writes",
        ])
        .unwrap();
        assert_eq!(config.case, ProbeCase::PrepareProfileOne);
    }

    #[test]
    fn write_cases_are_capped_at_three_repetitions() {
        let error = parse(&["--case", "verified-edge", "--repetitions", "4"]).unwrap_err();
        assert!(error.contains("1..=3"));
    }

    #[test]
    fn read_cases_accept_larger_bounded_samples() {
        let config = parse(&[
            "--transport",
            "wired",
            "--case",
            "metadata-read",
            "--repetitions",
            "20",
            "--execute",
        ])
        .unwrap();
        assert_eq!(config.transport, Transport::Wired);
        assert_eq!(config.repetitions, 20);
    }
}
