use std::{
    error::Error,
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread,
    time::Duration,
};

use attack_shark_x3_manager::{
    ApplicationVerification, BaselineSource, ButtonSlotDelta, DeviceEvent, DeviceId,
    DeviceIdentity, DeviceManager, DpiDelta, DpiValue, EventSubscriptions, ManagerError,
    PollingRate, ProfileId, ProfileMetadata, SafeButtonAction, SafeButtonSlot, StageIndex,
    StateStore, TransportKind, TransportSelection, UpdatePolicy, VerificationMethod,
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

slint::include_modules!();

const BINDING_ACTIONS: [&str; 49] = [
    "disabled",
    "left click",
    "right click",
    "middle click",
    "double click",
    "dpi cycle",
    "dpi plus",
    "dpi minus",
    "profile cycle",
    "profile plus",
    "profile minus",
    "forward",
    "backward",
    "fire button",
    "scroll up",
    "scroll down",
    "media player",
    "previous track",
    "next track",
    "play/pause",
    "stop",
    "mute",
    "volume up",
    "volume down",
    "calculator",
    "email",
    "browser forward",
    "browser backward",
    "browser stop",
    "my computer",
    "browser refresh",
    "browser home",
    "browser search",
    "browser favorites",
    "cut",
    "copy",
    "paste",
    "open",
    "save",
    "find",
    "redo",
    "select all",
    "print",
    "close window",
    "swap windows",
    "show desktop",
    "run command",
    "lock pc",
    "screen capture",
];
const SAFE_BUTTON_SLOTS: [SafeButtonSlot; 6] = [
    SafeButtonSlot::Left,
    SafeButtonSlot::Right,
    SafeButtonSlot::Middle,
    SafeButtonSlot::Dpi,
    SafeButtonSlot::Forward,
    SafeButtonSlot::Backward,
];
const MAX_DPI_STAGES: usize = 8;
const DPI_MIN: f32 = 50.0;
const DPI_MAX: f32 = 26_000.0;
const DPI_STEP: f32 = 50.0;
const DPI_VALUES: [&str; MAX_DPI_STAGES] = [
    "800", "1200", "1600", "2000", "2400", "3200", "12000", "26000",
];
const DPI_LABELS: [&str; MAX_DPI_STAGES] = [
    "stage 01", "stage 02", "stage 03", "stage 04", "stage 05", "stage 06", "stage 07", "stage 08",
];

#[derive(Debug)]
enum Command {
    Startup,
    Refresh,
    SelectProfile(u8),
    Apply(Draft),
    Discard,
    Shutdown,
}

#[derive(Clone, Copy, Debug)]
enum VerificationChoice {
    Transport,
    Readback,
}

#[derive(Clone, Debug)]
struct Draft {
    profile: u8,
    dpi_values: Vec<u16>,
    active_stage: u8,
    buttons: Vec<String>,
    polling_rate_hz: u16,
    verification: VerificationChoice,
}
struct ProfileData {
    dpi: attack_shark_x3_manager::DpiState,
    buttons: attack_shark_x3_manager::ButtonsState,
}
struct LoadedDevice {
    device: DeviceId,
    identity: DeviceIdentity,
    metadata: ProfileMetadata,
    profile: ProfileData,
    polling_rate: PollingRate,
    battery: Option<u8>,
    polling_rate_ready: bool,
}

struct LiveProfile {
    number: u8,
    enabled: bool,
    current: bool,
}
struct LiveSnapshot {
    device_name: String,
    stable_id: String,
    product_id: String,
    transport: String,
    battery: String,
    selected_profile: u8,
    active_dpi: u16,
    active_stage: u8,
    stage_count: usize,
    polling_rate_hz: u16,
    polling_rate_ready: bool,
    motion_sync: bool,
    ripple_control: bool,
    sensor: String,
    profile_summary: String,
    metadata_summary: String,
    verification_summary: String,
    stages: Vec<(u16, bool)>,
    bindings: Vec<(String, String, String)>,
    profiles: Vec<LiveProfile>,
    status: String,
}

enum UiEvent {
    Busy(String),
    Snapshot(Box<LiveSnapshot>),
    Error(String),
}

fn main() -> Result<(), Box<dyn Error>> {
    let ui = AppWindow::new()?;
    let dpi_stages = std::rc::Rc::new(VecModel::from(Vec::<DpiStage>::new()));
    let bindings = std::rc::Rc::new(VecModel::from(Vec::<BindingRow>::new()));
    let profiles = std::rc::Rc::new(VecModel::from(Vec::<ProfileRow>::new()));
    ui.set_dpi_stages(ModelRc::from(dpi_stages.clone()));
    ui.set_bindings(ModelRc::from(bindings.clone()));
    ui.set_profiles(ModelRc::from(profiles.clone()));
    ui.set_hardware_ready(false);
    ui.set_busy(true);
    ui.set_lifecycle_text("starting USB discovery".into());
    ui.set_status_text("discovering a compatible USB device — no hardware state loaded yet".into());
    ui.set_transport_label("USB not selected".into());
    ui.set_battery_text("unavailable".into());
    ui.set_dpi_min(DPI_MIN);
    ui.set_dpi_max(DPI_MAX);
    ui.set_dpi_min_text(format_dpi_setting(DPI_MIN).into());
    ui.set_dpi_max_text(format_dpi_setting(DPI_MAX).into());

    let (commands, receiver) = mpsc::channel();
    let weak = ui.as_weak();
    let worker = thread::Builder::new()
        .name("x3-gui-manager".into())
        .spawn(move || worker_main(receiver, weak))?;
    commands.send(Command::Startup)?;

    install_callbacks(
        &ui,
        commands.clone(),
        dpi_stages.clone(),
        bindings.clone(),
        profiles.clone(),
    );
    let run_result = ui.run();
    let _ = commands.send(Command::Shutdown);
    let worker_result = worker
        .join()
        .map_err(|_| std::io::Error::other("x3 manager worker terminated unexpectedly"))?;
    run_result?;
    worker_result.map_err(|_| std::io::Error::other("x3 manager worker failed"))?;
    Ok(())
}

fn install_callbacks(
    ui: &AppWindow,
    commands: Sender<Command>,
    dpi_stages: std::rc::Rc<VecModel<DpiStage>>,
    bindings: std::rc::Rc<VecModel<BindingRow>>,
    profiles: std::rc::Rc<VecModel<ProfileRow>>,
) {
    {
        let weak = ui.as_weak();
        ui.on_navigate(move |page| {
            if let Some(ui) = weak.upgrade() {
                ui.set_current_page(page);
                ui.set_status_text(page_status(page).into());
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_open_config(move || {
            if let Some(ui) = weak.upgrade() {
                let page = if ui.get_current_page() == 5 { 0 } else { 5 };
                ui.set_current_page(page);
                ui.set_status_text(page_status(page).into());
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_select_profile({
            let commands = commands.clone();
            move |selected| {
                let Some(ui) = weak.upgrade() else { return };
                if !ui.get_hardware_ready() || ui.get_busy() {
                    return;
                }
                if ui.get_dirty() {
                    ui.set_status_text(
                        "discard the current draft before switching profiles".into(),
                    );
                    return;
                }
                let Some(row) = profiles.row_data(selected.max(0) as usize) else {
                    return;
                };
                if !row.enabled {
                    ui.set_status_text("that profile slot is not enabled by the device".into());
                    return;
                }
                ui.set_busy(true);
                ui.set_lifecycle_text("switching profile".into());
                ui.set_status_text(format!("activating profile {}…", selected + 1).into());
                queue_command(&ui, &commands, Command::SelectProfile((selected + 1) as u8));
            }
        });
    }
    {
        let weak = ui.as_weak();
        let dpi_stages = dpi_stages.clone();
        ui.on_select_dpi(move |selected| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if !ui.get_hardware_ready() || ui.get_busy() || selected < 0 {
                return;
            }
            let selected = selected as usize;
            if selected >= dpi_stages.row_count() {
                return;
            }
            set_active_dpi_stage(&dpi_stages, selected);
            ui.set_active_dpi(selected as i32);
            ui.set_dirty(true);
            ui.set_status_text("DPI active stage changed in the draft — save to apply".into());
        });
    }
    {
        let weak = ui.as_weak();
        let dpi_stages = dpi_stages.clone();
        ui.on_drag_dpi(move |row, ratio| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if !ui.get_hardware_ready() || ui.get_busy() || row < 0 {
                return;
            }
            let row = row as usize;
            let Some(mut stage) = dpi_stages.row_data(row) else {
                return;
            };
            let dpi = dpi_from_ratio(
                ratio,
                ui.get_dpi_log_scale(),
                ui.get_dpi_min(),
                ui.get_dpi_max(),
            );
            set_active_dpi_stage(&dpi_stages, row);
            stage.active = true;
            stage.dpi = dpi;
            stage.value = format_dpi_setting(dpi).into();
            dpi_stages.set_row_data(row, stage);
            ui.set_active_dpi(row as i32);
            refresh_dpi_ratios(
                &dpi_stages,
                ui.get_dpi_log_scale(),
                ui.get_dpi_min(),
                ui.get_dpi_max(),
            );
            ui.set_dirty(true);
            ui.set_status_text("DPI ladder changed in the draft — save to apply".into());
        });
    }
    {
        let weak = ui.as_weak();
        let dpi_stages = dpi_stages.clone();
        ui.on_toggle_dpi_scale(move || {
            if let Some(ui) = weak.upgrade() {
                let logarithmic = !ui.get_dpi_log_scale();
                ui.set_dpi_log_scale(logarithmic);
                refresh_dpi_ratios(&dpi_stages, logarithmic, ui.get_dpi_min(), ui.get_dpi_max());
                ui.set_status_text(
                    "visual DPI scale changed locally only; hardware draft unchanged".into(),
                );
            }
        });
    }
    {
        let weak = ui.as_weak();
        let dpi_stages = dpi_stages.clone();
        ui.on_dpi_min_changed(move |text| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let Some(value) = parse_dpi_setting(text.as_str()) else {
                ui.set_dpi_min_text(format_dpi_setting(ui.get_dpi_min()).into());
                ui.set_status_text("visual minimum DPI must be between 50 and 26,000".into());
                return;
            };
            if value >= ui.get_dpi_max() {
                ui.set_dpi_min_text(format_dpi_setting(ui.get_dpi_min()).into());
                ui.set_status_text("visual minimum DPI must be lower than maximum DPI".into());
                return;
            }
            ui.set_dpi_min(value);
            ui.set_dpi_min_text(format_dpi_setting(value).into());
            refresh_dpi_ratios(&dpi_stages, ui.get_dpi_log_scale(), value, ui.get_dpi_max());
            ui.set_status_text(
                "visual DPI range changed locally only; hardware draft unchanged".into(),
            );
        });
    }
    {
        let weak = ui.as_weak();
        let dpi_stages = dpi_stages.clone();
        ui.on_dpi_max_changed(move |text| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let Some(value) = parse_dpi_setting(text.as_str()) else {
                ui.set_dpi_max_text(format_dpi_setting(ui.get_dpi_max()).into());
                ui.set_status_text("visual maximum DPI must be between 50 and 26,000".into());
                return;
            };
            if value <= ui.get_dpi_min() {
                ui.set_dpi_max_text(format_dpi_setting(ui.get_dpi_max()).into());
                ui.set_status_text("visual maximum DPI must be higher than minimum DPI".into());
                return;
            }
            ui.set_dpi_max(value);
            ui.set_dpi_max_text(format_dpi_setting(value).into());
            refresh_dpi_ratios(&dpi_stages, ui.get_dpi_log_scale(), ui.get_dpi_min(), value);
            ui.set_status_text(
                "visual DPI range changed locally only; hardware draft unchanged".into(),
            );
        });
    }
    {
        let weak = ui.as_weak();
        let bindings = bindings.clone();
        ui.on_cycle_binding(move |row| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if !ui.get_hardware_ready() || ui.get_busy() || row < 0 {
                return;
            }
            let row = row as usize;
            let Some(mut item) = bindings.row_data(row) else {
                return;
            };
            let current = BINDING_ACTIONS
                .iter()
                .position(|action| *action == item.action.as_str());
            let Some(current) = current else {
                ui.set_status_text(
                    "this device assignment is unsupported and cannot be edited".into(),
                );
                return;
            };
            item.action = BINDING_ACTIONS[(current + 1) % BINDING_ACTIONS.len()].into();
            bindings.set_row_data(row, item);
            ui.set_dirty(true);
            ui.set_status_text(
                "safe button assignment changed in the draft — save to apply".into(),
            );
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_toggle_theme(move || {
            if let Some(ui) = weak.upgrade() {
                let theme = ui.global::<Theme>();
                theme.set_dark_mode(!theme.get_dark_mode());
                ui.set_status_text(
                    "theme preference is local-only; hardware draft unchanged".into(),
                );
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_toggle_shell(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_white_mouse(!ui.get_white_mouse());
                ui.set_status_text(
                    "shell artwork preference is local-only; hardware draft unchanged".into(),
                );
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_select_product_name(move |choice| {
            if let Some(ui) = weak.upgrade() {
                ui.set_product_name_choice(choice.clamp(0, 2));
                ui.set_status_text(
                    "product label preference is local-only; live identity remains unchanged"
                        .into(),
                );
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_custom_name_changed(move |name| {
            if let Some(ui) = weak.upgrade() {
                let name = name.trim();
                if !name.is_empty() {
                    ui.set_custom_product_name(name.into());
                    ui.set_status_text(
                        "custom label is local-only; live identity remains unchanged".into(),
                    );
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_set_validation_choice(move |choice| {
            if let Some(ui) = weak.upgrade() {
                ui.set_validation_choice(choice.clamp(0, 1));
                ui.set_status_text(if choice == 1 {
                    "readback verification selected for the next USB write".into()
                } else {
                    "transport submission selected for the next USB write".into()
                });
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_rate_changed(move |rate| {
            if let Some(ui) = weak.upgrade()
                && ui.get_hardware_ready()
                && !ui.get_busy()
            {
                ui.set_selected_rate_hz(rate);
                ui.set_dirty(true);
                ui.set_status_text(
                    "polling-rate choice changed in the draft — save to apply".into(),
                );
            }
        });
    }
    {
        let weak = ui.as_weak();
        let commands = commands.clone();
        let dpi_stages = dpi_stages.clone();
        let bindings = bindings.clone();
        ui.on_request_save(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            let mut values = Vec::with_capacity(dpi_stages.row_count());
            for row in 0..dpi_stages.row_count() {
                let Some(stage) = dpi_stages.row_data(row) else {
                    return;
                };
                values.push(stage.dpi.round().clamp(DPI_MIN, DPI_MAX) as u16);
            }
            let buttons = (0..bindings.row_count())
                .filter_map(|row| bindings.row_data(row).map(|item| item.action.to_string()))
                .collect();
            let draft = Draft {
                profile: (ui.get_selected_profile() + 1).clamp(1, 5) as u8,
                dpi_values: values,
                active_stage: (ui.get_active_dpi() + 1).clamp(1, 8) as u8,
                buttons,
                polling_rate_hz: ui.get_selected_rate_hz() as u16,
                verification: if ui.get_validation_choice() == 1 {
                    VerificationChoice::Readback
                } else {
                    VerificationChoice::Transport
                },
            };
            ui.set_busy(true);
            ui.set_lifecycle_text("applying safe manager operations".into());
            ui.set_status_text("submitting the draft through DeviceManager…".into());
            queue_command(&ui, &commands, Command::Apply(draft));
        });
    }
    {
        let weak = ui.as_weak();
        let commands = commands.clone();
        ui.on_request_reset(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            ui.set_busy(true);
            ui.set_lifecycle_text("reloading device state".into());
            ui.set_status_text("discarding the local draft and re-reading the device".into());
            queue_command(&ui, &commands, Command::Discard);
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_request_refresh(move || {
            if let Some(ui) = weak.upgrade() {
                if ui.get_busy() {
                    return;
                }
                if ui.get_dirty() {
                    ui.set_status_text(
                        "Refresh would discard the draft; use discard / reload explicitly".into(),
                    );
                    return;
                }
                ui.set_busy(true);
                ui.set_lifecycle_text("refreshing USB state".into());
                ui.set_status_text("re-discovering and reading the device".into());
                if commands.send(Command::Refresh).is_err() {
                    ui.set_busy(false);
                    ui.set_hardware_ready(false);
                    ui.set_status_text(
                        "manager worker is unavailable; restart the application".into(),
                    );
                }
            }
        });
    }
}

fn queue_command(ui: &AppWindow, commands: &Sender<Command>, command: Command) {
    if commands.send(command).is_err() {
        ui.set_busy(false);
        ui.set_hardware_ready(false);
        ui.set_status_text("manager worker is unavailable; restart the application".into());
    }
}

fn worker_main(receiver: Receiver<Command>, weak: slint::Weak<AppWindow>) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|error| format!("could not start Tokio manager runtime: {error}"))?;
    let mut manager = None;
    let mut loaded = None;
    let mut events: Option<tokio::sync::broadcast::Receiver<DeviceEvent>> = None;
    let mut subscriptions: Option<EventSubscriptions> = None;

    loop {
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(command) => match command {
                Command::Startup | Command::Refresh => {
                    emit(&weak, UiEvent::Busy("discovering USB devices".into()));
                    match startup(&runtime) {
                        Ok((new_manager, new_loaded)) => {
                            let subscribed =
                                runtime.block_on(new_manager.subscribe_events(&new_loaded.device));
                            match subscribed {
                                Ok(mut sub) => {
                                    events = sub.events.take();
                                    subscriptions = Some(sub);
                                }
                                Err(_) => {
                                    events = None;
                                    subscriptions = None;
                                }
                            }
                            manager = Some(new_manager);
                            loaded = Some(new_loaded);
                            if let Some(ready) = loaded.as_ref() {
                                emit_snapshot(&weak, ready, "USB device state loaded");
                            }
                        }
                        Err(error) => {
                            manager = None;
                            loaded = None;
                            emit(&weak, UiEvent::Error(error));
                        }
                    }
                }
                Command::SelectProfile(number) => {
                    let Some(manager_ref) = manager.as_ref() else {
                        emit(
                            &weak,
                            UiEvent::Error(
                                "manager is not initialized; press Refresh to retry".into(),
                            ),
                        );
                        continue;
                    };
                    let Some(current) = loaded.as_ref() else {
                        emit(
                            &weak,
                            UiEvent::Error(
                                "no USB device is loaded; press Refresh to retry".into(),
                            ),
                        );
                        continue;
                    };
                    emit(&weak, UiEvent::Busy(format!("activating profile {number}")));
                    match select_profile(&runtime, manager_ref, current, number) {
                        Ok(new_loaded) => {
                            loaded = Some(new_loaded);
                            if let Some(ready) = loaded.as_ref() {
                                emit_snapshot(&weak, ready, "profile readback loaded");
                            }
                        }
                        Err(error) => emit(&weak, UiEvent::Error(error)),
                    }
                }
                Command::Apply(draft) => {
                    let Some(manager_ref) = manager.as_ref() else {
                        emit(
                            &weak,
                            UiEvent::Error(
                                "manager is not initialized; press Refresh to retry".into(),
                            ),
                        );
                        continue;
                    };
                    let Some(current) = loaded.as_ref() else {
                        emit(
                            &weak,
                            UiEvent::Error(
                                "no USB device is loaded; press Refresh to retry".into(),
                            ),
                        );
                        continue;
                    };
                    match apply_draft(&runtime, manager_ref, current, &draft) {
                        Ok((status, profile)) => {
                            let refreshed = runtime.block_on(read_loaded(
                                manager_ref,
                                &current.device,
                                Some(profile),
                            ));
                            match refreshed {
                                Ok(new_loaded) => {
                                    loaded = Some(new_loaded);
                                    if let Some(ready) = loaded.as_ref() {
                                        emit_snapshot(&weak, ready, &status);
                                    }
                                }
                                Err(error) => {
                                    emit(
                                        &weak,
                                        format_error("write succeeded but refresh failed", error),
                                    );
                                }
                            }
                        }
                        Err(error) => {
                            let refreshed = runtime.block_on(read_loaded(
                                manager_ref,
                                &current.device,
                                Some(current.metadata.current()),
                            ));
                            match refreshed {
                                Ok(new_loaded) => {
                                    loaded = Some(new_loaded);
                                    if let Some(ready) = loaded.as_ref() {
                                        emit_snapshot(
                                            &weak,
                                            ready,
                                            &format!(
                                                "apply failed; earlier section writes may have applied, so live state was re-read: {error}"
                                            ),
                                        );
                                    }
                                }
                                Err(refresh_error) => emit(
                                    &weak,
                                    UiEvent::Error(format!(
                                        "apply failed ({error}); live state could not be re-read ({refresh_error})"
                                    )),
                                ),
                            }
                        }
                    }
                }
                Command::Discard => {
                    let Some(manager_ref) = manager.as_ref() else {
                        emit(
                            &weak,
                            UiEvent::Error(
                                "manager is not initialized; press Refresh to retry".into(),
                            ),
                        );
                        continue;
                    };
                    let Some(current) = loaded.as_ref() else {
                        emit(
                            &weak,
                            UiEvent::Error(
                                "no USB device is loaded; press Refresh to retry".into(),
                            ),
                        );
                        continue;
                    };
                    emit(&weak, UiEvent::Busy("re-reading USB state".into()));
                    match runtime.block_on(read_loaded(
                        manager_ref,
                        &current.device,
                        Some(current.metadata.current()),
                    )) {
                        Ok(new_loaded) => {
                            loaded = Some(new_loaded);
                            if let Some(ready) = loaded.as_ref() {
                                emit_snapshot(
                                    &weak,
                                    ready,
                                    "draft discarded; device state reloaded",
                                );
                            }
                        }
                        Err(error) => {
                            emit(&weak, format_error("could not reload device state", error))
                        }
                    }
                }
                Command::Shutdown => break,
            },
            Err(RecvTimeoutError::Timeout) => {
                if let Some(rx) = events.as_mut() {
                    runtime.block_on(async { tokio::task::yield_now().await });
                    while let Ok(event) = rx.try_recv() {
                        handle_device_event(&runtime, &mut loaded, &mut manager, event, &weak);
                    }
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    // The subscriptions hold the hardware session open; drop them (with the
    // rest of the worker state) only when the worker exits.
    let _ = subscriptions;
    Ok(())
}

fn handle_device_event(
    runtime: &tokio::runtime::Runtime,
    loaded: &mut Option<LoadedDevice>,
    manager: &mut Option<DeviceManager>,
    event: DeviceEvent,
    weak: &slint::Weak<AppWindow>,
) {
    match event {
        DeviceEvent::ActiveDpiStageChanged(e) => {
            if let Some(dev) = loaded.as_mut() {
                dev.profile.dpi.active_stage = e.active_stage;
                emit_snapshot(weak, dev, "active DPI stage changed on device");
            }
        }
        DeviceEvent::ProfileChanged(e) | DeviceEvent::ProfileSync(e) => {
            if let (Some(dev), Some(mgr)) = (loaded.as_mut(), manager.as_ref())
                && e.profile != dev.metadata.current()
            {
                match select_profile(runtime, mgr, dev, e.profile.get()) {
                    Ok(new_loaded) => {
                        *loaded = Some(new_loaded);
                        if let Some(ready) = loaded.as_ref() {
                            emit_snapshot(weak, ready, "device profile changed; state reloaded");
                        }
                    }
                    Err(error) => emit(weak, UiEvent::Error(error)),
                }
            }
        }
        DeviceEvent::BatteryChanged(e) => {
            if let Some(dev) = loaded.as_mut() {
                dev.battery = Some(e.level);
                emit_snapshot(weak, dev, "device battery level changed");
            }
        }
        DeviceEvent::ConnectionChanged(e) if !e.connected => {
            *loaded = None;
            *manager = None;
            emit(weak, UiEvent::Error("device disconnected".into()));
        }
        _ => {}
    }
}

fn emit(weak: &slint::Weak<AppWindow>, event: UiEvent) {
    let weak = weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = weak.upgrade() {
            apply_event(&ui, event);
        }
    });
}

fn emit_snapshot(weak: &slint::Weak<AppWindow>, loaded: &LoadedDevice, status: &str) {
    emit(
        weak,
        UiEvent::Snapshot(Box::new(make_live_snapshot(loaded, status))),
    );
}

fn apply_event(ui: &AppWindow, event: UiEvent) {
    match event {
        UiEvent::Busy(status) => {
            ui.set_busy(true);
            ui.set_lifecycle_text(status.clone().into());
            ui.set_status_text(status.into());
        }
        UiEvent::Error(error) => {
            ui.set_busy(false);
            ui.set_hardware_ready(false);
            ui.set_lifecycle_text("error — no live device state".into());
            ui.set_status_text(error.into());
            ui.set_product_name("USB device not loaded".into());
            ui.set_transport_label("USB not available".into());
            ui.set_polling_rate_ready(false);
            ui.set_motion_sync_readback(false);
            ui.set_ripple_control_readback(false);
            ui.set_battery_text("unavailable".into());
            ui.set_device_name("USB device not loaded".into());
            ui.set_device_id_text("not available".into());
            ui.set_device_product_text("not available".into());
            ui.set_profile_summary("no live profile".into());
            ui.set_metadata_summary("not available".into());
            ui.set_sensor_summary("not available".into());
            ui.set_verification_text("no evidence".into());
            ui.set_active_dpi_text("not available".into());
            ui.set_active_stage_text("not available".into());
            ui.set_polling_rate_text("not available".into());
            ui.set_dirty(false);
            replace_dpi_model(
                &ui.get_dpi_stages(),
                &[],
                ui.get_dpi_log_scale(),
                ui.get_dpi_min(),
                ui.get_dpi_max(),
            );
            replace_binding_model(&ui.get_bindings(), &[]);
            replace_profile_model(&ui.get_profiles(), &[]);
        }
        UiEvent::Snapshot(snapshot) => {
            ui.set_busy(false);
            ui.set_hardware_ready(true);
            ui.set_lifecycle_text("connected — live USB readback loaded".into());
            ui.set_status_text(snapshot.status.clone().into());
            ui.set_product_name(snapshot.device_name.clone().into());
            ui.set_transport_label(snapshot.transport.into());
            ui.set_battery_text(snapshot.battery.into());
            ui.set_device_name(snapshot.device_name.into());
            ui.set_device_id_text(snapshot.stable_id.into());
            ui.set_device_product_text(snapshot.product_id.into());
            ui.set_profile_summary(snapshot.profile_summary.into());
            ui.set_metadata_summary(snapshot.metadata_summary.into());
            ui.set_sensor_summary(snapshot.sensor.into());
            ui.set_verification_text(snapshot.verification_summary.into());
            ui.set_active_dpi_text(format!("{} dpi", snapshot.active_dpi).into());
            ui.set_active_stage_text(
                format!(
                    "stage {} of {}",
                    snapshot.active_stage, snapshot.stage_count
                )
                .into(),
            );
            ui.set_selected_profile((snapshot.selected_profile.saturating_sub(1)) as i32);
            ui.set_active_dpi((snapshot.active_stage.saturating_sub(1)) as i32);
            ui.set_selected_rate_hz(snapshot.polling_rate_hz as i32);
            ui.set_polling_rate_ready(snapshot.polling_rate_ready);
            ui.set_motion_sync_readback(snapshot.motion_sync);
            ui.set_ripple_control_readback(snapshot.ripple_control);
            ui.set_dirty(false);
            replace_dpi_model(
                &ui.get_dpi_stages(),
                &snapshot.stages,
                ui.get_dpi_log_scale(),
                ui.get_dpi_min(),
                ui.get_dpi_max(),
            );
            replace_binding_model(&ui.get_bindings(), &snapshot.bindings);
            replace_profile_model(&ui.get_profiles(), &snapshot.profiles);
        }
    }
}

fn replace_dpi_model(
    model: &ModelRc<DpiStage>,
    stages: &[(u16, bool)],
    logarithmic: bool,
    min: f32,
    max: f32,
) {
    let Some(model) = model.as_any().downcast_ref::<VecModel<DpiStage>>() else {
        return;
    };
    clear_model(model);
    for (index, (value, active)) in stages.iter().copied().enumerate() {
        let mut stage = dpi_stage(
            &format!("{:02}", index + 1),
            &value.to_string(),
            DPI_LABELS.get(index).copied().unwrap_or("device stage"),
            96 + ((index as u8) * 8),
            98 + ((index as u8) * 8),
            104 + ((index as u8) * 8),
            active,
        );
        stage.ratio = dpi_ratio(stage.dpi, logarithmic, min, max);
        model.push(stage);
    }
}

fn replace_binding_model(model: &ModelRc<BindingRow>, bindings: &[(String, String, String)]) {
    let Some(model) = model.as_any().downcast_ref::<VecModel<BindingRow>>() else {
        return;
    };
    clear_model(model);
    for (button, location, action) in bindings {
        model.push(binding(button, location, action));
    }
}

fn replace_profile_model(model: &ModelRc<ProfileRow>, profiles: &[LiveProfile]) {
    let Some(model) = model.as_any().downcast_ref::<VecModel<ProfileRow>>() else {
        return;
    };
    clear_model(model);
    for profile in profiles {
        model.push(ProfileRow {
            name: format!("profile {}", profile.number).into(),
            subtitle: if profile.enabled {
                "enabled device slot"
            } else {
                "not enabled by device"
            }
            .into(),
            active: profile.current,
            visible: true,
            enabled: profile.enabled,
        });
    }
}

fn clear_model<T: Clone + 'static>(model: &VecModel<T>) {
    while model.row_count() > 0 {
        model.remove(0);
    }
}

fn startup(runtime: &tokio::runtime::Runtime) -> Result<(DeviceManager, LoadedDevice), String> {
    let store = StateStore::with_default_paths()
        .map_err(|error| format!("could not open manager state store: {error}"))?;
    let manager = DeviceManager::new(store)
        .map_err(|error| format!("could not initialize DeviceManager: {error}"))?;
    let mut discovered = runtime
        .block_on(manager.list_devices(TransportSelection::Exact(TransportKind::Wired)))
        .map_err(|error| format!("wired USB discovery failed: {error}"))?;
    discovered.extend(
        runtime
            .block_on(manager.list_devices(TransportSelection::Exact(TransportKind::Receiver)))
            .map_err(|error| format!("receiver USB discovery failed: {error}"))?,
    );
    let usb: Vec<_> = discovered
        .iter()
        .filter(|device| {
            matches!(
                device.identity.transport,
                TransportKind::Wired | TransportKind::Receiver
            )
        })
        .collect();
    let (candidate, selection) = match usb.as_slice() {
        [] => {
            return Err(
                "no compatible USB device is connected; connect one and press Refresh".into(),
            );
        }
        [candidate] => (
            candidate.identity.id.clone(),
            TransportSelection::Exact(candidate.identity.transport),
        ),
        many => {
            return Err(format!(
                "multiple compatible USB devices are connected ({}); the in-app device selector is not implemented yet, so disconnect all but the intended device and press Refresh",
                many.len()
            ));
        }
    };
    let device = runtime
        .block_on(manager.resolve_device(Some(&candidate), selection))
        .map_err(|error| format!("USB device resolution failed: {error}"))?;
    let loaded = runtime
        .block_on(read_loaded(&manager, &device, None))
        .map_err(|error| format_error_string("initial USB read failed", error))?;
    Ok((manager, loaded))
}
async fn read_loaded(
    manager: &DeviceManager,
    device: &DeviceId,
    requested: Option<ProfileId>,
) -> Result<LoadedDevice, ManagerError> {
    let status = manager.read_status(device).await?;
    let metadata = status
        .profile_metadata
        .as_ref()
        .and_then(|snapshot| snapshot.resource.observed.as_ref())
        .map(|observed| observed.value)
        .ok_or_else(|| {
            ManagerError::InvalidUpdate(
                "USB status did not include profile metadata readback".into(),
            )
        })?;
    let target = requested
        .filter(|profile| *profile <= metadata.maximum())
        .unwrap_or(metadata.current());
    let profile = manager.read_profile(device, target).await?;
    let polling_rate = status
        .polling_rate
        .as_ref()
        .and_then(|snapshot| snapshot.resource.observed.as_ref())
        .map(|observed| observed.value)
        .ok_or_else(|| {
            ManagerError::InvalidUpdate("USB status did not include polling-rate readback".into())
        })?;
    let state = manager.store().load()?;
    let polling_rate_ready = state
        .devices
        .get(device)
        .and_then(|device| device.profiles.get(&target))
        .is_some_and(|profile| {
            profile.dpi.desired.is_some()
                && profile.preferences.desired.is_some()
                && profile.buttons.desired.is_some()
        });
    Ok(LoadedDevice {
        device: device.clone(),
        identity: status.identity,
        metadata,
        profile: ProfileData {
            dpi: profile.dpi,
            buttons: profile.buttons,
        },
        polling_rate,
        battery: status.battery,
        polling_rate_ready,
    })
}

fn select_profile(
    runtime: &tokio::runtime::Runtime,
    manager: &DeviceManager,
    current: &LoadedDevice,
    number: u8,
) -> Result<LoadedDevice, String> {
    let target = ProfileId::new(number)
        .ok_or_else(|| format!("profile {number} is outside the fixed device range 1..=5"))?;
    if target > current.metadata.maximum() {
        return Err(format!(
            "profile {number} is not enabled (maximum enabled profile is {})",
            current.metadata.maximum()
        ));
    }
    if target == current.metadata.current() {
        return runtime
            .block_on(read_loaded(manager, &current.device, Some(target)))
            .map_err(|error| format_error_string("profile read failed", error));
    }
    runtime
        .block_on(manager.activate_profile(&current.device, target))
        .map_err(|error| format_error_string("profile activation failed", error))?;
    runtime
        .block_on(read_loaded(manager, &current.device, Some(target)))
        .map_err(|error| format_error_string("profile readback failed", error))
}

fn apply_draft(
    runtime: &tokio::runtime::Runtime,
    manager: &DeviceManager,
    current: &LoadedDevice,
    draft: &Draft,
) -> Result<(String, ProfileId), String> {
    let profile = ProfileId::new(draft.profile)
        .ok_or_else(|| "draft profile is outside the fixed device range".to_owned())?;
    if profile != current.metadata.current() {
        return Err(
            "draft targets a profile that is no longer current; reload before applying".into(),
        );
    }
    if current.identity.transport == TransportKind::Ble {
        return Err("BLE configuration readback is unsupported; no write was attempted".into());
    }
    let fresh_status = runtime
        .block_on(manager.read_status(&current.device))
        .map_err(|error| format_error_string("pre-write status read failed", error))?;
    let fresh_current = fresh_status
        .profile_metadata
        .as_ref()
        .and_then(|snapshot| snapshot.resource.observed.as_ref())
        .map(|observed| observed.value.current())
        .ok_or_else(|| {
            "pre-write USB status did not include current-profile readback; no write was attempted"
                .to_owned()
        })?;
    if fresh_current != profile {
        return Err(format!(
            "device profile changed outside the GUI from {profile} to {fresh_current}; no write was attempted; reload the draft"
        ));
    }
    let policy = UpdatePolicy {
        allow_explicit_defaults: false,
        verification: match draft.verification {
            VerificationChoice::Transport => VerificationMethod::Transport,
            VerificationChoice::Readback => VerificationMethod::Readback,
        },
        baseline: BaselineSource::Stored,
    };
    let baseline_dpi: Vec<u16> = current
        .profile
        .dpi
        .stages
        .iter()
        .copied()
        .map(DpiValue::get)
        .collect();
    let requested_rate = PollingRate::new(draft.polling_rate_hz)
        .ok_or_else(|| "unsupported polling rate".to_owned())?;
    if requested_rate != current.polling_rate && !current.polling_rate_ready {
        return Err(
            "polling-rate write is unavailable: the manager does not yet have complete desired DPI, preferences, and buttons for this profile; no hardware write was attempted"
                .into(),
        );
    }
    let dpi_changed = baseline_dpi != draft.dpi_values
        || current.profile.dpi.active_stage.get() != draft.active_stage;
    let baseline_buttons: Vec<String> = SAFE_BUTTON_SLOTS
        .iter()
        .copied()
        .map(|slot| button_action_name(current.profile.buttons.slots[slot.index()]))
        .collect();
    let mut button_changes = Vec::new();
    for (index, requested) in draft
        .buttons
        .iter()
        .enumerate()
        .take(SAFE_BUTTON_SLOTS.len())
    {
        if baseline_buttons
            .get(index)
            .is_some_and(|baseline| baseline != requested)
        {
            let action = safe_button_action(requested).ok_or_else(|| {
                format!("unsupported button action {requested:?}; no write was attempted")
            })?;
            button_changes.push((SAFE_BUTTON_SLOTS[index], action));
        }
    }
    let rate_changed = requested_rate != current.polling_rate;
    if rate_changed && (dpi_changed || !button_changes.is_empty()) {
        return Err(
            "polling-rate changes must be applied separately from DPI or button changes so the manager can validate the complete live profile before report 0x06; no hardware write was attempted"
                .into(),
        );
    }
    let mut evidence = Vec::new();
    if dpi_changed {
        let stages = draft
            .dpi_values
            .iter()
            .copied()
            .map(|value| DpiValue::new(value).ok_or_else(|| format!("invalid DPI value {value}")))
            .collect::<Result<Vec<_>, _>>()?;
        let active_stage = StageIndex::new(draft.active_stage)
            .ok_or_else(|| "invalid active DPI stage".to_owned())?;
        let outcome = runtime
            .block_on(manager.update_dpi_delta(
                &current.device,
                profile,
                DpiDelta {
                    stages: Some(stages),
                    active_stage: Some(active_stage),
                    sensor: None,
                },
                policy,
            ))
            .map_err(|error| format_error_string("DPI update failed", error))?;
        evidence.push(verification_summary(&outcome.verification));
    }

    for (slot, action) in button_changes {
        let outcome = runtime
            .block_on(manager.update_button_slot(
                &current.device,
                profile,
                ButtonSlotDelta::new(slot, action),
                policy,
            ))
            .map_err(|error| format_error_string("button update failed", error))?;
        evidence.push(verification_summary(&outcome.verification));
    }

    let rate = requested_rate;
    if rate_changed {
        // Report 0x06 can persist the complete live profile image. The manager
        // must reject a missing or stale desired baseline rather than the GUI
        // creating one through hidden, redundant hardware writes.
        let outcome = runtime
            .block_on(manager.update_polling_rate(&current.device, profile, rate, policy))
            .map_err(|error| format_error_string("polling-rate update failed", error))?;
        evidence.push(verification_summary(&outcome.verification));
    }
    if evidence.is_empty() {
        return Ok((
            "no changes to write; live device state is unchanged".into(),
            profile,
        ));
    }
    Ok((
        format!(
            "safe writes complete: {}; persistence verification unknown",
            evidence.join("; ")
        ),
        profile,
    ))
}

fn make_live_snapshot(loaded: &LoadedDevice, status: &str) -> LiveSnapshot {
    let active_index = loaded.profile.dpi.active_stage.get().saturating_sub(1) as usize;
    let active_dpi = loaded
        .profile
        .dpi
        .stages
        .get(active_index)
        .copied()
        .map(DpiValue::get)
        .unwrap_or(0);
    let transport = match loaded.identity.transport {
        TransportKind::Wired => "usb wired",
        TransportKind::Receiver => "2.4g receiver",
        TransportKind::Ble => "ble (readback unsupported)",
    };
    let battery = loaded.battery.map_or_else(
        || "unavailable (wired USB)".to_owned(),
        |value| format!("{value}%"),
    );
    let stages = loaded
        .profile
        .dpi
        .stages
        .iter()
        .copied()
        .enumerate()
        .map(|(index, value)| (value.get(), index == active_index))
        .collect();
    let bindings = SAFE_BUTTON_SLOTS
        .iter()
        .copied()
        .map(|slot| {
            let button = match slot {
                SafeButtonSlot::Left => "lmb",
                SafeButtonSlot::Right => "rmb",
                SafeButtonSlot::Middle => "wheel",
                SafeButtonSlot::Dpi => "dpi",
                SafeButtonSlot::Forward => "forward",
                SafeButtonSlot::Backward => "back",
            };
            let location = match slot {
                SafeButtonSlot::Left => "primary",
                SafeButtonSlot::Right => "secondary",
                SafeButtonSlot::Middle => "middle",
                SafeButtonSlot::Dpi => "top button",
                SafeButtonSlot::Forward => "side upper",
                SafeButtonSlot::Backward => "side lower",
            };
            (
                button.to_owned(),
                location.to_owned(),
                button_action_name(loaded.profile.buttons.slots[slot.index()]),
            )
        })
        .collect();
    let profiles = (1..=5)
        .map(|number| LiveProfile {
            number,
            enabled: number <= loaded.metadata.maximum().get(),
            current: number == loaded.metadata.current().get(),
        })
        .collect();
    LiveSnapshot {
        device_name: loaded
            .identity
            .display_name
            .clone()
            .unwrap_or_else(|| "X3-compatible USB device".to_owned()),
        stable_id: loaded.identity.id.to_string(),
        product_id: format!(
            "{}:{}",
            loaded
                .identity
                .vendor_id
                .map_or_else(|| "unknown".to_owned(), |id| format!("{id:04x}")),
            loaded
                .identity
                .product_id
                .map_or_else(|| "unknown".to_owned(), |id| format!("{id:04x}"))
        ),
        transport: transport.to_owned(),
        polling_rate_ready: loaded.polling_rate_ready,
        motion_sync: loaded.profile.dpi.sensor.motion_sync,
        ripple_control: loaded.profile.dpi.sensor.ripple_control,
        battery,
        selected_profile: loaded.metadata.current().get(),
        active_dpi,
        active_stage: loaded.profile.dpi.active_stage.get(),
        stage_count: loaded.profile.dpi.stages.len(),
        polling_rate_hz: loaded.polling_rate.hz(),
        sensor: format!(
            "lift-off: {:?}; ripple: {}; angle snap: {}; motion sync: {}",
            loaded.profile.dpi.sensor.lift_off_distance,
            on_off(loaded.profile.dpi.sensor.ripple_control),
            on_off(loaded.profile.dpi.sensor.angle_snap),
            on_off(loaded.profile.dpi.sensor.motion_sync)
        ),
        profile_summary: format!(
            "profile {} · {} dpi · {} hz",
            loaded.metadata.current(),
            active_dpi,
            loaded.polling_rate.hz()
        ),
        metadata_summary: format!(
            "current {} / maximum {}",
            loaded.metadata.current(),
            loaded.metadata.maximum()
        ),
        verification_summary: "USB readback observed; persistence unknown".to_owned(),
        stages,
        bindings,
        profiles,
        status: status.to_owned(),
    }
}

fn verification_summary(verification: &attack_shark_x3_manager::Verification) -> String {
    match verification.application {
        ApplicationVerification::ReadbackVerified => "readback verified".to_owned(),
        ApplicationVerification::Acknowledged => "transport submitted".to_owned(),
        ApplicationVerification::Mismatch => "verification mismatch".to_owned(),
        ApplicationVerification::NotSent => "not sent".to_owned(),
    }
}

fn format_error(prefix: &str, error: ManagerError) -> UiEvent {
    UiEvent::Error(format_error_string(prefix, error))
}
fn format_error_string(prefix: &str, error: ManagerError) -> String {
    format!("{prefix}: {error}; no retry was attempted")
}
fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

fn shortcut_preset_name(
    assignment: attack_shark_x3_manager::ButtonAssignment,
) -> Option<&'static str> {
    Some(
        match (assignment.action, assignment.modifier, assignment.key_code) {
            (0x11, 0x03, 0x12) => "browser favorites",
            (0x11, 0x01, 0x1b) => "cut",
            (0x11, 0x01, 0x06) => "copy",
            (0x11, 0x01, 0x19) => "paste",
            (0x11, 0x01, 0x12) => "open",
            (0x11, 0x01, 0x16) => "save",
            (0x11, 0x01, 0x09) => "find",
            (0x11, 0x01, 0x1c) => "redo",
            (0x11, 0x01, 0x04) => "select all",
            (0x11, 0x01, 0x13) => "print",
            (0x11, 0x04, 0x3d) => "close window",
            (0x11, 0x04, 0x2b) => "swap windows",
            (0x11, 0x08, 0x07) => "show desktop",
            (0x11, 0x08, 0x15) => "run command",
            (0x11, 0x08, 0x0f) => "lock pc",
            (0x11, 0x0a, 0x16) => "screen capture",
            _ => return None,
        },
    )
}

fn button_action_name(assignment: attack_shark_x3_manager::ButtonAssignment) -> String {
    if let Some(name) = shortcut_preset_name(assignment) {
        return name.to_owned();
    }
    if assignment.modifier != 0 || assignment.key_code != 0 {
        return "unsupported (not editable)".to_owned();
    }
    match assignment.action {
        0x01 => "disabled",
        0x02 => "left click",
        0x03 => "right click",
        0x04 => "middle click",
        0x05 => "backward",
        0x06 => "forward",
        0x07 => "double click",
        0x08 => "fire button",
        0x09 => "scroll up",
        0x0a => "scroll down",
        0x0d => "dpi cycle",
        0x0e => "dpi plus",
        0x0f => "dpi minus",
        0x15 => "media player",
        0x16 => "previous track",
        0x17 => "next track",
        0x18 => "play/pause",
        0x19 => "stop",
        0x1a => "mute",
        0x1b => "volume up",
        0x1c => "volume down",
        0x1d => "calculator",
        0x1e => "email",
        0x20 => "browser forward",
        0x21 => "browser backward",
        0x22 => "browser stop",
        0x23 => "my computer",
        0x24 => "browser refresh",
        0x25 => "browser home",
        0x26 => "browser search",
        0x34 => "profile cycle",
        0x35 => "profile plus",
        0x36 => "profile minus",
        _ => "unsupported (not editable)",
    }
    .to_owned()
}

fn safe_button_action(name: &str) -> Option<SafeButtonAction> {
    Some(match name {
        "disabled" => SafeButtonAction::Disable,
        "left click" => SafeButtonAction::LeftClick,
        "right click" => SafeButtonAction::RightClick,
        "middle click" => SafeButtonAction::MiddleClick,
        "backward" => SafeButtonAction::Backward,
        "forward" => SafeButtonAction::Forward,
        "double click" => SafeButtonAction::DoubleClick,
        "dpi cycle" => SafeButtonAction::DpiCycle,
        "dpi plus" => SafeButtonAction::DpiPlus,
        "dpi minus" => SafeButtonAction::DpiMinus,
        "profile cycle" => SafeButtonAction::ProfileCycle,
        "profile plus" => SafeButtonAction::ProfilePlus,
        "profile minus" => SafeButtonAction::ProfileMinus,
        "fire button" => SafeButtonAction::FireButton,
        "scroll up" => SafeButtonAction::ScrollUp,
        "scroll down" => SafeButtonAction::ScrollDown,
        "media player" => SafeButtonAction::MediaPlayer,
        "previous track" => SafeButtonAction::PreviousTrack,
        "next track" => SafeButtonAction::NextTrack,
        "play/pause" => SafeButtonAction::PlayPause,
        "stop" => SafeButtonAction::Stop,
        "mute" => SafeButtonAction::Mute,
        "volume up" => SafeButtonAction::VolumeUp,
        "volume down" => SafeButtonAction::VolumeDown,
        "calculator" => SafeButtonAction::Calculator,
        "email" => SafeButtonAction::Email,
        "browser forward" => SafeButtonAction::BrowserForward,
        "browser backward" => SafeButtonAction::BrowserBackward,
        "browser stop" => SafeButtonAction::BrowserStop,
        "my computer" => SafeButtonAction::MyComputer,
        "browser refresh" => SafeButtonAction::BrowserRefresh,
        "browser home" => SafeButtonAction::BrowserHome,
        "browser search" => SafeButtonAction::BrowserSearch,
        "browser favorites" => SafeButtonAction::BrowserFavorites,
        "cut" => SafeButtonAction::Cut,
        "copy" => SafeButtonAction::Copy,
        "paste" => SafeButtonAction::Paste,
        "open" => SafeButtonAction::Open,
        "save" => SafeButtonAction::Save,
        "find" => SafeButtonAction::Find,
        "redo" => SafeButtonAction::Redo,
        "select all" => SafeButtonAction::SelectAll,
        "print" => SafeButtonAction::Print,
        "close window" => SafeButtonAction::CloseWindow,
        "swap windows" => SafeButtonAction::SwapWindows,
        "show desktop" => SafeButtonAction::ShowDesktop,
        "run command" => SafeButtonAction::RunCommand,
        "lock pc" => SafeButtonAction::LockPc,
        "screen capture" => SafeButtonAction::ScreenCapture,
        _ => return None,
    })
}

#[allow(dead_code)]
fn default_dpi_stage(index: usize, active: bool) -> DpiStage {
    let (red, green, blue) = match index {
        0 => (96, 98, 104),
        1 => (112, 114, 120),
        2 => (128, 130, 136),
        3 => (144, 146, 152),
        4 => (96, 98, 104),
        5 => (112, 114, 120),
        6 => (128, 130, 136),
        _ => (144, 146, 152),
    };
    dpi_stage(
        &format!("{:02}", index + 1),
        DPI_VALUES[index],
        DPI_LABELS[index],
        red,
        green,
        blue,
        active,
    )
}

#[allow(dead_code)]
fn set_active_dpi_stage(stages: &VecModel<DpiStage>, selected: usize) {
    for row in 0..stages.row_count() {
        if let Some(mut stage) = stages.row_data(row) {
            stage.active = row == selected;
            stages.set_row_data(row, stage);
        }
    }
}
#[allow(dead_code)]
fn remove_dpi_stage(stages: &VecModel<DpiStage>, row: usize, active: usize) -> Option<usize> {
    let count = stages.row_count();
    if count <= 1 || row >= count {
        return None;
    }
    let active = active.min(count - 1);
    stages.remove(row);
    renumber_dpi_stages(stages);
    Some(if row == active {
        active.min(count - 2)
    } else if row < active {
        active - 1
    } else {
        active
    })
}
#[allow(dead_code)]
fn renumber_dpi_stages(stages: &VecModel<DpiStage>) {
    for row in 0..stages.row_count() {
        if let Some(mut stage) = stages.row_data(row) {
            stage.index = format!("{:02}", row + 1).into();
            stages.set_row_data(row, stage);
        }
    }
}
fn parse_dpi_setting(text: &str) -> Option<f32> {
    let value = text.trim().parse::<f32>().ok()?;
    if !value.is_finite() || !(DPI_MIN..=DPI_MAX).contains(&value) {
        return None;
    }
    Some((value / DPI_STEP).round() * DPI_STEP)
}
fn format_dpi_setting(value: f32) -> String {
    (value as u32).to_string()
}
#[allow(dead_code)]
fn dpi_bounds(min: f32, max: f32) -> (f32, f32) {
    let min = min.clamp(DPI_MIN, DPI_MAX - DPI_STEP);
    let max = max.clamp(min + DPI_STEP, DPI_MAX);
    (min, max)
}
fn clamp_dpi_value(dpi: f32, min: f32, max: f32) -> f32 {
    let (min, max) = dpi_bounds(min, max);
    ((dpi / DPI_STEP).round() * DPI_STEP).clamp(min, max)
}
#[allow(dead_code)]
fn clamp_dpi_stage(stage: &mut DpiStage, min: f32, max: f32) {
    let dpi = clamp_dpi_value(stage.dpi, min, max);
    stage.dpi = dpi;
    stage.value = format_dpi_setting(dpi).into();
}
#[allow(dead_code)]
fn clamp_dpi_stages(stages: &VecModel<DpiStage>, min: f32, max: f32) {
    for row in 0..stages.row_count() {
        if let Some(mut stage) = stages.row_data(row) {
            clamp_dpi_stage(&mut stage, min, max);
            stages.set_row_data(row, stage);
        }
    }
}
fn dpi_from_ratio(ratio: f32, logarithmic: bool, min: f32, max: f32) -> f32 {
    let (min, max) = dpi_bounds(min, max);
    let ratio = ratio.clamp(0.0, 1.0);
    let raw = if logarithmic {
        min * (max / min).powf(ratio)
    } else {
        min + (max - min) * ratio
    };
    clamp_dpi_value(raw, min, max)
}
fn dpi_ratio(dpi: f32, logarithmic: bool, min: f32, max: f32) -> f32 {
    let (min, max) = dpi_bounds(min, max);
    let dpi = dpi.clamp(min, max);
    let ratio = if logarithmic {
        (dpi / min).ln() / (max / min).ln()
    } else {
        (dpi - min) / (max - min)
    };
    ratio.clamp(0.0, 1.0)
}
fn refresh_dpi_ratios(stages: &VecModel<DpiStage>, logarithmic: bool, min: f32, max: f32) {
    for row in 0..stages.row_count() {
        if let Some(mut stage) = stages.row_data(row) {
            stage.ratio = dpi_ratio(stage.dpi, logarithmic, min, max);
            stages.set_row_data(row, stage);
        }
    }
}
fn dpi_stage(
    index: &str,
    value: &str,
    label: &str,
    red: u8,
    green: u8,
    blue: u8,
    active: bool,
) -> DpiStage {
    DpiStage {
        index: index.into(),
        value: value.into(),
        dpi: value.parse().unwrap_or(DPI_MIN),
        ratio: 0.0,
        label: label.into(),
        accent: slint::Color::from_rgb_u8(red, green, blue),
        active,
    }
}
fn binding(button: &str, location: &str, action: &str) -> BindingRow {
    BindingRow {
        button: button.into(),
        location: location.into(),
        action: action.into(),
    }
}

fn page_status(page: i32) -> &'static str {
    match page {
        0 => "live device overview",
        1 => "safe button assignments; changes are drafts until applied",
        2 => "live DPI ladder; changes are drafts until applied",
        3 => "polling rate is supported; other performance controls are not implemented",
        4 => "live identity and write verification settings",
        5 => "local-only appearance and slider preferences",
        _ => "live USB manager session",
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn removing_a_stage_reindexes_and_shifts_active_stage() {
        let stages = VecModel::from(
            (0..6)
                .map(|index| default_dpi_stage(index, false))
                .collect::<Vec<_>>(),
        );
        set_active_dpi_stage(&stages, 4);
        let new_active = remove_dpi_stage(&stages, 1, 4).expect("stage should be removable");
        set_active_dpi_stage(&stages, new_active);
        assert_eq!(stages.row_count(), 5);
        assert_eq!(
            stages.row_data(1).expect("stage remains").index.as_str(),
            "02"
        );
        assert!(stages.row_data(3).expect("active stage remains").active);
    }
    #[test]
    fn dpi_slider_snaps_to_fifty_and_clamps_to_configured_range() {
        let min = 200.0;
        let max = 3_200.0;
        for logarithmic in [false, true] {
            let low = dpi_from_ratio(-1.0, logarithmic, min, max);
            let middle = dpi_from_ratio(0.5, logarithmic, min, max);
            let high = dpi_from_ratio(2.0, logarithmic, min, max);
            assert_eq!(low, min);
            assert_eq!(high, max);
            assert_eq!(middle as u32 % DPI_STEP as u32, 0);
        }
    }
    #[test]
    fn dpi_thumb_ratio_tracks_configured_dpi() {
        assert_eq!(dpi_ratio(200.0, false, 200.0, 3_200.0), 0.0);
        assert_eq!(dpi_ratio(3_200.0, false, 200.0, 3_200.0), 1.0);
        assert!((dpi_ratio(800.0, false, 200.0, 3_200.0) - 0.2).abs() < 0.0001);
        assert!((dpi_ratio(800.0, true, 200.0, 3_200.0) - 0.5).abs() < 0.0001);
    }
    #[test]
    fn dpi_settings_round_to_fifty_and_reject_invalid_values() {
        assert_eq!(parse_dpi_setting("224"), Some(200.0));
        assert_eq!(parse_dpi_setting("226"), Some(250.0));
        assert_eq!(parse_dpi_setting("75"), Some(100.0));
        assert_eq!(parse_dpi_setting("49"), None);
        assert_eq!(parse_dpi_setting("26001"), None);
        assert_eq!(parse_dpi_setting("nan"), None);
    }
    #[test]
    fn logarithmic_slider_spends_more_travel_on_low_dpi_values() {
        let linear = dpi_from_ratio(0.5, false, 200.0, 3_200.0);
        let logarithmic = dpi_from_ratio(0.5, true, 200.0, 3_200.0);
        assert!(logarithmic > 200.0);
        assert!(logarithmic < linear);
    }
    #[test]
    fn removing_the_last_stage_is_rejected() {
        let stages = VecModel::from(vec![default_dpi_stage(0, true)]);
        assert!(remove_dpi_stage(&stages, 0, 0).is_none());
        assert_eq!(stages.row_count(), 1);
    }

    #[test]
    fn every_exposed_button_label_round_trips_through_the_safe_manager_action() {
        for label in BINDING_ACTIONS {
            let action = safe_button_action(label).expect("every exposed label must be safe");
            assert_eq!(button_action_name(action.to_assignment()), label);
        }
    }

    #[test]
    fn modified_or_unknown_button_assignments_remain_read_only() {
        let modified = attack_shark_x3_manager::ButtonAssignment::new(0x02, 1, 0);
        let unknown = attack_shark_x3_manager::ButtonAssignment::new(0xff, 0, 0);
        assert_eq!(button_action_name(modified), "unsupported (not editable)");
        assert_eq!(button_action_name(unknown), "unsupported (not editable)");
        assert!(safe_button_action("unsupported (not editable)").is_none());
    }
}
