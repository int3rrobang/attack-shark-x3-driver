use clap::{Args, Parser, Subcommand, ValueEnum};

/// Clap-safe button slot for `bind set --slot`.
#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum SlotArg {
    Left,
    Right,
    Middle,
    Dpi,
    Forward,
    Backward,
}

/// Clap-safe button action for `bind set --action`.
#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum ActionArg {
    Disable,
    #[value(alias = "left-click")]
    LeftClick,
    #[value(alias = "right-click")]
    RightClick,
    #[value(alias = "middle-click")]
    MiddleClick,
    Backward,
    Forward,
    #[value(alias = "double-click")]
    DoubleClick,
    #[value(alias = "dpi-cycle")]
    DpiCycle,
    #[value(alias = "dpi-plus")]
    DpiPlus,
    #[value(alias = "dpi-minus")]
    DpiMinus,
    #[value(alias = "profile-cycle")]
    ProfileCycle,
    #[value(alias = "profile-plus")]
    ProfilePlus,
    #[value(alias = "profile-minus")]
    ProfileMinus,
    #[value(alias = "fire-button")]
    FireButton,
    #[value(alias = "scroll-up")]
    ScrollUp,
    #[value(alias = "scroll-down")]
    ScrollDown,
    #[value(alias = "media-player")]
    MediaPlayer,
    #[value(alias = "previous-track")]
    PreviousTrack,
    #[value(alias = "next-track")]
    NextTrack,
    #[value(alias = "play-pause")]
    PlayPause,
    #[value(alias = "stop")]
    Stop,
    #[value(alias = "mute")]
    Mute,
    #[value(alias = "volume-up")]
    VolumeUp,
    #[value(alias = "volume-down")]
    VolumeDown,
    #[value(alias = "calculator")]
    Calculator,
    #[value(alias = "email")]
    Email,
    #[value(alias = "browser-forward")]
    BrowserForward,
    #[value(alias = "browser-backward")]
    BrowserBackward,
    #[value(alias = "browser-stop")]
    BrowserStop,
    #[value(alias = "my-computer")]
    MyComputer,
    #[value(alias = "browser-refresh")]
    BrowserRefresh,
    #[value(alias = "browser-home")]
    BrowserHome,
    #[value(alias = "browser-search")]
    BrowserSearch,
    #[value(alias = "browser-favorites")]
    BrowserFavorites,
    #[value(alias = "cut")]
    Cut,
    #[value(alias = "copy")]
    Copy,
    #[value(alias = "paste")]
    Paste,
    #[value(alias = "open")]
    Open,
    #[value(alias = "save")]
    Save,
    #[value(alias = "find")]
    Find,
    #[value(alias = "redo")]
    Redo,
    #[value(alias = "select-all")]
    SelectAll,
    #[value(alias = "print")]
    Print,
    #[value(alias = "close-window")]
    CloseWindow,
    #[value(alias = "swap-windows")]
    SwapWindows,
    #[value(alias = "show-desktop")]
    ShowDesktop,
    #[value(alias = "run-command")]
    RunCommand,
    #[value(alias = "lock-pc")]
    LockPc,
    #[value(alias = "screen-capture")]
    ScreenCapture,
}

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

/// Post-write validation: transport evidence (USB submission or BLE ACK) or USB readback.
#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum ValidationArg {
    /// Accept transport evidence without re-reading (USB submission or BLE parser ACK).
    Transport,
    /// Re-read the device over USB after the write to confirm the immediate applied state.
    Readback,
}

/// Delta-merge baseline: read the live image from the device or merge against the durable stored baseline.
#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum BaselineArg {
    /// Read the live image from the device immediately before merging.
    Live,
    /// Merge against durable stored baseline without a live read (single-writer assumption).
    Stored,
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
    /// Validate writes by transport evidence (default) or USB readback.
    #[arg(long, value_enum, default_value_t = ValidationArg::Transport, global = true)]
    pub validation: ValidationArg,
    /// Merge against the live device image (default) or the durable stored baseline.
    #[arg(long, value_enum, default_value_t = BaselineArg::Live, global = true)]
    pub baseline: BaselineArg,
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
    /// Read or update the polling rate of the profile selected by --profile.
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
    /// Inspect, invalidate, or reset manager state through typed manager APIs.
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
    /// Temporarily enable and read all five USB profiles, then restore metadata.
    RefreshAll,
}

