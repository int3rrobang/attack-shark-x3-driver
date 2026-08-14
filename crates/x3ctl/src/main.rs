#![forbid(unsafe_code)]

#[path = "x3ctl/args.rs"]
mod args;
#[path = "x3ctl/output.rs"]
mod output;

use std::path::PathBuf;
use std::process::ExitCode;

use attack_shark_x3_manager::{
    BaselineSource, ButtonAssignment, ButtonSlotDelta, ButtonsState, ConfigurationExport, DeviceId,
    DeviceManager, DeviceStatus, DpiDelta, DpiState, DpiValue, LiftOffDistance, PollingRate,
    PreferencesDelta, PreferencesFraming, PreferencesState, ProfileId, ResourceSnapshot,
    SafeButtonAction, SafeButtonSlot, SensorOptions, SensorOptionsDelta, StageIndex, StateStore,
    TransportKind, TransportSelection, UpdatePolicy, VerificationMethod, encode_debug_buttons,
    encode_debug_dpi, encode_debug_prefs,
};
use clap::Parser;
use serde::Serialize;

use args::{
    ActionArg, BaselineArg, BindCommand, BindSetArgs, Cli, Command, DebugCommand, DebugDpiArgs,
    DebugPrefsArgs, DpiCommand, DpiSetArgs, LodArg, PrefsCommand, PrefsSetArgs, ProfileCommand,
    ProfileSetArgs, RateCommand, RateSetArgs, SlotArg, StateCommand, TransportArg, ValidationArg,
    VerifyMethodArg,
};
use output::Output;

#[derive(Debug, Serialize)]
enum VerificationAction {
    ProfileReload,
    PowerCycle,
}

#[derive(Debug, Serialize)]
enum Action {
    Devices,
    Use {
        device: DeviceId,
    },
    Status,
    ProfileGet {
        profile: ProfileId,
    },
    ProfileSet {
        profile: ProfileId,
        maximum: Option<ProfileId>,
    },
    DpiGet {
        profile: ProfileId,
    },
    DpiSet {
        profile: ProfileId,
        delta: DpiDelta,
    },
    RateGet {
        profile: ProfileId,
    },
    RateSet {
        profile: ProfileId,
        rate: PollingRate,
    },
    /// Explicitly authorized BLE-only direct packet write: transport ACK only,
    /// no profile-image checks, persistence unknown.
    RateSetUnverifiedBle {
        profile: ProfileId,
        rate: PollingRate,
    },
    PrefsGet {
        profile: ProfileId,
    },
    PrefsSet {
        profile: ProfileId,
        delta: PreferencesDelta,
    },
    BindGet {
        profile: ProfileId,
    },
    BindSet {
        profile: ProfileId,
        delta: ButtonSlotDelta,
    },
    Battery,
    Verify {
        profile: ProfileId,
        method: VerificationAction,
    },
    Export,
    Import {
        file: PathBuf,
    },
    StateSelected,
    StateInvalidate,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let output = Output::new(cli.output);

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = output.error(&format!("failed to create async runtime: {error}"));
            return ExitCode::FAILURE;
        }
    };

    match runtime.block_on(run(&cli, &output)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = output.error(&error);
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: &Cli, output: &Output) -> Result<(), String> {
    let command = cli.command.as_ref().unwrap_or(&Command::Status);

    // Debug encoding is entirely offline. Do this before creating a state store
    // or manager so it cannot accidentally discover a device or open hardware.
    if let Command::Debug(command) = command {
        return run_debug(command, output);
    }

    let action = build_action(cli, command)?;
    if cli.dry_run {
        return output.print(
            format!(
                "dry-run: {} (no hardware or state access)",
                action_name(&action)
            ),
            &action,
        );
    }

    let store = if cli.stateless {
        StateStore::memory()
    } else {
        StateStore::with_default_paths().map_err(|error| error.to_string())?
    };
    let manager = DeviceManager::new(store).map_err(|error| error.to_string())?;
    dispatch(cli, &manager, action, output).await
}

