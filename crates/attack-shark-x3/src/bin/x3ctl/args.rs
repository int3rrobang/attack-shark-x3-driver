use clap::{Args, Parser, Subcommand, ValueEnum};

// ---------------------------------------------------------------------------
// Global flags
// ---------------------------------------------------------------------------

#[cfg(feature = "ble")]
const LONG_ABOUT: &str = "Inspect, configure, and manage Attack Shark X3/M600-family mice across wired, 2.4 GHz receiver, and BLE transports.";

#[cfg(not(feature = "ble"))]
const LONG_ABOUT: &str = "Inspect, configure, and manage Attack Shark X3/M600-family mice across wired and 2.4 GHz receiver transports.";

#[derive(Debug, Parser)]
#[command(
    name = "x3ctl",
    version,
    about = "Attack Shark X3/M600 configuration CLI",
    long_about = LONG_ABOUT
)]
pub struct Cli {
    #[cfg_attr(
        feature = "ble",
        doc = "Transport: auto-detect, wired USB, 2.4 GHz receiver, or BLE."
    )]
    #[cfg_attr(
        not(feature = "ble"),
        doc = "Transport: auto-detect, wired USB, or 2.4 GHz receiver."
    )]
    #[arg(
        short = 't',
        long,
        value_enum,
        default_value = "auto",
        global = true,
        verbatim_doc_comment
    )]
    pub transport: TransportArg,
    #[cfg_attr(
        feature = "ble",
        doc = "Device path (USB) or name prefix (BLE / receiver)."
    )]
    #[cfg_attr(
        not(feature = "ble"),
        doc = "Device path (USB) or name prefix (receiver)."
    )]
    #[arg(short = 'd', long, global = true, verbatim_doc_comment)]
    pub device: Option<String>,

    /// Target profile (1–5); serves as default when a subcommand omits its own
    /// --profile.
    #[arg(
        short = 'p',
        long,
        global = true,
        value_parser = clap::value_parser!(u8).range(1..=5),
        verbatim_doc_comment
    )]
    pub profile: Option<u8>,

    /// Emit JSON to stdout.
    #[arg(long, global = true, conflicts_with = "quiet")]
    pub json: bool,

    /// Quiet mode: emit only machine-parseable scalar values.
    #[arg(short = 'q', long, global = true, conflicts_with = "json")]
    pub quiet: bool,

    /// Dry run: validate and print what would be sent; never touch hardware.
    #[arg(short = 'n', long, global = true)]
    pub dry_run: bool,

    /// Talk to hardware directly; skip the local IPC broker.
    #[arg(long, global = true)]
    pub direct: bool,

    /// Skip durable-state readback and writeback for this invocation.
    #[arg(long, global = true)]
    pub no_state: bool,

    /// Equivalent to --direct --no-state.
    #[arg(long, global = true)]
    pub stateless: bool,

    /// Use explicit defaults as the BLE baseline when no stored or imported
    /// configuration is available.  Has no effect on USB transports.
    #[arg(long, global = true)]
    pub replace_defaults: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    /// The effective output format derived from `--json` / `--quiet`.
    #[must_use]
    pub fn output_format(&self) -> OutputFormat {
        if self.json {
            OutputFormat::Json
        } else if self.quiet {
            OutputFormat::Quiet
        } else {
            OutputFormat::Human
        }
    }

    /// Whether direct hardware access is requested (explicitly or via
    /// `--stateless`).
    #[must_use]
    pub fn effective_direct(&self) -> bool {
        self.direct || self.stateless
    }

    /// Whether durable state is suppressed (explicitly or via `--stateless`).
    #[must_use]
    pub fn effective_no_state(&self) -> bool {
        self.no_state || self.stateless
    }

    /// Resolve a command-level profile against the global `--profile` default.
    #[must_use]
    pub fn resolve_profile(&self, cmd_profile: Option<u8>) -> Option<u8> {
        cmd_profile.or(self.profile)
    }
}

// ---------------------------------------------------------------------------
// Output format (mirrored in output.rs — kept here for standalone testing)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    Human,
    Json,
    Quiet,
}