#[derive(Debug, Args)]
pub struct ProfileSetArgs {
    #[arg(value_parser = clap::value_parser!(u8).range(1..=5))]
    pub profile: Option<u8>,
    /// Set the maximum enabled profile count (1..=5).
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=5))]
    pub maximum: Option<u8>,
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
    /// Authorize a single unverified BLE-only polling-rate write.
    ///
    /// Skips the loader and the complete-profile safety checks: the raw
    /// polling-rate packet is sent directly over BLE and only the transport
    /// ACK is accepted as evidence. Persistence is not verified and there is
    /// no readback over BLE. This flag authorizes exactly one `rate set`
    /// invocation and has no effect on other commands.
    #[arg(long)]
    pub allow_unverified_ble_rate_write: bool,
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
    #[arg(long, value_enum)]
    pub slot: SlotArg,
    #[arg(long, value_enum)]
    pub action: ActionArg,
}

#[derive(Debug, Subcommand)]
pub enum StateCommand {
    Selected,
    Invalidate,
    /// Replace an unreadable state file with a fresh empty state, preserving
    /// the previous file at a sibling backup path.
    Reset,
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
    use super::{
        ActionArg, BaselineArg, BindCommand, BindSetArgs, Cli, Command, DebugCommand, OutputFormat,
        ProfileCommand, RateCommand, RateSetArgs, SlotArg, StateCommand, TransportArg,
        ValidationArg, VerifyMethodArg,
    };
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
    fn validation_defaults_to_transport_and_parses_for_all_set_actions() {
        let default =
            Cli::try_parse_from(["x3ctl", "dpi", "set", "--stages", "800"]).expect("parse");
        assert_eq!(default.validation, ValidationArg::Transport);

        let sets: &[&[&str]] = &[
            &["profile", "set"],
            &["dpi", "set", "--stages", "800,1600"],
            &["rate", "set", "1000"],
            &["prefs", "set", "--debounce", "2"],
            &["bind", "set", "--slot", "left", "--action", "left-click"],
        ];
        for (value, expected) in [
            ("transport", ValidationArg::Transport),
            ("readback", ValidationArg::Readback),
        ] {
            for set in sets {
                let mut argv = vec!["x3ctl", "--validation", value];
                argv.extend_from_slice(set);
                let cli = Cli::try_parse_from(&argv)
                    .unwrap_or_else(|error| panic!("--validation {value} with {set:?}: {error}"));
                assert_eq!(cli.validation, expected);
            }
        }
    }

    #[test]
    fn baseline_defaults_to_live_and_parses_for_all_set_actions() {
        let default =
            Cli::try_parse_from(["x3ctl", "dpi", "set", "--stages", "800"]).expect("parse");
        assert_eq!(default.baseline, BaselineArg::Live);

        let sets: &[&[&str]] = &[
            &["profile", "set"],
            &["dpi", "set", "--stages", "800,1600"],
            &["rate", "set", "1000"],
            &["prefs", "set", "--debounce", "2"],
            &["bind", "set", "--slot", "left", "--action", "left-click"],
        ];
        for (value, expected) in [("live", BaselineArg::Live), ("stored", BaselineArg::Stored)] {
            for set in sets {
                let mut argv = vec!["x3ctl", "--baseline", value];
                argv.extend_from_slice(set);
                let cli = Cli::try_parse_from(&argv)
                    .unwrap_or_else(|error| panic!("--baseline {value} with {set:?}: {error}"));
                assert_eq!(cli.baseline, expected);
            }
        }
    }

    #[test]
    fn rate_set_unverified_ble_flag_defaults_false_and_parses_explicit_true() {
        let default = Cli::try_parse_from(["x3ctl", "rate", "set", "1000"]).expect("parse");
        let (hz, flag) = match default.command {
            Some(Command::Rate(RateCommand::Set(RateSetArgs {
                hz,
                allow_unverified_ble_rate_write,
            }))) => (hz, allow_unverified_ble_rate_write),
            other => panic!("expected RateSet, got {other:?}"),
        };
        assert_eq!(hz, 1000);
        assert!(!flag);

        let explicit = Cli::try_parse_from([
            "x3ctl",
            "rate",
            "set",
            "1000",
            "--allow-unverified-ble-rate-write",
        ])
        .expect("parse");
        match explicit.command {
            Some(Command::Rate(RateCommand::Set(RateSetArgs {
                hz,
                allow_unverified_ble_rate_write,
            }))) => {
                assert_eq!(hz, 1000);
                assert!(allow_unverified_ble_rate_write);
            }
            other => panic!("expected RateSet, got {other:?}"),
        }
    }

