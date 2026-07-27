use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum TransportArg {
    Auto,
    Wired,
    Receiver,
    #[cfg(feature = "ble")]
    Ble,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum LodArg {
    One,
    Two,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum VerifyMethodArg {
    ProfileReload,
    PowerCycle,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum PreferencesFramingArg {
    Compact,
    Full,
}

#[derive(Debug, Parser)]
#[command(
    name = "x3ctl",
    version,
    about = "Manage Attack Shark X3/M600-family mice"
)]
pub struct Cli {
    #[arg(long, value_enum, default_value_t = TransportArg::Auto, global = true)]
    pub transport: TransportArg,
    /// Exact stable device ID reported by `x3ctl devices`.
    #[arg(long, global = true)]
    pub device: Option<String>,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=5), global = true)]
    pub profile: u8,
    /// Keep state only in memory for this invocation.
    #[arg(long, global = true)]
    pub stateless: bool,
    /// Validate and print an operation without hardware or state access.
    #[arg(long, global = true)]
    pub dry_run: bool,
    /// Authorize evidence-qualified captured defaults when no baseline exists.
    #[arg(long, global = true)]
    pub replace_defaults: bool,
    #[arg(long, value_enum, default_value_t = OutputFormat::Human, global = true)]
    pub output: OutputFormat,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List exact discoverable devices.
    Devices,
    /// Select a discoverable exact device ID.
    Use { device: String },
    /// Read device status.
    Status,
    /// Read or activate a profile.
    #[command(subcommand)]
    Profile(ProfileCommand),
    /// Read or update DPI configuration.
    #[command(subcommand)]
    Dpi(DpiCommand),
    /// Read or update polling rate.
    #[command(subcommand)]
    Rate(RateCommand),
    /// Read or update raw preference fields.
    #[command(subcommand)]
    Prefs(PrefsCommand),
    /// Read the complete button table or update one bounded slot.
    #[command(subcommand)]
    Bind(BindCommand),
    /// Read the current battery percentage when supported.
    Battery,
    /// Run an explicit persistence verification workflow.
    Verify {
        #[arg(long, value_enum)]
        method: VerifyMethodArg,
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=5))]
        profile: Option<u8>,
    },
    /// Export portable desired configuration as JSON.
    Export,
    /// Import portable desired configuration without writing hardware.
    Import { file: String },
    /// Inspect or invalidate manager state through typed manager APIs.
    #[command(subcommand)]
    State(StateCommand),
    /// Build protocol packets offline through manager encoders.
    #[command(subcommand)]
    Debug(DebugCommand),
}

#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    Get,
    Set(ProfileSetArgs),
}

#[derive(Debug, Args)]
pub struct ProfileSetArgs {
    #[arg(value_parser = clap::value_parser!(u8).range(1..=5))]
    pub profile: Option<u8>,
}

#[derive(Debug, Subcommand)]
pub enum DpiCommand {
    Get,
    Set(DpiSetArgs),
}

#[derive(Debug, Args)]
pub struct DpiSetArgs {
    /// Comma-separated DPI values (50..26000, multiples of 50).
    #[arg(long)]
    pub stages: Option<String>,
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=8))]
    pub active_stage: Option<u8>,
    #[arg(long, value_enum)]
    pub lod: Option<LodArg>,
    #[arg(long, action = clap::ArgAction::Set)]
    pub ripple_control: Option<bool>,
    #[arg(long, action = clap::ArgAction::Set)]
    pub angle_snap: Option<bool>,
    #[arg(long, action = clap::ArgAction::Set)]
    pub motion_sync: Option<bool>,
}

#[derive(Debug, Subcommand)]
pub enum RateCommand {
    Get,
    Set(RateSetArgs),
}

#[derive(Debug, Args)]
pub struct RateSetArgs {
    pub hz: u16,
}

#[derive(Debug, Subcommand)]
pub enum PrefsCommand {
    Get,
    Set(PrefsSetArgs),
}

#[derive(Debug, Args)]
pub struct PrefsSetArgs {
    #[arg(long)]
    pub light_mode: Option<u8>,
    #[arg(long)]
    pub configuration: Option<u8>,
    #[arg(long)]
    pub deep_sleep: Option<u8>,
    #[arg(long, value_parser = parse_color)]
    pub host_color: Option<[u8; 3]>,
    #[arg(long)]
    pub sleep_timer: Option<u8>,
    #[arg(long)]
    pub debounce: Option<u8>,
}

#[derive(Debug, Subcommand)]
pub enum BindCommand {
    Get,
    Set(BindSetArgs),
}

#[derive(Debug, Args)]
pub struct BindSetArgs {
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=17))]
    pub slot: u8,
    #[arg(long)]
    pub action: u8,
    #[arg(long, default_value_t = 0)]
    pub modifier: u8,
    #[arg(long, default_value_t = 0)]
    pub key_code: u8,
}

#[derive(Debug, Subcommand)]
pub enum StateCommand {
    Selected,
    Invalidate,
}

#[derive(Debug, Subcommand)]
pub enum DebugCommand {
    Dpi(DebugDpiArgs),
    Prefs(DebugPrefsArgs),
    Buttons(DebugButtonsArgs),
}

