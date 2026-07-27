use std::process::ExitCode;
#[cfg(feature = "usb")]
use std::time::Duration;

#[cfg(any(feature = "usb", feature = "ble", test))]
use attack_shark_x3::ButtonAssignment;
#[cfg(any(feature = "usb", feature = "ble"))]
use attack_shark_x3::{ButtonsState, PreferencesState};
use attack_shark_x3::{
    DpiReport, DpiState, DpiValue, PollingRate, ProfileControlFraming, ProfileControlReport,
    ProfileId, ReadSelector, ReadbackRequest, SensorOptions, StageIndex, TransportKind,
    protocol::dpi::LiftOffDistance,
};
use clap::{Args, Parser, Subcommand, ValueEnum};

#[cfg(feature = "usb")]
use attack_shark_x3::{
    DeviceSelector, MouseHandle, ProfileSnapshot, UsbDeviceKind, list_devices_for,
};

#[cfg(feature = "ble")]
use attack_shark_x3::{BleHandle, BleReport, BleSelector};

#[derive(Debug, Parser)]
#[command(name = "attack-shark-x3", about = "Attack Shark X3/M600 protocol CLI")]
struct Cli {
    /// Exact USB HID path, or BLE device name when `--transport ble` is used.
    #[arg(long, global = true)]
    device: Option<String>,

    /// Hardware transport for commands that access a device.
    #[arg(long, global = true, value_enum, default_value = "wired")]
    transport: TransportArgument,