// ---------------------------------------------------------------------------
// Shared enums
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum TransportArg {
    /// Auto-detect the available transport.
    Auto,
    /// Wired USB HID.
    Wired,
    /// 2.4 GHz receiver (FA61 dongle).
    Receiver,
    /// Bluetooth Low Energy GATT.
    #[cfg(feature = "ble")]
    Ble,
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum ToggleArg {
    On,
    Off,
}

impl From<ToggleArg> for bool {
    fn from(value: ToggleArg) -> Self {
        matches!(value, ToggleArg::On)
    }
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum LodArg {
    /// 1 mm lift-off distance.
    #[value(name = "1")]
    One,
    /// 2 mm lift-off distance.
    #[value(name = "2")]
    Two,
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum LightModeArg {
    Off,
    Static,
    Breathing,
    Neon,
    #[value(name = "color-breathing")]
    ColorBreathing,
    #[value(name = "static-dpi")]
    StaticDpi,
    #[value(name = "breathing-dpi")]
    BreathingDpi,
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum ButtonArg {
    Left,
    Right,
    Middle,
    Dpi,
    Forward,
    Backward,
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum ButtonActionArg {
    Disable,
    #[value(name = "left-click")]
    LeftClick,
    #[value(name = "right-click")]
    RightClick,
    #[value(name = "middle-click")]
    MiddleClick,
    Forward,
    Backward,
    #[value(name = "double-click")]
    DoubleClick,
    #[value(name = "dpi-cycle")]
    DpiCycle,
    #[value(name = "dpi-plus")]
    DpiPlus,
    #[value(name = "dpi-minus")]
    DpiMinus,
    #[value(name = "profile-cycle")]
    ProfileCycle,
    #[value(name = "profile-plus")]
    ProfilePlus,
    #[value(name = "profile-minus")]
    ProfileMinus,
}

// ---------------------------------------------------------------------------
// Profile-control framing (debug commands)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum ProfileFramingArg {
    Compact,
    Full,
}

// ---------------------------------------------------------------------------
// Top-level commands
// ---------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List discoverable X3 devices.
    Devices,

    /// Select the active device for subsequent commands.
    Use {
        /// Device path or name prefix.
        device: String,
    },

    /// Show device, broker, and durable-state status.
    Status,

    /// Profile selection and maximum-configuration management.
    #[command(subcommand)]
    Profile(ProfileCommand),

    /// Read or write DPI configuration.
    ///
    /// With no arguments, read the current DPI state.  Provide stages and/or
    /// flags to write.
    Dpi(DpiArgs),

    /// Read or set the global polling rate.
    ///
    /// With no argument, read the current rate.  Provide a frequency in hertz
    /// to set it.
    Rate(RateArgs),

    /// Read or write per-profile preferences (lighting, sleep, debounce).
    Prefs(PrefsArgs),

    /// Read or set button bindings.
    ///
    /// With no arguments, read the current button table.  Provide both BUTTON
    /// and ACTION to set a binding.
    Bind(BindArgs),

    /// Read the current battery percentage.
    Battery,
    /// Reset the device to factory defaults and reapply configuration.
    ///
    /// Sends the 0x0c reset image followed by the complete factory-default
    /// sequence (DPI, preferences, polling rate, buttons) with 500 ms
    /// quiet periods between each report.
    Reset(ResetArgs),

    /// Apply durable-state configuration to the device.
    ///
    /// Without a FILE argument, applies the full stored state for the selected
    /// device.  With a FILE, imports and applies a JSON export document.
    Apply {
        /// JSON state file to import and apply (created by `x3ctl export`).
        file: Option<String>,
    },

    /// Export durable state as JSON to stdout or a file.
    Export {
        /// File to write the exported state to.  Writes to stdout when omitted.
        file: Option<String>,
    },

    /// Disconnect from the device and release the hardware handle.
    Disconnect,

    /// Durable-state management commands.
    #[command(subcommand)]
    State(StateCommand),

    /// Daemon lifecycle commands.
    #[command(subcommand)]
    Daemon(DaemonCommand),

    /// Debug packet construction (offline — no hardware required).
    #[command(subcommand)]
    Debug(DebugCommand),
}

// ---------------------------------------------------------------------------
// Subcommand groups
// ---------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    /// Activate a profile (1–5).
    Use {
        /// Profile number.
        profile: u8,
    },
    /// Set the maximum enabled profile (1–5).
    Max {
        /// Maximum profile number.
        max: u8,
    },
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Show broker daemon status.
    Status,
    /// Stop the broker daemon.
    Stop,
}

/// Durable-state management subcommands.
#[derive(Debug, Subcommand)]
pub enum StateCommand {
    /// Populate durable state with explicit hardware defaults for the selected
    /// device and transport.  Useful as a BLE baseline seed before applying
    /// partial configuration.
    InitDefaults,

    /// Remove a device (and all its profiles) from durable state.
    ///
    /// If the forgotten device was the selected device, the selection is
    /// cleared.
    Forget {
        /// Device key to forget.  Uses the currently-selected device when
        /// omitted.
        device: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Command argument structs
// ---------------------------------------------------------------------------

/// DPI command arguments.  All fields are optional; providing any field
/// triggers a write.
#[derive(Debug, Args)]
pub struct DpiArgs {
    /// Comma-separated DPI stages (e.g. "800,1600,2400,3200").
    pub stages: Option<String>,

    /// Active stage index (1-based, 1–8).
    #[arg(long)]
    pub active: Option<u8>,

    /// Lift-off distance in millimetres.
    #[arg(long, value_enum)]
    pub lod: Option<LodArg>,

    /// Ripple control.
    #[arg(long, value_enum)]
    pub ripple_control: Option<ToggleArg>,

    /// Angle snapping.
    #[arg(long, value_enum)]
    pub angle_snap: Option<ToggleArg>,

    /// Motion sync.
    #[arg(long, value_enum)]
    pub motion_sync: Option<ToggleArg>,
}

impl DpiArgs {
    /// Returns true when at least one write field is set.
    #[must_use]
    pub fn is_write(&self) -> bool {
        self.stages.is_some()
            || self.active.is_some()
            || self.lod.is_some()
            || self.ripple_control.is_some()
            || self.angle_snap.is_some()
            || self.motion_sync.is_some()
    }
}

/// Polling-rate command arguments.
#[derive(Debug, Args)]
pub struct RateArgs {
    /// Polling rate in hertz (125, 250, 500, or 1000).  Omit to read.
    pub hz: Option<u16>,
}

/// Preferences command arguments.  All fields are optional; providing any
/// field triggers a write.
#[derive(Debug, Args)]
pub struct PrefsArgs {
    /// LED light mode.
    #[arg(long, value_enum)]
    pub light_mode: Option<LightModeArg>,

    /// Opaque raw light-mode byte (e.g. "0x70"); mutually exclusive with
    /// --light-mode.
    #[arg(long, value_name = "BYTE", conflicts_with = "light_mode")]
    pub light_mode_raw: Option<String>,

    /// LED animation speed (1–5, 1 = slowest).
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=5))]
    pub led_speed: Option<u8>,

    /// Deep-sleep timeout in whole minutes (1–60).
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=60))]
    pub deep_sleep: Option<u8>,

    /// Normal-sleep timeout in minutes (0.5–30, half-minute steps).
    #[arg(long)]
    pub sleep: Option<f32>,

    /// Button debounce in milliseconds (4–50, even values).
    #[arg(long, value_parser = clap::value_parser!(u8).range(4..=50))]
    pub debounce: Option<u8>,
}

