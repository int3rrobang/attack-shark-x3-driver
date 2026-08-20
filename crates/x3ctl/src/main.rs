#![forbid(unsafe_code)]

#[path = "x3ctl/args.rs"]
mod args;
#[path = "x3ctl/output.rs"]
mod output;

use attack_shark_x3_manager::{
    BaselineSource, ButtonAssignment, ButtonSlotDelta, ButtonsState, ConfigurationExport, DeviceId,
    DeviceManager, DeviceStatus, DpiDelta, DpiState, DpiValue, FullProfileRefreshOutcome,
    LiftOffDistance, PollingRate, PreferencesDelta, PreferencesFraming, PreferencesState,
    ProfileId, ProfileResourceKind, ResourceSnapshot, SafeButtonAction, SafeButtonSlot,
    SensorOptions, SensorOptionsDelta, StageIndex, StateStore, TransportKind, TransportSelection,
    UpdatePolicy, VerificationMethod, X3ButtonAction, encode_debug_buttons, encode_debug_dpi,
    encode_debug_prefs,
};
use clap::Parser;
use serde::Serialize;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

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
    ProfileRefreshAll,
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
    StateReset,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let output = Output::new(cli.output);

    let runtime = match tokio::runtime::Builder::new_current_thread()
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
        return run_debug(cli, command, output);
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
            ProfileCommand::RefreshAll => Ok(Action::ProfileRefreshAll),
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
            StateCommand::Reset => Ok(Action::StateReset),
        },
        Command::Debug(_) => unreachable!("debug commands are handled offline before build_action"),
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
                        let transport_label = device
                            .identity
                            .selected_transport()
                            .map(format_transport)
                            .unwrap_or("unknown");
                        format!(
                            "{} [{}] {}",
                            device.identity.id,
                            transport_label,
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
            output.print(format_profile_set_human(profile, maximum), &metadata)
        }
        Action::ProfileRefreshAll => {
            let device = resolve_hardware(manager, cli, selection).await?;
            let outcome = manager
                .refresh_all_profiles(&device)
                .await
                .map_err(|error| error.to_string())?;
            output.print(format_refresh_human(&outcome), &outcome)
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
                    "Polling rate {rate} sent for profile {profile} over BLE (delivered; not confirmed after restart)"
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
            let mut human = String::new();
            let _ = write!(
                human,
                "Set {} button to {}",
                slot_label(delta.slot()),
                bind_set_action_human(delta.action())
            );
            output.print(human, &outcome)
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
                let instruction = "Unplug USB, turn the mouse off, and wait for it to disappear. Then turn it on, reconnect, and wait for it to reappear. Unplugging USB alone isn't a real power cycle — the mouse battery keeps it running.";
                if output.is_json() {
                    let outcome = manager
                        .verify_power_cycle(&device, profile)
                        .await
                        .map_err(|error| error.to_string())?;
                    let payload = serde_json::json!({
                        "instruction": instruction,
                        "outcome": outcome,
                    });
                    output.print(
                        format!("Power-cycle verification for profile {profile}"),
                        &payload,
                    )
                } else {
                    println!("{instruction}");
                    let outcome = manager
                        .verify_power_cycle(&device, profile)
                        .await
                        .map_err(|error| error.to_string())?;
                    output.print(
                        format!("Power-cycle verification for profile {profile}"),
                        &outcome,
                    )
                }
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
            output.print(format_import_human(&device), &device)
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
            output.print(format!("Cleared saved confirmation for {device}"), &device)
        }
        Action::StateReset => {
            let reset = manager
                .discard_unreadable_state()
                .map_err(|error| error.to_string())?;
            let backup = reset
                .backup
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "no previous file existed".to_owned());
            output.print(
                format!(
                    "Replaced the state file with a fresh empty state; previous file preserved at {backup}"
                ),
                &reset,
            )
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

fn run_debug(cli: &Cli, command: &DebugCommand, output: &Output) -> Result<(), String> {
    match command {
        DebugCommand::Dpi(args) => debug_dpi(cli, args, output),
        DebugCommand::Prefs(args) => debug_prefs(args, output),
        DebugCommand::Buttons(args) => debug_buttons(args, output),
    }
}