fn build_action(cli: &Cli, command: &Command) -> Result<Action, String> {
    match command {
        Command::Devices => Ok(Action::Devices),
        Command::Use { device } => Ok(Action::Use {
            device: parse_device_id(device)?,
        }),
        Command::Status => Ok(Action::Status),
        Command::Profile(command) => match command {
            ProfileCommand::Get => Ok(Action::ProfileGet {
                profile: parse_profile(cli.profile)?,
            }),
            ProfileCommand::Set(ProfileSetArgs { profile, maximum }) => Ok(Action::ProfileSet {
                profile: parse_profile(profile.unwrap_or(cli.profile))?,
                maximum: maximum.map(parse_profile).transpose()?,
            }),
        },
        Command::Dpi(command) => match command {
            DpiCommand::Get => Ok(Action::DpiGet {
                profile: parse_profile(cli.profile)?,
            }),
            DpiCommand::Set(args) => build_dpi_action(cli, args),
        },
        Command::Rate(command) => match command {
            RateCommand::Get => Ok(Action::RateGet {
                profile: parse_profile(cli.profile)?,
            }),
            RateCommand::Set(RateSetArgs {
                hz,
                allow_unverified_ble_rate_write,
            }) => {
                let profile = parse_profile(cli.profile)?;
                let rate = PollingRate::new(*hz).ok_or_else(|| {
                    "polling rate must be one of 125, 250, 500, or 1000 Hz".to_owned()
                })?;
                if *allow_unverified_ble_rate_write {
                    Ok(Action::RateSetUnverifiedBle { profile, rate })
                } else {
                    Ok(Action::RateSet { profile, rate })
                }
            }
        },
        Command::Prefs(command) => match command {
            PrefsCommand::Get => Ok(Action::PrefsGet {
                profile: parse_profile(cli.profile)?,
            }),
            PrefsCommand::Set(args) => build_prefs_action(cli, args),
        },
        Command::Bind(command) => match command {
            BindCommand::Get => Ok(Action::BindGet {
                profile: parse_profile(cli.profile)?,
            }),
            BindCommand::Set(args) => build_bind_action(cli, args),
        },
        Command::Battery => Ok(Action::Battery),
        Command::Verify { method, profile } => Ok(Action::Verify {
            profile: parse_profile(profile.unwrap_or(cli.profile))?,
            method: match method {
                VerifyMethodArg::ProfileReload => VerificationAction::ProfileReload,
                VerifyMethodArg::PowerCycle => VerificationAction::PowerCycle,
            },
        }),
        Command::Export => Ok(Action::Export),
        Command::Import { file } => Ok(Action::Import {
            file: PathBuf::from(file),
        }),
        Command::State(command) => match command {
            StateCommand::Selected => Ok(Action::StateSelected),
            StateCommand::Invalidate => Ok(Action::StateInvalidate),
        },
        Command::Debug(_) => Err("debug commands are handled offline".into()),
    }
}

fn build_dpi_action(cli: &Cli, args: &DpiSetArgs) -> Result<Action, String> {
    let profile = parse_profile(cli.profile)?;
    let stages = parse_dpi_stages(&args.stages)?;
    let active_stage = args.active_stage.map(parse_stage).transpose()?;
    if let (Some(stages), Some(active)) = (stages.as_ref(), active_stage)
        && active.get() as usize > stages.len()
    {
        return Err(format!(
            "active stage {} exceeds the configured stage count {}",
            active,
            stages.len()
        ));
    }

    let sensor = if args.lod.is_some()
        || args.ripple_control.is_some()
        || args.angle_snap.is_some()
        || args.motion_sync.is_some()
    {
        Some(SensorOptionsDelta {
            lift_off_distance: args.lod.map(lod_value),
            ripple_control: args.ripple_control,
            angle_snap: args.angle_snap,
            motion_sync: args.motion_sync,
        })
    } else {
        None
    };

    let delta = DpiDelta {
        stages,
        active_stage,
        sensor,
    };
    if delta.is_empty() {
        return Err("DPI update must contain at least one field".into());
    }
    Ok(Action::DpiSet { profile, delta })
}

fn build_prefs_action(cli: &Cli, args: &PrefsSetArgs) -> Result<Action, String> {
    let profile = parse_profile(cli.profile)?;
    let delta = PreferencesDelta {
        light_mode: args.light_mode,
        configuration: args.configuration,
        deep_sleep: args.deep_sleep,
        host_color: args.host_color,
        sleep_timer: args.sleep_timer,
        debounce: args.debounce,
    };
    if delta.is_empty() {
        return Err("preferences update must contain at least one field".into());
    }
    Ok(Action::PrefsSet { profile, delta })
}

