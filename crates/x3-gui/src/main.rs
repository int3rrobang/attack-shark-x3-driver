use std::{
    collections::BTreeMap,
    error::Error,
    path::PathBuf,
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread,
    time::Duration,
};

use attack_shark_x3::{DebounceMs, DeepSleepMinutes, SleepTimer};
use attack_shark_x3_manager::{
    ApplicationVerification, BaselineSource, ButtonSlotDelta, ConfigurationExport, DesiredSource,
    DeviceEvent, DeviceId, DeviceIdentity, DeviceManager, DeviceStatus, DiscoveredDevice, DpiDelta,
    DpiValue, EventSubscriptions, FullProfileRefreshOutcome, LiftOffDistance, ManagerError,
    PersistenceVerification, PollingRate, PreferencesDelta, ProfileId, ProfileMetadata,
    ProfileResourceKind, ResourceState, SafeButtonAction, SafeButtonSlot, SensorOptionsDelta,
    StageIndex, StateFile, StateStore, TransportKind, TransportSelection, UpdatePolicy,
    Verification, VerificationMethod,
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
const DPI_LABELS: [&str; MAX_DPI_STAGES] = [
    "stage 01", "stage 02", "stage 03", "stage 04", "stage 05", "stage 06", "stage 07", "stage 08",
];
/// Raw-preference field indices used by `set-preference-raw`.
const RAW_PREFERENCE_CONFIGURATION: i32 = 0;
const RAW_PREFERENCE_DEEP_SLEEP: i32 = 1;
const RAW_PREFERENCE_SLEEP_TIMER: i32 = 2;
const RAW_PREFERENCE_DEBOUNCE: i32 = 3;

/// Verification banner for profile-reload verification: the device stays
/// connected the whole time; the manager switches the active profile twice.
const VERIFICATION_INSTRUCTION_PROFILE_RELOAD: &str = "Keep the selected device connected — profile-reload verification switches the active profile twice on the device; do not unplug or power off the mouse.";

/// Verification banner for power-cycle verification: the device must
/// physically leave and return so the saved profile survives a full power
/// loss. Mirrors the status line shown while the verification runs.
const VERIFICATION_INSTRUCTION_POWER_CYCLE: &str = "Unplug USB, switch the mouse off, wait for it to disappear, then switch it on and reconnect — verification waits for each transition.";

#[derive(Debug)]
enum Command {
    Startup,
    Refresh,
    RefreshAllProfiles,
    SelectDevice(String),
    SelectProfile(u8),
    Apply(Draft),
    Discard,
    DiscardStateSchema,
    AddProfile,
    RenameProfile(u8, String),
    HideProfile(u8),
    Import(PathBuf),
    Export(PathBuf),
    InvalidateState,
    VerifyProfile,
    VerifyPowerCycle,
    Shutdown,
}

#[derive(Clone, Copy, Debug)]
enum VerificationChoice {
    Transport,
    Readback,
}

#[derive(Clone, Copy, Debug)]
enum BaselineChoice {
    Live,
    Stored,
}

/// The complete draft captured from the UI when the user saves.
///
/// Preference fields carry the raw wire bytes; the typed fields in the UI
/// write through to those bytes through `DebounceMs`/`SleepTimer`/
/// `DeepSleepMinutes`, so no byte formula is duplicated here.
#[derive(Clone, Debug)]
struct Draft {
    profile: u8,
    dpi_values: Vec<u16>,
    active_stage: u8,
    buttons: Vec<String>,
    lift_off_choice: u8,
    ripple_control: bool,
    angle_snap: bool,
    motion_sync: bool,
    raw_configuration: u8,
    raw_deep_sleep: u8,
    raw_sleep_timer: u8,
    raw_debounce: u8,
    polling_rate_hz: u16,
    verification: VerificationChoice,
    baseline: BaselineChoice,
    allow_explicit_defaults: bool,
}

struct ProfileData {
    dpi: attack_shark_x3_manager::DpiState,
    preferences: attack_shark_x3_manager::PreferencesState,
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
    all_profiles_observed: bool,
    /// True when the profile image came from captured stock evidence instead
    /// of a stored baseline or USB readback (BLE with no stored image).
    captured_image: bool,
    /// True when stored desired/observed preferences exist for the profile.
    has_stored_preferences: bool,
    profile_names: BTreeMap<ProfileId, String>,
    verification_summary: String,
}

struct LiveProfile {
    enabled: bool,
    current: bool,
    name: String,
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
    is_ble: bool,
    all_profiles_observed: bool,
    motion_sync: bool,
    ripple_control: bool,
    angle_snap: bool,
    lift_off_choice: u8,
    debounce_ms: Option<u8>,
    sleep_half_minutes: Option<u8>,
    deep_sleep_minutes: Option<u8>,
    raw_configuration: String,
    raw_deep_sleep: String,
    raw_sleep_timer: String,
    raw_debounce: String,
    preferences_ready: bool,
    last_enabled_profile: i32,
    sensor: String,
    profile_summary: String,
    metadata_summary: String,
    verification_summary: String,
    stages: Vec<(u16, bool)>,
    bindings: Vec<(String, String, String)>,
    profiles: Vec<LiveProfile>,
    status: String,
}

struct DeviceListEntry {
    id: String,
    name: String,
    detail: String,
    selected: bool,
    connected: bool,
}

/// Typed decodes of one preferences image, resolved through the manager's
/// typed helpers so the GUI never duplicates the wire formulas.
struct PreferenceFields {
    debounce_ms: Option<u8>,
    sleep_half_minutes: Option<u8>,
    deep_sleep_minutes: Option<u8>,
    raw_configuration: String,
    raw_deep_sleep: String,
    raw_sleep_timer: String,
    raw_debounce: String,
}

enum UiEvent {
    Busy(String),
    Snapshot(Box<LiveSnapshot>),
    /// An event-driven snapshot pushed by the device (battery, active DPI
    /// stage, profile change) while the UI is otherwise idle. Unlike explicit
    /// operation/load snapshots it preserves an open draft.
    EventSnapshot(Box<LiveSnapshot>),
    Devices(Vec<DeviceListEntry>),
    Verification(bool),
    /// An operation failed but the currently loaded device state is still
    /// valid and is left displayed.
    OperationError(String),
    Error(String),
    StateUnreadable(String),
}

enum StartupError {
    /// The state file exists but cannot be read as current-schema state;
    /// the user can discard it (with a backup) and retry.
    UnreadableState(String),
    Message(String),
}

fn main() -> Result<(), Box<dyn Error>> {
    let ui = AppWindow::new()?;
    let dpi_stages = std::rc::Rc::new(VecModel::from(Vec::<DpiStage>::new()));
    let bindings = std::rc::Rc::new(VecModel::from(Vec::<BindingRow>::new()));
    let profiles = std::rc::Rc::new(VecModel::from(Vec::<ProfileRow>::new()));
    let devices = std::rc::Rc::new(VecModel::from(Vec::<DeviceRow>::new()));
    ui.set_dpi_stages(ModelRc::from(dpi_stages.clone()));
    ui.set_bindings(ModelRc::from(bindings.clone()));
    ui.set_profiles(ModelRc::from(profiles.clone()));
    ui.set_devices(ModelRc::from(devices.clone()));
    let binding_actions = std::rc::Rc::new(VecModel::from(
        BINDING_ACTIONS
            .iter()
            .map(|action| slint::SharedString::from(*action))
            .collect::<Vec<_>>(),
    ));
    ui.set_binding_actions(ModelRc::from(binding_actions));
    ui.set_product_name(configured_product_label(0, "custom mouse").into());
    ui.set_hardware_ready(false);
    ui.set_busy(true);
    ui.set_lifecycle_text("starting device discovery".into());
    ui.set_status_text("discovering wired, receiver, and BLE devices".into());
    ui.set_transport_label("no device selected".into());
    ui.set_battery_text("unavailable".into());
    ui.set_dpi_min(DPI_MIN);
    ui.set_dpi_max(DPI_MAX);
    ui.set_dpi_min_text(format_dpi_setting(DPI_MIN).into());
    ui.set_dpi_max_text(format_dpi_setting(DPI_MAX).into());
    ui.set_preferences_ready(false);
    ui.set_debounce_ms(0);
    ui.set_sleep_half_minutes(0);
    ui.set_deep_sleep_minutes(0);
    ui.set_deep_sleep_known(false);
    ui.set_lift_off_choice(0);
    ui.set_angle_snap(false);
    ui.set_advanced_preferences(false);
    ui.set_preference_configuration_raw("00".into());
    ui.set_preference_deep_sleep_raw("00".into());
    ui.set_preference_sleep_timer_raw("00".into());
    ui.set_preference_debounce_raw("00".into());
    ui.set_baseline_choice(0);
    ui.set_allow_explicit_defaults(false);
    ui.set_ble_device(false);
    ui.set_verification_running(false);
    ui.set_last_enabled_profile(-1);

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
        let commands = commands.clone();
        ui.on_select_device(move |row| {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_busy() {
                return;
            }
            if ui.get_dirty() {
                ui.set_status_text(
                    "save or discard the current draft before switching devices".into(),
                );
                return;
            }
            let Some(device) = ui.get_devices().row_data(row.max(0) as usize) else {
                return;
            };
            if !device.connected {
                ui.set_status_text("that device is not connected; press refresh to rescan".into());
                return;
            }
            ui.set_all_profiles_refresh_dismissed(false);
            ui.set_busy(true);
            ui.set_lifecycle_text("switching device".into());
            ui.set_status_text(format!("switching to {}…", device.name).into());
            queue_command(&ui, &commands, Command::SelectDevice(device.id.to_string()));
        });
    }
    {
        let weak = ui.as_weak();
        let commands = commands.clone();
        ui.on_select_profile(move |selected| {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            if ui.get_dirty() {
                ui.set_status_text("discard the current draft before switching profiles".into());
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
        });
    }
    {
        let weak = ui.as_weak();
        let commands = commands.clone();
        ui.on_add_profile(move || {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            if ui.get_dirty() {
                ui.set_status_text("save or discard the current draft before adding a profile".into());
                return;
            }
            if ui.get_ble_device() {
                ui.set_status_text(
                    "adding a profile writes profile metadata, which is disabled over BLE (no readback)"
                        .into(),
                );
                return;
            }
            ui.set_busy(true);
            ui.set_lifecycle_text("enabling profile slot".into());
            ui.set_status_text("raising the maximum enabled profile…".into());
            queue_command(&ui, &commands, Command::AddProfile);
        });
    }
    {
        let weak = ui.as_weak();
        let commands = commands.clone();
        ui.on_rename_profile(move |row, name| {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            if let Some(message) = dirty_draft_message(ui.get_dirty(), "renaming a profile") {
                ui.set_status_text(message.into());
                return;
            }
            let number = (row + 1).clamp(1, ProfileId::MAX as i32) as u8;
            let name = name.trim().to_owned();
            ui.set_busy(true);
            ui.set_lifecycle_text("renaming profile".into());
            ui.set_status_text(format!("renaming profile {number}…").into());
            queue_command(&ui, &commands, Command::RenameProfile(number, name));
        });
    }
    {
        let weak = ui.as_weak();
        let commands = commands.clone();
        ui.on_hide_profile(move |row| {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            if ui.get_dirty() {
                ui.set_status_text("save or discard the current draft before hiding a profile".into());
                return;
            }
            if ui.get_ble_device() {
                ui.set_status_text(
                    "hiding a profile writes profile metadata, which is disabled over BLE (no readback)"
                        .into(),
                );
                return;
            }
            let number = (row + 1).clamp(1, ProfileId::MAX as i32) as u8;
            ui.set_busy(true);
            ui.set_lifecycle_text("disabling profile slot".into());
            ui.set_status_text(format!("hiding profile {number}…").into());
            queue_command(&ui, &commands, Command::HideProfile(number));
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
            let dpi = round_dpi_step(dpi) as f32;
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
        ui.on_add_dpi_stage(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            if !append_dpi_stage(&dpi_stages) {
                ui.set_status_text(
                    format!("the device supports at most {MAX_DPI_STAGES} DPI stages").into(),
                );
                return;
            }
            ui.set_dirty(true);
            ui.set_status_text(
                "DPI stage added in the draft — save to apply (this raises the stage count)".into(),
            );
        });
    }
    {
        let weak = ui.as_weak();
        let dpi_stages = dpi_stages.clone();
        ui.on_remove_dpi_stage(move |row| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if !ui.get_hardware_ready() || ui.get_busy() || row < 0 {
                return;
            }
            let row = row as usize;
            let active = ui.get_active_dpi().max(0) as usize;
            let Some(new_active) = remove_dpi_stage(&dpi_stages, row, active) else {
                ui.set_status_text("at least one DPI stage is required".into());
                return;
            };
            set_active_dpi_stage(&dpi_stages, new_active);
            ui.set_active_dpi(new_active as i32);
            refresh_dpi_ratios(
                &dpi_stages,
                ui.get_dpi_log_scale(),
                ui.get_dpi_min(),
                ui.get_dpi_max(),
            );
            ui.set_dirty(true);
            ui.set_status_text(
                "DPI stage removed; remaining stages compacted — save to apply".into(),
            );
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
                ui.set_status_text("DPI scale updated".into());
            }
        });
    }
    {
        let weak = ui.as_weak();
        let dpi_stages = dpi_stages.clone();
        ui.on_dpi_range_changed(move |minimum, maximum| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let Some((minimum, maximum)) = parse_dpi_range(minimum.as_str(), maximum.as_str())
            else {
                ui.set_dpi_min_text(format_dpi_setting(ui.get_dpi_min()).into());
                ui.set_dpi_max_text(format_dpi_setting(ui.get_dpi_max()).into());
                ui.set_status_text("enter a minimum and maximum between 50 and 26,000 DPI".into());
                return;
            };
            ui.set_dpi_min(minimum);
            ui.set_dpi_max(maximum);
            ui.set_dpi_min_text(format_dpi_setting(minimum).into());
            ui.set_dpi_max_text(format_dpi_setting(maximum).into());
            refresh_dpi_ratios(&dpi_stages, ui.get_dpi_log_scale(), minimum, maximum);
            ui.set_status_text("DPI display range updated".into());
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_set_lift_off(move |choice| {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            ui.set_lift_off_choice(choice.clamp(0, 1));
            ui.set_dirty(true);
            ui.set_status_text("lift-off distance changed in the draft — save to apply".into());
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_toggle_ripple_control(move |enabled| {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            ui.set_ripple_control_readback(enabled);
            ui.set_dirty(true);
            ui.set_status_text("ripple control changed in the draft — save to apply".into());
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_toggle_angle_snap(move |enabled| {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            ui.set_angle_snap(enabled);
            ui.set_dirty(true);
            ui.set_status_text("angle snap changed in the draft — save to apply".into());
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_toggle_motion_sync(move |enabled| {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            ui.set_motion_sync_readback(enabled);
            ui.set_dirty(true);
            ui.set_status_text("motion sync changed in the draft — save to apply".into());
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_set_debounce_ms(move |ms| {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            let Some(debounce) = canonical_debounce_ms(ms as f64) else {
                ui.set_status_text("debounce must be an even value between 4 and 50 ms".into());
                return;
            };
            ui.set_debounce_ms(debounce.get() as i32);
            ui.set_preference_debounce_raw(format_byte(debounce.raw()).into());
            ui.set_dirty(true);
            ui.set_status_text("debounce changed in the draft — save to apply".into());
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_set_debounce_text(move |text| {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            let Some(value) = text
                .as_str()
                .trim()
                .parse::<f64>()
                .ok()
                .and_then(canonical_debounce_ms)
            else {
                ui.set_status_text("debounce must be an even value between 4 and 50 ms".into());
                return;
            };
            ui.set_debounce_ms(value.get() as i32);
            ui.set_preference_debounce_raw(format_byte(value.raw()).into());
            ui.set_dirty(true);
            ui.set_status_text("debounce changed in the draft — save to apply".into());
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_set_sleep_half_minutes(move |half| {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            let Some(timer) = canonical_sleep_minutes(half as f64 / 2.0) else {
                ui.set_status_text(
                    "sleep timer must be 0.5 to 30 minutes in half-minute steps; no off state is supported"
                        .into(),
                );
                return;
            };
            ui.set_sleep_half_minutes(timer.get() as i32);
            ui.set_preference_sleep_timer_raw(format_byte(timer.raw()).into());
            ui.set_dirty(true);
            ui.set_status_text("sleep timer changed in the draft — save to apply".into());
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_set_sleep_text(move |text| {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            let Some(timer) = text
                .as_str()
                .trim()
                .parse::<f64>()
                .ok()
                .and_then(canonical_sleep_minutes)
            else {
                ui.set_status_text(
                    "sleep timer must be 0.5 to 30 minutes in half-minute steps; no off state is supported"
                        .into(),
                );
                return;
            };
            ui.set_sleep_half_minutes(timer.get() as i32);
            ui.set_preference_sleep_timer_raw(format_byte(timer.raw()).into());
            ui.set_dirty(true);
            ui.set_status_text("sleep timer changed in the draft — save to apply".into());
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_set_deep_sleep_minutes(move |minutes| {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            let Some(deep) = canonical_deep_sleep_minutes(minutes as f64) else {
                ui.set_status_text("deep sleep must be 1 to 60 minutes".into());
                return;
            };
            let Some(current_configuration) =
                parse_raw_byte(ui.get_preference_configuration_raw().as_str())
            else {
                ui.set_status_text(
                    "fix the raw configuration byte before changing deep sleep (the last valid value is kept)"
                        .into(),
                );
                return;
            };
            ui.set_preference_configuration_raw(
                format_byte(deep.configuration_with(current_configuration)).into(),
            );
            ui.set_preference_deep_sleep_raw(format_byte(deep.deep_sleep_byte()).into());
            ui.set_deep_sleep_minutes(deep.get() as i32);
            ui.set_deep_sleep_known(true);
            ui.set_dirty(true);
            ui.set_status_text("deep sleep changed in the draft — save to apply".into());
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_set_deep_sleep_text(move |text| {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            let Some(deep) = text
                .as_str()
                .trim()
                .parse::<f64>()
                .ok()
                .and_then(canonical_deep_sleep_minutes)
            else {
                ui.set_status_text("deep sleep must be 1 to 60 minutes".into());
                return;
            };
            let Some(current_configuration) =
                parse_raw_byte(ui.get_preference_configuration_raw().as_str())
            else {
                ui.set_status_text(
                    "fix the raw configuration byte before changing deep sleep (the last valid value is kept)"
                        .into(),
                );
                return;
            };
            ui.set_preference_configuration_raw(
                format_byte(deep.configuration_with(current_configuration)).into(),
            );
            ui.set_preference_deep_sleep_raw(format_byte(deep.deep_sleep_byte()).into());
            ui.set_deep_sleep_minutes(deep.get() as i32);
            ui.set_deep_sleep_known(true);
            ui.set_dirty(true);
            ui.set_status_text("deep sleep changed in the draft — save to apply".into());
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_toggle_advanced_preferences(move || {
            if let Some(ui) = weak.upgrade() {
                let advanced = !ui.get_advanced_preferences();
                ui.set_advanced_preferences(advanced);
                ui.set_status_text(
                    if advanced {
                        "raw preference bytes shown; typed fields update from raw bytes"
                    } else {
                        "raw preference bytes hidden"
                    }
                    .into(),
                );
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_set_preference_raw(move |field, text| {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            let Some(value) = parse_raw_byte(text.as_str()) else {
                ui.set_status_text(
                    "raw preference bytes must be one or two hex digits (for example 0a or 0x0a)"
                        .into(),
                );
                return;
            };
            match field {
                RAW_PREFERENCE_CONFIGURATION => {
                    ui.set_preference_configuration_raw(format_byte(value).into());
                    refresh_deep_sleep_display(&ui);
                }
                RAW_PREFERENCE_DEEP_SLEEP => {
                    ui.set_preference_deep_sleep_raw(format_byte(value).into());
                    refresh_deep_sleep_display(&ui);
                }
                RAW_PREFERENCE_SLEEP_TIMER => {
                    ui.set_preference_sleep_timer_raw(format_byte(value).into());
                    let half = SleepTimer::from_raw(value).map(|timer| timer.get() as i32);
                    if let Some(half) = half {
                        ui.set_sleep_half_minutes(half);
                    }
                }
                RAW_PREFERENCE_DEBOUNCE => {
                    ui.set_preference_debounce_raw(format_byte(value).into());
                    let ms = DebounceMs::from_raw(value).map(|debounce| debounce.get() as i32);
                    if let Some(ms) = ms {
                        ui.set_debounce_ms(ms);
                    }
                }
                _ => {
                    ui.set_status_text("unknown raw preference field".into());
                    return;
                }
            }
            ui.set_dirty(true);
            ui.set_status_text("raw preference byte changed in the draft — save to apply".into());
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_set_baseline_choice(move |choice| {
            if let Some(ui) = weak.upgrade() {
                let choice = choice.clamp(0, 1);
                ui.set_baseline_choice(choice);
                if ui.get_ble_device() {
                    ui.set_status_text(
                        "BLE always merges configuration against the stored baseline".into(),
                    );
                } else if choice == 1 {
                    ui.set_status_text(
                        "stored baseline selected for the next USB write (no live pre-read)".into(),
                    );
                } else {
                    ui.set_status_text(
                        "live baseline selected for the next USB write (fresh pre-read)".into(),
                    );
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_set_allow_explicit_defaults(move |enabled| {
            if let Some(ui) = weak.upgrade() {
                ui.set_allow_explicit_defaults(enabled);
                ui.set_status_text(
                    if enabled {
                        "explicit captured defaults are allowed when no stored baseline exists"
                    } else {
                        "explicit defaults refused; missing baselines will fail safely"
                    }
                    .into(),
                );
            }
        });
    }
    {
        let weak = ui.as_weak();
        let commands = commands.clone();
        ui.on_request_import(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_busy() {
                return;
            }
            if let Some(message) = dirty_draft_message(ui.get_dirty(), "importing a configuration")
            {
                ui.set_status_text(message.into());
                return;
            }
            let Some(path) = rfd::FileDialog::new()
                .add_filter("x3 configuration JSON", &["json"])
                .set_title("Import configuration JSON")
                .pick_file()
            else {
                return;
            };
            ui.set_busy(true);
            ui.set_lifecycle_text("importing configuration".into());
            ui.set_status_text("reading and importing the configuration JSON…".into());
            queue_command(&ui, &commands, Command::Import(path));
        });
    }
    {
        let weak = ui.as_weak();
        let commands = commands.clone();
        ui.on_request_export(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_busy() {
                return;
            }
            let Some(path) = rfd::FileDialog::new()
                .add_filter("x3 configuration JSON", &["json"])
                .set_file_name("x3-config.json")
                .set_title("Export configuration JSON")
                .save_file()
            else {
                return;
            };
            ui.set_busy(true);
            ui.set_lifecycle_text("exporting configuration".into());
            ui.set_status_text("writing the configuration JSON…".into());
            queue_command(&ui, &commands, Command::Export(path));
        });
    }
    {
        let weak = ui.as_weak();
        let commands = commands.clone();
        ui.on_request_refresh_all_profiles(move || {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            if let Some(message) =
                dirty_draft_message(ui.get_dirty(), "refreshing all profile observations")
            {
                ui.set_status_text(message.into());
                return;
            }
            if ui.get_ble_device() {
                ui.set_status_text(
                    "all-profile readback requires USB; it is unavailable over BLE".into(),
                );
                return;
            }
            ui.invoke_show_all_profiles_refresh();
        });
        let weak = ui.as_weak();
        ui.on_dismiss_all_profiles_refresh(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_all_profiles_refresh_dismissed(true);
            }
        });
        let weak = ui.as_weak();
        ui.on_confirm_all_profiles_refresh(move || {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            if let Some(message) =
                dirty_draft_message(ui.get_dirty(), "refreshing all profile observations")
            {
                ui.set_status_text(message.into());
                return;
            }
            if ui.get_ble_device() {
                ui.set_status_text(
                    "all-profile readback requires USB; it is unavailable over BLE".into(),
                );
                return;
            }
            ui.set_busy(true);
            ui.set_lifecycle_text("reading all profile observations".into());
            ui.set_status_text(
                "reading all five profiles; the original profile settings will be restored…".into(),
            );
            queue_command(&ui, &commands, Command::RefreshAllProfiles);
        });
    }
    {
        let weak = ui.as_weak();
        let commands = commands.clone();
        ui.on_request_invalidate_state(move || {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            if let Some(message) =
                dirty_draft_message(ui.get_dirty(), "invalidating persistence evidence")
            {
                ui.set_status_text(message.into());
                return;
            }
            ui.set_busy(true);
            ui.set_lifecycle_text("invalidating persistence evidence".into());
            ui.set_status_text("clearing persistence claims; desired values are preserved…".into());
            queue_command(&ui, &commands, Command::InvalidateState);
        });
    }
    {
        let weak = ui.as_weak();
        let commands = commands.clone();
        ui.on_request_verify_profile(move || {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            if let Some(message) =
                dirty_draft_message(ui.get_dirty(), "verifying profile persistence")
            {
                ui.set_status_text(message.into());
                return;
            }
            if ui.get_ble_device() {
                ui.set_status_text(
                    "profile-reload verification requires USB readback; unavailable over BLE"
                        .into(),
                );
                return;
            }
            ui.set_busy(true);
            ui.set_lifecycle_text("verifying profile persistence".into());
            ui.set_verification_instruction(VERIFICATION_INSTRUCTION_PROFILE_RELOAD.into());
            ui.set_status_text(
                "profile-reload verification switches profiles twice — controls are locked…".into(),
            );
            queue_command(&ui, &commands, Command::VerifyProfile);
        });
    }
    {
        let weak = ui.as_weak();
        let commands = commands.clone();
        ui.on_request_verify_power_cycle(move || {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_hardware_ready() || ui.get_busy() {
                return;
            }
            if let Some(message) =
                dirty_draft_message(ui.get_dirty(), "verifying power-cycle persistence")
            {
                ui.set_status_text(message.into());
                return;
            }
            if ui.get_ble_device() {
                ui.set_status_text(
                    "power-cycle verification requires USB readback; unavailable over BLE".into(),
                );
                return;
            }
            ui.set_busy(true);
            ui.set_lifecycle_text("verifying power-cycle persistence".into());
            ui.set_verification_instruction(VERIFICATION_INSTRUCTION_POWER_CYCLE.into());
            ui.set_status_text(
                "unplug USB, switch the mouse off, wait for it to disappear, then switch it on and reconnect — verification waits for each transition…"
                    .into(),
            );
            queue_command(&ui, &commands, Command::VerifyPowerCycle);
        });
    }
    {
        let weak = ui.as_weak();
        let bindings = bindings.clone();
        ui.on_set_binding_action(move |row, label| {
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
            let Some(action) = validated_binding_action(item.action.as_str(), label.as_str())
            else {
                ui.set_status_text(
                    if BINDING_ACTIONS.contains(&item.action.as_str()) {
                        "that button action is not supported"
                    } else {
                        "this button has an assignment that cannot be changed here"
                    }
                    .into(),
                );
                return;
            };
            if item.action == action {
                return;
            }
            item.action = action.into();
            bindings.set_row_data(row, item);
            ui.set_dirty(true);
            refresh_button_change_summary(&ui);
            ui.set_status_text("button assignment changed — save to apply".into());
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_toggle_theme(move || {
            if let Some(ui) = weak.upgrade() {
                let theme = ui.global::<Theme>();
                theme.set_dark_mode(!theme.get_dark_mode());
                ui.set_status_text("theme updated".into());
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_select_product_name(move |choice| {
            if let Some(ui) = weak.upgrade() {
                let choice = choice.clamp(0, 2);
                ui.set_product_name_choice(choice);
                let label = configured_product_label(choice, ui.get_custom_product_name().as_str());
                ui.set_product_name(label.into());
                ui.set_status_text("display name updated".into());
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
                    let label = configured_product_label(ui.get_product_name_choice(), name);
                    ui.set_product_name(label.into());
                    ui.set_status_text("display name updated".into());
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_set_validation_choice(move |choice| {
            if let Some(ui) = weak.upgrade() {
                ui.set_validation_choice(choice.clamp(0, 1));
                if ui.get_ble_device() {
                    ui.set_status_text(
                        "BLE writes are always transport-verified; readback is unavailable".into(),
                    );
                } else {
                    ui.set_status_text(if choice == 1 {
                        "readback verification selected for the next USB write".into()
                    } else {
                        "transport submission selected for the next USB write".into()
                    });
                }
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
                if ui.get_ble_device() {
                    ui.set_status_text(
                        "polling-rate writes are disabled over BLE (no readback preflight)".into(),
                    );
                    return;
                }
                if !ui.get_polling_rate_ready() {
                    ui.set_status_text(
                        "polling-rate writes need complete stored DPI, preferences, and buttons for this profile"
                            .into(),
                    );
                    return;
                }
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
                values.push(round_dpi_step(stage.dpi));
            }
            let stage_count = values.len().clamp(1, MAX_DPI_STAGES) as i32;
            let buttons = (0..bindings.row_count())
                .filter_map(|row| bindings.row_data(row).map(|item| item.action.to_string()))
                .collect();
            // The raw preference text fields keep whatever the user typed, so
            // an invalid draft must abort here — never serialize as a zero
            // byte — before any Command is queued. The model retains the last
            // valid byte until the offending field is corrected.
            let raw = match parse_raw_preference_draft(
                ui.get_preference_configuration_raw().as_str(),
                ui.get_preference_deep_sleep_raw().as_str(),
                ui.get_preference_sleep_timer_raw().as_str(),
                ui.get_preference_debounce_raw().as_str(),
            ) {
                Ok(raw) => raw,
                Err(field) => {
                    ui.set_status_text(
                        format!(
                            "raw {} byte is not valid hex (one or two digits); the last valid value is kept — fix it before saving",
                            raw_preference_field_label(field)
                        )
                        .into(),
                    );
                    return;
                }
            };
            let draft = Draft {
                profile: (ui.get_selected_profile() + 1).clamp(1, ProfileId::MAX as i32) as u8,
                dpi_values: values,
                active_stage: (ui.get_active_dpi() + 1).clamp(1, stage_count) as u8,
                buttons,
                lift_off_choice: ui.get_lift_off_choice().clamp(0, 1) as u8,
                ripple_control: ui.get_ripple_control_readback(),
                angle_snap: ui.get_angle_snap(),
                motion_sync: ui.get_motion_sync_readback(),
                raw_configuration: raw.configuration,
                raw_deep_sleep: raw.deep_sleep,
                raw_sleep_timer: raw.sleep_timer,
                raw_debounce: raw.debounce,
                polling_rate_hz: ui.get_selected_rate_hz() as u16,
                verification: if ui.get_validation_choice() == 1 {
                    VerificationChoice::Readback
                } else {
                    VerificationChoice::Transport
                },
                baseline: if ui.get_baseline_choice() == 1 {
                    BaselineChoice::Stored
                } else {
                    BaselineChoice::Live
                },
                allow_explicit_defaults: ui.get_allow_explicit_defaults(),
            };
            ui.set_busy(true);
            ui.set_lifecycle_text("saving changes".into());
            ui.set_status_text("saving changes to your mouse…".into());
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
        let commands = commands.clone();
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
                ui.set_lifecycle_text("refreshing device state".into());
                ui.set_status_text("re-discovering and reading the selected device".into());
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
    {
        let weak = ui.as_weak();
        let commands = commands.clone();
        ui.on_discard_state_schema(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            ui.invoke_close_state_conflict();
            ui.set_busy(true);
            ui.set_status_text(
                "discarding the unreadable state file (a backup is kept) and retrying…".into(),
            );
            queue_command(&ui, &commands, Command::DiscardStateSchema);
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_close_app(move || {
            let _ = weak.upgrade();
            let _ = slint::quit_event_loop();
        });
    }
}

/// Re-decodes the deep-sleep typed fields from the two raw bytes that encode
/// them (the configuration high nibble and the deep-sleep byte). Invalid raw
/// text (a rejected edit) reports the typed value as unknown instead of
/// decoding against a synthesized zero.
fn refresh_deep_sleep_display(ui: &AppWindow) {
    let (Some(configuration), Some(deep_sleep)) = (
        parse_raw_byte(ui.get_preference_configuration_raw().as_str()),
        parse_raw_byte(ui.get_preference_deep_sleep_raw().as_str()),
    ) else {
        ui.set_deep_sleep_minutes(0);
        ui.set_deep_sleep_known(false);
        return;
    };
    match DeepSleepMinutes::from_raw(configuration >> 4, deep_sleep) {
        Some(deep) => {
            ui.set_deep_sleep_minutes(deep.get() as i32);
            ui.set_deep_sleep_known(true);
        }
        None => {
            ui.set_deep_sleep_minutes(0);
            ui.set_deep_sleep_known(false);
        }
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
    let mut manager: Option<DeviceManager> = None;
    let mut discovered: Vec<DiscoveredDevice> = Vec::new();
    let mut selected: Option<DeviceId> = None;
    let mut loaded: Option<LoadedDevice> = None;
    let mut events: Option<tokio::sync::broadcast::Receiver<DeviceEvent>> = None;
    let mut subscriptions: Option<EventSubscriptions> = None;

    loop {
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(command) => match command {
                Command::Startup | Command::Refresh => {
                    emit(&weak, UiEvent::Busy("discovering devices".into()));
                    match open_manager() {
                        Err(StartupError::UnreadableState(detail)) => {
                            manager = None;
                            discovered.clear();
                            selected = None;
                            loaded = None;
                            emit(&weak, UiEvent::StateUnreadable(detail));
                        }
                        Err(StartupError::Message(detail)) => {
                            manager = None;
                            discovered.clear();
                            selected = None;
                            loaded = None;
                            emit(&weak, UiEvent::Devices(Vec::new()));
                            emit(&weak, UiEvent::Error(detail));
                        }
                        Ok(new_manager) => {
                            match runtime
                                .block_on(new_manager.list_devices(TransportSelection::Auto))
                            {
                                Err(error) => {
                                    manager = Some(new_manager);
                                    emit(&weak, UiEvent::Devices(Vec::new()));
                                    emit(
                                        &weak,
                                        UiEvent::Error(format!("device discovery failed: {error}")),
                                    );
                                }
                                Ok(found) => {
                                    discovered = found;
                                    selected = new_manager.selected_device().ok().flatten();
                                    emit_devices(&weak, &discovered, selected.as_ref());
                                    match runtime.block_on(
                                        new_manager.resolve_device(None, TransportSelection::Auto),
                                    ) {
                                        Ok(resolved) => {
                                            selected = Some(resolved.clone());
                                            match runtime.block_on(read_loaded(
                                                &new_manager,
                                                &resolved,
                                                None,
                                            )) {
                                                Ok(new_loaded) => {
                                                    let new_loaded = subscribe_and_hold(
                                                        &runtime,
                                                        &new_manager,
                                                        new_loaded,
                                                        &mut events,
                                                        &mut subscriptions,
                                                    );
                                                    manager = Some(new_manager);
                                                    loaded = Some(new_loaded);
                                                    emit_devices(
                                                        &weak,
                                                        &discovered,
                                                        selected.as_ref(),
                                                    );
                                                    if let Some(ready) = loaded.as_ref() {
                                                        emit_snapshot(
                                                            &weak,
                                                            ready,
                                                            "device state loaded",
                                                        );
                                                    }
                                                }
                                                Err(error) => {
                                                    manager = Some(new_manager);
                                                    loaded = None;
                                                    emit_devices(
                                                        &weak,
                                                        &discovered,
                                                        selected.as_ref(),
                                                    );
                                                    emit(
                                                        &weak,
                                                        UiEvent::Error(format_error_string(
                                                            "device read failed",
                                                            error,
                                                        )),
                                                    );
                                                }
                                            }
                                        }
                                        Err(ManagerError::NoDevice { .. }) => {
                                            manager = Some(new_manager);
                                            loaded = None;
                                            emit_devices(&weak, &discovered, selected.as_ref());
                                            emit(
                                                &weak,
                                                UiEvent::Error(
                                                    "no compatible device is connected; connect one and press Refresh"
                                                        .into(),
                                                ),
                                            );
                                        }
                                        Err(ManagerError::AmbiguousDevice { .. }) => {
                                            manager = Some(new_manager);
                                            loaded = None;
                                            emit_devices(&weak, &discovered, selected.as_ref());
                                            emit(
                                                &weak,
                                                UiEvent::Error(
                                                    "multiple compatible devices are connected; select one in the device list"
                                                        .into(),
                                                ),
                                            );
                                        }
                                        Err(error) => {
                                            manager = Some(new_manager);
                                            loaded = None;
                                            emit_devices(&weak, &discovered, selected.as_ref());
                                            emit(
                                                &weak,
                                                UiEvent::Error(format_error_string(
                                                    "device resolution failed",
                                                    error,
                                                )),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Command::SelectDevice(id_string) => {
                    let Some(manager_ref) = manager.as_ref() else {
                        emit(
                            &weak,
                            UiEvent::Error(
                                "manager is not initialized; press Refresh to retry".into(),
                            ),
                        );
                        continue;
                    };
                    let id = match DeviceId::new(id_string) {
                        Ok(id) => id,
                        Err(error) => {
                            emit(
                                &weak,
                                UiEvent::OperationError(format!("invalid device id: {error}")),
                            );
                            continue;
                        }
                    };
                    if let Err(error) = manager_ref.select_device(&id) {
                        emit(
                            &weak,
                            UiEvent::OperationError(format_error_string(
                                "device selection failed",
                                error,
                            )),
                        );
                        continue;
                    }
                    match runtime
                        .block_on(manager_ref.resolve_device(Some(&id), TransportSelection::Auto))
                    {
                        Err(error) => {
                            selected = Some(id);
                            emit_devices(&weak, &discovered, selected.as_ref());
                            emit(
                                &weak,
                                UiEvent::OperationError(format_error_string(
                                    "device selection failed",
                                    error,
                                )),
                            );
                        }
                        Ok(resolved) => {
                            selected = Some(resolved.clone());
                            match runtime.block_on(read_loaded(manager_ref, &resolved, None)) {
                                Ok(new_loaded) => {
                                    let new_loaded = subscribe_and_hold(
                                        &runtime,
                                        manager_ref,
                                        new_loaded,
                                        &mut events,
                                        &mut subscriptions,
                                    );
                                    loaded = Some(new_loaded);
                                    emit_devices(&weak, &discovered, selected.as_ref());
                                    if let Some(ready) = loaded.as_ref() {
                                        emit_snapshot(
                                            &weak,
                                            ready,
                                            "device switched — state loaded",
                                        );
                                    }
                                }
                                Err(error) => {
                                    loaded = None;
                                    emit_devices(&weak, &discovered, selected.as_ref());
                                    emit(
                                        &weak,
                                        UiEvent::OperationError(format_error_string(
                                            "device read failed",
                                            error,
                                        )),
                                    );
                                }
                            }
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
                            UiEvent::Error("no device is loaded; press Refresh to retry".into()),
                        );
                        continue;
                    };
                    emit(&weak, UiEvent::Busy(format!("activating profile {number}")));
                    match select_profile(&runtime, manager_ref, current, number) {
                        Ok(new_loaded) => {
                            let status = if new_loaded.identity.transport == TransportKind::Ble {
                                format!(
                                    "profile {number} activation submitted over BLE; readback unavailable"
                                )
                            } else {
                                "profile activation complete".to_owned()
                            };
                            loaded = Some(new_loaded);
                            if let Some(ready) = loaded.as_ref() {
                                emit_snapshot(&weak, ready, &status);
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
                            UiEvent::Error("no device is loaded; press Refresh to retry".into()),
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
                            UiEvent::Error("no device is loaded; press Refresh to retry".into()),
                        );
                        continue;
                    };
                    emit(&weak, UiEvent::Busy("re-reading device state".into()));
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
                Command::DiscardStateSchema => {
                    emit(
                        &weak,
                        UiEvent::Busy("discarding unreadable manager state".into()),
                    );
                    match reset_unreadable_state_and_startup(&runtime) {
                        Ok((new_manager, found, resolved, new_loaded, reset_summary)) => {
                            let new_loaded = subscribe_and_hold(
                                &runtime,
                                &new_manager,
                                new_loaded,
                                &mut events,
                                &mut subscriptions,
                            );
                            manager = Some(new_manager);
                            discovered = found;
                            selected = Some(resolved);
                            loaded = Some(new_loaded);
                            emit_devices(&weak, &discovered, selected.as_ref());
                            if let Some(ready) = loaded.as_ref() {
                                emit_snapshot(&weak, ready, &reset_summary);
                            }
                        }
                        Err(error) => {
                            manager = None;
                            discovered.clear();
                            selected = None;
                            loaded = None;
                            emit(&weak, UiEvent::Error(error));
                        }
                    }
                }
                Command::AddProfile => {
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
                            UiEvent::Error("no device is loaded; press Refresh to retry".into()),
                        );
                        continue;
                    };
                    if current.identity.transport == TransportKind::Ble {
                        emit(
                            &weak,
                            UiEvent::OperationError(
                                "adding a profile is disabled over BLE (profile metadata needs USB readback)"
                                    .into(),
                            ),
                        );
                        continue;
                    }
                    let Some(target) = add_tail_metadata(current.metadata) else {
                        emit(
                            &weak,
                            UiEvent::OperationError(
                                "all profile slots are already enabled; nothing to add".into(),
                            ),
                        );
                        continue;
                    };
                    emit(
                        &weak,
                        UiEvent::Busy("raising the maximum enabled profile".into()),
                    );
                    match runtime.block_on(manager_ref.set_profile_metadata(
                        &current.device,
                        target.current(),
                        target.maximum(),
                    )) {
                        Ok(_) => {
                            let new_loaded = runtime.block_on(read_loaded(
                                manager_ref,
                                &current.device,
                                Some(target.current()),
                            ));
                            match new_loaded {
                                Ok(new_loaded) => {
                                    loaded = Some(new_loaded);
                                    if let Some(ready) = loaded.as_ref() {
                                        emit_snapshot(
                                            &weak,
                                            ready,
                                            &format!(
                                                "profile slot {} enabled (maximum raised to {})",
                                                target.maximum(),
                                                target.maximum()
                                            ),
                                        );
                                    }
                                }
                                Err(error) => emit(
                                    &weak,
                                    format_error("profile added but reload failed", error),
                                ),
                            }
                        }
                        Err(error) => {
                            emit(&weak, format_error("profile metadata update failed", error))
                        }
                    }
                }
                Command::RenameProfile(number, name) => {
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
                            UiEvent::Error("no device is loaded; press Refresh to retry".into()),
                        );
                        continue;
                    };
                    let Some(profile) = ProfileId::new(number) else {
                        emit(
                            &weak,
                            UiEvent::OperationError(format!(
                                "profile {number} is outside the fixed device range"
                            )),
                        );
                        continue;
                    };
                    if let Err(error) =
                        manager_ref.set_profile_name(&current.device, profile, &name)
                    {
                        emit(
                            &weak,
                            UiEvent::OperationError(format_error_string(
                                "profile rename failed",
                                error,
                            )),
                        );
                        continue;
                    }
                    let new_loaded = runtime.block_on(read_loaded(
                        manager_ref,
                        &current.device,
                        Some(current.metadata.current()),
                    ));
                    match new_loaded {
                        Ok(new_loaded) => {
                            loaded = Some(new_loaded);
                            if let Some(ready) = loaded.as_ref() {
                                emit_snapshot(
                                    &weak,
                                    ready,
                                    &format!(
                                        "profile {number} renamed (local display name; no hardware claim)"
                                    ),
                                );
                            }
                        }
                        Err(error) => emit(
                            &weak,
                            format_error("profile renamed but reload failed", error),
                        ),
                    }
                }
                Command::HideProfile(number) => {
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
                            UiEvent::Error("no device is loaded; press Refresh to retry".into()),
                        );
                        continue;
                    };
                    if current.identity.transport == TransportKind::Ble {
                        emit(
                            &weak,
                            UiEvent::OperationError(
                                "hiding a profile is disabled over BLE (profile metadata needs USB readback)"
                                    .into(),
                            ),
                        );
                        continue;
                    }
                    let Some(target) = hide_tail_metadata(current.metadata, number) else {
                        emit(
                            &weak,
                            UiEvent::OperationError(
                                "only the last enabled profile can be hidden; hiding a middle slot is not offered"
                                    .into(),
                            ),
                        );
                        continue;
                    };
                    emit(
                        &weak,
                        UiEvent::Busy("lowering the maximum enabled profile".into()),
                    );
                    let activation = if target.current() != current.metadata.current() {
                        // Hiding the currently active tail first activates the
                        // previous profile so the device never names a hidden
                        // slot as current.
                        Some(runtime.block_on(
                            manager_ref.activate_profile(&current.device, target.current()),
                        ))
                    } else {
                        None
                    };
                    if let Some(Err(error)) = activation {
                        emit(
                            &weak,
                            format_error("profile activation before hide failed", error),
                        );
                        continue;
                    }
                    match runtime.block_on(manager_ref.set_profile_metadata(
                        &current.device,
                        target.current(),
                        target.maximum(),
                    )) {
                        Ok(_) => {
                            let new_loaded = runtime.block_on(read_loaded(
                                manager_ref,
                                &current.device,
                                Some(target.current()),
                            ));
                            match new_loaded {
                                Ok(new_loaded) => {
                                    loaded = Some(new_loaded);
                                    if let Some(ready) = loaded.as_ref() {
                                        emit_snapshot(
                                            &weak,
                                            ready,
                                            &format!(
                                                "profile {number} hidden (maximum lowered to {})",
                                                target.maximum()
                                            ),
                                        );
                                    }
                                }
                                Err(error) => emit(
                                    &weak,
                                    format_error("profile hidden but reload failed", error),
                                ),
                            }
                        }
                        Err(error) => {
                            emit(&weak, format_error("profile metadata update failed", error))
                        }
                    }
                }
                Command::Import(path) => {
                    let Some(manager_ref) = manager.as_ref() else {
                        emit(
                            &weak,
                            UiEvent::Error(
                                "manager is not initialized; press Refresh to retry".into(),
                            ),
                        );
                        continue;
                    };
                    let Some(selected_id) = selected.as_ref() else {
                        emit(
                            &weak,
                            UiEvent::OperationError(
                                "no device is selected to import into; select one first".into(),
                            ),
                        );
                        continue;
                    };
                    let identity = discovered
                        .iter()
                        .find(|device| &device.identity.id == selected_id)
                        .map(|device| device.identity.clone())
                        .or_else(|| manager_ref.device_identity(selected_id).ok());
                    let Some(identity) = identity else {
                        emit(
                            &weak,
                            UiEvent::OperationError(
                                "selected device is not registered; press Refresh to rescan".into(),
                            ),
                        );
                        continue;
                    };
                    let text = match std::fs::read_to_string(&path) {
                        Ok(text) => text,
                        Err(error) => {
                            emit(
                                &weak,
                                UiEvent::OperationError(format!(
                                    "could not read {}: {error}",
                                    path.display()
                                )),
                            );
                            continue;
                        }
                    };
                    let configuration: ConfigurationExport = match serde_json::from_str(&text) {
                        Ok(configuration) => configuration,
                        Err(error) => {
                            emit(
                                &weak,
                                UiEvent::OperationError(format!(
                                    "invalid configuration JSON in {}: {error}",
                                    path.display()
                                )),
                            );
                            continue;
                        }
                    };
                    if let Err(error) = manager_ref.import_configuration(&identity, configuration) {
                        emit(
                            &weak,
                            UiEvent::OperationError(format_error_string(
                                "configuration import failed",
                                error,
                            )),
                        );
                        continue;
                    }
                    if let Some(current) = loaded.as_ref()
                        && current.device == identity.id
                    {
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
                                        &format!(
                                            "configuration imported from {}; stored baselines refreshed (no hardware write)",
                                            path.display()
                                        ),
                                    );
                                }
                            }
                            Err(error) => {
                                emit(
                                    &weak,
                                    format_error("import stored but reload failed", error),
                                );
                            }
                        }
                    } else {
                        emit(
                            &weak,
                            UiEvent::OperationError(format!(
                                "configuration imported from {}; no device is loaded to display",
                                path.display()
                            )),
                        );
                    }
                }
                Command::Export(path) => {
                    let Some(manager_ref) = manager.as_ref() else {
                        emit(
                            &weak,
                            UiEvent::Error(
                                "manager is not initialized; press Refresh to retry".into(),
                            ),
                        );
                        continue;
                    };
                    let Some(selected_id) = selected.as_ref() else {
                        emit(
                            &weak,
                            UiEvent::OperationError(
                                "no device is selected to export; select one first".into(),
                            ),
                        );
                        continue;
                    };
                    let configuration = match manager_ref.export_configuration(selected_id) {
                        Ok(configuration) => configuration,
                        Err(error) => {
                            emit(
                                &weak,
                                UiEvent::OperationError(format_error_string(
                                    "configuration export failed",
                                    error,
                                )),
                            );
                            continue;
                        }
                    };
                    let json = match serde_json::to_string_pretty(&configuration) {
                        Ok(json) => json,
                        Err(error) => {
                            emit(
                                &weak,
                                UiEvent::OperationError(format!(
                                    "configuration could not be encoded: {error}"
                                )),
                            );
                            continue;
                        }
                    };
                    if let Err(error) = std::fs::write(&path, json) {
                        emit(
                            &weak,
                            UiEvent::OperationError(format!(
                                "could not write {}: {error}",
                                path.display()
                            )),
                        );
                        continue;
                    }
                    emit(
                        &weak,
                        UiEvent::OperationError(format!(
                            "configuration exported to {} (desired values, then observed)",
                            path.display()
                        )),
                    );
                }
                Command::RefreshAllProfiles => {
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
                            UiEvent::Error("no device is loaded; press Refresh to retry".into()),
                        );
                        continue;
                    };
                    let device = current.device.clone();
                    match runtime.block_on(manager_ref.refresh_all_profiles(&device)) {
                        Ok(outcome) => {
                            let status = format_refresh_summary(&outcome);
                            match runtime.block_on(read_loaded(
                                manager_ref,
                                &device,
                                Some(outcome.restored_metadata.current()),
                            )) {
                                Ok(new_loaded) => {
                                    loaded = Some(new_loaded);
                                    if let Some(ready) = loaded.as_ref() {
                                        emit_snapshot(&weak, ready, &status);
                                    }
                                }
                                Err(error) => {
                                    loaded = None;
                                    subscriptions = None;
                                    events = None;
                                    emit(
                                        &weak,
                                        format_error(
                                            "profiles refreshed but the restored profile could not be reloaded",
                                            error,
                                        ),
                                    );
                                }
                            }
                        }
                        Err(error) => emit(
                            &weak,
                            UiEvent::OperationError(format_error_string(
                                "all-profile refresh failed",
                                error,
                            )),
                        ),
                    }
                }
                Command::InvalidateState => {
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
                            UiEvent::Error("no device is loaded; press Refresh to retry".into()),
                        );
                        continue;
                    };
                    if let Err(error) = manager_ref.invalidate_state(&current.device) {
                        emit(
                            &weak,
                            UiEvent::OperationError(format_error_string(
                                "state invalidation failed",
                                error,
                            )),
                        );
                        continue;
                    }
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
                                    "persistence evidence invalidated; desired values preserved",
                                );
                            }
                        }
                        Err(error) => {
                            emit(
                                &weak,
                                format_error("state invalidated but reload failed", error),
                            );
                        }
                    }
                }
                Command::VerifyProfile => {
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
                            UiEvent::Error("no device is loaded; press Refresh to retry".into()),
                        );
                        continue;
                    };
                    if current.identity.transport == TransportKind::Ble {
                        emit(
                            &weak,
                            UiEvent::OperationError(
                                "profile-reload verification requires USB readback; unavailable over BLE"
                                    .into(),
                            ),
                        );
                        continue;
                    }
                    let target = current.metadata.current();
                    emit(
                        &weak,
                        UiEvent::Busy(
                            "profile-reload verification — the active profile switches twice"
                                .into(),
                        ),
                    );
                    emit(&weak, UiEvent::Verification(true));
                    let outcome = runtime
                        .block_on(manager_ref.verify_profile_reload(&current.device, target));
                    emit(&weak, UiEvent::Verification(false));
                    match outcome {
                        Ok(outcome) => {
                            let summary = workflow_summary(
                                "profile reload",
                                outcome.profile,
                                &outcome.dpi.verification,
                            );
                            match runtime.block_on(read_loaded(
                                manager_ref,
                                &current.device,
                                Some(target),
                            )) {
                                Ok(new_loaded) => {
                                    loaded = Some(new_loaded);
                                    if let Some(ready) = loaded.as_ref() {
                                        emit_snapshot(&weak, ready, &summary);
                                    }
                                }
                                Err(error) => emit(
                                    &weak,
                                    format_error("verification complete but reload failed", error),
                                ),
                            }
                        }
                        Err(error) => {
                            emit(
                                &weak,
                                UiEvent::Error(format_error_string(
                                    "profile-reload verification failed",
                                    error,
                                )),
                            );
                        }
                    }
                }
                Command::VerifyPowerCycle => {
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
                            UiEvent::Error("no device is loaded; press Refresh to retry".into()),
                        );
                        continue;
                    };
                    if current.identity.transport == TransportKind::Ble {
                        emit(
                            &weak,
                            UiEvent::OperationError(
                                "power-cycle verification requires USB readback; unavailable over BLE"
                                    .into(),
                            ),
                        );
                        continue;
                    }
                    let target = current.metadata.current();
                    emit(
                        &weak,
                        UiEvent::Busy(
                            "power-cycle verification — unplug USB, switch the mouse off, wait for it to disappear, then switch it on and reconnect"
                                .into(),
                        ),
                    );
                    emit(&weak, UiEvent::Verification(true));
                    // Release the event subscription so no stale handle is held
                    // while the device physically disappears and returns.
                    subscriptions = None;
                    events = None;
                    let outcome =
                        runtime.block_on(manager_ref.verify_power_cycle(&current.device, target));
                    emit(&weak, UiEvent::Verification(false));
                    match outcome {
                        Ok(outcome) => {
                            let summary = workflow_summary(
                                "power cycle",
                                outcome.profile,
                                &outcome.dpi.verification,
                            );
                            match runtime.block_on(read_loaded(
                                manager_ref,
                                &current.device,
                                Some(target),
                            )) {
                                Ok(new_loaded) => {
                                    let new_loaded = subscribe_and_hold(
                                        &runtime,
                                        manager_ref,
                                        new_loaded,
                                        &mut events,
                                        &mut subscriptions,
                                    );
                                    loaded = Some(new_loaded);
                                    if let Some(ready) = loaded.as_ref() {
                                        emit_snapshot(&weak, ready, &summary);
                                    }
                                }
                                Err(error) => emit(
                                    &weak,
                                    format_error("verification complete but reload failed", error),
                                ),
                            }
                        }
                        Err(error) => {
                            emit(
                                &weak,
                                UiEvent::Error(format_error_string(
                                    "power-cycle verification failed",
                                    error,
                                )),
                            );
                        }
                    }
                }
                Command::Shutdown => break,
            },
            Err(RecvTimeoutError::Timeout) => {
                if let Some(rx) = events.as_mut() {
                    runtime.block_on(async { tokio::task::yield_now().await });
                    while let Ok(event) = rx.try_recv() {
                        handle_device_event(
                            &runtime,
                            &mut loaded,
                            &mut manager,
                            event,
                            &weak,
                            &mut discovered,
                            &mut selected,
                        );
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

/// Subscribes the freshly loaded device to event delivery and keeps the
/// subscription alive for the lifetime of the worker loop.
fn subscribe_and_hold(
    runtime: &tokio::runtime::Runtime,
    manager: &DeviceManager,
    loaded: LoadedDevice,
    events: &mut Option<tokio::sync::broadcast::Receiver<DeviceEvent>>,
    subscriptions: &mut Option<EventSubscriptions>,
) -> LoadedDevice {
    match runtime.block_on(manager.subscribe_events(&loaded.device)) {
        Ok(mut sub) => {
            *events = sub.events.take();
            *subscriptions = Some(sub);
        }
        Err(_) => {
            *events = None;
            *subscriptions = None;
        }
    }
    loaded
}

fn handle_device_event(
    runtime: &tokio::runtime::Runtime,
    loaded: &mut Option<LoadedDevice>,
    manager: &mut Option<DeviceManager>,
    event: DeviceEvent,
    weak: &slint::Weak<AppWindow>,
    discovered: &mut [DiscoveredDevice],
    selected: &mut Option<DeviceId>,
) {
    match event {
        DeviceEvent::BatteryChanged(e) => {
            if let Some(dev) = loaded.as_mut() {
                dev.battery = Some(e.level);
                emit_event_snapshot(weak, dev, "device battery level changed");
            }
        }
        DeviceEvent::ActiveDpiStageChanged(e) => {
            if let Some(dev) = loaded.as_mut() {
                dev.profile.dpi.active_stage = e.active_stage;
                emit_event_snapshot(weak, dev, "active DPI stage changed on device");
            }
        }
        DeviceEvent::ProfileChanged(_)
        | DeviceEvent::ProfileSync(_)
        | DeviceEvent::SecondaryProfileChanged(_) => {
            if let (Some(dev), Some(mgr)) = (loaded.as_mut(), manager.as_ref())
                && let Some(profile) = reported_profile_switch(event, dev.metadata.current())
            {
                match select_profile(runtime, mgr, dev, profile.get()) {
                    Ok(new_loaded) => {
                        *loaded = Some(new_loaded);
                        if let Some(ready) = loaded.as_ref() {
                            emit_event_snapshot(
                                weak,
                                ready,
                                "device profile changed; state reloaded",
                            );
                        }
                    }
                    Err(error) => emit(weak, UiEvent::Error(error)),
                }
            }
        }
        DeviceEvent::Disconnected => {
            handle_disconnect(loaded, discovered, selected, weak);
        }
        DeviceEvent::ConnectionChanged(e) if !e.connected => {
            handle_disconnect(loaded, discovered, selected, weak);
        }
        _ => {}
    }
}

/// Drops the loaded device, marks the exact device disconnected in the
/// discovery list, and keeps the UI on discovery controls.
fn handle_disconnect(
    loaded: &mut Option<LoadedDevice>,
    discovered: &mut [DiscoveredDevice],
    selected: &mut Option<DeviceId>,
    weak: &slint::Weak<AppWindow>,
) {
    let Some(dev) = loaded.as_ref() else {
        return;
    };
    if let Some(entry) = discovered
        .iter_mut()
        .find(|entry| entry.identity.id == dev.device)
    {
        entry.connected = false;
    }
    *loaded = None;
    *selected = None;
    emit_devices(weak, discovered, selected.as_ref());
    emit(
        weak,
        UiEvent::Error("device disconnected; select another device or press Refresh".into()),
    );
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

/// Emits a snapshot originating from an asynchronous device event rather than
/// an explicit user operation, so the UI can preserve an open draft.
fn emit_event_snapshot(weak: &slint::Weak<AppWindow>, loaded: &LoadedDevice, status: &str) {
    emit(
        weak,
        UiEvent::EventSnapshot(Box::new(make_live_snapshot(loaded, status))),
    );
}

fn emit_devices(
    weak: &slint::Weak<AppWindow>,
    discovered: &[DiscoveredDevice],
    selected: Option<&DeviceId>,
) {
    let entries = discovered
        .iter()
        .map(|device| {
            let transport = match device.identity.transport {
                TransportKind::Wired => "usb wired",
                TransportKind::Receiver => "2.4g receiver",
                TransportKind::Ble => "ble",
            };
            DeviceListEntry {
                id: device.identity.id.to_string(),
                name: device
                    .identity
                    .display_name
                    .clone()
                    .unwrap_or_else(|| "X3-compatible device".to_owned()),
                detail: format!(
                    "{transport} · {}",
                    if device.connected {
                        "connected"
                    } else {
                        "disconnected"
                    }
                ),
                selected: selected.is_some_and(|id| *id == device.identity.id),
                connected: device.connected,
            }
        })
        .collect();
    emit(weak, UiEvent::Devices(entries));
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
            clear_live_state(ui);
            ui.set_status_text(error.into());
        }
        UiEvent::OperationError(error) => {
            ui.set_busy(false);
            ui.set_status_text(error.into());
        }
        UiEvent::StateUnreadable(error) => {
            clear_live_state(ui);
            ui.set_status_text(
                format!("{error}; discard the file (a backup is kept) to continue").into(),
            );
            ui.set_state_conflict_text(error.into());
            ui.invoke_show_state_conflict();
        }
        UiEvent::Verification(running) => {
            ui.set_verification_running(running);
            if !running {
                ui.set_verification_instruction("".into());
            }
        }
        UiEvent::Devices(entries) => {
            let devices = ui.get_devices();
            let Some(model) = devices.as_any().downcast_ref::<VecModel<DeviceRow>>() else {
                return;
            };
            clear_model(model);
            for entry in entries {
                model.push(DeviceRow {
                    id: entry.id.into(),
                    name: entry.name.into(),
                    detail: entry.detail.into(),
                    selected: entry.selected,
                    connected: entry.connected,
                });
            }
        }
        UiEvent::Snapshot(snapshot) => apply_snapshot(ui, &snapshot, false),
        UiEvent::EventSnapshot(snapshot) => apply_snapshot(ui, &snapshot, true),
    }
}

/// Applies a snapshot to the UI. Event-driven snapshots preserve an open
/// draft (see `snapshot_preserves_draft`); explicit operation/load snapshots
/// replace every model and clear the draft.
fn apply_snapshot(ui: &AppWindow, snapshot: &LiveSnapshot, event_driven: bool) {
    if snapshot_preserves_draft(event_driven, ui.get_dirty()) {
        apply_snapshot_preserving_draft(ui, snapshot);
    } else {
        apply_snapshot_replacing(ui, snapshot);
    }
}

/// True when an incoming snapshot must preserve the draft: an event-driven
/// snapshot arriving while the user has unsaved edits. Explicit operations
/// (refresh, apply, select, discard, reload) may always replace the models
/// and clear the draft.
fn snapshot_preserves_draft(event_driven: bool, dirty: bool) -> bool {
    event_driven && dirty
}

/// Status-line warning shown when an event-driven snapshot arrives while a
/// draft is open: the draft is preserved, but the external change may make
/// the next Save fail until the user discards or reloads.
fn event_snapshot_draft_warning(status: &str) -> String {
    format!(
        "{status} — external state changed while a draft was open; the draft was preserved, but Save may be rejected until you discard / reload"
    )
}

/// Applies an explicit operation/load snapshot: every live field and the
/// editable models are replaced from the device state and the draft is
/// cleared.
fn apply_snapshot_replacing(ui: &AppWindow, snapshot: &LiveSnapshot) {
    ui.set_busy(false);
    ui.set_hardware_ready(true);
    ui.set_lifecycle_text(
        if snapshot.is_ble {
            "connected — stored/imported baseline loaded (no readback)"
        } else {
            "connected — live USB readback loaded"
        }
        .into(),
    );
    ui.set_status_text(snapshot.status.clone().into());
    ui.set_transport_label(snapshot.transport.clone().into());
    ui.set_battery_text(snapshot.battery.clone().into());
    ui.set_device_name(snapshot.device_name.clone().into());
    ui.set_device_id_text(snapshot.stable_id.clone().into());
    ui.set_device_product_text(snapshot.product_id.clone().into());
    ui.set_profile_summary(snapshot.profile_summary.clone().into());
    ui.set_metadata_summary(snapshot.metadata_summary.clone().into());
    ui.set_sensor_summary(snapshot.sensor.clone().into());
    ui.set_verification_text(snapshot.verification_summary.clone().into());
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
    ui.set_polling_rate_text(polling_rate_text(snapshot).as_str().into());
    ui.set_motion_sync_readback(snapshot.motion_sync);
    ui.set_ripple_control_readback(snapshot.ripple_control);
    ui.set_angle_snap(snapshot.angle_snap);
    ui.set_lift_off_choice(snapshot.lift_off_choice as i32);
    ui.set_debounce_ms(snapshot.debounce_ms.unwrap_or(0) as i32);
    ui.set_sleep_half_minutes(snapshot.sleep_half_minutes.unwrap_or(0) as i32);
    ui.set_deep_sleep_minutes(snapshot.deep_sleep_minutes.unwrap_or(0) as i32);
    ui.set_deep_sleep_known(snapshot.deep_sleep_minutes.is_some());
    ui.set_preferences_ready(snapshot.preferences_ready);
    ui.set_preference_configuration_raw(snapshot.raw_configuration.clone().into());
    ui.set_preference_deep_sleep_raw(snapshot.raw_deep_sleep.clone().into());
    ui.set_preference_sleep_timer_raw(snapshot.raw_sleep_timer.clone().into());
    ui.set_preference_debounce_raw(snapshot.raw_debounce.clone().into());
    ui.set_ble_device(snapshot.is_ble);
    ui.set_all_profiles_observed(snapshot.all_profiles_observed);
    if snapshot.is_ble {
        ui.set_baseline_choice(1);
    }
    ui.set_verification_running(false);
    ui.set_last_enabled_profile(snapshot.last_enabled_profile);
    ui.set_dirty(false);
    replace_dpi_model(
        &ui.get_dpi_stages(),
        &snapshot.stages,
        ui.get_dpi_log_scale(),
        ui.get_dpi_min(),
        ui.get_dpi_max(),
    );
    replace_binding_model(&ui.get_bindings(), &snapshot.bindings);
    replace_profile_model(&ui.get_profiles(), &snapshot.profiles, snapshot.is_ble);
    refresh_button_change_summary(ui);
}

/// Applies an event-driven snapshot while a draft is open: the editable
/// models and every draft property are preserved, only safe readback
/// telemetry and status are refreshed, and the user is warned that the
/// external change may make the next Save fail until discard/reload.
fn apply_snapshot_preserving_draft(ui: &AppWindow, snapshot: &LiveSnapshot) {
    ui.set_busy(false);
    ui.set_hardware_ready(true);
    ui.set_lifecycle_text(
        if snapshot.is_ble {
            "connected — stored/imported baseline loaded (no readback)"
        } else {
            "connected — live USB readback loaded"
        }
        .into(),
    );
    ui.set_status_text(event_snapshot_draft_warning(&snapshot.status).into());
    ui.set_transport_label(snapshot.transport.clone().into());
    ui.set_battery_text(snapshot.battery.clone().into());
    ui.set_device_name(snapshot.device_name.clone().into());
    ui.set_device_id_text(snapshot.stable_id.clone().into());
    ui.set_device_product_text(snapshot.product_id.clone().into());
    ui.set_profile_summary(snapshot.profile_summary.clone().into());
    ui.set_metadata_summary(snapshot.metadata_summary.clone().into());
    ui.set_sensor_summary(snapshot.sensor.clone().into());
    ui.set_verification_text(snapshot.verification_summary.clone().into());
    ui.set_active_dpi_text(format!("{} dpi", snapshot.active_dpi).into());
    ui.set_active_stage_text(
        format!(
            "stage {} of {}",
            snapshot.active_stage, snapshot.stage_count
        )
        .into(),
    );
    ui.set_polling_rate_ready(snapshot.polling_rate_ready);
    ui.set_ble_device(snapshot.is_ble);
    ui.set_all_profiles_observed(snapshot.all_profiles_observed);
    ui.set_last_enabled_profile(snapshot.last_enabled_profile);
    replace_profile_model(&ui.get_profiles(), &snapshot.profiles, snapshot.is_ble);
    refresh_button_change_summary(ui);
}

fn polling_rate_text(snapshot: &LiveSnapshot) -> String {
    if snapshot.is_ble {
        "unavailable (ble — no readback)".to_owned()
    } else if snapshot.polling_rate_ready {
        format!("{} hz", snapshot.polling_rate_hz)
    } else {
        format!("{} hz · rate write unavailable", snapshot.polling_rate_hz)
    }
}

/// Resets every live-readback field shown by the UI, used when a worker
/// operation fails and no device state can be displayed. The discovered
/// device list is left untouched so the user can still switch devices.
fn clear_live_state(ui: &AppWindow) {
    ui.set_busy(false);
    ui.set_hardware_ready(false);
    ui.set_lifecycle_text("error — no live device state".into());
    ui.set_transport_label("no device selected".into());
    ui.set_polling_rate_ready(false);
    ui.set_polling_rate_text("not available".into());
    ui.set_motion_sync_readback(false);
    ui.set_ripple_control_readback(false);
    ui.set_angle_snap(false);
    ui.set_lift_off_choice(0);
    ui.set_preferences_ready(false);
    ui.set_debounce_ms(0);
    ui.set_sleep_half_minutes(0);
    ui.set_deep_sleep_minutes(0);
    ui.set_deep_sleep_known(false);
    ui.set_preference_configuration_raw("00".into());
    ui.set_preference_deep_sleep_raw("00".into());
    ui.set_preference_sleep_timer_raw("00".into());
    ui.set_preference_debounce_raw("00".into());
    ui.set_baseline_choice(0);
    ui.set_allow_explicit_defaults(false);
    ui.set_ble_device(false);
    ui.set_all_profiles_observed(false);
    ui.set_verification_running(false);
    ui.set_verification_instruction("".into());
    ui.set_last_enabled_profile(-1);
    ui.set_battery_text("unavailable".into());
    ui.set_device_name("device not loaded".into());
    ui.set_device_id_text("not available".into());
    ui.set_device_product_text("not available".into());
    ui.set_profile_summary("no live profile".into());
    ui.set_metadata_summary("not available".into());
    ui.set_sensor_summary("not available".into());
    ui.set_verification_text("no evidence".into());
    ui.set_active_dpi_text("not available".into());
    ui.set_active_stage_text("not available".into());
    ui.set_dirty(false);
    ui.invoke_close_state_conflict();
    replace_dpi_model(
        &ui.get_dpi_stages(),
        &[],
        ui.get_dpi_log_scale(),
        ui.get_dpi_min(),
        ui.get_dpi_max(),
    );
    replace_binding_model(&ui.get_bindings(), &[]);
    replace_profile_model(&ui.get_profiles(), &[], false);
    refresh_button_change_summary(ui);
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

/// Exact per-button change summary: one `button → action` line per physical
/// button whose draft action differs from its loaded baseline, or the exact
/// no-change sentence. The summary is recomputed after every binding edit and
/// every snapshot so the save confirmation always matches the draft.
fn button_change_summary(bindings: &[BindingRow]) -> String {
    let lines: Vec<String> = bindings
        .iter()
        .filter(|item| item.action != item.original_action)
        .map(|item| format!("{} → {}", item.button, item.action))
        .collect();
    if lines.is_empty() {
        "No physical button changes.".to_owned()
    } else {
        lines.join("\n")
    }
}

/// Recomputes the save-confirmation button summary from the current binding
/// model after a binding edit or a snapshot.
fn refresh_button_change_summary(ui: &AppWindow) {
    let mut rows = Vec::new();
    let model = ui.get_bindings();
    if let Some(model) = model.as_any().downcast_ref::<VecModel<BindingRow>>() {
        for row in 0..model.row_count() {
            if let Some(item) = model.row_data(row) {
                rows.push(item);
            }
        }
    }
    ui.set_button_change_summary(button_change_summary(&rows).into());
}

fn replace_profile_model(model: &ModelRc<ProfileRow>, profiles: &[LiveProfile], is_ble: bool) {
    let Some(model) = model.as_any().downcast_ref::<VecModel<ProfileRow>>() else {
        return;
    };
    clear_model(model);
    for profile in profiles {
        let subtitle = if is_ble {
            if profile.enabled {
                "saved settings"
            } else {
                "hidden"
            }
        } else if profile.enabled {
            "available"
        } else {
            "hidden"
        };
        model.push(ProfileRow {
            name: profile.name.clone().into(),
            subtitle: subtitle.into(),
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

fn open_manager() -> Result<DeviceManager, StartupError> {
    let store = StateStore::with_default_paths().map_err(|error| {
        StartupError::Message(format!("could not open manager state store: {error}"))
    })?;
    if let Err(error) = store.load() {
        return Err(StartupError::UnreadableState(format!(
            "manager state file {} is unreadable ({error}); use the dialog to discard it and continue",
            store.paths().state_file.display()
        )));
    }
    DeviceManager::new(store).map_err(|error| {
        StartupError::Message(format!("could not initialize DeviceManager: {error}"))
    })
}

/// Discards an unreadable state file (keeping a sibling backup) and reruns
/// device startup, returning a human-readable reset summary for the UI.
fn reset_unreadable_state_and_startup(
    runtime: &tokio::runtime::Runtime,
) -> Result<
    (
        DeviceManager,
        Vec<DiscoveredDevice>,
        DeviceId,
        LoadedDevice,
        String,
    ),
    String,
> {
    let store = StateStore::with_default_paths()
        .map_err(|error| format!("could not open manager state store: {error}"))?;
    let reset_manager = DeviceManager::new(store)
        .map_err(|error| format!("could not initialize DeviceManager: {error}"))?;
    let reset = reset_manager
        .discard_unreadable_state()
        .map_err(|error| format!("state discard failed: {error}"))?;
    let summary = match (&reset.backup, reset.discarded_schema) {
        (Some(path), Some(found)) => format!(
            "unreadable state (schema {found}) backed up to {}; fresh empty state written",
            path.display()
        ),
        (Some(path), None) => format!(
            "unreadable state backed up to {}; fresh empty state written",
            path.display()
        ),
        (None, _) => "no state file existed; fresh empty state written".into(),
    };
    match startup_flow(runtime) {
        Ok((manager, discovered, resolved, loaded)) => {
            Ok((manager, discovered, resolved, loaded, summary))
        }
        Err(StartupError::UnreadableState(detail)) => Err(detail),
        Err(StartupError::Message(detail)) => Err(detail),
    }
}

/// Discovers every enabled transport and resolves one device without
/// guessing, honoring the durable selection.
fn startup_flow(
    runtime: &tokio::runtime::Runtime,
) -> Result<(DeviceManager, Vec<DiscoveredDevice>, DeviceId, LoadedDevice), StartupError> {
    let manager = open_manager()?;
    let discovered = runtime
        .block_on(manager.list_devices(TransportSelection::Auto))
        .map_err(|error| StartupError::Message(format!("device discovery failed: {error}")))?;
    let resolved = runtime
        .block_on(manager.resolve_device(None, TransportSelection::Auto))
        .map_err(|error| {
            StartupError::Message(format_error_string("device resolution failed", error))
        })?;
    let loaded = runtime
        .block_on(read_loaded(&manager, &resolved, None))
        .map_err(|error| {
            StartupError::Message(format_error_string("initial device read failed", error))
        })?;
    Ok((manager, discovered, resolved, loaded))
}

async fn read_loaded(
    manager: &DeviceManager,
    device: &DeviceId,
    requested: Option<ProfileId>,
) -> Result<LoadedDevice, ManagerError> {
    let status = manager.read_status(device).await?;
    if status.identity.transport == TransportKind::Ble {
        return read_ble_loaded(manager, status, requested);
    }
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
    let all_profiles_observed = has_complete_profile_observations(&state, device);
    let profile_names = manager.profile_names(device).unwrap_or_default();
    let verification_summary = stored_evidence_summary(&state, device, target, false, false);
    Ok(LoadedDevice {
        device: device.clone(),
        identity: status.identity,
        metadata,
        profile: ProfileData {
            dpi: profile.dpi,
            preferences: profile.preferences,
            buttons: profile.buttons,
        },
        polling_rate,
        battery: status.battery,
        polling_rate_ready,
        all_profiles_observed,
        captured_image: false,
        has_stored_preferences: true,
        profile_names,
        verification_summary,
    })
}

/// Loads the BLE view of a device: identity and battery from status, plus the
/// stored desired/imported profile baseline (falling back to the captured
/// stock image) for the requested working profile. BLE never reads back.
fn read_ble_loaded(
    manager: &DeviceManager,
    status: DeviceStatus,
    requested: Option<ProfileId>,
) -> Result<LoadedDevice, ManagerError> {
    let identity = status.identity;
    let working = requested.unwrap_or_else(|| ProfileId::new(1).expect("profile 1 is valid"));
    let state = manager.store().load()?;
    let profile_state = state
        .devices
        .get(&identity.id)
        .and_then(|device| device.profiles.get(&working));
    let dpi = profile_state
        .and_then(|profile| stored_value(&profile.dpi))
        .or_else(|| attack_shark_x3_manager::DpiState::captured_stock_reset(working).ok());
    let preferences = profile_state
        .and_then(|profile| stored_value(&profile.preferences))
        .or_else(|| Some(attack_shark_x3_manager::PreferencesState::captured_stock_reset(working)));
    let buttons = profile_state
        .and_then(|profile| stored_value(&profile.buttons))
        .or_else(|| {
            Some(attack_shark_x3_manager::ButtonsState::default_for_profile(
                working,
            ))
        });
    let captured_image = profile_state.is_none()
        || profile_state.is_some_and(|profile| {
            profile.dpi.desired.is_none()
                && profile.dpi.observed.is_none()
                && profile.preferences.desired.is_none()
                && profile.preferences.observed.is_none()
                && profile.buttons.desired.is_none()
                && profile.buttons.observed.is_none()
        });
    let has_stored_preferences = profile_state.is_some_and(|profile| {
        profile.preferences.desired.is_some() || profile.preferences.observed.is_some()
    });
    let polling_rate = profile_state
        .and_then(|profile| stored_value(&profile.polling_rate))
        .unwrap_or(PollingRate::Hz1000);
    let metadata = ProfileMetadata::new(
        working,
        ProfileId::new(ProfileId::MAX).expect("profile maximum must be valid"),
    )
    .map_err(|error| ManagerError::InvalidUpdate(error.to_string()))?;
    let all_profiles_observed = false;
    let profile_names = manager.profile_names(&identity.id).unwrap_or_default();
    let verification_summary =
        stored_evidence_summary(&state, &identity.id, working, true, captured_image);
    Ok(LoadedDevice {
        device: identity.id.clone(),
        identity,
        metadata,
        profile: ProfileData {
            dpi: dpi.expect("captured stock DPI must be valid"),
            preferences: preferences.expect("captured stock preferences must be valid"),
            buttons: buttons.expect("default button image must be valid"),
        },
        polling_rate,
        battery: status.battery,
        polling_rate_ready: false,
        all_profiles_observed,
        captured_image,
        has_stored_preferences,
        profile_names,
        verification_summary,
    })
}

/// Resolves the stored desired value, falling back to the stored observed
/// value, for one profile resource.
fn stored_value<T: Clone>(resource: &ResourceState<T>) -> Option<T> {
    resource
        .desired
        .as_ref()
        .map(|desired| desired.value.clone())
        .or_else(|| {
            resource
                .observed
                .as_ref()
                .map(|observed| observed.value.clone())
        })
}

fn has_complete_profile_observations(state: &StateFile, device: &DeviceId) -> bool {
    let Some(device_state) = state.devices.get(device) else {
        return false;
    };

    (ProfileId::MIN..=ProfileId::MAX)
        .filter_map(ProfileId::new)
        .all(|profile| {
            device_state.profiles.get(&profile).is_some_and(|state| {
                state.dpi.observed.is_some()
                    && state.preferences.observed.is_some()
                    && state.buttons.observed.is_some()
                    && state.polling_rate.observed.is_some()
            })
        })
}

/// Which persistence claims a profile's stored resources carry.
#[derive(Default)]
struct SubmissionSummary {
    power_cycle: bool,
    reload: bool,
    submitted: bool,
    imported_only: bool,
}

/// Records the persistence claim carried by one stored resource's desired
/// value, if any. Generic over the resource payload so it works uniformly
/// for every resource kind without allocation.
fn record_submission<T>(resource: &ResourceState<T>, summary: &mut SubmissionSummary) {
    let Some(desired) = resource.desired.as_ref() else {
        return;
    };
    summary.submitted = true;
    if desired.source != DesiredSource::Imported {
        summary.imported_only = false;
    }
    match desired.verification.persistence {
        PersistenceVerification::PowerCycleVerified { .. } => summary.power_cycle = true,
        PersistenceVerification::ProfileReloadVerified { .. } => summary.reload = true,
        PersistenceVerification::Unknown => {}
    }
}

/// Builds the strongest evidence summary for one profile from the durable
/// store: power-cycle verification beats profile-reload verification, which
/// beats a plain submission.
fn stored_evidence_summary(
    state: &StateFile,
    device: &DeviceId,
    profile: ProfileId,
    is_ble: bool,
    captured_image: bool,
) -> String {
    if captured_image {
        return "captured stock image shown; no stored baseline (import one or enable explicit defaults)"
            .to_owned();
    }
    let Some(profile_state) = state
        .devices
        .get(device)
        .and_then(|device| device.profiles.get(&profile))
    else {
        return if is_ble {
            "stored/imported baseline only; no submissions yet".to_owned()
        } else {
            "live readback observed; persistence unverified".to_owned()
        };
    };
    let mut summary = SubmissionSummary::default();
    record_submission(&profile_state.dpi, &mut summary);
    record_submission(&profile_state.preferences, &mut summary);
    record_submission(&profile_state.buttons, &mut summary);
    if summary.power_cycle {
        "power-cycle persistence verified".to_owned()
    } else if summary.reload {
        "profile-reload persistence verified".to_owned()
    } else if summary.submitted && summary.imported_only {
        "imported configuration stored; not sent to hardware".to_owned()
    } else if summary.submitted {
        "writes submitted; persistence not yet verified".to_owned()
    } else if is_ble {
        "stored/imported baseline only; no submissions yet".to_owned()
    } else {
        "live readback observed; persistence unverified".to_owned()
    }
}

fn select_profile(
    runtime: &tokio::runtime::Runtime,
    manager: &DeviceManager,
    current: &LoadedDevice,
    number: u8,
) -> Result<LoadedDevice, String> {
    let target = ProfileId::new(number)
        .ok_or_else(|| format!("profile {number} is outside the fixed device range 1..=5"))?;
    if current.identity.transport == TransportKind::Ble {
        runtime
            .block_on(manager.activate_profile(&current.device, target))
            .map_err(|error| format_error_string("BLE profile activation failed", error))?;
        return runtime
            .block_on(read_loaded(manager, &current.device, Some(target)))
            .map_err(|error| format_error_string("stored baseline reload failed", error));
    }
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
    let is_ble = current.identity.transport == TransportKind::Ble;
    if !is_ble {
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
    }
    let policy = UpdatePolicy {
        allow_explicit_defaults: draft.allow_explicit_defaults,
        verification: if is_ble {
            VerificationMethod::Transport
        } else {
            match draft.verification {
                VerificationChoice::Transport => VerificationMethod::Transport,
                VerificationChoice::Readback => VerificationMethod::Readback,
            }
        },
        baseline: if is_ble {
            BaselineSource::Stored
        } else {
            match draft.baseline {
                BaselineChoice::Live => BaselineSource::Live,
                BaselineChoice::Stored => BaselineSource::Stored,
            }
        },
    };

    if draft.dpi_values.is_empty() || draft.dpi_values.len() > MAX_DPI_STAGES {
        return Err(format!(
            "draft has {} DPI stages; the device supports 1..={MAX_DPI_STAGES}; no write was attempted",
            draft.dpi_values.len()
        ));
    }
    let active_stage_index = draft.active_stage;
    if active_stage_index == 0 || active_stage_index as usize > draft.dpi_values.len() {
        return Err(
            "draft active DPI stage does not refer to a configured stage; no write was attempted"
                .into(),
        );
    }

    let baseline_dpi: Vec<u16> = current
        .profile
        .dpi
        .stages
        .iter()
        .copied()
        .map(DpiValue::get)
        .collect();
    let requested_dpi: Vec<u16> = draft
        .dpi_values
        .iter()
        .copied()
        .map(|value| round_dpi_step(value as f32))
        .collect();
    let dpi_changed = baseline_dpi != requested_dpi
        || current.profile.dpi.active_stage.get() != active_stage_index;
    let sensor = current.profile.dpi.sensor;
    let requested_lift = lift_off_choice_value(draft.lift_off_choice);
    let lift_changed = sensor.lift_off_distance != requested_lift;
    let ripple_changed = sensor.ripple_control != draft.ripple_control;
    let angle_changed = sensor.angle_snap != draft.angle_snap;
    let motion_changed = sensor.motion_sync != draft.motion_sync;
    let sensor_delta = SensorOptionsDelta {
        lift_off_distance: lift_changed.then_some(requested_lift),
        ripple_control: ripple_changed.then_some(draft.ripple_control),
        angle_snap: angle_changed.then_some(draft.angle_snap),
        motion_sync: motion_changed.then_some(draft.motion_sync),
    };

    let preferences = current.profile.preferences;
    let configuration_changed = preferences.configuration != draft.raw_configuration;
    let deep_sleep_changed = preferences.deep_sleep != draft.raw_deep_sleep;
    let sleep_timer_changed = preferences.sleep_timer != draft.raw_sleep_timer;
    let debounce_changed = preferences.debounce != draft.raw_debounce;
    let preferences_changed =
        configuration_changed || deep_sleep_changed || sleep_timer_changed || debounce_changed;
    // With a captured image (BLE with no stored baseline) the merge baseline
    // differs from the displayed image, so any preference change writes the
    // full displayed preference image to stay deterministic.
    let preferences_delta = if preferences_changed && current.captured_image {
        PreferencesDelta {
            light_mode: None,
            configuration: Some(draft.raw_configuration),
            deep_sleep: Some(draft.raw_deep_sleep),
            host_color: None,
            sleep_timer: Some(draft.raw_sleep_timer),
            debounce: Some(draft.raw_debounce),
        }
    } else {
        PreferencesDelta {
            light_mode: None,
            configuration: configuration_changed.then_some(draft.raw_configuration),
            deep_sleep: deep_sleep_changed.then_some(draft.raw_deep_sleep),
            host_color: None,
            sleep_timer: sleep_timer_changed.then_some(draft.raw_sleep_timer),
            debounce: debounce_changed.then_some(draft.raw_debounce),
        }
    };

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

    let requested_rate = PollingRate::new(draft.polling_rate_hz)
        .ok_or_else(|| "unsupported polling rate".to_owned())?;
    let rate_changed = requested_rate != current.polling_rate;
    if rate_changed {
        if is_ble {
            return Err(
                "polling-rate writes are disabled over BLE (the safe preflight needs USB readback); no hardware write was attempted"
                    .into(),
            );
        }
        if !current.polling_rate_ready {
            return Err(
                "polling-rate write is unavailable: the manager does not yet have complete desired DPI, preferences, and buttons for this profile; no hardware write was attempted"
                    .into(),
            );
        }
        if dpi_changed
            || !sensor_delta.is_empty()
            || !preferences_delta.is_empty()
            || !button_changes.is_empty()
        {
            return Err(
                "polling-rate changes must be applied separately from DPI, sensor, preference, or button changes so the manager can validate the complete live profile before report 0x06; no hardware write was attempted"
                    .into(),
            );
        }
    }

    let mut evidence = Vec::new();
    if dpi_changed || !sensor_delta.is_empty() {
        let stages = requested_dpi
            .iter()
            .copied()
            .map(|value| DpiValue::new(value).ok_or_else(|| format!("invalid DPI value {value}")))
            .collect::<Result<Vec<_>, _>>()?;
        let active_stage = StageIndex::new(active_stage_index)
            .ok_or_else(|| "invalid active DPI stage".to_owned())?;
        let full_image = dpi_changed || current.captured_image;
        let delta = DpiDelta {
            stages: full_image.then_some(stages),
            active_stage: full_image.then_some(active_stage),
            sensor: (!sensor_delta.is_empty()).then_some(sensor_delta),
        };
        if !delta.is_empty() {
            let outcome = runtime
                .block_on(manager.update_dpi_delta(&current.device, profile, delta, policy))
                .map_err(|error| format_error_string("DPI update failed", error))?;
            evidence.push(verification_summary(&outcome.verification));
        }
    }
    if !preferences_delta.is_empty() {
        let outcome = runtime
            .block_on(manager.update_preferences_delta(
                &current.device,
                profile,
                preferences_delta,
                policy,
            ))
            .map_err(|error| format_error_string("preferences update failed", error))?;
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
    if rate_changed {
        // Report 0x06 can persist the complete live profile image. The manager
        // must reject a missing or stale desired baseline rather than the GUI
        // creating one through hidden, redundant hardware writes.
        let outcome = runtime
            .block_on(manager.update_polling_rate(&current.device, profile, requested_rate, policy))
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
            "safe writes complete: {}; persistence not yet verified (use the verification workflows)",
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
    let is_ble = loaded.identity.transport == TransportKind::Ble;
    let transport = match loaded.identity.transport {
        TransportKind::Wired => "usb wired",
        TransportKind::Receiver => "2.4g receiver",
        TransportKind::Ble => "ble",
    };
    let battery = loaded.battery.map_or_else(
        || {
            if is_ble {
                "unavailable (ble)".to_owned()
            } else {
                "unavailable (wired USB)".to_owned()
            }
        },
        |value| format!("{value}%"),
    );
    let sensor = loaded.profile.dpi.sensor;
    let fields = preference_fields(&loaded.profile.preferences);
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
    let profiles = (1..=ProfileId::MAX)
        .map(|number| {
            let profile = ProfileId::new(number).expect("profile number in range");
            LiveProfile {
                enabled: number <= loaded.metadata.maximum().get(),
                current: number == loaded.metadata.current().get(),
                name: loaded
                    .profile_names
                    .get(&profile)
                    .cloned()
                    .unwrap_or_else(|| format!("profile {number}")),
            }
        })
        .collect();
    let metadata_summary = if is_ble {
        format!(
            "current {} / maximum {} · not read back (ble)",
            loaded.metadata.current(),
            loaded.metadata.maximum()
        )
    } else {
        format!(
            "current {} / maximum {}",
            loaded.metadata.current(),
            loaded.metadata.maximum()
        )
    };
    let profile_summary = if is_ble {
        format!(
            "profile {} · {} dpi · stored baseline (no readback)",
            loaded.metadata.current(),
            active_dpi
        )
    } else {
        format!(
            "profile {} · {} dpi · {} hz",
            loaded.metadata.current(),
            active_dpi,
            loaded.polling_rate.hz()
        )
    };
    LiveSnapshot {
        device_name: loaded
            .identity
            .display_name
            .clone()
            .unwrap_or_else(|| "X3-compatible device".to_owned()),
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
        is_ble,
        all_profiles_observed: loaded.all_profiles_observed,
        motion_sync: sensor.motion_sync,
        ripple_control: sensor.ripple_control,
        angle_snap: sensor.angle_snap,
        lift_off_choice: match sensor.lift_off_distance {
            LiftOffDistance::OneMillimeter => 0,
            LiftOffDistance::TwoMillimeters => 1,
        },
        debounce_ms: fields.debounce_ms,
        sleep_half_minutes: fields.sleep_half_minutes,
        deep_sleep_minutes: fields.deep_sleep_minutes,
        raw_configuration: fields.raw_configuration,
        raw_deep_sleep: fields.raw_deep_sleep,
        raw_sleep_timer: fields.raw_sleep_timer,
        raw_debounce: fields.raw_debounce,
        preferences_ready: loaded.has_stored_preferences || !is_ble,
        last_enabled_profile: (loaded.metadata.maximum().get() as i32) - 1,
        battery,
        selected_profile: loaded.metadata.current().get(),
        active_dpi,
        active_stage: loaded.profile.dpi.active_stage.get(),
        stage_count: loaded.profile.dpi.stages.len(),
        polling_rate_hz: loaded.polling_rate.hz(),
        sensor: format!(
            "lift-off: {}; ripple: {}; angle snap: {}; motion sync: {}",
            if sensor.lift_off_distance == LiftOffDistance::OneMillimeter {
                "1 mm"
            } else {
                "2 mm"
            },
            on_off(sensor.ripple_control),
            on_off(sensor.angle_snap),
            on_off(sensor.motion_sync)
        ),
        profile_summary,
        metadata_summary,
        verification_summary: loaded.verification_summary.clone(),
        stages,
        bindings,
        profiles,
        status: status.to_owned(),
    }
}

/// Decodes one preferences image into typed fields and raw hex bytes, using
/// only the manager's typed helpers so no wire formula lives in the GUI.
fn preference_fields(preferences: &attack_shark_x3_manager::PreferencesState) -> PreferenceFields {
    let debounce_ms = DebounceMs::from_raw(preferences.debounce).map(|value| value.get());
    let sleep_half_minutes = SleepTimer::from_raw(preferences.sleep_timer).map(|value| value.get());
    let deep_sleep_minutes =
        DeepSleepMinutes::from_raw(preferences.configuration >> 4, preferences.deep_sleep)
            .map(|value| value.get());
    PreferenceFields {
        debounce_ms,
        sleep_half_minutes,
        deep_sleep_minutes,
        raw_configuration: format_byte(preferences.configuration),
        raw_deep_sleep: format_byte(preferences.deep_sleep),
        raw_sleep_timer: format_byte(preferences.sleep_timer),
        raw_debounce: format_byte(preferences.debounce),
    }
}

fn workflow_summary(workflow: &str, profile: ProfileId, verification: &Verification) -> String {
    let persistence = match verification.persistence {
        PersistenceVerification::PowerCycleVerified { .. } => "power-cycle persistence verified",
        PersistenceVerification::ProfileReloadVerified { .. } => {
            "profile-reload persistence verified"
        }
        PersistenceVerification::Unknown => "persistence unverified",
    };
    format!("{workflow} verification: profile {profile} readback verified; {persistence}")
}

fn verification_summary(verification: &Verification) -> String {
    match verification.application {
        ApplicationVerification::ReadbackVerified => "readback verified".to_owned(),
        ApplicationVerification::Acknowledged => "transport submitted".to_owned(),
        ApplicationVerification::Mismatch => "verification mismatch".to_owned(),
        ApplicationVerification::NotSent => "not sent".to_owned(),
    }
}

fn format_refresh_summary(outcome: &FullProfileRefreshOutcome) -> String {
    let mut text = format!(
        "all five profile observations refreshed; restored current {} / maximum {}",
        outcome.restored_metadata.current(),
        outcome.restored_metadata.maximum()
    );
    if outcome.temporarily_expanded {
        text.push_str("; profile slots were temporarily enabled through 5");
    }
    if outcome.profile_metadata_drift || !outcome.drift.is_empty() {
        text.push_str("; desired/observed drift found");
        if outcome.profile_metadata_drift {
            text.push_str(" (profile metadata)");
        }
        for (profile, resources) in &outcome.drift {
            let names = resources
                .iter()
                .map(profile_resource_name)
                .collect::<Vec<_>>()
                .join(", ");
            text.push_str(&format!("; profile {profile}: {names}"));
        }
    } else {
        text.push_str("; desired values match fresh observations");
    }
    text.push_str("; persistence remains unverified");
    text
}

fn profile_resource_name(resource: &ProfileResourceKind) -> &'static str {
    match resource {
        ProfileResourceKind::Dpi => "DPI",
        ProfileResourceKind::Preferences => "preferences",
        ProfileResourceKind::Buttons => "buttons",
        ProfileResourceKind::PollingRate => "polling rate",
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

/// Pure tail rule for enabling a profile: the maximum is raised by one.
/// Returns `None` when every slot is already enabled.
fn add_tail_metadata(metadata: ProfileMetadata) -> Option<ProfileMetadata> {
    let maximum = metadata.maximum().get();
    if maximum >= ProfileId::MAX {
        return None;
    }
    let added = ProfileId::new(maximum + 1)?;
    ProfileMetadata::new(metadata.current(), added).ok()
}

/// Pure tail rule for hiding a profile: only the last enabled profile can be
/// hidden, and hiding the currently active tail also activates the previous
/// profile so the device never names a hidden slot as current.
fn hide_tail_metadata(metadata: ProfileMetadata, number: u8) -> Option<ProfileMetadata> {
    let maximum = metadata.maximum().get();
    let number = ProfileId::new(number)?;
    if number.get() != maximum || maximum <= 1 {
        return None;
    }
    let previous = ProfileId::new(maximum - 1)?;
    let current = if metadata.current() == number {
        previous
    } else {
        metadata.current()
    };
    ProfileMetadata::new(current, previous).ok()
}

fn lift_off_choice_value(choice: u8) -> LiftOffDistance {
    match choice {
        0 => LiftOffDistance::OneMillimeter,
        _ => LiftOffDistance::TwoMillimeters,
    }
}

fn format_byte(value: u8) -> String {
    format!("{value:02x}")
}

/// Parses a one- or two-digit hex byte, tolerating an optional `0x` prefix.
fn parse_raw_byte(text: &str) -> Option<u8> {
    let trimmed = text.trim();
    let body = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    if body.is_empty() || body.len() > 2 {
        return None;
    }
    u8::from_str_radix(body, 16).ok()
}

/// The four raw preference bytes captured from the UI at Save time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RawPreferenceBytes {
    configuration: u8,
    deep_sleep: u8,
    sleep_timer: u8,
    debounce: u8,
}

/// Parses all four raw preference text fields at Save time so invalid text
/// can never serialize as a zero byte. Returns the failing field index (one
/// of the `RAW_PREFERENCE_*` constants) when any displayed text is invalid;
/// the model keeps the last valid byte until that field is corrected.
fn parse_raw_preference_draft(
    configuration: &str,
    deep_sleep: &str,
    sleep_timer: &str,
    debounce: &str,
) -> Result<RawPreferenceBytes, i32> {
    let configuration = parse_raw_byte(configuration).ok_or(RAW_PREFERENCE_CONFIGURATION)?;
    let deep_sleep = parse_raw_byte(deep_sleep).ok_or(RAW_PREFERENCE_DEEP_SLEEP)?;
    let sleep_timer = parse_raw_byte(sleep_timer).ok_or(RAW_PREFERENCE_SLEEP_TIMER)?;
    let debounce = parse_raw_byte(debounce).ok_or(RAW_PREFERENCE_DEBOUNCE)?;
    Ok(RawPreferenceBytes {
        configuration,
        deep_sleep,
        sleep_timer,
        debounce,
    })
}

fn raw_preference_field_label(field: i32) -> &'static str {
    match field {
        RAW_PREFERENCE_CONFIGURATION => "configuration",
        RAW_PREFERENCE_DEEP_SLEEP => "deep sleep",
        RAW_PREFERENCE_SLEEP_TIMER => "sleep timer",
        RAW_PREFERENCE_DEBOUNCE => "debounce",
        _ => "unknown",
    }
}

/// Status message for an auxiliary action that would silently discard an
/// unsaved draft, or `None` when the draft is clean. Callers refuse the
/// action when this returns a message, so auxiliary operations cannot clear
/// pending edits.
fn dirty_draft_message(dirty: bool, action: &str) -> Option<String> {
    if dirty {
        Some(format!("save or discard the current draft before {action}"))
    } else {
        None
    }
}

/// The profile an external device notification reports as current, when it
/// differs from the loaded metadata so the UI reloads through the safe
/// profile path. Covers primary, sync, and secondary profile-change events;
/// unrelated events and no-op reports return `None`.
fn reported_profile_switch(event: DeviceEvent, loaded_current: ProfileId) -> Option<ProfileId> {
    match event {
        DeviceEvent::ProfileChanged(e)
        | DeviceEvent::ProfileSync(e)
        | DeviceEvent::SecondaryProfileChanged(e)
            if e.profile != loaded_current =>
        {
            Some(e.profile)
        }
        _ => None,
    }
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

fn validated_binding_action(current: &str, requested: &str) -> Option<String> {
    if !BINDING_ACTIONS.contains(&current) {
        return None;
    }
    let action = safe_button_action(requested)?;
    Some(button_action_name(action.to_assignment()))
}

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
        DPI_VALUES_APPEND[index],
        DPI_LABELS[index],
        red,
        green,
        blue,
        active,
    )
}

/// Appends one draft stage (never active), returning false when the ladder is
/// already at the device maximum of eight stages.
fn append_dpi_stage(stages: &VecModel<DpiStage>) -> bool {
    if stages.row_count() >= MAX_DPI_STAGES {
        return false;
    }
    stages.push(default_dpi_stage(stages.row_count(), false));
    true
}

fn set_active_dpi_stage(stages: &VecModel<DpiStage>, selected: usize) {
    for row in 0..stages.row_count() {
        if let Some(mut stage) = stages.row_data(row) {
            stage.active = row == selected;
            stages.set_row_data(row, stage);
        }
    }
}

/// Removes an arbitrary row, compacts the remaining stages, and returns the
/// adjusted zero-based active stage index so it always refers to a real row.
/// Returns `None` when the last stage cannot be removed.
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

fn renumber_dpi_stages(stages: &VecModel<DpiStage>) {
    for row in 0..stages.row_count() {
        if let Some(mut stage) = stages.row_data(row) {
            stage.index = format!("{:02}", row + 1).into();
            stages.set_row_data(row, stage);
        }
    }
}
fn configured_product_label(choice: i32, custom: &str) -> String {
    match choice.clamp(0, 2) {
        0 => "Attack Shark X3".to_owned(),
        1 => "Kysona M600".to_owned(),
        _ => {
            let custom = custom.trim();
            if custom.is_empty() {
                "Attack Shark X3".to_owned()
            } else {
                custom.to_owned()
            }
        }
    }
}

/// Rounds a user-entered debounce value to the nearest supported even
/// millisecond value in the typed helper's 4..=50 ms range.
fn canonical_debounce_ms(value: f64) -> Option<DebounceMs> {
    if !value.is_finite() {
        return None;
    }
    let rounded = ((value / 2.0).round() * 2.0).clamp(4.0, 50.0) as u8;
    DebounceMs::new(rounded)
}

/// Rounds a user-entered sleep duration to the nearest supported half-minute
/// step and returns the manager-owned typed value.
fn canonical_sleep_minutes(value: f64) -> Option<SleepTimer> {
    if !value.is_finite() {
        return None;
    }
    let half_minutes = (value * 2.0).round().clamp(1.0, 60.0) as u8;
    SleepTimer::new(half_minutes)
}

/// Rounds a user-entered deep-sleep duration to the nearest whole minute.
fn canonical_deep_sleep_minutes(value: f64) -> Option<DeepSleepMinutes> {
    if !value.is_finite() {
        return None;
    }
    let minutes = value.clamp(1.0, 60.0).round() as u8;
    DeepSleepMinutes::new(minutes)
}

fn round_dpi_step(value: f32) -> u16 {
    if !value.is_finite() {
        return DPI_MIN as u16;
    }
    let step = (value / DPI_STEP)
        .round()
        .clamp(DPI_MIN / DPI_STEP, DPI_MAX / DPI_STEP);
    (step * DPI_STEP) as u16
}

fn parse_dpi_setting(text: &str) -> Option<f32> {
    let value = text.trim().parse::<f32>().ok()?;
    if !value.is_finite() || !(DPI_MIN..=DPI_MAX).contains(&value) {
        return None;
    }
    Some(round_dpi_step(value) as f32)
}
fn parse_dpi_range(minimum: &str, maximum: &str) -> Option<(f32, f32)> {
    let minimum = parse_dpi_setting(minimum)?;
    let maximum = parse_dpi_setting(maximum)?;
    (minimum < maximum).then_some((minimum, maximum))
}

fn format_dpi_setting(value: f32) -> String {
    (round_dpi_step(value) as u32).to_string()
}
fn dpi_bounds(min: f32, max: f32) -> (f32, f32) {
    let min = (round_dpi_step(min) as f32).clamp(DPI_MIN, DPI_MAX - DPI_STEP);
    let max = (round_dpi_step(max) as f32).clamp(min + DPI_STEP, DPI_MAX);
    (min, max)
}
fn clamp_dpi_value(dpi: f32, min: f32, max: f32) -> f32 {
    let (min, max) = dpi_bounds(min, max);
    (round_dpi_step(dpi) as f32).clamp(min, max)
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
    let dpi = clamp_dpi_value(dpi, min, max);
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
        original_action: action.into(),
        editable: BINDING_ACTIONS.contains(&action),
    }
}

fn page_status(page: i32) -> &'static str {
    match page {
        0 => "mouse overview",
        1 => "button assignments",
        2 => "DPI and sensor settings",
        3 => "polling rate and battery-saving timers",
        4 => "device details and maintenance",
        5 => "app preferences",
        _ => "ready",
    }
}

/// Fresh values used when appending a new draft stage.
const DPI_VALUES_APPEND: [&str; MAX_DPI_STAGES] = [
    "800", "1200", "1600", "2000", "2400", "3200", "12000", "26000",
];

#[cfg(test)]
mod tests {
    use super::*;
    use attack_shark_x3_manager::{BatteryEvent, PreferencesState, ProfileChangedEvent};

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
    fn removing_an_arbitrary_row_compacts_and_adjusts_the_active_stage() {
        // Active before the removed row: active stays put.
        let stages = VecModel::from(
            (0..5)
                .map(|index| default_dpi_stage(index, false))
                .collect::<Vec<_>>(),
        );
        set_active_dpi_stage(&stages, 1);
        let new_active = remove_dpi_stage(&stages, 3, 1).expect("removable");
        set_active_dpi_stage(&stages, new_active);
        assert_eq!(new_active, 1);
        assert_eq!(stages.row_count(), 4);
        assert_eq!(stages.row_data(3).expect("last stage").index.as_str(), "04");

        // Active after the removed row: active shifts down by one.
        let stages = VecModel::from(
            (0..5)
                .map(|index| default_dpi_stage(index, false))
                .collect::<Vec<_>>(),
        );
        set_active_dpi_stage(&stages, 3);
        let new_active = remove_dpi_stage(&stages, 1, 3).expect("removable");
        set_active_dpi_stage(&stages, new_active);
        assert_eq!(new_active, 2);
        assert!(stages.row_data(2).expect("active stage").active);

        // Active row removed: active lands on the compacted last row.
        let stages = VecModel::from(
            (0..5)
                .map(|index| default_dpi_stage(index, false))
                .collect::<Vec<_>>(),
        );
        set_active_dpi_stage(&stages, 2);
        let new_active = remove_dpi_stage(&stages, 2, 2).expect("removable");
        set_active_dpi_stage(&stages, new_active);
        assert_eq!(new_active, 2);
        assert_eq!(stages.row_count(), 4);
        assert!(stages.row_data(2).expect("active stage").active);
    }

    #[test]
    fn removing_the_last_stage_is_rejected() {
        let stages = VecModel::from(vec![default_dpi_stage(0, true)]);
        assert!(remove_dpi_stage(&stages, 0, 0).is_none());
        assert_eq!(stages.row_count(), 1);
    }

    #[test]
    fn appending_stages_stops_at_the_device_maximum() {
        let stages = VecModel::from(Vec::<DpiStage>::new());
        for expected in 1..=MAX_DPI_STAGES {
            assert!(append_dpi_stage(&stages));
            assert_eq!(stages.row_count(), expected);
        }
        assert!(!append_dpi_stage(&stages));
        assert_eq!(stages.row_count(), MAX_DPI_STAGES);
    }

    #[test]
    fn hiding_the_active_tail_activates_the_previous_profile_first() {
        let metadata = ProfileMetadata::new(
            ProfileId::new(5).expect("profile 5"),
            ProfileId::new(5).expect("profile 5"),
        )
        .expect("valid metadata");
        let target = hide_tail_metadata(metadata, 5).expect("tail hide is allowed");
        assert_eq!(target.current().get(), 4);
        assert_eq!(target.maximum().get(), 4);
    }

    #[test]
    fn hiding_a_non_current_tail_keeps_the_active_profile() {
        let metadata = ProfileMetadata::new(
            ProfileId::new(2).expect("profile 2"),
            ProfileId::new(5).expect("profile 5"),
        )
        .expect("valid metadata");
        let target = hide_tail_metadata(metadata, 5).expect("tail hide is allowed");
        assert_eq!(target.current().get(), 2);
        assert_eq!(target.maximum().get(), 4);
    }

    #[test]
    fn middle_profiles_and_the_sole_profile_cannot_be_hidden() {
        let metadata = ProfileMetadata::new(
            ProfileId::new(1).expect("profile 1"),
            ProfileId::new(5).expect("profile 5"),
        )
        .expect("valid metadata");
        assert!(hide_tail_metadata(metadata, 3).is_none());
        assert!(hide_tail_metadata(metadata, 4).is_none());

        let sole = ProfileMetadata::new(
            ProfileId::new(1).expect("profile 1"),
            ProfileId::new(1).expect("profile 1"),
        )
        .expect("valid metadata");
        assert!(hide_tail_metadata(sole, 1).is_none());
    }

    #[test]
    fn adding_a_profile_raises_the_maximum_and_stops_at_five() {
        let metadata = ProfileMetadata::new(
            ProfileId::new(2).expect("profile 2"),
            ProfileId::new(3).expect("profile 3"),
        )
        .expect("valid metadata");
        let target = add_tail_metadata(metadata).expect("add is allowed");
        assert_eq!(target.current().get(), 2);
        assert_eq!(target.maximum().get(), 4);

        let full = ProfileMetadata::new(
            ProfileId::new(1).expect("profile 1"),
            ProfileId::new(ProfileId::MAX).expect("profile maximum"),
        )
        .expect("valid metadata");
        assert!(add_tail_metadata(full).is_none());
    }

    #[test]
    fn raw_byte_parsing_accepts_hex_and_rejects_garbage() {
        assert_eq!(parse_raw_byte("a8"), Some(0xa8));
        assert_eq!(parse_raw_byte("A8"), Some(0xa8));
        assert_eq!(parse_raw_byte("0xa8"), Some(0xa8));
        assert_eq!(parse_raw_byte("0X10"), Some(0x10));
        assert_eq!(parse_raw_byte("ff"), Some(0xff));
        assert_eq!(parse_raw_byte(" 0a "), Some(0x0a));
        assert_eq!(parse_raw_byte("100"), None);
        assert_eq!(parse_raw_byte("zz"), None);
        assert_eq!(parse_raw_byte(""), None);
        assert_eq!(parse_raw_byte("0x"), None);
    }

    #[test]
    fn raw_draft_parsing_rejects_each_invalid_field_with_its_index() {
        let ok = parse_raw_preference_draft("03", "a8", "01", "04").expect("valid draft parses");
        assert_eq!(ok.configuration, 0x03);
        assert_eq!(ok.deep_sleep, 0xa8);
        assert_eq!(ok.sleep_timer, 0x01);
        assert_eq!(ok.debounce, 0x04);

        assert_eq!(
            parse_raw_preference_draft("zz", "a8", "01", "04"),
            Err(RAW_PREFERENCE_CONFIGURATION)
        );
        assert_eq!(
            parse_raw_preference_draft("03", "zz", "01", "04"),
            Err(RAW_PREFERENCE_DEEP_SLEEP)
        );
        assert_eq!(
            parse_raw_preference_draft("03", "a8", "zz", "04"),
            Err(RAW_PREFERENCE_SLEEP_TIMER)
        );
        assert_eq!(
            parse_raw_preference_draft("03", "a8", "01", "zz"),
            Err(RAW_PREFERENCE_DEBOUNCE)
        );
        assert_eq!(
            parse_raw_preference_draft("100", "a8", "01", "04"),
            Err(RAW_PREFERENCE_CONFIGURATION)
        );
    }

    #[test]
    fn dirty_draft_guard_refuses_auxiliary_actions() {
        assert_eq!(
            dirty_draft_message(true, "renaming a profile").as_deref(),
            Some("save or discard the current draft before renaming a profile")
        );
        assert_eq!(
            dirty_draft_message(true, "importing a configuration").as_deref(),
            Some("save or discard the current draft before importing a configuration")
        );
        assert_eq!(
            dirty_draft_message(true, "verifying power-cycle persistence").as_deref(),
            Some("save or discard the current draft before verifying power-cycle persistence")
        );
        assert!(dirty_draft_message(false, "renaming a profile").is_none());
    }

    #[test]
    fn secondary_profile_change_event_reports_the_external_profile() {
        let profile_three = ProfileId::new(3).expect("profile 3 is valid");
        let loaded = ProfileId::new(1).expect("profile 1 is valid");
        let event = ProfileChangedEvent {
            raw_report: [0x03, 0, 0, 0, 0],
            profile: profile_three,
        };
        assert_eq!(
            reported_profile_switch(DeviceEvent::SecondaryProfileChanged(event), loaded),
            Some(profile_three)
        );
        assert_eq!(
            reported_profile_switch(DeviceEvent::ProfileSync(event), loaded),
            Some(profile_three)
        );
        // A report matching the loaded profile is a no-op.
        assert_eq!(
            reported_profile_switch(DeviceEvent::SecondaryProfileChanged(event), profile_three),
            None
        );
        // Unrelated events are not treated as profile switches.
        assert_eq!(
            reported_profile_switch(
                DeviceEvent::BatteryChanged(BatteryEvent {
                    raw_report: [0x03, 0, 0, 0, 0],
                    level: 8,
                }),
                loaded,
            ),
            None
        );
    }

    #[test]
    fn typed_preference_helpers_round_trip_without_gui_formulas() {
        assert_eq!(DebounceMs::new(8).expect("8 ms").raw(), 0x04);
        assert_eq!(DebounceMs::from_raw(0x04).expect("raw 4").get(), 8);
        assert_eq!(DebounceMs::new(6).expect("6 ms").raw(), 0x03);
        assert!(DebounceMs::new(5).is_none());

        assert_eq!(SleepTimer::new(1).expect("half minute").raw(), 0x01);
        assert_eq!(SleepTimer::from_raw(60).expect("raw 60").minutes(), 30.0);
        assert_eq!(SleepTimer::from_raw(3).expect("raw 3").minutes(), 1.5);
        assert!(SleepTimer::new(61).is_none());

        assert_eq!(
            DeepSleepMinutes::new(10).expect("10 min").deep_sleep_byte(),
            0xa8
        );
        assert_eq!(
            DeepSleepMinutes::new(10)
                .expect("10 min")
                .configuration_with(0x03),
            0x03
        );
        assert_eq!(
            DeepSleepMinutes::new(25)
                .expect("25 min")
                .configuration_with(0x03),
            0x13
        );
        assert_eq!(
            DeepSleepMinutes::from_raw(1, 0x98)
                .expect("25 min decode")
                .get(),
            25
        );
        for (minutes, bucket) in [(16, 1), (32, 2), (48, 3)] {
            let deep = DeepSleepMinutes::new(minutes).expect("boundary minute");
            assert_eq!(deep.bucket(), bucket);
            assert_eq!(deep.deep_sleep_byte(), 0x08);
            assert_eq!(DeepSleepMinutes::from_raw(bucket, 0x08), Some(deep));
        }
        assert!(DeepSleepMinutes::new(0).is_none());
        assert!(DeepSleepMinutes::new(61).is_none());
    }

    #[test]
    fn stock_reset_preferences_decode_through_the_typed_helpers() {
        let preferences = PreferencesState::captured_stock_reset(ProfileId::new(1).expect("1"));
        let fields = preference_fields(&preferences);
        assert_eq!(fields.debounce_ms, Some(8));
        assert_eq!(fields.sleep_half_minutes, Some(1));
        assert_eq!(fields.deep_sleep_minutes, Some(10));
        assert_eq!(fields.raw_configuration, "03");
        assert_eq!(fields.raw_deep_sleep, "a8");
        assert_eq!(fields.raw_sleep_timer, "01");
        assert_eq!(fields.raw_debounce, "04");
    }

    #[test]
    fn captured_sixteen_minute_deep_sleep_pair_decodes_exactly() {
        let preferences = PreferencesState::new(
            ProfileId::new(1).expect("1"),
            0x00,
            0x10,
            0x08,
            [0x00, 0x00, 0xff],
            0x05,
            0x04,
        );
        let fields = preference_fields(&preferences);
        assert_eq!(fields.deep_sleep_minutes, Some(16));
        assert_eq!(fields.raw_configuration, "10");
        assert_eq!(fields.raw_deep_sleep, "08");
    }

    #[test]
    fn noncanonical_preference_bytes_report_unknown_typed_values() {
        let preferences = PreferencesState::new(
            ProfileId::new(1).expect("1"),
            0x00,
            0x03,
            0xa8,
            [0x00, 0x00, 0xff],
            0x00,
            0x00,
        );
        let fields = preference_fields(&preferences);
        assert_eq!(fields.debounce_ms, None);
        assert_eq!(fields.sleep_half_minutes, None);
        assert_eq!(fields.raw_sleep_timer, "00");
        assert_eq!(fields.raw_debounce, "00");
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
    fn numeric_text_values_round_to_the_nearest_encodable_typed_value() {
        assert_eq!(canonical_debounce_ms(4.0).expect("4 ms").get(), 4);
        assert_eq!(canonical_debounce_ms(5.0).expect("5 ms").get(), 6);
        assert_eq!(canonical_debounce_ms(49.0).expect("49 ms").get(), 50);
        assert_eq!(canonical_debounce_ms(100.0).expect("clamped ms").get(), 50);
        assert_eq!(
            canonical_sleep_minutes(0.1).expect("clamped sleep").get(),
            1
        );
        assert_eq!(canonical_sleep_minutes(2.24).expect("sleep").get(), 4);
        assert_eq!(canonical_sleep_minutes(2.26).expect("sleep").get(), 5);
        assert_eq!(
            canonical_sleep_minutes(40.0).expect("clamped sleep").get(),
            60
        );
        assert_eq!(
            canonical_deep_sleep_minutes(16.0)
                .expect("16 minute boundary")
                .get(),
            16
        );
        assert_eq!(
            canonical_deep_sleep_minutes(16.4)
                .expect("round below half")
                .get(),
            16
        );
        assert_eq!(
            canonical_deep_sleep_minutes(16.6)
                .expect("round above half")
                .get(),
            17
        );
        assert_eq!(
            canonical_deep_sleep_minutes(32.0)
                .expect("32 minute boundary")
                .get(),
            32
        );
        assert_eq!(
            canonical_deep_sleep_minutes(48.0)
                .expect("48 minute boundary")
                .get(),
            48
        );
        assert!(canonical_deep_sleep_minutes(f64::NAN).is_none());
    }

    #[test]
    fn dpi_step_canonicalization_keeps_bounds_and_visual_ranges_valid() {
        assert_eq!(round_dpi_step(224.0), 200);
        assert_eq!(round_dpi_step(226.0), 250);
        assert_eq!(round_dpi_step(49.0), 50);
        assert_eq!(round_dpi_step(26_001.0), 26_000);
        assert_eq!(dpi_bounds(225.0, 225.0), (250.0, 300.0));
        assert_eq!(dpi_bounds(26_000.0, 50.0), (25_950.0, 26_000.0));
    }
    #[test]
    fn dpi_display_range_applies_both_rounded_bounds_atomically() {
        assert_eq!(
            parse_dpi_range("12001", "19999"),
            Some((12_000.0, 20_000.0))
        );
        assert_eq!(parse_dpi_range("12000", "10000"), None);
        assert_eq!(parse_dpi_range("invalid", "20000"), None);
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

    #[test]
    fn dropdown_selection_accepts_exact_safe_labels_and_rejects_invalid_rows() {
        assert_eq!(
            validated_binding_action("left click", "right click").as_deref(),
            Some("right click")
        );
        assert!(validated_binding_action("unsupported (not editable)", "right click").is_none());
        assert!(validated_binding_action("left click", "not a safe action").is_none());
        assert!(binding("lmb", "primary", "left click").editable);
        assert!(!binding("unknown", "device", "unsupported (not editable)").editable);
    }

    #[test]
    fn configured_product_label_never_uses_live_device_identity() {
        assert_eq!(
            configured_product_label(0, "USB Gaming Mouse"),
            "Attack Shark X3"
        );
        assert_eq!(
            configured_product_label(1, "USB Gaming Mouse"),
            "Kysona M600"
        );
        assert_eq!(
            configured_product_label(2, " USB Gaming Mouse "),
            "USB Gaming Mouse"
        );
        assert_eq!(configured_product_label(2, " "), "Attack Shark X3");
    }

    #[test]
    fn event_snapshot_preserves_the_draft_only_when_dirty_and_event_driven() {
        // An event snapshot while a draft is open must not clear it.
        assert!(snapshot_preserves_draft(true, true));
        // A clean UI takes event snapshots normally.
        assert!(!snapshot_preserves_draft(true, false));
        // Explicit refresh/apply/select operations may always replace models
        // and clear the draft, even with edits pending.
        assert!(!snapshot_preserves_draft(false, true));
        assert!(!snapshot_preserves_draft(false, false));
    }

    #[test]
    fn complete_profile_observations_require_every_resource_in_all_five_slots() {
        let identity = DeviceIdentity::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa61,
            Some("GUI-OBSERVATION-TEST"),
            r"\\?\hid#gui-observation-test",
            Some("GUI observation test"),
        )
        .expect("valid test identity");
        let mut state = StateFile::default();
        let mut device = attack_shark_x3_manager::DeviceState::new(identity.clone());
        for number in ProfileId::MIN..=ProfileId::MAX {
            let profile = ProfileId::new(number).expect("profile is valid");
            let profile_state = device.profiles.entry(profile).or_default();
            profile_state.dpi.observed = Some(attack_shark_x3_manager::ObservedState {
                value: attack_shark_x3_manager::DpiState::captured_stock_reset(profile)
                    .expect("stock DPI"),
                source: attack_shark_x3_manager::ObservationSource::UsbReadback,
                observed_at: attack_shark_x3_manager::Timestamp { unix_seconds: 1 },
            });
            profile_state.preferences.observed = Some(attack_shark_x3_manager::ObservedState {
                value: attack_shark_x3_manager::PreferencesState::captured_stock_reset(profile),
                source: attack_shark_x3_manager::ObservationSource::UsbReadback,
                observed_at: attack_shark_x3_manager::Timestamp { unix_seconds: 1 },
            });
            profile_state.buttons.observed = Some(attack_shark_x3_manager::ObservedState {
                value: attack_shark_x3_manager::ButtonsState::default_for_profile(profile),
                source: attack_shark_x3_manager::ObservationSource::UsbReadback,
                observed_at: attack_shark_x3_manager::Timestamp { unix_seconds: 1 },
            });
            profile_state.polling_rate.observed = Some(attack_shark_x3_manager::ObservedState {
                value: PollingRate::Hz1000,
                source: attack_shark_x3_manager::ObservationSource::UsbReadback,
                observed_at: attack_shark_x3_manager::Timestamp { unix_seconds: 1 },
            });
        }
        state.devices.insert(identity.id.clone(), device);
        assert!(has_complete_profile_observations(&state, &identity.id));

        state
            .devices
            .get_mut(&identity.id)
            .unwrap()
            .profiles
            .get_mut(&ProfileId::new(3).unwrap())
            .unwrap()
            .buttons
            .observed = None;
        assert!(!has_complete_profile_observations(&state, &identity.id));
    }

    #[test]
    fn refresh_summary_surfaces_restoration_and_drift() {
        let profile = ProfileId::new(2).expect("profile is valid");
        let metadata = ProfileMetadata::new(profile, ProfileId::new(3).unwrap()).unwrap();
        let outcome = FullProfileRefreshOutcome {
            original_metadata: metadata,
            restored_metadata: metadata,
            temporarily_expanded: true,
            profiles: BTreeMap::new(),
            drift: BTreeMap::from([(
                profile,
                vec![
                    ProfileResourceKind::PollingRate,
                    ProfileResourceKind::Buttons,
                ],
            )]),
            profile_metadata_drift: false,
        };
        let summary = format_refresh_summary(&outcome);
        assert!(summary.contains("restored current 2 / maximum 3"));
        assert!(summary.contains("temporarily enabled through 5"));
        assert!(summary.contains("profile 2: polling rate, buttons"));
        assert!(summary.contains("persistence remains unverified"));
    }

    #[test]
    fn event_snapshot_draft_warning_preserves_the_draft_and_warns_about_save() {
        assert_eq!(
            event_snapshot_draft_warning("device battery level changed").as_str(),
            "device battery level changed — external state changed while a draft was open; the draft was preserved, but Save may be rejected until you discard / reload"
        );
    }

    #[test]
    fn loaded_bindings_baseline_original_action_to_the_loaded_action() {
        let row = binding("lmb", "primary", "left click");
        assert_eq!(row.action.as_str(), "left click");
        assert_eq!(row.original_action.as_str(), "left click");
        assert_eq!(row.action, row.original_action);
    }

    #[test]
    fn button_change_summary_lists_changed_buttons_as_exact_lines() {
        let mut rows = vec![
            binding("lmb", "primary", "left click"),
            binding("rmb", "secondary", "right click"),
            binding("forward", "side upper", "forward"),
        ];
        // One changed binding yields exactly one `button → action` line.
        rows[2].action = "refresh rate".into();
        assert_eq!(
            button_change_summary(&rows).as_str(),
            "forward → refresh rate"
        );

        // Multiple changes yield one line per changed button, in row order.
        rows[0].action = "dpi up".into();
        assert_eq!(
            button_change_summary(&rows).as_str(),
            "lmb → dpi up\nforward → refresh rate"
        );
    }

    #[test]
    fn button_change_summary_reports_no_physical_changes_for_a_clean_model() {
        let rows = vec![
            binding("lmb", "primary", "left click"),
            binding("rmb", "secondary", "right click"),
        ];
        assert_eq!(
            button_change_summary(&rows).as_str(),
            "No physical button changes."
        );
        assert_eq!(
            button_change_summary(&[]).as_str(),
            "No physical button changes."
        );
    }

    #[test]
    fn button_change_summary_uses_button_labels_and_skips_unmodified_rows() {
        let mut rows = vec![
            binding("dpi", "top button", "dpi cycle"),
            binding("wheel", "middle", "wheel click"),
        ];
        // Cycling back to the original action is not a physical change.
        rows[0].action = "stage 02".into();
        assert_eq!(button_change_summary(&rows).as_str(), "dpi → stage 02");
        rows[0].action = "dpi cycle".into();
        assert_eq!(
            button_change_summary(&rows).as_str(),
            "No physical button changes."
        );
        // The location never appears in the summary lines.
        rows[1].action = "scroll".into();
        assert_eq!(button_change_summary(&rows).as_str(), "wheel → scroll");
    }
}