fn debug_dpi(cli: &Cli, args: &DebugDpiArgs, output: &Output) -> Result<(), String> {
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
    let packet = encode_debug_dpi(&state, transport_kind(cli.transport))
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
    if raw.trim().is_empty() {
        return Err("DPI stages must not be empty".into());
    }
    let mut values = Vec::new();
    for part in raw.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            return Err(
                "DPI stages must not contain empty elements; check for doubled commas or trailing commas".into(),
            );
        }
        let value = trimmed
            .parse::<u16>()
            .map_err(|_| format!("invalid DPI value `{trimmed}`"))?;
        let dpi = DpiValue::try_from(value).map_err(|error| error.to_string())?;
        values.push(dpi);
    }
    debug_assert!(!values.is_empty(), "empty DPI stages already rejected");
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

fn format_profile_set_human(profile: ProfileId, maximum: Option<ProfileId>) -> String {
    if let Some(maximum) = maximum {
        format!("Updated profile setup: current {profile}, maximum {maximum}")
    } else {
        format!("Activated profile {profile}")
    }
}

fn format_import_human(device: &DeviceId) -> String {
    format!("Saved settings locally for {device} (not sent to mouse)")
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
        let readable = assignment_human_readable(*slot);
        let slot_title = match index {
            0 => "left",
            1 => "right",
            2 => "middle",
            3 => "dpi",
            6 => "forward",
            7 => "backward",
            _ => "",
        };
        if slot_title.is_empty() {
            let _ = write!(text, "\n  [{index:2}] slot {index}: {readable}");
        } else {
            let _ = write!(text, "\n  [{index:2}] {slot_title}: {readable}");
        }
    }
    text
}

fn assignment_human_readable(assignment: ButtonAssignment) -> String {
    // Friendly preset names that share the 0x11 shortcut encoding but deserve
    // effect-first labels like "copy (Ctrl+C)" for ordinary output.
    let preset = match assignment.as_bytes() {
        [0x11, 0x01, 0x1b] => Some(("cut", "Ctrl", "X")),
        [0x11, 0x01, 0x06] => Some(("copy", "Ctrl", "C")),
        [0x11, 0x01, 0x19] => Some(("paste", "Ctrl", "V")),
        [0x11, 0x01, 0x12] => Some(("open", "Ctrl", "O")),
        [0x11, 0x01, 0x16] => Some(("save", "Ctrl", "S")),
        [0x11, 0x01, 0x09] => Some(("find", "Ctrl", "F")),
        [0x11, 0x01, 0x1c] => Some(("redo", "Ctrl", "Y")),
        [0x11, 0x01, 0x04] => Some(("select-all", "Ctrl", "A")),
        [0x11, 0x01, 0x13] => Some(("print", "Ctrl", "P")),
        [0x11, 0x04, 0x3d] => Some(("close-window", "Alt", "F4")),
        [0x11, 0x04, 0x2b] => Some(("swap-windows", "Alt", "Tab")),
        [0x11, 0x08, 0x07] => Some(("show-desktop", "Win", "D")),
        [0x11, 0x08, 0x15] => Some(("run-command", "Win", "R")),
        [0x11, 0x08, 0x0f] => Some(("lock-pc", "Win", "L")),
        [0x11, 0x0a, 0x16] => Some(("screen-capture", "Win+Shift", "S")),
        [0x11, 0x03, 0x12] => Some(("browser-favorites", "Ctrl+Shift", "O")),
        _ => None,
    };
    if let Some((name, mods, key)) = preset {
        let mut out = String::new();
        let _ = write!(out, "{name} ({mods}+{key})");
        return out;
    }
    match assignment.decode_x3_action() {
        Ok(action) => match action {
            X3ButtonAction::Disable => "disable".to_owned(),
            X3ButtonAction::LeftClick => "left-click".to_owned(),
            X3ButtonAction::RightClick => "right-click".to_owned(),
            X3ButtonAction::MiddleClick => "middle-click".to_owned(),
            X3ButtonAction::Backward => "backward".to_owned(),
            X3ButtonAction::Forward => "forward".to_owned(),
            X3ButtonAction::DoubleClick => "double-click".to_owned(),
            X3ButtonAction::FireButton => "fire-button".to_owned(),
            X3ButtonAction::ScrollUp => "scroll-up".to_owned(),
            X3ButtonAction::ScrollDown => "scroll-down".to_owned(),
            X3ButtonAction::DpiCycle => "dpi-cycle".to_owned(),
            X3ButtonAction::DpiPlus => "dpi-plus".to_owned(),
            X3ButtonAction::DpiMinus => "dpi-minus".to_owned(),
            X3ButtonAction::ProfileCycle => "profile-cycle".to_owned(),
            X3ButtonAction::ProfilePlus => "profile-plus".to_owned(),
            X3ButtonAction::ProfileMinus => "profile-minus".to_owned(),
            X3ButtonAction::MediaPlayer => "media-player".to_owned(),
            X3ButtonAction::PreviousTrack => "previous-track".to_owned(),
            X3ButtonAction::NextTrack => "next-track".to_owned(),
            X3ButtonAction::PlayPause => "play-pause".to_owned(),
            X3ButtonAction::Stop => "stop".to_owned(),
            X3ButtonAction::Mute => "mute".to_owned(),
            X3ButtonAction::VolumeUp => "volume-up".to_owned(),
            X3ButtonAction::VolumeDown => "volume-down".to_owned(),
            X3ButtonAction::Calculator => "calculator".to_owned(),
            X3ButtonAction::Email => "email".to_owned(),
            X3ButtonAction::BrowserForward => "browser-forward".to_owned(),
            X3ButtonAction::BrowserBackward => "browser-backward".to_owned(),
            X3ButtonAction::BrowserStop => "browser-stop".to_owned(),
            X3ButtonAction::MyComputer => "my-computer".to_owned(),
            X3ButtonAction::BrowserRefresh => "browser-refresh".to_owned(),
            X3ButtonAction::BrowserHome => "browser-home".to_owned(),
            X3ButtonAction::BrowserSearch => "browser-search".to_owned(),
            X3ButtonAction::KeyboardShortcut { modifiers, key } => {
                let mods = modifiers_human(modifiers.bits());
                let key_name = hid_key_human(key.get());
                if mods.is_empty() {
                    key_name
                } else {
                    let mut out = String::new();
                    let _ = write!(out, "{mods}+{key_name}");
                    out
                }
            }
            X3ButtonAction::Macro { reference } => {
                let mut out = String::new();
                let _ = write!(out, "macro {reference}");
                out
            }
        },
        Err(_) => match assignment.action {
            0x10 => "easy-aim".to_owned(),
            0x3c => "wheel-scroll-up".to_owned(),
            _ => "unknown".to_owned(),
        },
    }
}