#[derive(Debug, Args)]
pub struct DebugDpiArgs {
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=5))]
    pub profile: u8,
    #[arg(long, value_delimiter = ',', default_values_t = [800_u16, 1600, 2400, 3200, 5000, 26000])]
    pub stages: Vec<u16>,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=8))]
    pub active_stage: u8,
    /// Exact 25-byte preserved tail as comma-separated decimal/0x-prefixed bytes.
    #[arg(long, value_delimiter = ',', default_value = "255,0,0,0,255,0,0,0,255,255,255,0,0,255,255,255,0,255,255,64,0,255,255,255,1", value_parser = parse_byte)]
    pub preserved_tail: Vec<u8>,
    #[arg(long, value_enum, default_value_t = LodArg::One)]
    pub lod: LodArg,
    #[arg(long, default_value_t = false)]
    pub ripple_control: bool,
    #[arg(long, default_value_t = false)]
    pub angle_snap: bool,
    #[arg(long, default_value_t = false)]
    pub motion_sync: bool,
    #[arg(long, value_enum, default_value_t = TransportArg::Wired)]
    pub transport: TransportArg,
}

#[derive(Debug, Args)]
pub struct DebugPrefsArgs {
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=5))]
    pub profile: u8,
    #[arg(long, default_value_t = 2)]
    pub light_mode: u8,
    #[arg(long, default_value_t = 1)]
    pub configuration: u8,
    #[arg(long, default_value_t = 0)]
    pub deep_sleep: u8,
    #[arg(long, default_value = "ff0000", value_parser = parse_color)]
    pub host_color: [u8; 3],
    #[arg(long, default_value_t = 5)]
    pub sleep_timer: u8,
    #[arg(long, default_value_t = 0)]
    pub debounce: u8,
    #[arg(long, value_enum, default_value_t = PreferencesFramingArg::Compact)]
    pub framing: PreferencesFramingArg,
}

#[derive(Debug, Args)]
pub struct DebugButtonsArgs {
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=5))]
    pub profile: u8,
    /// Exactly 54 comma-separated bytes: action, modifier, key-code for all 18 slots.
    #[arg(long, value_delimiter = ',', value_parser = parse_byte, default_value = "0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0")]
    pub slots: Vec<u8>,
}

fn parse_byte(raw: &str) -> Result<u8, String> {
    let raw = raw.trim();
    if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        u8::from_str_radix(hex, 16).map_err(|_| format!("invalid byte: {raw}"))
    } else {
        raw.parse::<u8>()
            .map_err(|_| format!("invalid byte: {raw}"))
    }
}

fn parse_color(raw: &str) -> Result<[u8; 3], String> {
    let compact = raw.trim().trim_start_matches('#');
    if compact.len() != 6 || !compact.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("color must be exactly six hexadecimal digits (RRGGBB)".to_owned());
    }
    Ok([
        u8::from_str_radix(&compact[0..2], 16).map_err(|error| error.to_string())?,
        u8::from_str_radix(&compact[2..4], 16).map_err(|error| error.to_string())?,
        u8::from_str_radix(&compact[4..6], 16).map_err(|error| error.to_string())?,
    ])
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command, DebugCommand, OutputFormat, StateCommand, TransportArg};
    use clap::Parser;

    #[test]
    fn no_command_defaults_in_dispatch_to_status() {
        let cli = Cli::try_parse_from(["x3ctl"]).expect("parse");
        assert!(cli.command.is_none());
        assert_eq!(cli.profile, 1);
        assert_eq!(cli.output, OutputFormat::Human);
    }

    #[test]
    fn global_selection_and_json_parse() {
        let cli = Cli::try_parse_from([
            "x3ctl",
            "--output",
            "json",
            "--transport",
            "receiver",
            "--device",
            "receiver:1d57:fa60:serial:x",
            "--profile",
            "3",
            "status",
        ])
        .expect("parse");
        assert_eq!(cli.output, OutputFormat::Json);
        assert_eq!(cli.transport, TransportArg::Receiver);
        assert_eq!(cli.profile, 3);
    }

    #[test]
    fn rejects_invalid_profile_and_color() {
        assert!(Cli::try_parse_from(["x3ctl", "--profile", "0", "status"]).is_err());
        assert!(Cli::try_parse_from(["x3ctl", "prefs", "set", "--host-color", "xyz"]).is_err());
    }

    #[test]
    fn parses_stateless_dry_run_and_debug_commands() {
        let cli = Cli::try_parse_from(["x3ctl", "--stateless", "--dry-run", "state", "selected"])
            .expect("parse");
        assert!(cli.stateless && cli.dry_run);
        assert!(matches!(
            cli.command,
            Some(Command::State(StateCommand::Selected))
        ));

        let cli =
            Cli::try_parse_from(["x3ctl", "debug", "dpi", "--stages", "800,1600"]).expect("parse");
        assert!(matches!(
            cli.command,
            Some(Command::Debug(DebugCommand::Dpi(_)))
        ));
    }
}