impl PrefsArgs {
    #[must_use]
    pub fn is_write(&self) -> bool {
        self.light_mode.is_some()
            || self.light_mode_raw.is_some()
            || self.led_speed.is_some()
            || self.deep_sleep.is_some()
            || self.sleep.is_some()
            || self.debounce.is_some()
    }
}

/// Button-binding command arguments.
#[derive(Debug, Args)]
pub struct BindArgs {
    /// Physical button to bind.
    #[arg(value_enum)]
    pub button: Option<ButtonArg>,

    /// Action to assign to the button.
    #[arg(value_enum)]
    pub action: Option<ButtonActionArg>,
}

impl BindArgs {
    /// Returns true when a write is requested (both fields present).
    #[must_use]
    pub fn is_write(&self) -> bool {
        self.button.is_some() && self.action.is_some()
    }

    /// Validates that button and action are either both present or both absent.
    /// Returns an error message when only one is provided.
    #[must_use]
    pub fn validate(&self) -> Result<(), String> {
        match (self.button, self.action) {
            (Some(_), Some(_)) | (None, None) => Ok(()),
            (Some(_), None) => {
                Err("bind requires both BUTTON and ACTION, or neither (to read)".into())
            }
            (None, Some(_)) => {
                Err("bind requires both BUTTON and ACTION, or neither (to read)".into())
            }
        }
    }
}

/// Reset command arguments.
#[derive(Debug, Args)]
pub struct ResetArgs {
    /// Target profile to reset (1–5).
    #[arg(
        short = 'p',
        long,
        value_parser = clap::value_parser!(u8).range(1..=5),
        default_value = "1",
        verbatim_doc_comment
    )]
    pub profile: u8,
}

// ---------------------------------------------------------------------------
// Debug subcommands (offline packet construction)
// ---------------------------------------------------------------------------