fn build_bind_action(cli: &Cli, args: &BindSetArgs) -> Result<Action, String> {
    let profile = parse_profile(cli.profile)?;
    let slot = match args.slot {
        SlotArg::Left => SafeButtonSlot::Left,
        SlotArg::Right => SafeButtonSlot::Right,
        SlotArg::Middle => SafeButtonSlot::Middle,
        SlotArg::Dpi => SafeButtonSlot::Dpi,
        SlotArg::Forward => SafeButtonSlot::Forward,
        SlotArg::Backward => SafeButtonSlot::Backward,
    };
    let action = match args.action {
        ActionArg::Disable => SafeButtonAction::Disable,
        ActionArg::LeftClick => SafeButtonAction::LeftClick,
        ActionArg::RightClick => SafeButtonAction::RightClick,
        ActionArg::MiddleClick => SafeButtonAction::MiddleClick,
        ActionArg::Backward => SafeButtonAction::Backward,
        ActionArg::Forward => SafeButtonAction::Forward,
        ActionArg::DoubleClick => SafeButtonAction::DoubleClick,
        ActionArg::DpiCycle => SafeButtonAction::DpiCycle,
        ActionArg::DpiPlus => SafeButtonAction::DpiPlus,
        ActionArg::DpiMinus => SafeButtonAction::DpiMinus,
        ActionArg::ProfileCycle => SafeButtonAction::ProfileCycle,
        ActionArg::ProfilePlus => SafeButtonAction::ProfilePlus,
        ActionArg::ProfileMinus => SafeButtonAction::ProfileMinus,
        ActionArg::FireButton => SafeButtonAction::FireButton,
        ActionArg::ScrollUp => SafeButtonAction::ScrollUp,
        ActionArg::ScrollDown => SafeButtonAction::ScrollDown,
        ActionArg::MediaPlayer => SafeButtonAction::MediaPlayer,
        ActionArg::PreviousTrack => SafeButtonAction::PreviousTrack,
        ActionArg::NextTrack => SafeButtonAction::NextTrack,
        ActionArg::PlayPause => SafeButtonAction::PlayPause,
        ActionArg::Stop => SafeButtonAction::Stop,
        ActionArg::Mute => SafeButtonAction::Mute,
        ActionArg::VolumeUp => SafeButtonAction::VolumeUp,
        ActionArg::VolumeDown => SafeButtonAction::VolumeDown,
        ActionArg::Calculator => SafeButtonAction::Calculator,
        ActionArg::Email => SafeButtonAction::Email,
        ActionArg::BrowserForward => SafeButtonAction::BrowserForward,
        ActionArg::BrowserBackward => SafeButtonAction::BrowserBackward,
        ActionArg::BrowserStop => SafeButtonAction::BrowserStop,
        ActionArg::MyComputer => SafeButtonAction::MyComputer,
        ActionArg::BrowserRefresh => SafeButtonAction::BrowserRefresh,
        ActionArg::BrowserHome => SafeButtonAction::BrowserHome,
        ActionArg::BrowserSearch => SafeButtonAction::BrowserSearch,
        ActionArg::BrowserFavorites => SafeButtonAction::BrowserFavorites,
        ActionArg::Cut => SafeButtonAction::Cut,
        ActionArg::Copy => SafeButtonAction::Copy,
        ActionArg::Paste => SafeButtonAction::Paste,
        ActionArg::Open => SafeButtonAction::Open,
        ActionArg::Save => SafeButtonAction::Save,
        ActionArg::Find => SafeButtonAction::Find,
        ActionArg::Redo => SafeButtonAction::Redo,
        ActionArg::SelectAll => SafeButtonAction::SelectAll,
        ActionArg::Print => SafeButtonAction::Print,
        ActionArg::CloseWindow => SafeButtonAction::CloseWindow,
        ActionArg::SwapWindows => SafeButtonAction::SwapWindows,
        ActionArg::ShowDesktop => SafeButtonAction::ShowDesktop,
        ActionArg::RunCommand => SafeButtonAction::RunCommand,
        ActionArg::LockPc => SafeButtonAction::LockPc,
        ActionArg::ScreenCapture => SafeButtonAction::ScreenCapture,
    };
    let delta = ButtonSlotDelta::new(slot, action);
    Ok(Action::BindSet { profile, delta })
}