    #[command(subcommand)]
    command: Command,
}
#[derive(Debug, Subcommand)]
enum Command {
    /// Discover matching X3 wired or 2.4 GHz receiver collections without opening them.
    List,
    /// Read the current battery percentage from the 2.4 GHz receiver.
    Battery,
    /// Discover already-connected BLE configuration devices without pairing.
    #[cfg(feature = "ble")]
    BleList,
    /// Read validated state. Targeted section reads can change live working buffers.
    Read {
        #[command(subcommand)]
        command: ReadCommand,
    },
    /// Update DPI fields; BLE replaces omitted fields and unresolved bytes with defaults.
    SetDpi(SetDpiArgs),
    /// Update preference fields; BLE replaces omitted fields with zero/default values.
    SetPreferences(SetPreferencesArgs),
    /// Set the global polling rate and verify USB readback; BLE reports ACK acceptance.
    SetRate(SetRateArgs),
    /// Change one button; USB preserves the table, while BLE uses defaults for other slots.
    SetButton(SetButtonArgs),
    /// Change the enabled maximum; BLE defaults the current profile to 1.
    SetMaxProfile(MaximumProfileArgs),
    /// Activate a profile; BLE defaults the maximum profile to 5.
    ActivateProfile(ProfileArg),
    /// Build packets without opening hardware.
    Hex {
        #[command(subcommand)]
        command: HexCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ReadCommand {
    /// Read persistent profile metadata (current and maximum profile).
    Metadata,
    /// Read the global polling rate.
    Rate,
    /// Read the target profile's working DPI state.
    Dpi(ProfileArg),
    /// Read the target profile's working preferences.
    Preferences(ProfileArg),
    /// Read the target profile's complete raw button table.
    Buttons(ProfileArg),
    /// Read persistent metadata and every target working-profile section.
    Profile(ProfileArg),
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
    Ble,
}

impl From<TransportArgument> for TransportKind {
    fn from(value: TransportArgument) -> Self {
        match value {
            TransportArgument::Wired => Self::Wired,
            TransportArgument::Receiver => Self::Receiver,
            TransportArgument::Ble => Self::Ble,
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
enum ToggleArgument {
    On,
    Off,
}

impl From<ToggleArgument> for bool {
    fn from(value: ToggleArgument) -> Self {
        match value {
            ToggleArgument::On => true,
            ToggleArgument::Off => false,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum LightModeArgument {
    Off,
    Static,
    Breathing,
    Neon,
    ColorBreathing,
    StaticDpi,
    BreathingDpi,
}

impl From<LightModeArgument> for u8 {
    fn from(value: LightModeArgument) -> Self {
        match value {
            LightModeArgument::Off => 0x00,
            LightModeArgument::Static => 0x10,
            LightModeArgument::Breathing => 0x20,
            LightModeArgument::Neon => 0x30,
            LightModeArgument::ColorBreathing => 0x40,
            LightModeArgument::StaticDpi => 0x50,
            LightModeArgument::BreathingDpi => 0x60,
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
#[derive(Clone, Copy, Debug, Args)]
struct MaximumProfileArgs {
    #[arg(long)]
    maximum: u8,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ReadReportArgument {
    Version,
    ProfileMetadata,
    PollingRate,
    Dpi,
    Preferences,
    Buttons,
}

impl ReadReportArgument {
    const fn name(self) -> &'static str {
        match self {
            Self::Version => "version",
            Self::ProfileMetadata => "profile-metadata",
            Self::PollingRate => "polling-rate",
            Self::Dpi => "dpi",
            Self::Preferences => "preferences",
            Self::Buttons => "buttons",
        }
    }
}

#[derive(Clone, Copy, Debug, Args)]
struct SetRateArgs {
    /// Polling rate in hertz: 125, 250, 500, or 1000.
    #[arg(long)]
    rate: u16,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ButtonArgument {
    Left,
    Right,
    Middle,
    Forward,
    Backward,
}

#[cfg(any(feature = "usb", feature = "ble", test))]
impl ButtonArgument {
    const fn slot(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
            Self::Middle => 2,
            Self::Forward => 6,
            Self::Backward => 7,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ButtonActionArgument {
    Disable,
    LeftClick,
    RightClick,
    MiddleClick,
    Forward,
    Backward,
    DoubleClick,
    ProfileCycle,
    ProfilePlus,
    ProfileMinus,
}

#[cfg(any(feature = "usb", feature = "ble", test))]
impl ButtonActionArgument {
    const fn assignment(self) -> ButtonAssignment {
        let action = match self {
            Self::Disable => 0x01,
            Self::LeftClick => 0x02,
            Self::RightClick => 0x03,
            Self::MiddleClick => 0x04,
            Self::Backward => 0x05,
            Self::Forward => 0x06,
            Self::DoubleClick => 0x07,
            Self::ProfileCycle => 0x34,
            Self::ProfilePlus => 0x35,
            Self::ProfileMinus => 0x36,
        };
        ButtonAssignment::new(action, 0, 0)
    }
}

#[derive(Clone, Copy, Debug, Args)]
struct SetButtonArgs {
    #[arg(long)]
    profile: u8,

    #[arg(long, value_enum)]
    button: ButtonArgument,

    #[arg(long, value_enum)]
    action: ButtonActionArgument,
}

#[derive(Clone, Copy, Debug, Args)]
struct ProfileArg {
    #[arg(long)]
    profile: u8,
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

#[derive(Debug, Args)]
struct SetDpiArgs {
    #[arg(long)]
    profile: u8,

    #[arg(long, value_delimiter = ',')]
    stages: Option<Vec<u16>>,

    #[arg(long)]
    active: Option<u8>,

    #[arg(long, value_enum)]
    lod: Option<LiftOffDistanceArgument>,

    #[arg(long, value_enum)]
    ripple_control: Option<ToggleArgument>,

    #[arg(long, value_enum)]
    angle_snap: Option<ToggleArgument>,

    #[arg(long, value_enum)]
    motion_sync: Option<ToggleArgument>,
}

#[derive(Debug, Args)]
struct SetPreferencesArgs {
    #[arg(long)]
    profile: u8,

    #[arg(long, value_enum)]
    light_mode: Option<LightModeArgument>,

    /// Opaque raw light-mode byte, such as `0x70`; mutually exclusive with `--light-mode`.
    #[arg(long, value_name = "BYTE")]
    light_mode_raw: Option<String>,

    /// LED animation speed from 1 (slowest) to 5 (fastest).
    #[arg(long)]
    led_speed: Option<u8>,

    /// Deep-sleep timeout in whole minutes (1..=60).
    #[arg(long)]
    deep_sleep_minutes: Option<u8>,

    /// Normal sleep timeout in half-minute steps (0.5..=30).
    #[arg(long)]
    sleep_minutes: Option<f32>,

    /// Button debounce in even milliseconds (4..=50).
    #[arg(long)]
    debounce_ms: Option<u8>,
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
    let Cli {
        device,
        transport,
        command,
    } = cli;
    match command {
        Command::Hex { command } => match command {
            HexCommand::Dpi(arguments) => print_dpi(arguments),
            HexCommand::ProfileControl(arguments) => print_profile_control(arguments),
            HexCommand::ReadSelector(arguments) => print_read_selector(arguments),
        },
        Command::List => {
            #[cfg(feature = "ble")]
            if matches!(transport, TransportArgument::Ble) {
                return run_ble_list();
            }
            run_list(transport)
        }
        Command::Battery => run_hardware(device, transport, HardwareCommand::Battery),
        #[cfg(feature = "ble")]
        Command::BleList => run_ble_list(),
        Command::Read { command } => {
            validate_read_command(&command)?;
            run_hardware(device, transport, HardwareCommand::Read(command))
        }
        Command::SetDpi(arguments) => {
            parse_profile(arguments.profile)?;
            require_set_dpi_field(&arguments)?;
            run_hardware(device, transport, HardwareCommand::SetDpi(arguments))
        }
        Command::SetPreferences(arguments) => {
            parse_profile(arguments.profile)?;
            require_set_preferences_field(&arguments)?;
            run_hardware(
                device,
                transport,
                HardwareCommand::SetPreferences(arguments),
            )
        }
        Command::SetRate(arguments) => {
            let rate = parse_polling_rate(arguments.rate)?;
            run_hardware(device, transport, HardwareCommand::SetRate(rate))
        }
        Command::SetButton(arguments) => {
            parse_profile(arguments.profile)?;
            run_hardware(device, transport, HardwareCommand::SetButton(arguments))
        }
        Command::ActivateProfile(arguments) => {
            parse_profile(arguments.profile)?;
            run_hardware(
                device,
                transport,
                HardwareCommand::ActivateProfile(arguments),
            )
        }
        Command::SetMaxProfile(arguments) => {
            let maximum = parse_profile(arguments.maximum)?;
            run_hardware(device, transport, HardwareCommand::SetMaxProfile(maximum))
        }
    }
}

#[cfg_attr(not(feature = "usb"), allow(dead_code))]
#[derive(Debug)]
enum HardwareCommand {
    Read(ReadCommand),
    Battery,
    SetDpi(SetDpiArgs),
    SetButton(SetButtonArgs),
    SetPreferences(SetPreferencesArgs),
    ActivateProfile(ProfileArg),
    SetMaxProfile(ProfileId),
    SetRate(PollingRate),
}

#[cfg(feature = "usb")]
fn run_list(transport: TransportArgument) -> Result<(), String> {
    let kind = match transport {
        TransportArgument::Wired => UsbDeviceKind::Wired,
        TransportArgument::Receiver => UsbDeviceKind::Receiver,
        TransportArgument::Ble => {
            return Err("BLE device listing requires the ble feature".to_owned());
        }
    };
    let devices = list_devices_for(kind).map_err(|error| error.to_string())?;
    if devices.is_empty() {
        println!("no matching X3 USB configuration collections found");
        return Ok(());
    }
    for (index, device) in devices.iter().enumerate() {
        println!("device[{index}]");
        println!("  path: {}", device.path);
        println!("  vendor_id: 0x{:04x}", device.vendor_id);
        println!("  product_id: 0x{:04x}", device.product_id);
        println!("  interface_number: {}", device.interface_number);
        println!("  product: {}", optional_value(device.product.as_deref()));
        println!(
            "  serial_number: {}",
            optional_value(device.serial_number.as_deref())
        );
    }
    Ok(())
}

#[cfg(not(feature = "usb"))]
fn run_list(_transport: TransportArgument) -> Result<(), String> {
    Err("hardware commands require the usb feature".to_owned())
}

#[cfg(feature = "ble")]
fn run_ble_list() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not create async runtime: {error}"))?;
    runtime.block_on(async {
        let devices = BleHandle::list_connected()
            .await
            .map_err(|error| error.to_string())?;
        if devices.is_empty() {
            println!("no connected BLE FEE0 devices found");
            return Ok(());
        }
        for (index, device) in devices.iter().enumerate() {
            println!("device[{index}]");
            println!("  id: {}", device.id);
            println!("  name: {}", device.name.as_deref().unwrap_or("<unknown>"));
            println!("  connected: {}", device.connected);
        }
        Ok(())
    })
}

#[cfg(any(feature = "usb", feature = "ble"))]
fn run_hardware(
    device: Option<String>,
    transport: TransportArgument,
    command: HardwareCommand,
) -> Result<(), String> {
    match transport {
        TransportArgument::Ble => run_ble_transport(device, command),
        TransportArgument::Wired | TransportArgument::Receiver => {
            run_usb_transport(device, transport, command)
        }
    }
}

#[cfg(feature = "usb")]
fn run_usb_transport(
    device: Option<String>,
    transport: TransportArgument,
    command: HardwareCommand,
) -> Result<(), String> {
    let kind = match transport {
        TransportArgument::Wired => UsbDeviceKind::Wired,
        TransportArgument::Receiver => UsbDeviceKind::Receiver,
        TransportArgument::Ble => {
            return Err("BLE commands require the ble feature".to_owned());
        }
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not create async runtime: {error}"))?;
    runtime.block_on(async move {
        let selector = device.map_or(DeviceSelector::Unique, DeviceSelector::path);
        let handle = MouseHandle::open_for_kind(selector, kind)
            .map_err(|error| error.to_string())?;
        match command {
            HardwareCommand::Read(command) => run_read(&handle, command).await,
            HardwareCommand::SetDpi(arguments) => run_set_dpi(&handle, arguments).await,
            HardwareCommand::SetPreferences(arguments) => {
                run_set_preferences(&handle, arguments).await
            }
            HardwareCommand::Battery => {
                let level = handle
                    .read_battery(Duration::from_secs(3))
                    .await
                    .map_err(|error| error.to_string())?;
                println!("{level}%");
                Ok(())
            }
            HardwareCommand::SetRate(rate) => run_set_rate(&handle, rate).await,
            HardwareCommand::SetButton(arguments) => run_set_button(&handle, arguments).await,
            HardwareCommand::ActivateProfile(arguments) => {
                let profile = parse_profile(arguments.profile)?;
                let metadata = handle
                    .activate_profile(profile)
                    .await
                    .map_err(|error| error.to_string())?;
                println!("verified immediate profile metadata (not persistence proof):");
                print_metadata(metadata);
                println!(
                    "warning: verified readback is immediate state only; it does not prove persistence"
                );
                Ok(())
            }
            HardwareCommand::SetMaxProfile(maximum) => {
                let metadata = handle
                    .set_maximum_profile(maximum)
                    .await
                    .map_err(|error| error.to_string())?;
                println!("verified immediate maximum-profile metadata (not persistence proof):");
                print_metadata(metadata);
                println!(
                    "warning: profiles above the new maximum are no longer selectable; \
                     readback is immediate state only and does not prove persistence"
                );
                Ok(())
            }
        }
    })
}

#[cfg(not(feature = "usb"))]
#[allow(dead_code)]
fn run_usb_transport(
    _device: Option<String>,
    _transport: TransportArgument,
    _command: HardwareCommand,
) -> Result<(), String> {
    Err("USB hardware commands require the usb feature".to_owned())
}

#[cfg(feature = "ble")]
fn run_ble_transport(device: Option<String>, command: HardwareCommand) -> Result<(), String> {
    let report = match command {
        HardwareCommand::Read(_) => {
            return Err(
                "BLE configuration readback is unavailable; use a USB transport for read \
                 commands"
                    .to_owned(),
            );
        }
        HardwareCommand::Battery => {
            return Err("battery telemetry requires the USB receiver transport".to_owned());
        }
        HardwareCommand::SetDpi(arguments) => {
            eprintln!(
                "warning: BLE has no configuration readback; set-dpi will replace unspecified \
                 fields and unresolved DPI bytes with CLI defaults"
            );
            BleReport::Dpi(build_ble_dpi(arguments)?)
        }
        HardwareCommand::SetPreferences(arguments) => {
            eprintln!(
                "warning: BLE has no configuration readback; set-preferences will replace \
                 unspecified fields with zero/default values"
            );
            BleReport::Preferences(build_ble_preferences(arguments)?)
        }
        HardwareCommand::SetRate(rate) => BleReport::PollingRate(rate),
        HardwareCommand::SetButton(arguments) => {
            eprintln!(
                "warning: BLE has no configuration readback; set-button will replace the \
                 complete table with default assignments except for the selected button"
            );
            BleReport::Buttons(build_ble_buttons(arguments)?)
        }
        HardwareCommand::ActivateProfile(arguments) => {
            let profile = parse_profile(arguments.profile)?;
            let maximum = ProfileId::new(ProfileId::MAX).expect("profile maximum is valid");
            eprintln!(
                "warning: BLE cannot read the current maximum profile; activate-profile will \
                 use the default maximum profile {maximum}"
            );
            let metadata = attack_shark_x3::ProfileMetadata::new(profile, maximum)
                .map_err(|error| error.to_string())?;
            BleReport::ProfileControl(metadata)
        }
        HardwareCommand::SetMaxProfile(maximum) => {
            let current = ProfileId::new(ProfileId::MIN).expect("profile minimum is valid");
            eprintln!(
                "warning: BLE cannot read the current profile; set-max-profile will use the \
                 default current profile {current}"
            );
            let metadata = attack_shark_x3::ProfileMetadata::new(current, maximum)
                .map_err(|error| error.to_string())?;
            BleReport::ProfileControl(metadata)
        }
    };
    let selector = device.map_or(BleSelector::UniqueConnected, BleSelector::Name);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not create async runtime: {error}"))?;
    runtime.block_on(async move {
        let handle = BleHandle::open(selector)
            .await
            .map_err(|error| error.to_string())?;
        let receipt = handle
            .write(report)
            .await
            .map_err(|error| error.to_string())?;
        println!(
            "accepted BLE report 0x{:02x} with ACK status 0x{:02x}; \
             this command did not verify persistence or effective device state",
            receipt.report_id, receipt.ack_status
        );
        Ok(())
    })
}

#[cfg(feature = "ble")]
fn build_ble_dpi(arguments: SetDpiArgs) -> Result<DpiState, String> {
    const DEFAULT_STAGES: [u16; 6] = [800, 1600, 2400, 3200, 5000, 26000];
    let profile = parse_profile(arguments.profile)?;
    let stages = arguments
        .stages
        .unwrap_or_else(|| DEFAULT_STAGES.to_vec())
        .into_iter()
        .map(DpiValue::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let active =
        StageIndex::try_from(arguments.active.unwrap_or(2)).map_err(|error| error.to_string())?;
    let mut state =
        DpiState::new(profile, stages, active, [0; 25]).map_err(|error| error.to_string())?;
    if let Some(lod) = arguments.lod {
        state.sensor.lift_off_distance = lod.into();
    }
    if let Some(value) = arguments.ripple_control {
        state.sensor.ripple_control = value.into();
    }
    if let Some(value) = arguments.angle_snap {
        state.sensor.angle_snap = value.into();
    }
    if let Some(value) = arguments.motion_sync {
        state.sensor.motion_sync = value.into();
    }
    Ok(state)
}

#[cfg(feature = "ble")]
fn build_ble_preferences(arguments: SetPreferencesArgs) -> Result<PreferencesState, String> {
    let profile = parse_profile(arguments.profile)?;
    let mut state = PreferencesState::new(profile, 0, 0, 0, [0, 0, 0], 0, 0);
    if let Some(value) = arguments.light_mode {
        state.light_mode = value.into();
    }
    if let Some(value) = arguments.light_mode_raw {
        state.light_mode = parse_raw_byte(&value)?;
    }
    if let Some(minutes) = arguments.deep_sleep_minutes {
        let bucket = (minutes - 1) / 16;
        state.configuration = (bucket << 4) | (state.configuration & 0x0f);
        state.deep_sleep = (0x08_u16 + u16::from(minutes) * 0x10).to_le_bytes()[0];
    }
    if let Some(speed) = arguments.led_speed {
        state.configuration = (state.configuration & 0xf0) | (6 - speed);
    }
    if let Some(mut minutes) = arguments.sleep_minutes {
        state.sleep_timer = 0;
        while minutes >= 0.5 {
            state.sleep_timer += 1;
            minutes -= 0.5;
        }
    }
    if let Some(milliseconds) = arguments.debounce_ms {
        state.debounce = (milliseconds - 4) / 2 + 2;
    }
    Ok(state)
}

#[cfg(feature = "ble")]
fn build_ble_buttons(arguments: SetButtonArgs) -> Result<ButtonsState, String> {
    let profile = parse_profile(arguments.profile)?;
    let mut state = ButtonsState::new(
        profile,
        [ButtonAssignment::default(); attack_shark_x3::protocol::buttons::BUTTON_SLOT_COUNT],
    );
    state.slots[arguments.button.slot()] = arguments.action.assignment();
    Ok(state)
}

#[cfg(not(feature = "ble"))]
#[allow(dead_code)]
fn run_ble_transport(_device: Option<String>, _command: HardwareCommand) -> Result<(), String> {
    Err("BLE hardware commands require the ble feature".to_owned())
}

#[cfg(not(any(feature = "usb", feature = "ble")))]
#[allow(clippy::needless_pass_by_value)]
fn run_hardware(
    _device: Option<String>,
    _transport: TransportArgument,
    _command: HardwareCommand,
) -> Result<(), String> {
    Err("hardware commands require the usb or ble feature".to_owned())
}

#[cfg(feature = "usb")]
async fn run_read(handle: &MouseHandle, command: ReadCommand) -> Result<(), String> {
    if !matches!(&command, ReadCommand::Metadata | ReadCommand::Rate) {
        eprintln!(
            "warning: this targeted read can load profile working buffers and change live mouse behavior without changing persistent metadata"
        );
    }
    match command {
        ReadCommand::Metadata => {
            let metadata = handle
                .read_profile_metadata()
                .await
                .map_err(|error| error.to_string())?;
            print_metadata(metadata);
        }
        ReadCommand::Rate => {
            let rate = handle
                .read_polling_rate()
                .await
                .map_err(|error| error.to_string())?;
            print_polling_rate(rate);
        }
        ReadCommand::Dpi(arguments) => {
            let profile = parse_profile(arguments.profile)?;
            let state = handle
                .read_dpi(profile)
                .await
                .map_err(|error| error.to_string())?;
            print_dpi_state(&state);
        }
        ReadCommand::Preferences(arguments) => {
            let profile = parse_profile(arguments.profile)?;
            let state = handle
                .read_preferences(profile)
                .await
                .map_err(|error| error.to_string())?;
            print_preferences_state(&state);
        }
        ReadCommand::Buttons(arguments) => {
            let profile = parse_profile(arguments.profile)?;
            let state = handle
                .read_buttons(profile)
                .await
                .map_err(|error| error.to_string())?;
            print_buttons_state(&state);
        }
        ReadCommand::Profile(arguments) => {
            let profile = parse_profile(arguments.profile)?;
            let snapshot = handle
                .read_profile(profile)
                .await
                .map_err(|error| error.to_string())?;
            print_profile_snapshot(&snapshot);
        }
    }
    Ok(())
}

#[cfg(feature = "usb")]
async fn run_set_dpi(handle: &MouseHandle, arguments: SetDpiArgs) -> Result<(), String> {
    let profile = parse_profile(arguments.profile)?;
    let mut state = handle
        .read_dpi(profile)
        .await
        .map_err(|error| error.to_string())?;
    if let Some(stages) = arguments.stages {
        state.stages = stages
            .into_iter()
            .map(DpiValue::try_from)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
    }
    if let Some(active) = arguments.active {
        state.active_stage = StageIndex::try_from(active).map_err(|error| error.to_string())?;
    }
    if let Some(lod) = arguments.lod {
        state.sensor.lift_off_distance = lod.into();
    }
    if let Some(value) = arguments.ripple_control {
        state.sensor.ripple_control = value.into();
    }
    if let Some(value) = arguments.angle_snap {
        state.sensor.angle_snap = value.into();
    }
    if let Some(value) = arguments.motion_sync {
        state.sensor.motion_sync = value.into();
    }
    let verified = handle
        .write_dpi(state)
        .await
        .map_err(|error| error.to_string())?;
    println!("verified immediate DPI readback (not persistence proof):");
    print_dpi_state(&verified);
    println!("warning: verified readback is immediate state only; it does not prove persistence");
    Ok(())
}

#[cfg(feature = "usb")]
async fn run_set_preferences(
    handle: &MouseHandle,
    arguments: SetPreferencesArgs,
) -> Result<(), String> {
    let profile = parse_profile(arguments.profile)?;
    let current = handle
        .read_preferences(profile)
        .await
        .map_err(|error| error.to_string())?;
    let mut state = PreferencesState { profile, ..current };
    if let Some(value) = arguments.light_mode {
        state.light_mode = value.into();
    }
    if let Some(value) = arguments.light_mode_raw {
        state.light_mode = parse_raw_byte(&value)?;
    }
    if let Some(minutes) = arguments.deep_sleep_minutes {
        let bucket = (minutes - 1) / 16;
        state.configuration = (bucket << 4) | (state.configuration & 0x0f);
        state.deep_sleep = (0x08_u16 + u16::from(minutes) * 0x10).to_le_bytes()[0];
    }
    if let Some(speed) = arguments.led_speed {
        state.configuration = (state.configuration & 0xf0) | (6 - speed);
    }
    if let Some(mut minutes) = arguments.sleep_minutes {
        state.sleep_timer = 0;
        while minutes >= 0.5 {
            state.sleep_timer += 1;
            minutes -= 0.5;
        }
    }
    if let Some(milliseconds) = arguments.debounce_ms {
        state.debounce = (milliseconds - 4) / 2 + 2;
    }
    let verified = handle
        .write_preferences(state)
        .await
        .map_err(|error| error.to_string())?;
    println!("verified immediate preferences readback (not persistence proof):");
    print_preferences_state(&verified);
    println!("warning: verified readback is immediate state only; it does not prove persistence");
    Ok(())
}

#[cfg(feature = "usb")]
async fn run_set_button(handle: &MouseHandle, arguments: SetButtonArgs) -> Result<(), String> {
    let profile = parse_profile(arguments.profile)?;
    let mut state = handle
        .read_buttons(profile)
        .await
        .map_err(|error| error.to_string())?;
    state.slots[arguments.button.slot()] = arguments.action.assignment();
    let verified = handle
        .write_buttons(state)
        .await
        .map_err(|error| error.to_string())?;
    println!("verified immediate button-table readback (not persistence proof):");
    print_buttons_state(&verified);
    println!("warning: verified readback is immediate state only; it does not prove persistence");
    Ok(())
}

#[cfg(feature = "usb")]
async fn run_set_rate(handle: &MouseHandle, rate: PollingRate) -> Result<(), String> {
    let verified = handle
        .write_polling_rate(rate)
        .await
        .map_err(|error| error.to_string())?;
    println!("verified immediate polling-rate readback (not persistence proof):");
    print_polling_rate(verified);
    println!("warning: verified readback is immediate state only; it does not prove persistence");
    Ok(())
}

#[cfg(feature = "usb")]
fn print_metadata(metadata: attack_shark_x3::ProfileMetadata) {
    println!("persistent_metadata:");
    println!("  current_profile: {}", metadata.current());
    println!("  maximum_profile: {}", metadata.maximum());
}

#[cfg(feature = "usb")]
fn print_polling_rate(rate: PollingRate) {
    println!("polling_rate:");
    println!("  hz: {}", rate.hz());
    println!("  code: 0x{:02x}", rate.code());
}

#[cfg(feature = "usb")]
fn print_profile_snapshot(snapshot: &ProfileSnapshot) {
    print_metadata(snapshot.persistent_metadata);
    println!("target_working_profile: {}", snapshot.target_profile);
    print_dpi_state(&snapshot.dpi);
    print_preferences_state(&snapshot.preferences);
    print_buttons_state(&snapshot.buttons);
}

#[cfg(feature = "usb")]
fn print_dpi_state(state: &DpiState) {
    println!("dpi:");
    println!("  profile: {}", state.profile);
    println!(
        "  stages: {}",
        state
            .stages
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",")
    );
    println!("  active_stage: {}", state.active_stage);
    println!(
        "  lift_off_distance: {}",
        lift_off_name(state.sensor.lift_off_distance)
    );
    println!("  ripple_control: {}", state.sensor.ripple_control);
    println!("  angle_snap: {}", state.sensor.angle_snap);
    println!("  motion_sync: {}", state.sensor.motion_sync);
    println!("  preserved_tail: {}", to_hex(&state.preserved_tail));
}

#[cfg(feature = "usb")]
fn print_preferences_state(state: &PreferencesState) {
    println!("preferences:");
    println!("  profile: {}", state.profile);
    println!(
        "  light_mode: 0x{:02x} ({})",
        state.light_mode, state.light_mode
    );
    println!(
        "  configuration: 0x{:02x} ({})",
        state.configuration, state.configuration
    );
    println!(
        "  deep_sleep: 0x{:02x} ({})",
        state.deep_sleep, state.deep_sleep
    );
    println!(
        "  host_color: {:02x},{:02x},{:02x}",
        state.host_color[0], state.host_color[1], state.host_color[2]
    );
    println!(
        "  sleep_timer: 0x{:02x} ({})",
        state.sleep_timer, state.sleep_timer
    );
    println!("  debounce: 0x{:02x} ({})", state.debounce, state.debounce);
}

#[cfg(feature = "usb")]
fn print_buttons_state(state: &ButtonsState) {
    println!("buttons:");
    println!("  profile: {}", state.profile);
    for (index, slot) in state.slots.iter().enumerate() {
        println!(
            "  slot_{:02}: action=0x{:02x} modifier=0x{:02x} key_code=0x{:02x}",
            index + 1,
            slot.action,
            slot.modifier,
            slot.key_code
        );
    }
}

#[cfg(feature = "usb")]
fn lift_off_name(value: LiftOffDistance) -> &'static str {
    match value {
        LiftOffDistance::OneMillimeter => "one",
        LiftOffDistance::TwoMillimeters => "two",
    }
}

fn validate_read_command(command: &ReadCommand) -> Result<(), String> {
    match command {
        ReadCommand::Metadata | ReadCommand::Rate => Ok(()),
        ReadCommand::Dpi(arguments)
        | ReadCommand::Preferences(arguments)
        | ReadCommand::Buttons(arguments)
        | ReadCommand::Profile(arguments) => parse_profile(arguments.profile).map(|_| ()),
    }
}

fn require_set_dpi_field(arguments: &SetDpiArgs) -> Result<(), String> {
    if arguments.stages.is_none()
        && arguments.active.is_none()
        && arguments.lod.is_none()
        && arguments.ripple_control.is_none()
        && arguments.angle_snap.is_none()
        && arguments.motion_sync.is_none()
    {
        return Err("set-dpi requires at least one field".to_owned());
    }
    if let Some(stages) = &arguments.stages {
        if !(1..=8).contains(&stages.len()) {
            return Err("--stages requires 1 to 8 values".to_owned());
        }
        for value in stages {
            DpiValue::try_from(*value).map_err(|error| error.to_string())?;
        }
    }
    if let Some(active) = arguments.active {
        StageIndex::try_from(active).map_err(|error| error.to_string())?;
        if arguments
            .stages
            .as_ref()
            .is_some_and(|stages| usize::from(active) > stages.len())
        {
            return Err("--active cannot exceed the supplied stage count".to_owned());
        }
    }
    Ok(())
}

fn require_set_preferences_field(arguments: &SetPreferencesArgs) -> Result<(), String> {
    if arguments.light_mode.is_none()
        && arguments.light_mode_raw.is_none()
        && arguments.led_speed.is_none()
        && arguments.deep_sleep_minutes.is_none()
        && arguments.sleep_minutes.is_none()
        && arguments.debounce_ms.is_none()
    {
        return Err("set-preferences requires at least one field".to_owned());
    }
    if arguments.light_mode.is_some() && arguments.light_mode_raw.is_some() {
        return Err("--light-mode and --light-mode-raw are mutually exclusive".to_owned());
    }
    if let Some(value) = &arguments.light_mode_raw {
        parse_raw_byte(value)?;
    }
    if let Some(value) = arguments.led_speed
        && !(1..=5).contains(&value)
    {
        return Err("--led-speed must be between 1 and 5".to_owned());
    }
    if let Some(value) = arguments.deep_sleep_minutes
        && !(1..=60).contains(&value)
    {
        return Err("--deep-sleep-minutes must be between 1 and 60".to_owned());
    }
    if let Some(value) = arguments.sleep_minutes
        && (!value.is_finite() || !(0.5..=30.0).contains(&value) || (value * 2.0).fract() != 0.0)
    {
        return Err("--sleep-minutes must be 0.5 to 30 in half-minute steps".to_owned());
    }
    if let Some(value) = arguments.debounce_ms
        && (!(4..=50).contains(&value) || value % 2 != 0)
    {
        return Err("--debounce-ms must be an even value from 4 to 50".to_owned());
    }
    Ok(())
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
    let metadata = attack_shark_x3::ProfileMetadata::new(current, maximum)
        .map_err(|error| error.to_string())?;
    let report = ProfileControlReport::encode(metadata, arguments.framing.into());
    println!("{}", to_hex(report.as_bytes()));
    Ok(())
}

fn print_read_selector(arguments: ReadSelectorArgs) -> Result<(), String> {
    let request = match arguments.report {
        ReadReportArgument::Version
        | ReadReportArgument::ProfileMetadata
        | ReadReportArgument::PollingRate => {
            if arguments.profile.is_some() {
                return Err(format!(
                    "--profile is not valid for the {} report",
                    arguments.report.name()
                ));
            }
            match arguments.report {
                ReadReportArgument::Version => ReadbackRequest::Version,
                ReadReportArgument::ProfileMetadata => ReadbackRequest::ProfileMetadata,
                ReadReportArgument::PollingRate => ReadbackRequest::PollingRate,
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

fn parse_polling_rate(value: u16) -> Result<PollingRate, String> {
    PollingRate::try_from(value).map_err(|error| error.to_string())
}

fn parse_raw_byte(value: &str) -> Result<u8, String> {
    let (digits, radix) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .map_or((value, 10), |digits| (digits, 16));
    let parsed = u16::from_str_radix(digits, radix)
        .map_err(|_| format!("invalid raw byte {value:?}; expected 0x00..=0xff"))?;
    u8::try_from(parsed).map_err(|_| format!("invalid raw byte {value:?}; expected 0x00..=0xff"))
}

#[cfg(feature = "usb")]
fn optional_value(value: Option<&str>) -> &str {
    value.unwrap_or("<none>")
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

#[cfg(test)]
mod tests {
    use super::{Cli, Command, SetDpiArgs, SetPreferencesArgs};
    use clap::Parser;

    #[test]
    fn parses_explicit_toggle_and_comma_stages() {
        let cli = Cli::try_parse_from([
            "attack-shark-x3",
            "set-dpi",
            "--profile",
            "2",
            "--stages",
            "800,1600",
            "--ripple-control",
            "off",
        ])
        .expect("valid set-dpi command");
        let Command::SetDpi(arguments) = cli.command else {
            panic!("expected set-dpi");
        };
        assert_eq!(arguments.stages, Some(vec![800, 1600]));
        assert!(matches!(
            arguments.ripple_control,
            Some(super::ToggleArgument::Off)
        ));
    }

    #[test]
    fn parses_semantic_preference_fields() {
        let cli = Cli::try_parse_from([
            "attack-shark-x3",
            "set-preferences",
            "--profile",
            "3",
            "--light-mode",
            "breathing",
            "--led-speed",
            "5",
            "--deep-sleep-minutes",
            "60",
            "--sleep-minutes",
            "2.5",
            "--debounce-ms",
            "8",
        ])
        .expect("valid set-preferences command");
        let Command::SetPreferences(arguments) = cli.command else {
            panic!("expected set-preferences");
        };
        assert!(matches!(
            arguments.light_mode,
            Some(super::LightModeArgument::Breathing)
        ));
        assert_eq!(arguments.led_speed, Some(5));
        assert_eq!(arguments.deep_sleep_minutes, Some(60));
        assert_eq!(arguments.sleep_minutes, Some(2.5));
        assert_eq!(arguments.debounce_ms, Some(8));
        super::require_set_preferences_field(&arguments)
            .expect("documented semantic preference values must validate");
    }

    #[test]
    fn rejects_out_of_range_preference_fields_before_hardware() {
        let arguments = SetPreferencesArgs {
            profile: 1,
            light_mode: None,
            light_mode_raw: None,
            led_speed: Some(0),
            deep_sleep_minutes: None,
            sleep_minutes: None,
            debounce_ms: None,
        };
        assert!(super::require_set_preferences_field(&arguments).is_err());
    }

    #[test]
    fn parses_raw_light_mode_and_safe_button_action() {
        let cli = Cli::try_parse_from([
            "attack-shark-x3",
            "set-preferences",
            "--profile",
            "1",
            "--light-mode-raw",
            "0x70",
        ])
        .expect("valid raw light-mode command");
        let Command::SetPreferences(arguments) = cli.command else {
            panic!("expected set-preferences");
        };
        assert_eq!(
            super::parse_raw_byte(arguments.light_mode_raw.as_deref().unwrap()),
            Ok(0x70)
        );
        super::require_set_preferences_field(&arguments)
            .expect("raw light-mode value must validate");

        let cli = Cli::try_parse_from([
            "attack-shark-x3",
            "set-button",
            "--profile",
            "2",
            "--button",
            "forward",
            "--action",
            "profile-cycle",
        ])
        .expect("valid safe button command");
        let Command::SetButton(arguments) = cli.command else {
            panic!("expected set-button");
        };
        assert_eq!(arguments.button.slot(), 6);
        assert_eq!(arguments.action.assignment().action, 0x34);
    }

    #[test]
    fn rejects_out_of_range_raw_light_mode() {
        assert!(super::parse_raw_byte("0x100").is_err());
        assert!(super::parse_raw_byte("not-a-byte").is_err());
    }

    #[test]
    fn set_commands_require_a_field() {
        let dpi = SetDpiArgs {
            profile: 1,
            stages: None,
            active: None,
            lod: None,
            ripple_control: None,
            angle_snap: None,
            motion_sync: None,
        };
        assert!(super::require_set_dpi_field(&dpi).is_err());
        let preferences = SetPreferencesArgs {
            profile: 1,
            light_mode: None,
            light_mode_raw: None,
            led_speed: None,
            deep_sleep_minutes: None,
            sleep_minutes: None,
            debounce_ms: None,
        };
        assert!(super::require_set_preferences_field(&preferences).is_err());
    }

    #[test]
    fn parses_maximum_profile_command() {
        let cli = Cli::try_parse_from(["attack-shark-x3", "set-max-profile", "--maximum", "3"])
            .expect("valid maximum-profile command");
        let Command::SetMaxProfile(arguments) = cli.command else {
            panic!("expected set-max-profile");
        };
        assert_eq!(arguments.maximum, 3);
        assert_eq!(
            super::parse_profile(arguments.maximum)
                .expect("valid profile")
                .get(),
            3
        );
    }

    #[test]
    fn parses_polling_rate_read_and_write_commands() {
        let cli = Cli::try_parse_from(["attack-shark-x3", "read", "rate"])
            .expect("valid polling-rate read command");
        assert!(matches!(
            cli.command,
            Command::Read {
                command: super::ReadCommand::Rate
            }
        ));

        let cli = Cli::try_parse_from(["attack-shark-x3", "set-rate", "--rate", "1000"])
            .expect("valid polling-rate write command");
        let Command::SetRate(arguments) = cli.command else {
            panic!("expected set-rate");
        };
        assert_eq!(arguments.rate, 1000);
        assert_eq!(
            super::parse_polling_rate(arguments.rate)
                .expect("supported polling rate")
                .hz(),
            1000
        );
        assert!(super::parse_polling_rate(333).is_err());
    }
    #[test]
    fn parses_ble_transport_and_device_name() {
        let cli = Cli::try_parse_from([
            "attack-shark-x3",
            "--transport",
            "ble",
            "--device",
            "M600-5.2",
            "set-rate",
            "--rate",
            "500",
        ])
        .expect("valid BLE polling-rate command");
        assert!(matches!(cli.transport, super::TransportArgument::Ble));
        assert_eq!(cli.device.as_deref(), Some("M600-5.2"));
        assert!(matches!(cli.command, Command::SetRate(_)));
    }
    #[cfg(feature = "ble")]
    #[test]
    fn rejects_ble_reads_before_opening_device() {
        let error =
            super::run_ble_transport(None, super::HardwareCommand::Read(super::ReadCommand::Rate))
                .expect_err("BLE readback command must be rejected");
        assert!(error.contains("configuration readback is unavailable"));
    }

    #[cfg(feature = "ble")]
    #[test]
    fn builds_default_backed_ble_write_states() {
        let dpi = super::build_ble_dpi(super::SetDpiArgs {
            profile: 2,
            stages: None,
            active: None,
            lod: None,
            ripple_control: None,
            angle_snap: None,
            motion_sync: None,
        })
        .expect("default DPI state");
        assert_eq!(dpi.profile.get(), 2);
        assert_eq!(dpi.stages.len(), 6);
        assert_eq!(dpi.active_stage.get(), 2);
        assert_eq!(dpi.preserved_tail, [0; 25]);

        let preferences = super::build_ble_preferences(super::SetPreferencesArgs {
            profile: 2,
            light_mode: None,
            light_mode_raw: None,
            led_speed: None,
            deep_sleep_minutes: None,
            sleep_minutes: None,
            debounce_ms: None,
        })
        .expect("default preferences state");
        assert_eq!(preferences.profile.get(), 2);
        assert_eq!(preferences.light_mode, 0);
        assert_eq!(preferences.host_color, [0; 3]);

        let buttons = super::build_ble_buttons(super::SetButtonArgs {
            profile: 2,
            button: super::ButtonArgument::Forward,
            action: super::ButtonActionArgument::ProfileCycle,
        })
        .expect("default button state");
        assert_eq!(buttons.profile.get(), 2);
        assert_eq!(buttons.slots[6].action, 0x34);
        assert_eq!(buttons.slots[0], super::ButtonAssignment::default());
    }
}
