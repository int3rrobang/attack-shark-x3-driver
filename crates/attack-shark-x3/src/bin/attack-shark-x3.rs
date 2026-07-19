use std::process::ExitCode;

use attack_shark_x3::{
    DpiReport, DpiState, DpiValue, ProfileControlFraming, ProfileControlReport, ProfileId,
    ProfileMetadata, ReadSelector, ReadbackRequest, SensorOptions, StageIndex, TransportKind,
    protocol::dpi::LiftOffDistance,
};
use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "attack-shark-x3", about = "Attack Shark X3/M600 protocol CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build packets without opening hardware.
    Hex {
        #[command(subcommand)]
        command: HexCommand,
    },
}

#[derive(Debug, Subcommand)]
enum HexCommand {
    /// Build an offline DPI packet from a captured empty-profile-1 image.
    Dpi(DpiArgs),
    /// Build an edge-triggered current/maximum profile control packet.
    ProfileControl(ProfileControlArgs),
    /// Build an A0 selector that arms one configuration read.
    ReadSelector(ReadSelectorArgs),
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum TransportArgument {
    Wired,
    Receiver,
}

impl From<TransportArgument> for TransportKind {
    fn from(value: TransportArgument) -> Self {
        match value {
            TransportArgument::Wired => Self::Wired,
            TransportArgument::Receiver => Self::Receiver,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum LiftOffDistanceArgument {
    One,
    Two,
}

impl From<LiftOffDistanceArgument> for LiftOffDistance {
    fn from(value: LiftOffDistanceArgument) -> Self {
        match value {
            LiftOffDistanceArgument::One => Self::OneMillimeter,
            LiftOffDistanceArgument::Two => Self::TwoMillimeters,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ProfileFramingArgument {
    Compact,
    Full,
}

impl From<ProfileFramingArgument> for ProfileControlFraming {
    fn from(value: ProfileFramingArgument) -> Self {
        match value {
            ProfileFramingArgument::Compact => Self::Compact,
            ProfileFramingArgument::Full => Self::Full,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ReadReportArgument {
    Version,
    ProfileMetadata,
    Dpi,
    Preferences,
    Buttons,
}

impl ReadReportArgument {
    const fn name(self) -> &'static str {
        match self {
            Self::Version => "version",
            Self::ProfileMetadata => "profile-metadata",
            Self::Dpi => "dpi",
            Self::Preferences => "preferences",
            Self::Buttons => "buttons",
        }
    }
}

#[derive(Clone, Copy, Debug, Args)]
struct ProfileControlArgs {
    #[arg(long)]
    current: u8,

    #[arg(long, default_value_t = 5)]
    maximum: u8,

    #[arg(long, value_enum, default_value = "compact")]
    framing: ProfileFramingArgument,
}

#[derive(Clone, Copy, Debug, Args)]
struct ReadSelectorArgs {
    #[arg(long, value_enum)]
    report: ReadReportArgument,

    /// Required for DPI, preferences, and buttons; forbidden otherwise.
    #[arg(long)]
    profile: Option<u8>,
}

#[derive(Debug, Args)]
struct DpiArgs {
    #[arg(long, value_enum, default_value = "wired")]
    transport: TransportArgument,

    #[arg(long, default_value_t = 1)]
    profile: u8,

    #[arg(long, value_delimiter = ',', default_values_t = [800_u16, 1600, 2400, 3200, 5000, 26000])]
    stages: Vec<u16>,

    #[arg(long, default_value_t = 2)]
    active: u8,

    #[arg(long, value_enum, default_value = "one")]
    lod: LiftOffDistanceArgument,

    #[arg(long)]
    ripple_control: bool,

    #[arg(long)]
    angle_snap: bool,

    #[arg(long)]
    motion_sync: bool,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    let Command::Hex { command } = cli.command;
    match command {
        HexCommand::Dpi(arguments) => print_dpi(arguments),
        HexCommand::ProfileControl(arguments) => print_profile_control(arguments),
        HexCommand::ReadSelector(arguments) => print_read_selector(arguments),
    }
}

fn print_dpi(arguments: DpiArgs) -> Result<(), String> {
    let profile = ProfileId::try_from(arguments.profile).map_err(|error| error.to_string())?;
    let active_stage = StageIndex::try_from(arguments.active).map_err(|error| error.to_string())?;
    let stages = arguments
        .stages
        .into_iter()
        .map(DpiValue::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    if profile.get() != ProfileId::MIN {
        return Err(
            "the captured empty-profile packet is only evidenced for profile 1; \
             read an existing target profile before constructing a complete write"
                .to_owned(),
        );
    }
    let mut state = DpiState::captured_empty_profile_one(stages, active_stage)
        .map_err(|error| error.to_string())?;
    state.sensor = SensorOptions {
        lift_off_distance: arguments.lod.into(),
        ripple_control: arguments.ripple_control,
        angle_snap: arguments.angle_snap,
        motion_sync: arguments.motion_sync,
    };
    let report =
        DpiReport::encode(&state, arguments.transport.into()).map_err(|error| error.to_string())?;
    println!("{}", to_hex(report.as_bytes()));
    Ok(())
}

fn print_profile_control(arguments: ProfileControlArgs) -> Result<(), String> {
    let current = parse_profile(arguments.current)?;
    let maximum = parse_profile(arguments.maximum)?;
    let metadata = ProfileMetadata::new(current, maximum).map_err(|error| error.to_string())?;
    let report = ProfileControlReport::encode(metadata, arguments.framing.into());
    println!("{}", to_hex(report.as_bytes()));
    Ok(())
}

fn print_read_selector(arguments: ReadSelectorArgs) -> Result<(), String> {
    let request = match arguments.report {
        ReadReportArgument::Version | ReadReportArgument::ProfileMetadata => {
            if arguments.profile.is_some() {
                return Err(format!(
                    "--profile is not valid for the {} report",
                    arguments.report.name()
                ));
            }
            match arguments.report {
                ReadReportArgument::Version => ReadbackRequest::Version,
                ReadReportArgument::ProfileMetadata => ReadbackRequest::ProfileMetadata,
                _ => unreachable!(),
            }
        }
        ReadReportArgument::Dpi | ReadReportArgument::Preferences | ReadReportArgument::Buttons => {
            let raw_profile = arguments.profile.ok_or_else(|| {
                format!(
                    "--profile is required for the {} report",
                    arguments.report.name()
                )
            })?;
            let profile = parse_profile(raw_profile)?;
            match arguments.report {
                ReadReportArgument::Dpi => ReadbackRequest::Dpi(profile),
                ReadReportArgument::Preferences => ReadbackRequest::Preferences(profile),
                ReadReportArgument::Buttons => ReadbackRequest::Buttons(profile),
                _ => unreachable!(),
            }
        }
    };
    println!("{}", to_hex(ReadSelector::encode(request).as_bytes()));
    Ok(())
}

fn parse_profile(value: u8) -> Result<ProfileId, String> {
    ProfileId::try_from(value).map_err(|error| error.to_string())
}

fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}