fn modifiers_human(bits: u8) -> String {
    let mut out = String::new();
    let mut first = true;
    if bits & 0x01 != 0 {
        out.push_str("Ctrl");
        first = false;
    }
    if bits & 0x02 != 0 {
        if !first {
            out.push('+');
        }
        out.push_str("Shift");
        first = false;
    }
    if bits & 0x04 != 0 {
        if !first {
            out.push('+');
        }
        out.push_str("Alt");
        first = false;
    }
    if bits & 0x08 != 0 {
        if !first {
            out.push('+');
        }
        out.push_str("Win");
    }
    out
}

fn hid_key_human(usage: u8) -> String {
    match usage {
        0x04 => "A".to_owned(),
        0x05 => "B".to_owned(),
        0x06 => "C".to_owned(),
        0x07 => "D".to_owned(),
        0x08 => "E".to_owned(),
        0x09 => "F".to_owned(),
        0x0a => "G".to_owned(),
        0x0b => "H".to_owned(),
        0x0c => "I".to_owned(),
        0x0d => "J".to_owned(),
        0x0e => "K".to_owned(),
        0x0f => "L".to_owned(),
        0x10 => "M".to_owned(),
        0x11 => "N".to_owned(),
        0x12 => "O".to_owned(),
        0x13 => "P".to_owned(),
        0x14 => "Q".to_owned(),
        0x15 => "R".to_owned(),
        0x16 => "S".to_owned(),
        0x17 => "T".to_owned(),
        0x18 => "U".to_owned(),
        0x19 => "V".to_owned(),
        0x1a => "W".to_owned(),
        0x1b => "X".to_owned(),
        0x1c => "Y".to_owned(),
        0x1d => "Z".to_owned(),
        0x1e => "1".to_owned(),
        0x1f => "2".to_owned(),
        0x20 => "3".to_owned(),
        0x21 => "4".to_owned(),
        0x22 => "5".to_owned(),
        0x23 => "6".to_owned(),
        0x24 => "7".to_owned(),
        0x25 => "8".to_owned(),
        0x26 => "9".to_owned(),
        0x27 => "0".to_owned(),
        0x28 => "Enter".to_owned(),
        0x29 => "Esc".to_owned(),
        0x2a => "Backspace".to_owned(),
        0x2b => "Tab".to_owned(),
        0x2c => "Space".to_owned(),
        0x2d => "-".to_owned(),
        0x2e => "=".to_owned(),
        0x2f => "[".to_owned(),
        0x30 => "]".to_owned(),
        0x31 => "\\".to_owned(),
        0x32 => "#".to_owned(),
        0x33 => ";".to_owned(),
        0x34 => "'".to_owned(),
        0x35 => "`".to_owned(),
        0x36 => ",".to_owned(),
        0x37 => ".".to_owned(),
        0x38 => "/".to_owned(),
        0x39 => "CapsLock".to_owned(),
        0x3a => "F1".to_owned(),
        0x3b => "F2".to_owned(),
        0x3c => "F3".to_owned(),
        0x3d => "F4".to_owned(),
        0x3e => "F5".to_owned(),
        0x3f => "F6".to_owned(),
        0x40 => "F7".to_owned(),
        0x41 => "F8".to_owned(),
        0x42 => "F9".to_owned(),
        0x43 => "F10".to_owned(),
        0x44 => "F11".to_owned(),
        0x45 => "F12".to_owned(),
        0x49 => "Insert".to_owned(),
        0x4a => "Home".to_owned(),
        0x4b => "PageUp".to_owned(),
        0x4c => "Delete".to_owned(),
        0x4d => "End".to_owned(),
        0x4e => "PageDown".to_owned(),
        0x4f => "Right".to_owned(),
        0x50 => "Left".to_owned(),
        0x51 => "Down".to_owned(),
        0x52 => "Up".to_owned(),
        _ => {
            let mut out = String::new();
            let _ = write!(out, "key {usage}");
            out
        }
    }
}