    #[test]
    fn rate_set_unverified_ble_flag_accepts_global_argument_placement() {
        let cli = Cli::try_parse_from([
            "x3ctl",
            "--validation",
            "transport",
            "--profile",
            "2",
            "rate",
            "set",
            "500",
            "--allow-unverified-ble-rate-write",
        ])
        .expect("parse");
        assert_eq!(cli.validation, ValidationArg::Transport);
        assert_eq!(cli.profile, 2);
        match cli.command {
            Some(Command::Rate(RateCommand::Set(RateSetArgs {
                hz,
                allow_unverified_ble_rate_write,
            }))) => {
                assert_eq!(hz, 500);
                assert!(allow_unverified_ble_rate_write);
            }
            other => panic!("expected RateSet, got {other:?}"),
        }
    }

    #[test]
    fn rate_set_unverified_ble_flag_is_command_specific() {
        // Rejected on rate get and on other set commands.
        assert!(
            Cli::try_parse_from(["x3ctl", "rate", "get", "--allow-unverified-ble-rate-write"])
                .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "x3ctl",
                "dpi",
                "set",
                "--stages",
                "800",
                "--allow-unverified-ble-rate-write"
            ])
            .is_err()
        );
    }

    #[test]
    fn verify_method_flag_stays_separate_from_validation() {
        let cli = Cli::try_parse_from([
            "x3ctl",
            "--validation",
            "readback",
            "verify",
            "--method",
            "profile-reload",
        ])
        .expect("parse");
        assert_eq!(cli.validation, ValidationArg::Readback);
        assert!(matches!(
            cli.command,
            Some(Command::Verify {
                method: VerifyMethodArg::ProfileReload,
                ..
            })
        ));
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

    #[test]
    fn parses_explicit_all_profile_refresh_dry_run() {
        let cli =
            Cli::try_parse_from(["x3ctl", "--dry-run", "profile", "refresh-all"]).expect("parse");
        assert!(cli.dry_run);
        assert!(matches!(
            cli.command,
            Some(Command::Profile(ProfileCommand::RefreshAll))
        ));
    }

    #[test]
    fn parses_state_reset() {
        let cli = Cli::try_parse_from(["x3ctl", "state", "reset"]).expect("parse");
        assert!(matches!(
            cli.command,
            Some(Command::State(StateCommand::Reset))
        ));
    }

    #[test]
    fn bind_set_accepts_safe_symbolic_slots_and_actions() {
        let cases: &[(&str, &str, SlotArg, ActionArg)] = &[
            ("left", "left-click", SlotArg::Left, ActionArg::LeftClick),
            (
                "right",
                "right-click",
                SlotArg::Right,
                ActionArg::RightClick,
            ),
            (
                "middle",
                "middle-click",
                SlotArg::Middle,
                ActionArg::MiddleClick,
            ),
            ("dpi", "dpi-cycle", SlotArg::Dpi, ActionArg::DpiCycle),
            ("forward", "forward", SlotArg::Forward, ActionArg::Forward),
            (
                "backward",
                "backward",
                SlotArg::Backward,
                ActionArg::Backward,
            ),
            ("left", "disable", SlotArg::Left, ActionArg::Disable),
            (
                "left",
                "double-click",
                SlotArg::Left,
                ActionArg::DoubleClick,
            ),
            ("left", "dpi-cycle", SlotArg::Left, ActionArg::DpiCycle),
            ("left", "dpi-plus", SlotArg::Left, ActionArg::DpiPlus),
            ("left", "dpi-minus", SlotArg::Left, ActionArg::DpiMinus),
            (
                "left",
                "profile-cycle",
                SlotArg::Left,
                ActionArg::ProfileCycle,
            ),
            (
                "left",
                "profile-plus",
                SlotArg::Left,
                ActionArg::ProfilePlus,
            ),
            (
                "left",
                "profile-minus",
                SlotArg::Left,
                ActionArg::ProfileMinus,
            ),
            ("left", "fire-button", SlotArg::Left, ActionArg::FireButton),
            ("left", "scroll-up", SlotArg::Left, ActionArg::ScrollUp),
            ("left", "volume-up", SlotArg::Left, ActionArg::VolumeUp),
            (
                "left",
                "browser-home",
                SlotArg::Left,
                ActionArg::BrowserHome,
            ),
            (
                "left",
                "browser-favorites",
                SlotArg::Left,
                ActionArg::BrowserFavorites,
            ),
            ("left", "cut", SlotArg::Left, ActionArg::Cut),
            (
                "left",
                "screen-capture",
                SlotArg::Left,
                ActionArg::ScreenCapture,
            ),
        ];
        for &(slot, action, expected_slot, expected_action) in cases {
            let cli =
                Cli::try_parse_from(["x3ctl", "bind", "set", "--slot", slot, "--action", action])
                    .unwrap_or_else(|error| {
                        panic!("should parse slot={slot} action={action}: {error}")
                    });
            match cli.command {
                Some(Command::Bind(BindCommand::Set(BindSetArgs { slot, action }))) => {
                    assert_eq!(slot, expected_slot);
                    assert_eq!(action, expected_action);
                }
                other => panic!("expected BindSet, got {other:?}"),
            }
        }
    }

    #[test]
    fn bind_set_rejects_raw_numeric_slot_and_action() {
        // Raw numeric --slot must be rejected
        assert!(
            Cli::try_parse_from([
                "x3ctl",
                "bind",
                "set",
                "--slot",
                "0",
                "--action",
                "left-click"
            ])
            .is_err()
        );
        // Raw numeric --action must be rejected
        assert!(
            Cli::try_parse_from(["x3ctl", "bind", "set", "--slot", "left", "--action", "4"])
                .is_err()
        );
        // No scroll slots exposed
        assert!(
            Cli::try_parse_from([
                "x3ctl",
                "bind",
                "set",
                "--slot",
                "scroll-up",
                "--action",
                "left-click"
            ])
            .is_err()
        );
        // DPI slot IS now exposed (slot 3 is safe)
        assert!(
            Cli::try_parse_from([
                "x3ctl",
                "bind",
                "set",
                "--slot",
                "dpi",
                "--action",
                "left-click",
            ])
            .is_ok()
        );
        // Scroll slots still excluded
        assert!(
            Cli::try_parse_from([
                "x3ctl",
                "bind",
                "set",
                "--slot",
                "scroll-up",
                "--action",
                "left-click"
            ])
            .is_err()
        );
        // No modifier/key-code args
        assert!(
            Cli::try_parse_from([
                "x3ctl",
                "bind",
                "set",
                "--slot",
                "left",
                "--action",
                "left-click",
                "--modifier",
                "1"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "x3ctl",
                "bind",
                "set",
                "--slot",
                "left",
                "--action",
                "left-click",
                "--key-code",
                "4"
            ])
            .is_err()
        );
    }

    #[test]
    fn bind_set_requires_both_slot_and_action() {
        assert!(Cli::try_parse_from(["x3ctl", "bind", "set", "--slot", "left"]).is_err());
        assert!(Cli::try_parse_from(["x3ctl", "bind", "set", "--action", "left-click"]).is_err());
    }

    #[test]
    fn debug_dpi_global_transport_controls_packet_transport() {
        // Global --transport should be the single control for debug dpi; no shadowed local.
        let cli = Cli::try_parse_from([
            "x3ctl",
            "--transport",
            "receiver",
            "debug",
            "dpi",
            "--stages",
            "800,1600",
        ])
        .expect("parse with leading global");
        assert_eq!(cli.transport, TransportArg::Receiver);
        let debug = match &cli.command {
            Some(Command::Debug(DebugCommand::Dpi(args))) => args,
            _ => panic!("expected debug dpi"),
        };
        // DebugDpiArgs no longer has its own transport field; only global exists.
        // Verify stages still parse and other fields default correctly.
        assert_eq!(debug.stages, vec![800, 1600]);

        // Global can also be placed after the subcommand (clap global=true)
        let cli2 = Cli::try_parse_from([
            "x3ctl",
            "debug",
            "dpi",
            "--stages",
            "800,1600",
            "--transport",
            "wired",
        ])
        .expect("parse with trailing global");
        // Because DebugDpiArgs has no local transport, the trailing --transport must bind to global.
        assert_eq!(cli2.transport, TransportArg::Wired);

        // Default global is Auto when not specified
        let cli3 = Cli::try_parse_from(["x3ctl", "debug", "dpi", "--stages", "800"])
            .expect("parse default");
        assert_eq!(cli3.transport, TransportArg::Auto);
    }

    #[test]
    fn global_transport_selection_is_unambiguous_for_debug() {
        // Cli transport parsing accepts all known variants and debug sees same global
        for (flag, expected) in [
            ("auto", TransportArg::Auto),
            ("wired", TransportArg::Wired),
            ("receiver", TransportArg::Receiver),
        ] {
            let cli = Cli::try_parse_from(["x3ctl", "--transport", flag, "status"])
                .unwrap_or_else(|e| panic!("{flag}: {e}"));
            assert_eq!(cli.transport, expected);
            // Debug should see the same global value
            let cli_debug = Cli::try_parse_from([
                "x3ctl",
                "--transport",
                flag,
                "debug",
                "dpi",
                "--stages",
                "800",
            ])
            .unwrap_or_else(|e| panic!("debug {flag}: {e}"));
            assert_eq!(cli_debug.transport, expected);
        }
    }
}
