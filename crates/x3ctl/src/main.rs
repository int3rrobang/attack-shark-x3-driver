#[path = "x3ctl/args.rs"]
mod args;
#[path = "x3ctl/broker.rs"]
mod broker;
#[path = "x3ctl/executor.rs"]
mod executor;
#[path = "x3ctl/output.rs"]
mod output;
#[path = "x3ctl/state.rs"]
mod state;
#[path = "x3ctl/wire.rs"]
mod wire;

use clap::Parser;
use std::process::ExitCode;
use std::time::Duration;

use args::{
    Cli, Command, DaemonCommand, DebugCommand, DebugReadReportArg, LodArg, ProfileCommand,
    ProfileFramingArg, StateCommand, TransportArg,
};
use attack_shark_x3::{
    DpiReport, DpiState, DpiValue, ProfileControlFraming, ProfileControlReport, ProfileId,
    ProfileMetadata, ReadSelector, ReadbackRequest, SensorOptions, StageIndex, TransportKind,
    protocol::dpi::LiftOffDistance,
};
use broker::RequestHandler;
use output::Output;
use wire::{
    ButtonPayload, DeviceSelector, ExecutionContext, PreferencesDelta, Request, SensorOptionsDelta,
    TransportKind as WireTransport,
};

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() -> ExitCode {
    // Detect hidden __broker mode before clap parses.
    let is_broker = std::env::args().any(|a| a == "__broker");

    if is_broker {
        return run_broker_mode();
    }

    let cli = Cli::parse();
    let out = Output::new(cli.output_format());

    // BLE on Windows uses `block_in_place` inside the executor, which
    // requires a multi-threaded runtime.  One worker is enough and keeps
    // overhead minimal for the common non-BLE case.
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            out.error(&format!("failed to create async runtime: {e}"));
            return ExitCode::FAILURE;
        }
    };

    match rt.block_on(run(&cli, &out)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            out.error(&e);
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// Hidden broker mode
// ---------------------------------------------------------------------------

fn run_broker_mode() -> ExitCode {
    // See main() — BLE on Windows needs a multi-threaded runtime.
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("broker: failed to create async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    let exec = executor::Executor::with_state(Some(state::state_path()));

    let result = rt.block_on(broker::run_server(
        exec.clone(),
        exec.clone(),
        broker::SystemTime,
    ));

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("broker: {e}");
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

async fn run(cli: &Cli, out: &Output) -> Result<(), String> {
    // Treat no subcommand as an implicit `status`.
    let cmd = cli.command.as_ref().unwrap_or(&Command::Status);

    // Debug commands are offline — no hardware, no broker.
    if let Command::Debug(cmd) = cmd {
        return run_debug(cmd, out);
    }

    // Daemon status does NOT auto-start a missing broker.
    if let Command::Daemon(_) = cmd {
        return run_daemon(cli, out).await;
    }

    // State management commands are local-only.
    if let Command::State(cmd) = cmd {
        return run_state(cmd, out);
    }

    // Build execution context shared across all device commands.
    let ctx = build_context(cli);

    // Build the wire request from CLI args.
    let request = build_request(cli, &ctx)?;

    // Dry run: validate and print without touching hardware/broker.
    if cli.dry_run {
        return dry_run_print(&request, out);
    }

    // Direct mode: execute in-process through the executor.
    if cli.effective_direct() {
        return execute_direct(request, cli.effective_no_state(), out).await;
    }

    // Default: connect to broker (auto-starting if needed).
    execute_via_broker(request, out).await
}

// ---------------------------------------------------------------------------
// Execution context
// ---------------------------------------------------------------------------

fn build_context(cli: &Cli) -> ExecutionContext {
    let transport = match cli.transport {
        TransportArg::Auto => WireTransport::Auto,
        TransportArg::Wired => WireTransport::Wired,
        TransportArg::Receiver => WireTransport::Receiver,
        #[cfg(feature = "ble")]
        TransportArg::Ble => WireTransport::Ble,
    };

    let device = cli.device.as_ref().map(|d| {
        // Heuristic: if the device string looks like a path, treat as USB;
        // otherwise treat as BLE name.
        if d.contains('/') || d.contains('\\') || d.contains("hid") {
            DeviceSelector::UsbPath(d.clone())
        } else {
            DeviceSelector::BleName(d.clone())
        }
    });

    ExecutionContext {
        transport,
        device,
        no_state: cli.effective_no_state(),
        explicit_defaults: cli.replace_defaults,
    }
}

// ---------------------------------------------------------------------------
// Build typed request
// ---------------------------------------------------------------------------

fn build_request(cli: &Cli, ctx: &ExecutionContext) -> Result<Request, String> {
    match &cli.command {
        None | Some(Command::Status) => {
            let profile = cli.resolve_profile(None).unwrap_or(1);
            Ok(Request::Status {
                profile,
                ctx: ctx.clone(),
            })
        }

        Some(Command::Devices) => Ok(Request::ListDevices { ctx: ctx.clone() }),

        Some(Command::Use { device: dev }) => {
            let selector = if dev.contains('/') || dev.contains('\\') || dev.contains("hid") {
                DeviceSelector::UsbPath(dev.clone())
            } else {
                DeviceSelector::BleName(dev.clone())
            };
            Ok(Request::UseDevice {
                selector,
                transport: ctx.transport,
                ctx: ctx.clone(),
            })
        }

        Some(Command::Profile(cmd)) => build_profile_request(cmd, ctx),

        Some(Command::Dpi(args)) => {
            let profile = cli.resolve_profile(None).unwrap_or(1);
            if args.is_write() {
                Ok(Request::SetDpi {
                    profile,
                    stages: parse_dpi_values(&args.stages)?,
                    active_stage: args.active,
                    sensor: build_sensor_delta(args),
                    ctx: ctx.clone(),
                })
            } else {
                Ok(Request::ReadDpi {
                    profile,
                    ctx: ctx.clone(),
                })
            }
        }

        Some(Command::Rate(args)) => {
            if let Some(hz) = args.hz {
                Ok(Request::SetRate {
                    hz,
                    ctx: ctx.clone(),
                })
            } else {
                Ok(Request::ReadRate { ctx: ctx.clone() })
            }
        }

        Some(Command::Prefs(args)) => {
            let profile = cli.resolve_profile(None).unwrap_or(1);
            if args.is_write() {
                Ok(Request::SetPreferences {
                    profile,
                    prefs: build_preferences_delta(args)?,
                    ctx: ctx.clone(),
                })
            } else {
                Ok(Request::ReadPreferences {
                    profile,
                    ctx: ctx.clone(),
                })
            }
        }

        Some(Command::Bind(args)) => {
            let profile = cli.resolve_profile(None).unwrap_or(1);
            args.validate()?;
            if args.is_write() {
                let (index, assignment) = build_button_assignment(args)?;
                Ok(Request::SetButton {
                    profile,
                    index,
                    assignment,
                    ctx: ctx.clone(),
                })
            } else {
                Ok(Request::ReadButtons {
                    profile,
                    ctx: ctx.clone(),
                })
            }
        }

        Some(Command::Battery) => Ok(Request::Battery { ctx: ctx.clone() }),

        Some(Command::Reset(args)) => Ok(Request::Reset {
            profile: args.profile,
            ctx: ctx.clone(),
        }),

        Some(Command::Apply { .. }) => Ok(Request::Apply { ctx: ctx.clone() }),

        Some(Command::Export { .. }) => Ok(Request::Export { ctx: ctx.clone() }),

        Some(Command::Disconnect) => Ok(Request::Disconnect),

        Some(Command::State(_)) | Some(Command::Daemon(_)) | Some(Command::Debug(_)) => {
            Err("internal: unexpected command in build_request".into())
        }
    }
}

fn build_profile_request(cmd: &ProfileCommand, ctx: &ExecutionContext) -> Result<Request, String> {
    match cmd {
        ProfileCommand::Use { profile } => Ok(Request::ProfileUse {
            profile: *profile,
            ctx: ctx.clone(),
        }),
        ProfileCommand::Max { max } => Ok(Request::ProfileMax {
            max: *max,
            ctx: ctx.clone(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Daemon commands (no auto-start)
// ---------------------------------------------------------------------------

async fn run_daemon(cli: &Cli, out: &Output) -> Result<(), String> {
    match &cli.command {
        Some(Command::Daemon(cmd)) => match cmd {
            DaemonCommand::Status => match broker::connect_and_send(Request::DaemonStatus).await {
                Ok(resp) => {
                    out.render_response(&resp);
                    Ok(())
                }
                Err(e) => Err(format!("daemon is not running ({e})")),
            },
            DaemonCommand::Stop => match broker::connect_and_send(Request::DaemonStop).await {
                Ok(resp) => {
                    out.render_response(&resp);
                    Ok(())
                }
                Err(e) => Err(format!("daemon is not running ({e})")),
            },
        },
        _ => unreachable!(),
    }
}

// ---------------------------------------------------------------------------
// State commands (local-only)
// ---------------------------------------------------------------------------

fn run_state(cmd: &StateCommand, out: &Output) -> Result<(), String> {
    match cmd {
        StateCommand::InitDefaults => {
            let mut state_file = state::load().map_err(|e| format!("state: {e}"))?;
            let dev_key = state::selected_device(&state_file)
                .ok_or("no device selected — use `x3ctl use <device>` first")?
                .to_owned();

            // Populate explicit defaults for profile 1 (the typical default).
            state_init_profile_defaults(&mut state_file, &dev_key, 1);
            state::save(&state_file).map_err(|e| format!("state: {e}"))?;

            out.human("Initialized explicit defaults for the selected device.");
            Ok(())
        }
        StateCommand::Forget { device } => {
            let mut state_file = state::load().map_err(|e| format!("state: {e}"))?;
            let key = match device {
                Some(d) => d.clone(),
                None => state::selected_device(&state_file)
                    .ok_or("no device selected and no --device provided — nothing to forget")?
                    .to_owned(),
            };

            let removed = state::forget_device(&mut state_file, &key);
            state::save(&state_file).map_err(|e| format!("state: {e}"))?;

            if removed {
                out.human(&format!("Forgot device '{key}'."));
            } else {
                out.human(&format!(
                    "Device '{key}' was not in state — nothing to forget."
                ));
            }
            Ok(())
        }
    }
}

/// Populate profile-id defaults with explicit built-in values, marking them
/// with [`state::StateSource::ExplicitDefaults`].
fn state_init_profile_defaults(state_file: &mut state::StateFile, dev_key: &str, profile_id: u8) {
    use state::{
        StateSource, StateVerification, StoredButtonsState, StoredDpiState, StoredPreferencesState,
        VersionedState, profile_mut,
    };

    let profile = match profile_mut(state_file, dev_key, profile_id) {
        Some(p) => p,
        None => return,
    };

    let now = state::utc_now_iso8601();
    let source = StateSource::ExplicitDefaults;
    let verification = StateVerification::PersistenceUnknown;

    // Populate DPI defaults if absent.
    if profile.value.dpi.is_none() {
        profile.value.dpi = Some(VersionedState {
            value: StoredDpiState::default(),
            source,
            verification,
            updated_at: now.clone(),
        });
    }

    // Populate preferences defaults if absent.
    if profile.value.preferences.is_none() {
        profile.value.preferences = Some(VersionedState {
            value: StoredPreferencesState::default(),
            source,
            verification,
            updated_at: now.clone(),
        });
    }

    // Populate button defaults if absent.
    if profile.value.buttons.is_none() {
        profile.value.buttons = Some(VersionedState {
            value: StoredButtonsState::default(),
            source,
            verification,
            updated_at: now,
        });
    }
}

// ---------------------------------------------------------------------------
// Dry-run: validate and print
// ---------------------------------------------------------------------------

fn dry_run_print(request: &Request, out: &Output) -> Result<(), String> {
    out.human_section("Dry-run — would send:");
    let json_val = serde_json::to_value(request).map_err(|e| e.to_string())?;
    if out.format() == args::OutputFormat::Human {
        println!(
            "{}",
            serde_json::to_string_pretty(&json_val).unwrap_or_default()
        );
    }
    out.json(&json_val);
    out.human("  [dry-run: no hardware or broker was contacted]");
    Ok(())
}

// ---------------------------------------------------------------------------
// Direct execution (in-process executor, no broker)
// ---------------------------------------------------------------------------

async fn execute_direct(request: Request, no_state: bool, out: &Output) -> Result<(), String> {
    let exec = if no_state {
        executor::Executor::new()
    } else {
        executor::Executor::with_state(Some(state::state_path()))
    };

    let response = exec.handle(request).await;
    out.render_response(&response);
    Ok(())
}

// ---------------------------------------------------------------------------
// Broker-backed execution (auto-start if needed)
// ---------------------------------------------------------------------------

const BROKER_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const BROKER_RETRY_INTERVAL: Duration = Duration::from_millis(500);

async fn execute_via_broker(request: Request, out: &Output) -> Result<(), String> {
    match broker::connect_and_send(request.clone()).await {
        Ok(resp) => {
            out.render_response(&resp);
            return Ok(());
        }
        Err(e) => {
            if !is_connection_refused(&e) {
                return Err(format!("broker error: {e}"));
            }
        }
    }

    auto_start_broker()?;

    let deadline = tokio::time::Instant::now() + BROKER_STARTUP_TIMEOUT;
    loop {
        tokio::time::sleep(BROKER_RETRY_INTERVAL).await;

        match broker::connect_and_send(request.clone()).await {
            Ok(resp) => {
                out.render_response(&resp);
                return Ok(());
            }
            Err(e) => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(format!(
                        "broker did not start within {} seconds (last error: {e})",
                        BROKER_STARTUP_TIMEOUT.as_secs()
                    ));
                }
            }
        }
    }
}

fn auto_start_broker() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find own executable: {e}"))?;

    let mut cmd = std::process::Command::new(exe);
    cmd.arg("__broker");

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    }

    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    cmd.spawn()
        .map_err(|e| format!("failed to start broker: {e}"))?;
    Ok(())
}

fn is_connection_refused(e: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    matches!(
        e.kind(),
        ErrorKind::ConnectionRefused
            | ErrorKind::NotFound
            | ErrorKind::ConnectionReset
            | ErrorKind::BrokenPipe
    )
}

// ---------------------------------------------------------------------------
// Debug subcommand implementations
// ---------------------------------------------------------------------------

fn run_debug(cmd: &DebugCommand, out: &Output) -> Result<(), String> {
    match cmd {
        DebugCommand::Dpi(args) => debug_dpi(args, out),
        DebugCommand::ProfileControl(args) => debug_profile_control(args, out),
        DebugCommand::ReadSelector(args) => debug_read_selector(args, out),
    }
}

fn debug_dpi(args: &args::DebugDpiArgs, out: &Output) -> Result<(), String> {
    let transport = transport_kind(args.transport);
    let profile = ProfileId::try_from(args.profile).map_err(|e| e.to_string())?;
    let stages: Vec<DpiValue> = args
        .stages
        .iter()
        .map(|&s| DpiValue::try_from(s).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    let active = StageIndex::try_from(args.active).map_err(|e| e.to_string())?;

    let sensor = SensorOptions {
        lift_off_distance: match args.lod {
            LodArg::One => LiftOffDistance::OneMillimeter,
            LodArg::Two => LiftOffDistance::TwoMillimeters,
        },
        ripple_control: args.ripple_control,
        angle_snap: args.angle_snap,
        motion_sync: args.motion_sync,
    };

    let empty = DpiState::captured_empty_profile_one(stages, active).map_err(|e| e.to_string())?;
    let mut state = DpiState::new(profile, empty.stages, active, empty.preserved_tail)
        .map_err(|e| e.to_string())?;
    state.sensor = sensor;

    let report = DpiReport::encode(&state, transport).map_err(|e| e.to_string())?;
    out.hex_dump("DPI packet", report.as_bytes());
    Ok(())
}

fn debug_profile_control(args: &args::DebugProfileControlArgs, out: &Output) -> Result<(), String> {
    let current = ProfileId::try_from(args.current).map_err(|e| e.to_string())?;
    let maximum = ProfileId::try_from(args.maximum).map_err(|e| e.to_string())?;
    let metadata = ProfileMetadata::new(current, maximum).map_err(|e| e.to_string())?;
    let framing = match args.framing {
        ProfileFramingArg::Compact => ProfileControlFraming::Compact,
        ProfileFramingArg::Full => ProfileControlFraming::Full,
    };
    let report = ProfileControlReport::encode(metadata, framing);
    out.hex_dump("profile-control packet", report.as_bytes());
    Ok(())
}

fn debug_read_selector(args: &args::DebugReadSelectorArgs, out: &Output) -> Result<(), String> {
    let profile = args
        .profile
        .map(|p| ProfileId::try_from(p).map_err(|e| e.to_string()))
        .transpose()?;

    let selector = match args.report {
        DebugReadReportArg::Version => ReadSelector::encode(ReadbackRequest::Version),
        DebugReadReportArg::ProfileMetadata => {
            ReadSelector::encode(ReadbackRequest::ProfileMetadata)
        }
        DebugReadReportArg::PollingRate => ReadSelector::encode(ReadbackRequest::PollingRate),
        DebugReadReportArg::Dpi => {
            let p = profile.ok_or("--profile is required for DPI read-selector")?;
            ReadSelector::encode(ReadbackRequest::Dpi(p))
        }
        DebugReadReportArg::Preferences => {
            let p = profile.ok_or("--profile is required for preferences read-selector")?;
            ReadSelector::encode(ReadbackRequest::Preferences(p))
        }
        DebugReadReportArg::Buttons => {
            let p = profile.ok_or("--profile is required for buttons read-selector")?;
            ReadSelector::encode(ReadbackRequest::Buttons(p))
        }
    };

    out.hex_dump("read-selector packet", selector.as_bytes());
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers: request construction
// ---------------------------------------------------------------------------
fn transport_kind(arg: TransportArg) -> TransportKind {
    match arg {
        TransportArg::Auto => TransportKind::Wired,
        TransportArg::Wired => TransportKind::Wired,
        TransportArg::Receiver => TransportKind::Receiver,
        #[cfg(feature = "ble")]
        TransportArg::Ble => TransportKind::Ble,
    }
}

/// Parse comma-separated DPI values into a `Vec<u16>`.
fn parse_dpi_values(raw: &Option<String>) -> Result<Option<Vec<u16>>, String> {
    let Some(s) = raw else { return Ok(None) };
    let parts: Vec<&str> = s
        .split(',')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        return Ok(None);
    }
    let values: Vec<u16> = parts
        .iter()
        .map(|&p| {
            p.parse::<u16>()
                .map_err(|_| format!("invalid DPI value: '{p}'"))
        })
        .collect::<Result<_, _>>()?;
    Ok(Some(values))
}

fn build_sensor_delta(args: &args::DpiArgs) -> Option<SensorOptionsDelta> {
    let has_any = args.lod.is_some()
        || args.ripple_control.is_some()
        || args.angle_snap.is_some()
        || args.motion_sync.is_some();
    if !has_any {
        return None;
    }
    Some(SensorOptionsDelta {
        lift_off_distance: args.lod.map(|l| match l {
            LodArg::One => 1,
            LodArg::Two => 2,
        }),
        ripple_control: args.ripple_control.map(|t| t.into()),
        angle_snap: args.angle_snap.map(|t| t.into()),
        motion_sync: args.motion_sync.map(|t| t.into()),
    })
}

fn build_preferences_delta(args: &args::PrefsArgs) -> Result<PreferencesDelta, String> {
    let light_mode = if let Some(raw) = &args.light_mode_raw {
        let without_prefix = raw.strip_prefix("0x").unwrap_or(raw);
        Some(
            u8::from_str_radix(without_prefix, 16)
                .map_err(|e| format!("invalid light-mode-raw: {e}"))?,
        )
    } else {
        args.light_mode.map(|m| match m {
            args::LightModeArg::Off => 0,
            args::LightModeArg::Static => 1,
            args::LightModeArg::Breathing => 2,
            args::LightModeArg::Neon => 3,
            args::LightModeArg::ColorBreathing => 4,
            args::LightModeArg::StaticDpi => 5,
            args::LightModeArg::BreathingDpi => 6,
        })
    };

    Ok(PreferencesDelta {
        light_mode,
        configuration: None,
        deep_sleep: args.deep_sleep,
        host_color: None,
        sleep_timer: args.sleep.map(|s| (s * 2.0) as u8), // half-minute steps → internal units
        debounce: args.debounce,
    })
}

fn build_button_assignment(args: &args::BindArgs) -> Result<(u8, ButtonPayload), String> {
    let button = args.button.unwrap();
    let action = args.action.unwrap();

    let index = match button {
        args::ButtonArg::Left => 0,
        args::ButtonArg::Right => 1,
        args::ButtonArg::Middle => 2,
        args::ButtonArg::Dpi => 3,
        args::ButtonArg::Forward => 6,
        args::ButtonArg::Backward => 7,
    };

    let (action_byte, modifier) = match action {
        args::ButtonActionArg::Disable => (0x01, 0x00),
        args::ButtonActionArg::LeftClick => (0x02, 0x00),
        args::ButtonActionArg::RightClick => (0x03, 0x00),
        args::ButtonActionArg::MiddleClick => (0x04, 0x00),
        args::ButtonActionArg::Backward => (0x05, 0x00),
        args::ButtonActionArg::Forward => (0x06, 0x00),
        args::ButtonActionArg::DoubleClick => (0x07, 0x00),
        args::ButtonActionArg::DpiCycle => (0x0d, 0x00),
        args::ButtonActionArg::DpiPlus => (0x0e, 0x00),
        args::ButtonActionArg::DpiMinus => (0x0f, 0x00),
        args::ButtonActionArg::ProfileCycle => (0x34, 0x00),
        args::ButtonActionArg::ProfilePlus => (0x35, 0x00),
        args::ButtonActionArg::ProfileMinus => (0x36, 0x00),
    };

    Ok((
        index,
        ButtonPayload {
            action: action_byte,
            modifier,
            key_code: 0,
        },
    ))
}