fn bind_set_action_human(action: SafeButtonAction) -> String {
    assignment_human_readable(action.to_assignment())
}

fn slot_label(slot: SafeButtonSlot) -> &'static str {
    match slot {
        SafeButtonSlot::Left => "left",
        SafeButtonSlot::Right => "right",
        SafeButtonSlot::Middle => "middle",
        SafeButtonSlot::Dpi => "dpi",
        SafeButtonSlot::Forward => "forward",
        SafeButtonSlot::Backward => "backward",
    }
}

fn format_refresh_human(outcome: &FullProfileRefreshOutcome) -> String {
    let mut text = format!(
        "Read all {} profiles from USB.\nRestored current {} / maximum {}.",
        outcome.profiles.len(),
        outcome.restored_metadata.current(),
        outcome.restored_metadata.maximum()
    );
    if outcome.temporarily_expanded {
        text.push_str("\nProfile slots were temporarily enabled through slot 5.");
    }
    if outcome.profile_metadata_drift || !outcome.drift.is_empty() {
        text.push_str("\nDifferences found between saved settings and the mouse:");
        if outcome.profile_metadata_drift {
            text.push_str("\n  profile setup");
        }
        for (profile, resources) in &outcome.drift {
            let names = resources
                .iter()
                .map(cli_profile_resource_name)
                .collect::<Vec<_>>()
                .join(", ");
            text.push_str(&format!("\n  profile {profile}: {names}"));
        }
    } else {
        text.push_str("\nYour saved settings match what's on the mouse.");
    }
    text.push_str("\nNot yet confirmed to survive a restart. Run `verify` to check it.");
    text
}

fn cli_profile_resource_name(resource: &ProfileResourceKind) -> &'static str {
    match resource {
        ProfileResourceKind::Dpi => "DPI",
        ProfileResourceKind::Preferences => "preferences",
        ProfileResourceKind::Buttons => "buttons",
        ProfileResourceKind::PollingRate => "polling rate",
    }
}