#[derive(Debug, Subcommand)]
pub enum DebugCommand {
    /// Build an offline DPI packet.
    Dpi(DebugDpiArgs),
    /// Build an edge-triggered profile-control packet.
    #[command(name = "profile-control")]
    ProfileControl(DebugProfileControlArgs),
    /// Build an A0 read-selector packet.
    #[command(name = "read-selector")]
    ReadSelector(DebugReadSelectorArgs),
}

#[derive(Debug, Args)]
pub struct DebugDpiArgs {
    /// Transport framing.
    #[arg(long, value_enum, default_value = "wired")]
    pub transport: TransportArg,

    /// Target profile (1–5).
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=5))]
    pub profile: u8,

    /// Comma-separated DPI stages.
    #[arg(
        long,
        value_delimiter = ',',
        default_values_t = [800_u16, 1600, 2400, 3200, 5000, 26000]
    )]
    pub stages: Vec<u16>,

    /// Active stage index (1-based).
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(1..=8))]
    pub active: u8,

    /// Lift-off distance.
    #[arg(long, value_enum, default_value = "1")]
    pub lod: LodArg,

    /// Ripple control.
    #[arg(long)]
    pub ripple_control: bool,

    /// Angle snapping.
    #[arg(long)]
    pub angle_snap: bool,

    /// Motion sync.
    #[arg(long)]
    pub motion_sync: bool,
}

#[derive(Debug, Args)]
pub struct DebugProfileControlArgs {
    /// Current (active) profile.
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=5))]
    pub current: u8,

    /// Maximum enabled profile.
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u8).range(1..=5))]
    pub maximum: u8,

    /// Wire framing.
    #[arg(long, value_enum, default_value = "compact")]
    pub framing: ProfileFramingArg,
}

#[derive(Debug, Args)]
pub struct DebugReadSelectorArgs {
    /// Report type to read.
    #[arg(long, value_enum)]
    pub report: DebugReadReportArg,