async fn dispatch(
    cli: &Cli,
    manager: &DeviceManager,
    action: Action,
    output: &Output,
) -> Result<(), String> {
    let selection = transport_selection(cli.transport);
    match action {
        Action::Devices => {
            let devices = manager
                .list_devices(selection)
                .await
                .map_err(|error| error.to_string())?;
            for device in &devices {
                manager
                    .register_device(device.identity.clone())
                    .map_err(|error| error.to_string())?;
            }
            let human = if devices.is_empty() {
                "No X3 devices found.".to_owned()
            } else {
                devices
                    .iter()
                    .map(|device| {
                        format!(
                            "{} [{}] {}",
                            device.identity.id,
                            format_transport(device.identity.transport),
                            if device.connected {
                                "connected"
                            } else {
                                "disconnected"
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            output.print(human, &devices)
        }
        Action::Use { device } => {
            let resolved = manager
                .resolve_device(Some(&device), selection)
                .await
                .map_err(|error| error.to_string())?;
            manager
                .select_device(&resolved)
                .map_err(|error| error.to_string())?;
            output.print(format!("Selected device {resolved}"), &resolved)
        }
        Action::Status => {
            let device = resolve_hardware(manager, cli, selection).await?;
            let status = manager
                .read_status(&device)
                .await
                .map_err(|error| error.to_string())?;
            output.print(format_status_human(&status), &status)
        }
        Action::ProfileGet { profile } => {
            let device = resolve_hardware(manager, cli, selection).await?;
            let snapshot = manager
                .read_profile(&device, profile)
                .await
                .map_err(|error| error.to_string())?;
            output.print(format!("Profile {profile}"), &snapshot)
        }
        Action::ProfileSet { profile, maximum } => {
            let device = resolve_hardware(manager, cli, selection).await?;
            let metadata = if let Some(maximum) = maximum {
                manager
                    .set_profile_metadata(&device, profile, maximum)
                    .await
                    .map_err(|error| error.to_string())?
            } else {
                manager
                    .activate_profile(&device, profile)
                    .await
                    .map_err(|error| error.to_string())?
            };
            output.print(format!("Activated profile {profile}"), &metadata)
        }
        Action::DpiGet { profile } => {
            let device = resolve_hardware(manager, cli, selection).await?;
            let snapshot = manager
                .read_dpi(&device, profile)
                .await
                .map_err(|error| error.to_string())?;
            output.print(format_dpi_human(profile, &snapshot), &snapshot)
        }
        Action::DpiSet { profile, delta } => {
            let device = resolve_hardware(manager, cli, selection).await?;
            let outcome = manager
                .update_dpi_delta(
                    &device,
                    profile,
                    delta,
                    update_policy(cli.replace_defaults, cli.validation, cli.baseline),
                )
                .await
                .map_err(|error| error.to_string())?;
            output.print(format!("Updated DPI for profile {profile}"), &outcome)
        }
        Action::RateGet { profile } => {
            let device = resolve_hardware(manager, cli, selection).await?;
            let snapshot = manager
                .read_polling_rate(&device, profile)
                .await
                .map_err(|error| error.to_string())?;
            output.print(format_rate_human(profile, &snapshot), &snapshot)
        }
        Action::RateSet { profile, rate } => {
            let device = resolve_hardware(manager, cli, selection).await?;
            let outcome = manager
                .update_polling_rate(
                    &device,
                    profile,
                    rate,
                    update_policy(cli.replace_defaults, cli.validation, cli.baseline),
                )
                .await
                .map_err(|error| error.to_string())?;
            output.print(
                format!("Set polling rate for profile {profile} to {rate}"),
                &outcome,
            )
        }
        Action::RateSetUnverifiedBle { profile, rate } => {
            let device = resolve_hardware(manager, cli, selection).await?;
            let outcome = manager
                .update_polling_rate_unverified_ble(
                    &device,
                    profile,
                    rate,
                    update_policy(cli.replace_defaults, cli.validation, cli.baseline),
                )
                .await
                .map_err(|error| error.to_string())?;
            output.print(
                format!(
                    "Polling-rate packet accepted for profile {profile} to {rate} (unverified BLE write; persistence not verified)"
                ),
                &outcome,
            )
        }
        Action::PrefsGet { profile } => {
            let device = resolve_hardware(manager, cli, selection).await?;
            let snapshot = manager
                .read_preferences(&device, profile)
                .await
                .map_err(|error| error.to_string())?;
            output.print(format_prefs_human(profile, &snapshot), &snapshot)
        }
        Action::PrefsSet { profile, delta } => {
            let device = resolve_hardware(manager, cli, selection).await?;
            let outcome = manager
                .update_preferences_delta(
                    &device,
                    profile,
                    delta,
                    update_policy(cli.replace_defaults, cli.validation, cli.baseline),
                )
                .await
                .map_err(|error| error.to_string())?;
            output.print(
                format!("Updated preferences for profile {profile}"),
                &outcome,
            )
        }
        Action::BindGet { profile } => {
            let device = resolve_hardware(manager, cli, selection).await?;
            let snapshot = manager
                .read_buttons(&device, profile)
                .await
                .map_err(|error| error.to_string())?;
            output.print(format_buttons_human(profile, &snapshot), &snapshot)
        }
        Action::BindSet { profile, delta } => {
            let device = resolve_hardware(manager, cli, selection).await?;
            let outcome = manager
                .update_button_slot(
                    &device,
                    profile,
                    delta,
                    update_policy(cli.replace_defaults, cli.validation, cli.baseline),
                )
                .await
                .map_err(|error| error.to_string())?;
            output.print(
                format!("Updated button {:?} to {:?}", delta.slot(), delta.action()),
                &outcome,
            )
        }
        Action::Battery => {
            let device = resolve_hardware(manager, cli, selection).await?;
            let level = manager
                .read_battery(&device)
                .await
                .map_err(|error| error.to_string())?;
            output.print(format!("Battery: {level}%"), &level)
        }
        Action::Verify { profile, method } => match method {
            VerificationAction::ProfileReload => {
                let device = resolve_hardware(manager, cli, selection).await?;
                let outcome = manager
                    .verify_profile_reload(&device, profile)
                    .await
                    .map_err(|error| error.to_string())?;
                output.print(
                    format!("Profile-reload verification for profile {profile}"),
                    &outcome,
                )
            }
            VerificationAction::PowerCycle => {
                let device = resolve_hardware(manager, cli, selection).await?;
                let instruction = "Power off the mouse, wait for it to disappear, power it on, and wait for it to reappear.";
                output.print(
                    instruction,
                    &serde_json::json!({"instruction": instruction}),
                )?;
                let outcome = manager
                    .verify_power_cycle(&device, profile)
                    .await
                    .map_err(|error| error.to_string())?;
                output.print(
                    format!("Power-cycle verification for profile {profile}"),
                    &outcome,
                )
            }
        },
        Action::Export => {
            let device = resolve_state_device(manager, cli)?;
            let export = manager
                .export_configuration(&device)
                .map_err(|error| error.to_string())?;
            output.print("Portable configuration".to_owned(), &export)
        }
        Action::Import { file } => {
            let text = std::fs::read_to_string(&file).map_err(|error| {
                format!("failed to read import file {}: {error}", file.display())
            })?;
            let configuration: ConfigurationExport = serde_json::from_str(&text)
                .map_err(|error| format!("invalid portable configuration: {error}"))?;
            let device = resolve_state_device(manager, cli)?;
            let identity = manager
                .device_identity(&device)
                .map_err(|error| error.to_string())?;
            manager
                .import_configuration(&identity, configuration)
                .map_err(|error| error.to_string())?;
            output.print(format!("Imported configuration for {device}"), &device)
        }
        Action::StateSelected => {
            let selected = manager
                .selected_device()
                .map_err(|error| error.to_string())?;
            output.print(
                selected
                    .as_ref()
                    .map(|device| format!("Selected device: {device}"))
                    .unwrap_or_else(|| "No device selected".to_owned()),
                &selected,
            )
        }
        Action::StateInvalidate => {
            let device = resolve_state_device(manager, cli)?;
            manager
                .invalidate_state(&device)
                .map_err(|error| error.to_string())?;
            output.print(format!("Invalidated state evidence for {device}"), &device)
        }
    }
}

async fn resolve_hardware(
    manager: &DeviceManager,
    cli: &Cli,
    selection: TransportSelection,
) -> Result<DeviceId, String> {
    let explicit = cli.device.as_deref().map(parse_device_id).transpose()?;
    manager
        .resolve_device(explicit.as_ref(), selection)
        .await
        .map_err(|error| error.to_string())
}

fn resolve_state_device(manager: &DeviceManager, cli: &Cli) -> Result<DeviceId, String> {
    if let Some(device) = cli.device.as_deref() {
        return parse_device_id(device);
    }
    manager
        .selected_device()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "no device selected; use `use <stable-id>` first".to_owned())
}

fn run_debug(command: &DebugCommand, output: &Output) -> Result<(), String> {
    match command {
        DebugCommand::Dpi(args) => debug_dpi(args, output),
        DebugCommand::Prefs(args) => debug_prefs(args, output),
        DebugCommand::Buttons(args) => debug_buttons(args, output),
    }
}

fn debug_dpi(args: &DebugDpiArgs, output: &Output) -> Result<(), String> {
    let profile = parse_profile(args.profile)?;
    let stages = args
        .stages
        .iter()
        .copied()
        .map(DpiValue::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let active = parse_stage(args.active_stage)?;
    let preserved_tail = parse_tail(&args.preserved_tail)?;
    let mut state = DpiState::new(profile, stages, active, preserved_tail)
        .map_err(|error| error.to_string())?;
    state.sensor = SensorOptions {
        lift_off_distance: lod_value(args.lod),
        ripple_control: args.ripple_control,
        angle_snap: args.angle_snap,
        motion_sync: args.motion_sync,
    };
    let packet = encode_debug_dpi(&state, transport_kind(args.transport))
        .map_err(|error| error.to_string())?;
    let human = format!(
        "DPI packet: {} bytes\n{}",
        packet.len(),
        Output::hex(packet.as_bytes())
    );
    output.print(human, &packet)
}

fn debug_prefs(args: &DebugPrefsArgs, output: &Output) -> Result<(), String> {
    let profile = parse_profile(args.profile)?;
    let state = PreferencesState::new(
        profile,
        args.light_mode,
        args.configuration,
        args.deep_sleep,
        args.host_color,
        args.sleep_timer,
        args.debounce,
    );
    let framing = match args.framing {
        args::PreferencesFramingArg::Compact => PreferencesFraming::Compact,
        args::PreferencesFramingArg::Full => PreferencesFraming::Full,
    };
    let packet = encode_debug_prefs(&state, framing);
    let human = format!(
        "Preferences packet: {} bytes\n{}",
        packet.len(),
        Output::hex(packet.as_bytes())
    );
    output.print(human, &packet)
}

fn debug_buttons(args: &args::DebugButtonsArgs, output: &Output) -> Result<(), String> {
    let profile = parse_profile(args.profile)?;
    if args.slots.len() != 54 {
        return Err("debug buttons requires exactly 54 slot bytes".into());
    }
    let mut slots = [ButtonAssignment::default(); 18];
    for (index, bytes) in args.slots.chunks_exact(3).enumerate() {
        slots[index] = ButtonAssignment::new(bytes[0], bytes[1], bytes[2]);
    }
    let state = ButtonsState::new(profile, slots);
    let packet = encode_debug_buttons(&state);
    let human = format!(
        "Buttons packet: {} bytes\n{}",
        packet.len(),
        Output::hex(packet.as_bytes())
    );
    output.print(human, &packet)
}

fn parse_tail(raw: &[u8]) -> Result<[u8; 25], String> {
    raw.try_into()
        .map_err(|_| "debug DPI preserved tail must contain exactly 25 bytes".to_owned())
}

fn update_policy(
    replace_defaults: bool,
    validation: ValidationArg,
    baseline: BaselineArg,
) -> UpdatePolicy {
    UpdatePolicy {
        allow_explicit_defaults: replace_defaults,
        verification: match validation {
            ValidationArg::Transport => VerificationMethod::Transport,
            ValidationArg::Readback => VerificationMethod::Readback,
        },
        baseline: match baseline {
            BaselineArg::Live => BaselineSource::Live,
            BaselineArg::Stored => BaselineSource::Stored,
        },
    }
}

fn parse_device_id(raw: &str) -> Result<DeviceId, String> {
    DeviceId::new(raw).map_err(|error| error.to_string())
}

fn parse_profile(raw: u8) -> Result<ProfileId, String> {
    ProfileId::try_from(raw).map_err(|error| error.to_string())
}

fn parse_stage(raw: u8) -> Result<StageIndex, String> {
    StageIndex::try_from(raw).map_err(|error| error.to_string())
}

fn parse_dpi_stages(raw: &Option<String>) -> Result<Option<Vec<DpiValue>>, String> {
    let Some(raw) = raw else { return Ok(None) };
    let values = raw
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            let value = part
                .parse::<u16>()
                .map_err(|_| format!("invalid DPI value `{part}`"))?;
            DpiValue::try_from(value).map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if values.is_empty() {
        return Err("DPI stages must not be empty".into());
    }
    if values.len() > 8 {
        return Err("DPI supports at most eight stages".into());
    }
    Ok(Some(values))
}

fn lod_value(lod: LodArg) -> LiftOffDistance {
    match lod {
        LodArg::One => LiftOffDistance::OneMillimeter,
        LodArg::Two => LiftOffDistance::TwoMillimeters,
    }
}

fn transport_selection(transport: TransportArg) -> TransportSelection {
    match transport {
        TransportArg::Auto => TransportSelection::Auto,
        TransportArg::Wired => TransportSelection::Exact(TransportKind::Wired),
        TransportArg::Receiver => TransportSelection::Exact(TransportKind::Receiver),
        #[cfg(feature = "ble")]
        TransportArg::Ble => TransportSelection::Exact(TransportKind::Ble),
    }
}

fn transport_kind(transport: TransportArg) -> TransportKind {
    match transport {
        TransportArg::Auto | TransportArg::Wired => TransportKind::Wired,
        TransportArg::Receiver => TransportKind::Receiver,
        #[cfg(feature = "ble")]
        TransportArg::Ble => TransportKind::Ble,
    }
}

fn format_transport(transport: TransportKind) -> &'static str {
    match transport {
        TransportKind::Wired => "wired",
        TransportKind::Receiver => "receiver",
        TransportKind::Ble => "ble",
    }
}

fn resource_value<T: Clone>(snapshot: &ResourceSnapshot<T>) -> Option<T> {
    snapshot
        .resource
        .observed
        .as_ref()
        .map(|observed| observed.value.clone())
        .or_else(|| {
            snapshot
                .resource
                .desired
                .as_ref()
                .map(|desired| desired.value.clone())
        })
}

fn format_dpi_human(profile: ProfileId, snapshot: &ResourceSnapshot<DpiState>) -> String {
    let Some(dpi) = resource_value(snapshot) else {
        return format!("DPI for profile {profile}: no data");
    };
    let stages: Vec<String> = dpi.stages.iter().map(|stage| stage.to_string()).collect();
    format!(
        "DPI for profile {profile}\n  stages:      [{}]\n  active:      {}\n  LOD:         {}\n  ripple:      {}\n  angle snap:  {}\n  motion sync: {}",
        stages.join(", "),
        dpi.active_stage,
        match dpi.sensor.lift_off_distance {
            LiftOffDistance::OneMillimeter => "1 mm",
            LiftOffDistance::TwoMillimeters => "2 mm",
        },
        dpi.sensor.ripple_control,
        dpi.sensor.angle_snap,
        dpi.sensor.motion_sync,
    )
}

fn format_rate_human(profile: ProfileId, snapshot: &ResourceSnapshot<PollingRate>) -> String {
    match resource_value(snapshot) {
        Some(rate) => format!("Polling rate (profile {profile}): {rate}"),
        None => format!("Polling rate (profile {profile}): no data"),
    }
}

fn format_prefs_human(profile: ProfileId, snapshot: &ResourceSnapshot<PreferencesState>) -> String {
    let Some(prefs) = resource_value(snapshot) else {
        return format!("Preferences for profile {profile}: no data");
    };
    format!(
        "Preferences for profile {profile}\n  light mode:    {}\n  configuration: {}\n  deep sleep:    {}\n  host color:    #{:02x}{:02x}{:02x}\n  sleep timer:   {}\n  debounce:      {}",
        prefs.light_mode,
        prefs.configuration,
        prefs.deep_sleep,
        prefs.host_color[0],
        prefs.host_color[1],
        prefs.host_color[2],
        prefs.sleep_timer,
        prefs.debounce,
    )
}

fn format_buttons_human(profile: ProfileId, snapshot: &ResourceSnapshot<ButtonsState>) -> String {
    let Some(buttons) = resource_value(snapshot) else {
        return format!("Buttons for profile {profile}: no data");
    };
    let mut text = format!("Buttons for profile {profile}");
    for (index, slot) in buttons.slots.iter().enumerate() {
        if slot.action == 0 && slot.modifier == 0 && slot.key_code == 0 {
            continue;
        }
        let name = button_action_name(slot.action);
        text.push_str(&format!(
            "\n  [{index:2}] action=0x{action:02x} ({name}) mod=0x{mod:02x} key=0x{key:02x}",
            action = slot.action,
            mod = slot.modifier,
            key = slot.key_code,
        ));
    }
    text
}

fn button_action_name(action: u8) -> &'static str {
    match action {
        0x01 => "disable",
        0x02 => "left-click",
        0x03 => "right-click",
        0x04 => "middle-click",
        0x05 => "backward",
        0x06 => "forward",
        0x07 => "double-click",
        0x08 => "fire-button",
        0x09 => "scroll-up",
        0x0a => "scroll-down",
        0x0d => "dpi-cycle",
        0x0e => "dpi-plus",
        0x0f => "dpi-minus",
        0x10 => "easy-aim",
        0x11 => "shortcut",
        0x12 => "macro",
        0x15 => "media-player",
        0x16 => "previous-track",
        0x17 => "next-track",
        0x18 => "play-pause",
        0x19 => "stop",
        0x1a => "mute",
        0x1b => "volume-up",
        0x1c => "volume-down",
        0x1d => "calculator",
        0x1e => "email",
        0x20 => "browser-forward",
        0x21 => "browser-backward",
        0x22 => "browser-stop",
        0x23 => "my-computer",
        0x24 => "browser-refresh",
        0x25 => "browser-home",
        0x26 => "browser-search",
        0x34 => "profile-cycle",
        0x35 => "profile-plus",
        0x36 => "profile-minus",
        0x3c => "wheel-scroll-up",
        _ => "unknown",
    }
}

fn format_status_human(status: &DeviceStatus) -> String {
    let mut text = format!(
        "Status for {}\n  transport: {}",
        status.identity.id,
        format_transport(status.identity.transport)
    );
    if let Some(battery) = status.battery {
        text.push_str(&format!("\n  battery:   {battery}%"));
    }
    if let Some(metadata_snapshot) = &status.profile_metadata
        && let Some(metadata) = resource_value(metadata_snapshot)
    {
        text.push_str(&format!(
            "\n  profile:   {} (max {})",
            metadata.current(),
            metadata.maximum()
        ));
    }
    if let Some(rate_snapshot) = &status.polling_rate
        && let Some(rate) = resource_value(rate_snapshot)
    {
        text.push_str(&format!("\n  rate:      {rate}"));
    }
    text
}

fn action_name(action: &Action) -> &'static str {
    match action {
        Action::Devices => "list devices",
        Action::Use { .. } => "select device",
        Action::Status => "read status",
        Action::ProfileGet { .. } => "read profile",
        Action::ProfileSet { .. } => "set profile",
        Action::DpiGet { .. } => "read DPI",
        Action::DpiSet { .. } => "set DPI",
        Action::RateGet { .. } => "read polling rate",
        Action::RateSet { .. } => "set polling rate",
        Action::RateSetUnverifiedBle { .. } => "set polling rate (unverified BLE packet write)",
        Action::PrefsGet { .. } => "read preferences",
        Action::PrefsSet { .. } => "set preferences",
        Action::BindGet { .. } => "read buttons",
        Action::BindSet { .. } => "set button binding",
        Action::Battery => "read battery",
        Action::Verify { method, .. } => match method {
            VerificationAction::ProfileReload => "verify profile reload",
            VerificationAction::PowerCycle => "verify power cycle",
        },
        Action::Export => "export configuration",
        Action::Import { .. } => "import configuration",
        Action::StateSelected => "read selected device",
        Action::StateInvalidate => "invalidate state",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use args::BindCommand;
    #[test]
    fn build_bind_action_maps_safe_slot_and_action() {
        let cli = args::Cli::try_parse_from([
            "x3ctl",
            "bind",
            "set",
            "--slot",
            "left",
            "--action",
            "profile-cycle",
        ])
        .expect("parse");
        let command = cli.command.as_ref().unwrap();
        let args = match command {
            Command::Bind(BindCommand::Set(args)) => args,
            other => panic!("expected BindSet, got {other:?}"),
        };
        let action = build_bind_action(&cli, args).expect("build");
        match action {
            Action::BindSet { delta, .. } => {
                assert_eq!(delta.slot(), SafeButtonSlot::Left);
                assert_eq!(delta.slot_index(), 0);
                assert_eq!(delta.action(), SafeButtonAction::ProfileCycle);
                assert_eq!(delta.assignment().action, 0x34);
                assert_eq!(delta.assignment().modifier, 0);
                assert_eq!(delta.assignment().key_code, 0);
            }
            other => panic!("expected BindSet, got {other:?}"),
        }
    }

    #[test]
    fn build_bind_action_maps_captured_stock_actions() {
        for (name, expected) in [
            ("volume-up", [0x1b_u8, 0x00, 0x00]),
            ("fire-button", [0x08_u8, 0x00, 0x00]),
            ("scroll-up", [0x09_u8, 0x00, 0x00]),
            ("cut", [0x11_u8, 0x01, 0x1b]),
            ("browser-favorites", [0x11_u8, 0x03, 0x12]),
            ("screen-capture", [0x11_u8, 0x0a, 0x16]),
        ] {
            let cli = args::Cli::try_parse_from([
                "x3ctl", "bind", "set", "--slot", "forward", "--action", name,
            ])
            .expect("parse");
            let command = cli.command.as_ref().unwrap();
            let args = match command {
                Command::Bind(BindCommand::Set(args)) => args,
                other => panic!("expected BindSet, got {other:?}"),
            };
            let action = build_bind_action(&cli, args).expect("build");
            match action {
                Action::BindSet { delta, .. } => {
                    assert_eq!(delta.assignment().as_bytes(), expected, "{name}");
                }
                other => panic!("expected BindSet, got {other:?}"),
            }
        }
    }

    #[test]
    fn build_bind_action_maps_dpi_slot() {
        let cli = args::Cli::try_parse_from([
            "x3ctl", "bind", "set", "--slot", "dpi", "--action", "dpi-plus",
        ])
        .expect("parse");
        let command = cli.command.as_ref().unwrap();
        let args = match command {
            Command::Bind(BindCommand::Set(args)) => args,
            other => panic!("expected BindSet, got {other:?}"),
        };
        let action = build_bind_action(&cli, args).expect("build");
        match action {
            Action::BindSet { delta, .. } => {
                assert_eq!(delta.slot(), SafeButtonSlot::Dpi);
                assert_eq!(delta.slot_index(), 3);
                assert_eq!(delta.action(), SafeButtonAction::DpiPlus);
                assert_eq!(delta.assignment().action, 0x0e);
            }
            other => panic!("expected BindSet, got {other:?}"),
        }
    }

    #[test]
    fn build_bind_action_maps_backward_slot_and_forward_action() {
        let cli = args::Cli::try_parse_from([
            "x3ctl", "bind", "set", "--slot", "backward", "--action", "forward",
        ])
        .expect("parse");
        let command = cli.command.as_ref().unwrap();
        let args = match command {
            Command::Bind(BindCommand::Set(args)) => args,
            other => panic!("expected BindSet, got {other:?}"),
        };
        let action = build_bind_action(&cli, args).expect("build");
        match action {
            Action::BindSet { delta, .. } => {
                assert_eq!(delta.slot(), SafeButtonSlot::Backward);
                assert_eq!(delta.slot_index(), 7);
                assert_eq!(delta.action(), SafeButtonAction::Forward);
                assert_eq!(delta.assignment().action, 0x06);
            }
            other => panic!("expected BindSet, got {other:?}"),
        }
    }

    #[test]
    fn dpi_stage_parser_validates_domain_values() {
        assert_eq!(
            parse_dpi_stages(&Some("800, 1600".into()))
                .unwrap()
                .unwrap()
                .len(),
            2
        );
        assert!(parse_dpi_stages(&Some("801".into())).is_err());
        assert!(parse_dpi_stages(&Some("".into())).is_err());
    }

    #[test]
    fn build_rate_action_routes_unverified_ble_flag() {
        let cli = args::Cli::try_parse_from(["x3ctl", "rate", "set", "1000"]).expect("parse");
        let command = cli.command.as_ref().unwrap();
        let action = build_action(&cli, command).expect("build");
        assert!(matches!(
            action,
            Action::RateSet {
                rate: PollingRate::Hz1000,
                ..
            }
        ));

        let cli = args::Cli::try_parse_from([
            "x3ctl",
            "rate",
            "set",
            "1000",
            "--allow-unverified-ble-rate-write",
        ])
        .expect("parse");
        let command = cli.command.as_ref().unwrap();
        let action = build_action(&cli, command).expect("build");
        let rate = match action {
            Action::RateSetUnverifiedBle { rate, .. } => rate,
            other => panic!("expected RateSetUnverifiedBle, got {other:?}"),
        };
        assert_eq!(rate, PollingRate::Hz1000);
        assert_eq!(
            action_name(&Action::RateSetUnverifiedBle {
                profile: ProfileId::try_from(1).unwrap(),
                rate,
            }),
            "set polling rate (unverified BLE packet write)"
        );
    }

    #[test]
    fn build_rate_action_still_validates_rate_with_unverified_flag() {
        let cli = args::Cli::try_parse_from([
            "x3ctl",
            "rate",
            "set",
            "999",
            "--allow-unverified-ble-rate-write",
        ])
        .expect("parse");
        let command = cli.command.as_ref().unwrap();
        let error = build_action(&cli, command).expect_err("invalid rate must fail");
        assert!(error.contains("polling rate must be one of"));
    }

    #[test]
    fn update_policy_maps_validation_flag_to_verification_method() {
        let transport = update_policy(false, ValidationArg::Transport, BaselineArg::Live);
        assert!(!transport.allow_explicit_defaults);
        assert_eq!(transport.verification, VerificationMethod::Transport);
        assert_eq!(transport.baseline, BaselineSource::Live);

        let readback = update_policy(true, ValidationArg::Readback, BaselineArg::Stored);
        assert!(readback.allow_explicit_defaults);
        assert_eq!(readback.verification, VerificationMethod::Readback);
        assert_eq!(readback.baseline, BaselineSource::Stored);
    }

    #[test]
    fn profile_and_device_parsers_reject_invalid_values() {
        assert!(parse_profile(0).is_err());
        assert!(parse_device_id(" ").is_err());
    }
}