fn format_status_human(status: &DeviceStatus) -> String {
    let transport_label = status
        .identity
        .selected_endpoint()
        .map(|endpoint| format_transport(endpoint.transport))
        .unwrap_or("unknown");
    let mut text = format!(
        "Status for {}\n  transport: {}",
        status.identity.id, transport_label
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
        Action::ProfileRefreshAll => "read all five profiles, then restore the original setup",
        Action::DpiGet { .. } => "read DPI",
        Action::DpiSet { .. } => "set DPI",
        Action::RateGet { .. } => "read polling rate",
        Action::RateSet { .. } => "set polling rate",
        Action::RateSetUnverifiedBle { .. } => "set polling rate (BLE, not confirmed)",
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
        Action::StateInvalidate => "clear saved confirmation",
        Action::StateReset => "reset saved data",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use args::BindCommand;
    use attack_shark_x3_manager::{ObservationSource, ObservedState, ResourceState, Timestamp};
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
            "set polling rate (BLE, not confirmed)"
        );
    }

    #[test]
    fn build_profile_refresh_all_action_is_explicit() {
        let cli = args::Cli::try_parse_from(["x3ctl", "--dry-run", "profile", "refresh-all"])
            .expect("parse");
        let command = cli.command.as_ref().unwrap();
        let action = build_action(&cli, command).expect("build");
        assert!(matches!(action, Action::ProfileRefreshAll));
        assert_eq!(
            action_name(&action),
            "read all five profiles, then restore the original setup"
        );
    }

    #[test]
    fn refresh_human_output_uses_readable_resource_names() {
        assert_eq!(
            cli_profile_resource_name(&ProfileResourceKind::PollingRate),
            "polling rate"
        );
        assert_eq!(
            cli_profile_resource_name(&ProfileResourceKind::Preferences),
            "preferences"
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

    #[test]
    fn dpi_stage_parser_rejects_empty_elements() {
        for raw in [
            "800,,1600",
            "800,1600,",
            ",800",
            "800, 1600, ",
            "800,  ,1600",
            "800,,",
            ",,800",
        ] {
            let err = parse_dpi_stages(&Some(raw.into())).expect_err(raw);
            assert!(
                err.contains("empty elements") || err.contains("empty"),
                "{raw}: {err}"
            );
        }
        // valid should still pass
        assert_eq!(
            parse_dpi_stages(&Some("800,1600".into()))
                .unwrap()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn profile_set_human_for_maximum_does_not_claim_activation() {
        let active = format_profile_set_human(ProfileId::try_from(2).unwrap(), None);
        assert_eq!(active, "Activated profile 2");
        assert!(active.contains("Activated"));

        let with_max = format_profile_set_human(
            ProfileId::try_from(2).unwrap(),
            Some(ProfileId::try_from(4).unwrap()),
        );
        assert_eq!(with_max, "Updated profile setup: current 2, maximum 4");
        assert!(!with_max.contains("Activated"), "{with_max}");
        assert!(with_max.contains("maximum"));
    }
    #[test]
    fn bind_human_labels_are_readable() {
        assert_eq!(slot_label(SafeButtonSlot::Left), "left");
        assert_eq!(slot_label(SafeButtonSlot::Dpi), "dpi");
        assert_eq!(
            bind_set_action_human(SafeButtonAction::ProfileCycle),
            "profile-cycle"
        );
        assert_eq!(
            bind_set_action_human(SafeButtonAction::LeftClick),
            "left-click"
        );
        assert_eq!(
            bind_set_action_human(SafeButtonAction::ScreenCapture),
            "screen-capture (Win+Shift+S)"
        );
        let human = format!(
            "Set {} button to {}",
            slot_label(SafeButtonSlot::Left),
            bind_set_action_human(SafeButtonAction::ProfileCycle)
        );
        assert_eq!(human, "Set left button to profile-cycle");
    }
    #[test]
    fn bind_get_human_is_readable_and_hex_free() {
        let profile = ProfileId::try_from(1).unwrap();
        let mut slots = [ButtonAssignment::default(); 18];
        slots[0] = ButtonAssignment::new(0x02, 0x00, 0x00);
        slots[3] = ButtonAssignment::new(0x34, 0x00, 0x00);
        slots[4] = ButtonAssignment::new(0x11, 0x01, 0x06);
        slots[7] = ButtonAssignment::new(0x06, 0x00, 0x00);
        // unknown raw to verify fallback without hex
        slots[8] = ButtonAssignment::new(0xff, 0x00, 0x00);
        let state = ButtonsState::new(profile, slots);
        let snapshot = ResourceSnapshot {
            resource: ResourceState {
                desired: None,
                observed: Some(ObservedState {
                    value: state,
                    source: ObservationSource::UsbReadback,
                    observed_at: Timestamp { unix_seconds: 0 },
                }),
            },
        };
        let human = format_buttons_human(profile, &snapshot);
        assert!(human.starts_with("Buttons for profile 1"), "{human}");
        assert!(human.contains("left: left-click"), "{human}");
        assert!(human.contains("dpi: profile-cycle"), "{human}");
        assert!(human.contains("backward: forward"), "{human}");
        // shortcut preset must show effect name and Ctrl label
        assert!(human.contains("copy (Ctrl+C)"), "{human}");
        assert!(human.contains("unknown"), "{human}");
        // ordinary output must not contain raw hex fragments
        assert!(!human.contains("0x"), "{human}");
        assert!(!human.contains("mod="), "{human}");
        assert!(!human.contains("key=0x"), "{human}");
    }

    #[test]
    fn bind_get_human_shows_generic_shortcut_and_macro_readably() {
        let profile = ProfileId::try_from(2).unwrap();
        let mut slots = [ButtonAssignment::default(); 18];
        // Generic Ctrl+Shift+T shortcut
        slots[0] = ButtonAssignment::new(0x11, 0x03, 0x17);
        // Generic macro reference 7
        slots[1] = ButtonAssignment::new(0x12, 0x00, 0x07);
        // easy-aim legacy
        slots[2] = ButtonAssignment::new(0x10, 0x00, 0x00);
        let state = ButtonsState::new(profile, slots);
        let snapshot = ResourceSnapshot {
            resource: ResourceState {
                desired: None,
                observed: Some(ObservedState {
                    value: state,
                    source: ObservationSource::UsbReadback,
                    observed_at: Timestamp { unix_seconds: 0 },
                }),
            },
        };
        let human = format_buttons_human(profile, &snapshot);
        assert!(human.contains("Ctrl+Shift+T"), "{human}");
        assert!(human.contains("macro 7"), "{human}");
        assert!(human.contains("easy-aim"), "{human}");
        assert!(!human.contains("0x"), "{human}");
    }

    #[test]
    fn bind_set_human_leads_with_slot_and_readable_shortcut() {
        let delta = ButtonSlotDelta::new(SafeButtonSlot::Forward, SafeButtonAction::Copy);
        let mut human = String::new();
        let _ = write!(
            human,
            "Set {} button to {}",
            slot_label(delta.slot()),
            bind_set_action_human(delta.action())
        );
        assert_eq!(human, "Set forward button to copy (Ctrl+C)");
        assert!(!human.contains("0x"), "{human}");
        assert!(human.starts_with("Set forward button to"));
    }

    #[test]
    fn bind_set_human_for_parameterless_has_no_hex() {
        let delta = ButtonSlotDelta::new(SafeButtonSlot::Dpi, SafeButtonAction::DpiPlus);
        let mut human = String::new();
        let _ = write!(
            human,
            "Set {} button to {}",
            slot_label(delta.slot()),
            bind_set_action_human(delta.action())
        );
        assert_eq!(human, "Set dpi button to dpi-plus");
        assert!(!human.contains("0x"));
    }

    #[test]
    fn modifiers_and_keys_are_human_readable() {
        assert_eq!(modifiers_human(0x01), "Ctrl");
        assert_eq!(modifiers_human(0x03), "Ctrl+Shift");
        assert_eq!(modifiers_human(0x0a), "Shift+Win");
        assert_eq!(hid_key_human(0x04), "A");
        assert_eq!(hid_key_human(0x2b), "Tab");
        assert_eq!(hid_key_human(0x3d), "F4");
        // fallback without hex
        let fallback = hid_key_human(0xff);
        assert!(fallback.contains("key"), "{fallback}");
        assert!(!fallback.contains("0x"), "{fallback}");
    }
    #[test]
    fn import_human_says_saved_locally_not_sent() {
        let device = DeviceId::new("mouse-1").unwrap();
        let human = format_import_human(&device);
        assert!(human.contains("Saved settings locally"), "{human}");
        assert!(human.contains("not sent to mouse"), "{human}");
        assert!(
            !human
                .to_ascii_lowercase()
                .contains("imported configuration")
        );
    }

    #[test]
    fn debug_dpi_uses_global_transport() {
        let cli = args::Cli::try_parse_from([
            "x3ctl",
            "--transport",
            "receiver",
            "debug",
            "dpi",
            "--stages",
            "800,1600",
        ])
        .expect("parse");
        assert_eq!(cli.transport, TransportArg::Receiver);
        // DebugDpiArgs no longer shadows; parsing --transport after debug also resolves to global
        let cli2 = args::Cli::try_parse_from([
            "x3ctl",
            "debug",
            "dpi",
            "--stages",
            "800,1600",
            "--transport",
            "wired",
        ])
        .expect("parse with trailing global");
        // With global transport, trailing --transport should be interpreted as global
        // and not as a debug-local field (which no longer exists).
        assert_eq!(cli2.transport, TransportArg::Wired);
        assert!(matches!(
            cli2.command,
            Some(Command::Debug(DebugCommand::Dpi(_)))
        ));
    }
}