    /// Profile (required for DPI, preferences, and buttons).
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=5))]
    pub profile: Option<u8>,
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum DebugReadReportArg {
    Version,
    #[value(name = "profile-metadata")]
    ProfileMetadata,
    #[value(name = "polling-rate")]
    PollingRate,
    Dpi,
    Preferences,
    Buttons,
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("x3ctl").chain(args.iter().copied()))
            .expect("valid args")
    }

    fn parse_err(args: &[&str]) -> String {
        Cli::try_parse_from(std::iter::once("x3ctl").chain(args.iter().copied()))
            .unwrap_err()
            .to_string()
    }

    // -- concise examples ---------------------------------------------------

    #[test]
    fn devices_no_args() {
        let cli = parse(&["devices"]);
        assert!(matches!(cli.command, Some(Command::Devices)));
    }

    #[test]
    fn status_no_args() {
        let cli = parse(&["status"]);
        assert!(matches!(cli.command, Some(Command::Status)));
    }

    #[test]
    fn battery_no_args() {
        let cli = parse(&["battery"]);
        assert!(matches!(cli.command, Some(Command::Battery)));
    }

    #[test]
    fn apply_no_args() {
        let cli = parse(&["apply"]);
        assert!(matches!(cli.command, Some(Command::Apply { file: _ })));
    }

    #[test]
    fn export_no_args() {
        let cli = parse(&["export"]);
        assert!(matches!(cli.command, Some(Command::Export { file: _ })));
    }

    #[test]
    fn disconnect_no_args() {
        let cli = parse(&["disconnect"]);
        assert!(matches!(cli.command, Some(Command::Disconnect)));
    }

    #[test]
    fn use_device() {
        let cli = parse(&["use", "hid-1234"]);
        let Some(Command::Use { device }) = &cli.command else {
            panic!("expected Use");
        };
        assert_eq!(device, "hid-1234");
    }

    #[test]
    fn profile_use() {
        let cli = parse(&["profile", "use", "3"]);
        let Some(Command::Profile(ProfileCommand::Use { profile })) = &cli.command else {
            panic!("expected Profile::Use");
        };
        assert_eq!(*profile, 3);
    }

    #[test]
    fn profile_max() {
        let cli = parse(&["profile", "max", "5"]);
        let Some(Command::Profile(ProfileCommand::Max { max })) = &cli.command else {
            panic!("expected Profile::Max");
        };
        assert_eq!(*max, 5);
    }

    #[test]
    fn dpi_read_no_args() {
        let cli = parse(&["dpi"]);
        let Some(Command::Dpi(args)) = &cli.command else {
            panic!("expected Dpi");
        };
        assert!(!args.is_write());
    }

    #[test]
    fn dpi_write_stages() {
        let cli = parse(&["dpi", "800,1600,2400"]);
        let Some(Command::Dpi(args)) = &cli.command else {
            panic!("expected Dpi");
        };
        assert!(args.is_write());
        assert_eq!(args.stages.as_deref(), Some("800,1600,2400"));
    }

    #[test]
    fn dpi_write_flags() {
        let cli = parse(&["dpi", "--active", "3", "--lod", "2"]);
        let Some(Command::Dpi(args)) = &cli.command else {
            panic!("expected Dpi");
        };
        assert!(args.is_write());
        assert_eq!(args.active, Some(3));
        assert_eq!(args.lod, Some(LodArg::Two));
    }

    #[test]
    fn dpi_all_toggles() {
        let cli = parse(&[
            "dpi",
            "--ripple-control",
            "on",
            "--angle-snap",
            "off",
            "--motion-sync",
            "on",
        ]);
        let Some(Command::Dpi(args)) = &cli.command else {
            panic!("expected Dpi");
        };
        assert_eq!(args.ripple_control, Some(ToggleArg::On));
        assert_eq!(args.angle_snap, Some(ToggleArg::Off));
        assert_eq!(args.motion_sync, Some(ToggleArg::On));
    }

    #[test]
    fn rate_read_no_args() {
        let cli = parse(&["rate"]);
        assert!(matches!(cli.command, Some(Command::Rate(_))));
    }

    #[test]
    fn rate_write() {
        let cli = parse(&["rate", "1000"]);
        let Some(Command::Rate(args)) = &cli.command else {
            panic!("expected Rate");
        };
        assert_eq!(args.hz, Some(1000));
    }

    #[test]
    fn prefs_read_no_args() {
        let cli = parse(&["prefs"]);
        let Some(Command::Prefs(args)) = &cli.command else {
            panic!("expected Prefs");
        };
        assert!(!args.is_write());
    }

    #[test]
    fn prefs_write() {
        let cli = parse(&[
            "prefs",
            "--light-mode",
            "breathing",
            "--led-speed",
            "3",
            "--deep-sleep",
            "30",
            "--sleep",
            "5",
            "--debounce",
            "8",
        ]);
        let Some(Command::Prefs(args)) = &cli.command else {
            panic!("expected Prefs");
        };
        assert!(args.is_write());
        assert_eq!(args.light_mode, Some(LightModeArg::Breathing));
        assert_eq!(args.led_speed, Some(3));
        assert_eq!(args.deep_sleep, Some(30));
        assert_eq!(args.sleep, Some(5.0));
        assert_eq!(args.debounce, Some(8));
    }

    #[test]
    fn prefs_light_mode_raw() {
        let cli = parse(&["prefs", "--light-mode-raw", "0x70"]);
        let Some(Command::Prefs(args)) = &cli.command else {
            panic!("expected Prefs");
        };
        assert_eq!(args.light_mode_raw.as_deref(), Some("0x70"));
    }

    #[test]
    fn bind_read_no_args() {
        let cli = parse(&["bind"]);
        let Some(Command::Bind(args)) = &cli.command else {
            panic!("expected Bind");
        };
        assert!(!args.is_write());
        assert!(args.validate().is_ok());
    }

    #[test]
    fn bind_write() {
        let cli = parse(&["bind", "forward", "disable"]);
        let Some(Command::Bind(args)) = &cli.command else {
            panic!("expected Bind");
        };
        assert!(args.is_write());
        assert_eq!(args.button, Some(ButtonArg::Forward));
        assert_eq!(args.action, Some(ButtonActionArg::Disable));
        assert!(args.validate().is_ok());
    }

    #[test]
    fn bind_write_kebab_action() {
        let cli = parse(&["bind", "left", "profile-cycle"]);
        let Some(Command::Bind(args)) = &cli.command else {
            panic!("expected Bind");
        };
        assert_eq!(args.button, Some(ButtonArg::Left));
        assert_eq!(args.action, Some(ButtonActionArg::ProfileCycle));
    }

    #[test]
    fn bind_invalid_button_only() {
        let cli = parse(&["bind", "forward"]);
        let Some(Command::Bind(args)) = &cli.command else {
            panic!("expected Bind");
        };
        assert!(args.validate().is_err());
    }

    #[test]
    fn bind_invalid_action_only() {
        // With only an action argument, Clap tries to parse it as a button
        // first and fails because "disable" is not a valid ButtonArg.
        let err = parse_err(&["bind", "disable"]);
        assert!(
            err.contains("disable") || err.contains("button"),
            "expected parse error for bind with only action: {err}"
        );
    }

    #[test]
    fn bind_invalid_unknown_button() {
        let err = parse_err(&["bind", "nope", "disable"]);
        assert!(err.contains("nope"), "expected error about nope: {err}");
    }

    // -- global flags -------------------------------------------------------

    #[cfg(feature = "ble")]
    #[test]
    fn global_transport() {
        let cli = parse(&["-t", "ble", "devices"]);
        assert_eq!(cli.transport, TransportArg::Ble);
    }

    #[test]
    fn global_device() {
        let cli = parse(&["-d", "COM3", "status"]);
        assert_eq!(cli.device.as_deref(), Some("COM3"));
    }

    #[test]
    fn global_profile() {
        let cli = parse(&["-p", "2", "dpi"]);
        assert_eq!(cli.profile, Some(2));
    }

    #[test]
    fn global_json() {
        let cli = parse(&["--json", "status"]);
        assert!(cli.json);
        assert_eq!(cli.output_format(), OutputFormat::Json);
    }

    #[test]
    fn global_quiet() {
        let cli = parse(&["-q", "battery"]);
        assert!(cli.quiet);
        assert_eq!(cli.output_format(), OutputFormat::Quiet);
    }

    #[test]
    fn global_dry_run() {
        let cli = parse(&["-n", "apply"]);
        assert!(cli.dry_run);
    }

    #[test]
    fn global_direct() {
        let cli = parse(&["--direct", "devices"]);
        assert!(cli.direct);
        assert!(cli.effective_direct());
    }

    #[test]
    fn global_no_state() {
        let cli = parse(&["--no-state", "status"]);
        assert!(cli.no_state);
        assert!(cli.effective_no_state());
    }

    #[test]
    fn global_stateless_implies_direct_and_no_state() {
        let cli = parse(&["--stateless", "devices"]);
        assert!(cli.stateless);
        // --stateless alone does not set --direct or --no-state flags,
        // but the effective helpers should report true.
        assert!(cli.effective_direct());
        assert!(cli.effective_no_state());
    }

    #[test]
    fn global_json_conflicts_with_quiet() {
        let err = parse_err(&["--json", "-q", "status"]);
        assert!(
            err.contains("cannot be used with") || err.contains("conflict"),
            "expected conflict error: {err}"
        );
    }

    // -- daemon -------------------------------------------------------------

    #[test]
    fn daemon_status() {
        let cli = parse(&["daemon", "status"]);
        assert!(matches!(
            cli.command,
            Some(Command::Daemon(DaemonCommand::Status))
        ));
    }

    #[test]
    fn daemon_stop() {
        let cli = parse(&["daemon", "stop"]);
        assert!(matches!(
            cli.command,
            Some(Command::Daemon(DaemonCommand::Stop))
        ));
    }

    // -- debug subcommands --------------------------------------------------

    #[test]
    fn debug_dpi_defaults() {
        let cli = parse(&["debug", "dpi"]);
        let Some(Command::Debug(DebugCommand::Dpi(args))) = &cli.command else {
            panic!("expected Debug::Dpi");
        };
        assert_eq!(args.transport, TransportArg::Wired);
        assert_eq!(args.profile, 1);
        assert_eq!(args.stages, vec![800, 1600, 2400, 3200, 5000, 26000]);
        assert_eq!(args.active, 2);
        assert_eq!(args.lod, LodArg::One);
        assert!(!args.ripple_control);
        assert!(!args.angle_snap);
        assert!(!args.motion_sync);
    }

    #[test]
    fn debug_dpi_explicit() {
        let cli = parse(&[
            "debug",
            "dpi",
            "--transport",
            "receiver",
            "--profile",
            "3",
            "--stages",
            "400,800",
            "--active",
            "1",
            "--lod",
            "2",
            "--ripple-control",
            "--angle-snap",
            "--motion-sync",
        ]);
        let Some(Command::Debug(DebugCommand::Dpi(args))) = &cli.command else {
            panic!("expected Debug::Dpi");
        };
        assert_eq!(args.transport, TransportArg::Receiver);
        assert_eq!(args.profile, 3);
        assert_eq!(args.stages, vec![400, 800]);
        assert_eq!(args.active, 1);
        assert_eq!(args.lod, LodArg::Two);
        assert!(args.ripple_control);
        assert!(args.angle_snap);
        assert!(args.motion_sync);
    }

    #[test]
    fn debug_profile_control() {
        let cli = parse(&[
            "debug",
            "profile-control",
            "--current",
            "2",
            "--maximum",
            "4",
            "--framing",
            "full",
        ]);
        let Some(Command::Debug(DebugCommand::ProfileControl(args))) = &cli.command else {
            panic!("expected Debug::ProfileControl");
        };
        assert_eq!(args.current, 2);
        assert_eq!(args.maximum, 4);
        assert_eq!(args.framing, ProfileFramingArg::Full);
    }

    #[test]
    fn debug_read_selector() {
        let cli = parse(&[
            "debug",
            "read-selector",
            "--report",
            "dpi",
            "--profile",
            "2",
        ]);
        let Some(Command::Debug(DebugCommand::ReadSelector(args))) = &cli.command else {
            panic!("expected Debug::ReadSelector");
        };
        assert_eq!(args.report, DebugReadReportArg::Dpi);
        assert_eq!(args.profile, Some(2));
    }

    // -- resolve_profile ----------------------------------------------------

    #[test]
    fn resolve_profile_uses_command_first() {
        let cli = parse(&["-p", "3", "dpi"]);
        assert_eq!(cli.resolve_profile(Some(1)), Some(1));
    }

    #[test]
    fn resolve_profile_falls_back_to_global() {
        let cli = parse(&["-p", "4", "dpi"]);
        assert_eq!(cli.resolve_profile(None), Some(4));
    }

    #[test]
    fn resolve_profile_none_when_neither_set() {
        let cli = parse(&["dpi"]);
        assert_eq!(cli.resolve_profile(None), None);
    }

    // -- direct independent of no-state --------------------------------------

    /// `--direct` alone does not set `--no-state`; the flags are independent.
    #[test]
    fn direct_does_not_imply_no_state() {
        let cli = parse(&["--direct", "devices"]);
        assert!(cli.direct);
        assert!(!cli.no_state);
        assert!(cli.effective_direct());
        assert!(!cli.effective_no_state());
    }

    /// `--no-state` alone does not set `--direct`.
    #[test]
    fn no_state_does_not_imply_direct() {
        let cli = parse(&["--no-state", "status"]);
        assert!(cli.no_state);
        assert!(!cli.direct);
        assert!(!cli.effective_direct());
        assert!(cli.effective_no_state());
    }

    /// `--direct` + `--no-state` together — both raw flags and effective
    /// helpers report true.
    #[test]
    fn direct_and_no_state_together() {
        let cli = parse(&["--direct", "--no-state", "apply"]);
        assert!(cli.direct);
        assert!(cli.no_state);
        assert!(cli.effective_direct());
        assert!(cli.effective_no_state());
    }

    // -- stateless implies both effective modes ------------------------------

    /// `--stateless` sets `effective_direct()` and `effective_no_state()`
    /// without touching the raw `direct` / `no_state` fields.
    #[test]
    fn stateless_effective_modes_without_raw_flags() {
        let cli = parse(&["--stateless", "battery"]);
        assert!(cli.stateless);
        assert!(!cli.direct);
        assert!(!cli.no_state);
        assert!(cli.effective_direct());
        assert!(cli.effective_no_state());
    }

    /// `--stateless` combined with explicit `--direct` still reports both
    /// effective modes.
    #[test]
    fn stateless_combined_with_direct() {
        let cli = parse(&["--stateless", "--direct", "devices"]);
        assert!(cli.stateless);
        assert!(cli.direct);
        assert!(cli.effective_direct());
        assert!(cli.effective_no_state());
    }

    // -- daemon status parser ------------------------------------------------

    /// The `daemon status` subcommand maps to [`DaemonCommand::Status`].
    #[test]
    fn daemon_status_parsed() {
        let cli = parse(&["daemon", "status"]);
        assert!(matches!(
            cli.command,
            Some(Command::Daemon(DaemonCommand::Status))
        ));
    }

    /// The `daemon stop` subcommand maps to [`DaemonCommand::Stop`].
    #[test]
    fn daemon_stop_parsed() {
        let cli = parse(&["daemon", "stop"]);
        assert!(matches!(
            cli.command,
            Some(Command::Daemon(DaemonCommand::Stop))
        ));
    }

    // -- state init-defaults / forget ----------------------------------------

    #[test]
    fn state_init_defaults_parsed() {
        let cli = parse(&["state", "init-defaults"]);
        assert!(matches!(
            cli.command,
            Some(Command::State(StateCommand::InitDefaults))
        ));
    }

    #[test]
    fn state_forget_with_device_parsed() {
        let cli = parse(&["state", "forget", "hid-ABCD"]);
        let Some(Command::State(StateCommand::Forget { device })) = &cli.command else {
            panic!("expected State::Forget");
        };
        assert_eq!(device.as_deref(), Some("hid-ABCD"));
    }

    #[test]
    fn state_forget_without_device_parsed() {
        let cli = parse(&["state", "forget"]);
        let Some(Command::State(StateCommand::Forget { device })) = &cli.command else {
            panic!("expected State::Forget");
        };
        assert!(device.is_none());
    }

    // -- apply / export file operands ----------------------------------------

    #[test]
    fn apply_with_file_operand() {
        let cli = parse(&["apply", "state.json"]);
        let Some(Command::Apply { file }) = &cli.command else {
            panic!("expected Apply");
        };
        assert_eq!(file.as_deref(), Some("state.json"));
    }

    #[test]
    fn apply_without_file_operand() {
        let cli = parse(&["apply"]);
        let Some(Command::Apply { file }) = &cli.command else {
            panic!("expected Apply");
        };
        assert!(file.is_none());
    }

    #[test]
    fn export_with_file_operand() {
        let cli = parse(&["export", "out.json"]);
        let Some(Command::Export { file }) = &cli.command else {
            panic!("expected Export");
        };
        assert_eq!(file.as_deref(), Some("out.json"));
    }

    #[test]
    fn export_without_file_operand() {
        let cli = parse(&["export"]);
        let Some(Command::Export { file }) = &cli.command else {
            panic!("expected Export");
        };
        assert!(file.is_none());
    }

    // -- replace-defaults forwarding eligibility -----------------------------

    /// `--replace-defaults` is parsed as `true` when present.
    #[test]
    fn replace_defaults_flag_present() {
        let cli = parse(&["--replace-defaults", "apply"]);
        assert!(cli.replace_defaults);
    }

    /// `--replace-defaults` is `false` by default when omitted.
    #[test]
    fn replace_defaults_absent_by_default() {
        let cli = parse(&["apply"]);
        assert!(!cli.replace_defaults);
    }

    /// `--replace-defaults` is eligible anywhere — global flag on any
    /// subcommand.
    #[test]
    fn replace_defaults_with_export() {
        let cli = parse(&["--replace-defaults", "export", "state.json"]);
        assert!(cli.replace_defaults);
    }

    /// `--replace-defaults` can be combined with other global flags.
    #[test]
    fn replace_defaults_with_direct_and_stateless() {
        let cli = parse(&[
            "--replace-defaults",
            "--direct",
            "--no-state",
            "apply",
            "state.json",
        ]);
        assert!(cli.replace_defaults);
        assert!(cli.direct);
        assert!(cli.no_state);
        assert!(cli.effective_direct());
        assert!(cli.effective_no_state());
    }

    // -- no-command defaults to status --------------------------------------

    /// Bare `x3ctl` with no subcommand parses successfully and yields
    /// `None` for the command field, which the dispatch layer treats as
    /// an implicit `status`.
    #[test]
    fn no_command_parses_to_none() {
        let cli = parse(&[]);
        assert!(cli.command.is_none());
    }

    /// `x3ctl status` still maps to the explicit `Status` variant.
    #[test]
    fn explicit_status_command() {
        let cli = parse(&["status"]);
        assert!(matches!(cli.command, Some(Command::Status)));
    }

    // -- transport parsing with and without BLE ----------------------------

    /// With the `ble` feature enabled, `--transport ble` is accepted.
    #[cfg(feature = "ble")]
    #[test]
    fn transport_ble_accepted() {
        let cli = parse(&["--transport", "ble", "status"]);
        assert_eq!(cli.transport, TransportArg::Ble);
    }

    /// When the `ble` feature is disabled, `--transport ble` must be
    /// rejected by clap.
    #[cfg(not(feature = "ble"))]
    #[test]
    fn transport_ble_rejected_without_feature() {
        let err = parse_err(&["--transport", "ble", "status"]);
        assert!(
            err.contains("ble") || err.contains("invalid"),
            "expected parse error for 'ble' transport without feature: {err}"
        );
    }

    /// `--transport wired` is always valid regardless of features.
    #[test]
    fn transport_wired_always_valid() {
        let cli = parse(&["--transport", "wired", "status"]);
        assert_eq!(cli.transport, TransportArg::Wired);
    }

    /// `--transport receiver` is always valid regardless of features.
    #[test]
    fn transport_receiver_always_valid() {
        let cli = parse(&["--transport", "receiver", "status"]);
        assert_eq!(cli.transport, TransportArg::Receiver);
    }
}
