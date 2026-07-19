use std::process::ExitCode;

use attack_shark_x3::{
    DpiReport, DpiState, DpiValue, ProfileId, SensorOptions, StageIndex, TransportKind,
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
    /// Build an X3 DPI report.
    Dpi(DpiArgs),
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
    match cli.command {
        Command::Hex {
            command: HexCommand::Dpi(arguments),
        } => {
            let profile =
                ProfileId::try_from(arguments.profile).map_err(|error| error.to_string())?;
            let active_stage =
                StageIndex::try_from(arguments.active).map_err(|error| error.to_string())?;
            let stages = arguments
                .stages
                .into_iter()
                .map(DpiValue::try_from)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            if profile.get() != ProfileId::MIN {
                return Err(
                    "fresh DPI packet generation only has an evidenced template for profile 1; \
                     read the target profile before constructing a complete write"
                        .to_owned(),
                );
            }
            let mut state = DpiState::profile_one_template(stages, active_stage)
                .map_err(|error| error.to_string())?;
            state.sensor = SensorOptions {
                lift_off_distance: arguments.lod.into(),
                ripple_control: arguments.ripple_control,
                angle_snap: arguments.angle_snap,
                motion_sync: arguments.motion_sync,
            };
            let report = DpiReport::encode(&state, arguments.transport.into())
                .map_err(|error| error.to_string())?;
            println!("{}", to_hex(report.as_bytes()));
            Ok(())
        }
    }
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
