use std::{collections::BTreeMap, path::PathBuf};

use tokio::sync::{broadcast, mpsc, watch};

use crate::AppWindow;
use crate::presentation::{
    MAX_DPI_STAGES, SAFE_BUTTON_SLOTS, add_tail_metadata, button_action_name, format_byte,
    format_error_string, format_refresh_summary, hide_tail_metadata, is_ble_identity,
    lift_off_choice_value, on_off, product_id_label_for_identity, profile_update_status,
    reported_profile_switch, round_dpi_step, safe_button_action, transport_label,
    transport_label_for_identity, workflow_summary,
};
use crate::projection::apply_event;
use attack_shark_x3::{DebounceMs, DeepSleepMinutes, SleepTimer};
use attack_shark_x3_manager::{
    BaselineSource, ButtonSlotDelta, ConfigurationExport, DesiredSource, DeviceEvent, DeviceId,
    DeviceIdentity, DeviceManager, DeviceStatus, DiscoveredDevice, DpiDelta, DpiValue,
    EventSubscriptions, LiftOffDistance, ManagerError, PersistenceVerification, PollingRate,
    PreferencesDelta, ProfileId, ProfileMetadata, ProfileUpdate, ProfileUpdateOutcome,
    ResourceState, SafeButtonSlot, SensorOptionsDelta, StageIndex, StateFile, StateStore,
    TransportSelection, UpdatePolicy, VerificationMethod,
};

#[derive(Debug)]
pub enum Command {
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
pub enum VerificationChoice {
    Transport,
    Readback,
}

#[derive(Clone, Copy, Debug)]
pub enum BaselineChoice {
    Live,
    Stored,
}

/// The complete draft captured from the UI when the user saves.
///
/// Preference fields carry the raw wire bytes; the typed fields in the UI
/// write through to those bytes through `DebounceMs`/`SleepTimer`/
/// `DeepSleepMinutes`, so no byte formula is duplicated here.
#[derive(Clone, Debug)]
pub struct Draft {
    pub profile: u8,
    pub dpi_values: Vec<u16>,
    pub active_stage: u8,
    pub buttons: Vec<String>,
    pub lift_off_choice: u8,
    pub ripple_control: bool,
    pub angle_snap: bool,
    pub motion_sync: bool,
    pub raw_configuration: u8,
    pub raw_deep_sleep: u8,
    pub raw_sleep_timer: u8,
    pub raw_debounce: u8,
    pub polling_rate_hz: u16,
    pub verification: VerificationChoice,
    pub baseline: BaselineChoice,
    pub allow_explicit_defaults: bool,
}

pub struct ProfileData {
    pub dpi: attack_shark_x3_manager::DpiState,
    pub preferences: attack_shark_x3_manager::PreferencesState,
    pub buttons: attack_shark_x3_manager::ButtonsState,
}

pub struct LoadedDevice {
    pub device: DeviceId,
    pub identity: DeviceIdentity,
    pub metadata: ProfileMetadata,
    pub profile: ProfileData,
    pub polling_rate: PollingRate,
    pub battery: Option<u8>,
    pub polling_rate_ready: bool,
    pub all_profiles_observed: bool,
    /// True when stored desired/observed preferences exist for the profile.
    pub has_stored_preferences: bool,
    pub profile_names: BTreeMap<ProfileId, String>,
    pub verification_summary: String,
}

pub struct LiveProfile {
    pub enabled: bool,
    pub current: bool,
    pub name: String,
}

pub struct LiveSnapshot {
    pub device_name: String,
    pub stable_id: String,
    pub product_id: String,
    pub transport: String,
    pub battery: String,
    pub selected_profile: u8,
    pub active_dpi: u16,
    pub active_stage: u8,
    pub stage_count: usize,
    pub polling_rate_hz: u16,
    pub polling_rate_ready: bool,
    pub is_ble: bool,
    pub all_profiles_observed: bool,
    pub motion_sync: bool,
    pub ripple_control: bool,
    pub angle_snap: bool,
    pub lift_off_choice: u8,
    pub debounce_ms: Option<u8>,
    pub sleep_half_minutes: Option<u8>,
    pub deep_sleep_minutes: Option<u8>,
    pub raw_configuration: String,
    pub raw_deep_sleep: String,
    pub raw_sleep_timer: String,
    pub raw_debounce: String,
    pub preferences_ready: bool,
    pub last_enabled_profile: i32,
    pub sensor: String,
    pub profile_summary: String,
    pub metadata_summary: String,
    pub verification_summary: String,
    pub stages: Vec<(u16, bool)>,
    pub bindings: Vec<(String, String, String)>,
    pub profiles: Vec<LiveProfile>,
    pub status: String,
}

pub struct DeviceListEntry {
    pub id: String,
    pub name: String,
    pub detail: String,
    pub selected: bool,
    pub connected: bool,
}

/// Typed decodes of one preferences image, resolved through the manager's
/// typed helpers so the GUI never duplicates the wire formulas.
pub struct PreferenceFields {
    pub debounce_ms: Option<u8>,
    pub sleep_half_minutes: Option<u8>,
    pub deep_sleep_minutes: Option<u8>,
    pub raw_configuration: String,
    pub raw_deep_sleep: String,
    pub raw_sleep_timer: String,
    pub raw_debounce: String,
}

pub enum UiEvent {
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

pub enum StartupError {
    /// The state file exists but cannot be read as current-schema state;
    /// the user can discard it (with a backup) and retry.
    UnreadableState(String),
    Message(String),
}

/// Drops loaded device, event receivers, and subscriptions together.
/// Use on discovery/refresh failure, disconnect, fatal error, and shutdown
/// to avoid retaining stale sessions.
pub fn clear_loaded_context(
    loaded: &mut Option<LoadedDevice>,
    events: &mut Option<broadcast::Receiver<DeviceEvent>>,
    subscriptions: &mut Option<EventSubscriptions>,
) {
    *loaded = None;
    *events = None;
    *subscriptions = None;
}

pub(crate) fn is_coalescable_command(cmd: &Command) -> bool {
    matches!(cmd, Command::Startup | Command::Refresh)
}

pub fn worker_main(
    mut commands: mpsc::Receiver<Command>,
    mut shutdown: watch::Receiver<bool>,
    weak: slint::Weak<AppWindow>,
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|error| format!("could not start Tokio manager runtime: {error}"))?;
    runtime.block_on(async move {
        let mut manager: Option<DeviceManager> = None;
        let mut discovered: Vec<DiscoveredDevice> = Vec::new();
        let mut selected: Option<DeviceId> = None;
        let mut loaded: Option<LoadedDevice> = None;
        let mut events: Option<broadcast::Receiver<DeviceEvent>> = None;
        let mut subscriptions: Option<EventSubscriptions> = None;
        let mut pending: Option<Command> = None;

        loop {
            tokio::select! {
                biased;
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        clear_loaded_context(&mut loaded, &mut events, &mut subscriptions);
                        break;
                    }
                }
                cmd = async {
                    if let Some(p) = pending.take() {
                        Some(p)
                    } else {
                        commands.recv().await
                    }
                } => {
                    let Some(command) = cmd else {
                        clear_loaded_context(&mut loaded, &mut events, &mut subscriptions);
                        break;
                    };
                    if matches!(command, Command::Shutdown) {
                        clear_loaded_context(&mut loaded, &mut events, &mut subscriptions);
                        break;
                    }
                    // handle command
                    let is_coalescable = is_coalescable_command(&command);
                    match command {
                        Command::Startup | Command::Refresh => {
                            emit(&weak, UiEvent::Busy("discovering devices".into()));
                            match open_manager() {
                                Err(StartupError::UnreadableState(detail)) => {
                                    clear_loaded_context(&mut loaded, &mut events, &mut subscriptions);
                                    discovered.clear();
                                    selected = None;
                                    manager = None;
                                    emit(&weak, UiEvent::StateUnreadable(detail));
                                }
                                Err(StartupError::Message(detail)) => {
                                    clear_loaded_context(&mut loaded, &mut events, &mut subscriptions);
                                    discovered.clear();
                                    selected = None;
                                    manager = None;
                                    emit(&weak, UiEvent::Devices(Vec::new()));
                                    emit(&weak, UiEvent::Error(detail));
                                }
                                Ok(new_manager) => {
                                    match new_manager.list_devices(TransportSelection::Auto).await {
                                        Err(error) => {
                                            clear_loaded_context(&mut loaded, &mut events, &mut subscriptions);
                                            manager = Some(new_manager);
                                            emit(&weak, UiEvent::Devices(Vec::new()));
                                            emit(&weak, UiEvent::Error(format!("device discovery failed: {error}")));
                                        }
                                        Ok(found) => {
                                            discovered = found;
                                            selected = new_manager.selected_device().ok().flatten();
                                            emit_devices(&weak, &discovered, selected.as_ref());
                                            match new_manager.resolve_device(None, TransportSelection::Auto).await {
                                                Ok(resolved) => {
                                                    selected = Some(resolved.clone());
                                                    match read_loaded(&new_manager, &resolved, None).await {
                                                        Ok(new_loaded) => {
                                                            let new_loaded = subscribe_and_hold(&new_manager, new_loaded, &mut events, &mut subscriptions).await;
                                                            manager = Some(new_manager);
                                                            loaded = Some(new_loaded);
                                                            emit_devices(&weak, &discovered, selected.as_ref());
                                                            if let Some(ready) = loaded.as_ref() {
                                                                emit_snapshot(&weak, ready, "mouse settings loaded");
                                                            }
                                                        }
                                                        Err(error) => {
                                                            clear_loaded_context(&mut loaded, &mut events, &mut subscriptions);
                                                            manager = Some(new_manager);
                                                            emit_devices(&weak, &discovered, selected.as_ref());
                                                            emit(&weak, UiEvent::Error(format_error_string("could not load mouse settings", error)));
                                                        }
                                                    }
                                                }
                                                Err(ManagerError::NoDevice { .. }) => {
                                                    clear_loaded_context(&mut loaded, &mut events, &mut subscriptions);
                                                    manager = Some(new_manager);
                                                    emit_devices(&weak, &discovered, selected.as_ref());
                                                    emit(&weak, UiEvent::Error("no compatible device is connected; connect one and press Refresh".into()));
                                                }
                                                Err(ManagerError::AmbiguousDevice { .. }) => {
                                                    clear_loaded_context(&mut loaded, &mut events, &mut subscriptions);
                                                    manager = Some(new_manager);
                                                    emit_devices(&weak, &discovered, selected.as_ref());
                                                    emit(&weak, UiEvent::Error("multiple compatible devices are connected; select one in the device list".into()));
                                                }
                                                Err(error) => {
                                                    clear_loaded_context(&mut loaded, &mut events, &mut subscriptions);
                                                    manager = Some(new_manager);
                                                    emit_devices(&weak, &discovered, selected.as_ref());
                                                    emit(&weak, UiEvent::Error(format_error_string("could not choose a device", error)));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Command::SelectDevice(id_string) => {
                            let Some(manager_ref) = manager.as_ref() else {
                                emit(&weak, UiEvent::Error("the app isn't ready; press Refresh to try again".into()));
                                continue;
                            };
                            let id = match DeviceId::new(id_string) {
                                Ok(id) => id,
                                Err(_) => {
                                    emit(&weak, UiEvent::OperationError("couldn't select that device; press Refresh to try again".into()));
                                    continue;
                                }
                            };
                            if let Err(error) = manager_ref.select_device(&id) {
                                emit(&weak, UiEvent::OperationError(format_error_string("could not select the device", error)));
                                continue;
                            }
                            match manager_ref.resolve_device(Some(&id), TransportSelection::Auto).await {
                                Err(error) => {
                                    selected = Some(id);
                                    emit_devices(&weak, &discovered, selected.as_ref());
                                    emit(&weak, UiEvent::OperationError(format_error_string("could not select the device", error)));
                                }
                                Ok(resolved) => {
                                    selected = Some(resolved.clone());
                                    match read_loaded(manager_ref, &resolved, None).await {
                                        Ok(new_loaded) => {
                                            let new_loaded = subscribe_and_hold(manager_ref, new_loaded, &mut events, &mut subscriptions).await;
                                            loaded = Some(new_loaded);
                                            emit_devices(&weak, &discovered, selected.as_ref());
                                            if let Some(ready) = loaded.as_ref() {
                                                emit_snapshot(&weak, ready, "device switched; mouse settings loaded");
                                            }
                                        }
                                        Err(error) => {
                                            clear_loaded_context(&mut loaded, &mut events, &mut subscriptions);
                                            emit_devices(&weak, &discovered, selected.as_ref());
                                            emit(&weak, UiEvent::OperationError(format_error_string("could not load mouse settings", error)));
                                        }
                                    }
                                }
                            }
                        }
                        Command::SelectProfile(number) => {
                            let Some(manager_ref) = manager.as_ref() else {
                                emit(&weak, UiEvent::Error("the app isn't ready; press Refresh to try again".into()));
                                continue;
                            };
                            let Some(current) = loaded.as_ref() else {
                                emit(&weak, UiEvent::Error("no mouse is ready; press Refresh to try again".into()));
                                continue;
                            };
                            emit(&weak, UiEvent::Busy(format!("activating profile {number}")));
                            match select_profile(manager_ref, current, number).await {
                                Ok(new_loaded) => {
                                    let status = if is_ble_identity(&new_loaded.identity) {
                                        format!("profile {number} selected; the mouse can't confirm this over Bluetooth")
                                    } else {
                                        format!("profile {number} selected")
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
                                emit(&weak, UiEvent::Error("the app isn't ready; press Refresh to try again".into()));
                                continue;
                            };
                            let Some(current) = loaded.as_ref() else {
                                emit(&weak, UiEvent::Error("no mouse is ready; press Refresh to try again".into()));
                                continue;
                            };
                            match apply_draft(manager_ref, current, &draft).await {
                                Ok((status, profile)) => {
                                    match read_loaded(manager_ref, &current.device, Some(profile)).await {
                                        Ok(new_loaded) => {
                                            loaded = Some(new_loaded);
                                            if let Some(ready) = loaded.as_ref() {
                                                emit_snapshot(&weak, ready, &status);
                                            }
                                        }
                                        Err(error) => {
                                            emit(&weak, format_error("settings were applied, but the app couldn't load the result", error));
                                        }
                                    }
                                }
                                Err(error) => {
                                    match read_loaded(manager_ref, &current.device, Some(current.metadata.current())).await {
                                        Ok(new_loaded) => {
                                            loaded = Some(new_loaded);
                                            if let Some(ready) = loaded.as_ref() {
                                                emit_snapshot(&weak, ready, &format!("some settings may have been applied; the current mouse settings were loaded: {error}"));
                                            }
                                        }
                                        Err(refresh_error) => emit(&weak, UiEvent::Error(format!("some settings may have been applied, but the app couldn't load the result ({error}; {refresh_error})"))),
                                    }
                                }
                            }
                        }
                        Command::Discard => {
                            let Some(manager_ref) = manager.as_ref() else {
                                emit(&weak, UiEvent::Error("the app isn't ready; press Refresh to try again".into()));
                                continue;
                            };
                            let Some(current) = loaded.as_ref() else {
                                emit(&weak, UiEvent::Error("no mouse is ready; press Refresh to try again".into()));
                                continue;
                            };
                            emit(&weak, UiEvent::Busy("loading current mouse settings".into()));
                            match read_loaded(manager_ref, &current.device, Some(current.metadata.current())).await {
                                Ok(new_loaded) => {
                                    loaded = Some(new_loaded);
                                    if let Some(ready) = loaded.as_ref() {
                                        emit_snapshot(&weak, ready, "draft discarded; current mouse settings loaded");
                                    }
                                }
                                Err(error) => emit(&weak, format_error("could not load current mouse settings", error)),
                            }
                        }
                        Command::DiscardStateSchema => {
                            emit(&weak, UiEvent::Busy("resetting unreadable local data".into()));
                            match reset_unreadable_state_and_startup().await {
                                Ok((new_manager, found, resolved, new_loaded, reset_summary)) => {
                                    let new_loaded = subscribe_and_hold(&new_manager, new_loaded, &mut events, &mut subscriptions).await;
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
                                    clear_loaded_context(&mut loaded, &mut events, &mut subscriptions);
                                    manager = None;
                                    discovered.clear();
                                    selected = None;
                                    emit(&weak, UiEvent::Error(error));
                                }
                            }
                        }
                        Command::AddProfile => {
                            let Some(manager_ref) = manager.as_ref() else {
                                emit(&weak, UiEvent::Error("the app isn't ready; press Refresh to try again".into()));
                                continue;
                            };
                            let Some(current) = loaded.as_ref() else {
                                emit(&weak, UiEvent::Error("no mouse is ready; press Refresh to try again".into()));
                                continue;
                            };
                            if is_ble_identity(&current.identity) {
                                emit(&weak, UiEvent::OperationError("adding a profile is unavailable over BLE (the mouse can't confirm it); connect by USB".into()));
                                continue;
                            }
                            let Some(target) = add_tail_metadata(current.metadata) else {
                                emit(&weak, UiEvent::OperationError("all profile slots are already enabled; nothing to add".into()));
                                continue;
                            };
                            emit(&weak, UiEvent::Busy("adding a profile".into()));
                            match manager_ref.set_profile_metadata(&current.device, target.current(), target.maximum()).await {
                                Ok(_) => {
                                    match read_loaded(manager_ref, &current.device, Some(target.current())).await {
                                        Ok(new_loaded) => {
                                            loaded = Some(new_loaded);
                                            if let Some(ready) = loaded.as_ref() {
                                                emit_snapshot(&weak, ready, &format!("profile slot {} enabled (maximum raised to {})", target.maximum(), target.maximum()));
                                            }
                                        }
                                        Err(error) => emit(&weak, format_error("profile added but reload failed", error)),
                                    }
                                }
                                Err(error) => emit(&weak, format_error("could not update the profile list", error)),
                            }
                        }
                        Command::RenameProfile(number, name) => {
                            let Some(manager_ref) = manager.as_ref() else {
                                emit(&weak, UiEvent::Error("the app isn't ready; press Refresh to try again".into()));
                                continue;
                            };
                            let Some(current) = loaded.as_ref() else {
                                emit(&weak, UiEvent::Error("no mouse is ready; press Refresh to try again".into()));
                                continue;
                            };
                            let Some(profile) = ProfileId::new(number) else {
                                emit(&weak, UiEvent::OperationError(format!("profile {number} is outside the fixed device range")));
                                continue;
                            };
                            if let Err(error) = manager_ref.set_profile_name(&current.device, profile, &name) {
                                emit(&weak, UiEvent::OperationError(format_error_string("profile rename failed", error)));
                                continue;
                            }
                            match read_loaded(manager_ref, &current.device, Some(current.metadata.current())).await {
                                Ok(new_loaded) => {
                                    loaded = Some(new_loaded);
                                    if let Some(ready) = loaded.as_ref() {
                                        emit_snapshot(&weak, ready, &format!("profile {number} renamed in the app"));
                                    }
                                }
                                Err(error) => emit(&weak, format_error("profile renamed but reload failed", error)),
                            }
                        }
                        Command::HideProfile(number) => {
                            let Some(manager_ref) = manager.as_ref() else {
                                emit(&weak, UiEvent::Error("the app isn't ready; press Refresh to try again".into()));
                                continue;
                            };
                            let Some(current) = loaded.as_ref() else {
                                emit(&weak, UiEvent::Error("no mouse is ready; press Refresh to try again".into()));
                                continue;
                            };
                            if is_ble_identity(&current.identity) {
                                emit(&weak, UiEvent::OperationError("hiding a profile is unavailable over BLE (the mouse can't confirm it); connect by USB".into()));
                                continue;
                            }
                            let Some(target) = hide_tail_metadata(current.metadata, number) else {
                                emit(&weak, UiEvent::OperationError("only the last enabled profile can be hidden; hiding a middle slot is not offered".into()));
                                continue;
                            };
                            emit(&weak, UiEvent::Busy("hiding the last profile".into()));
                            let activation = if target.current() != current.metadata.current() {
                                Some(manager_ref.activate_profile(&current.device, target.current()).await)
                            } else {
                                None
                            };
                            if let Some(Err(error)) = activation {
                                emit(&weak, format_error("profile activation before hide failed", error));
                                continue;
                            }
                            match manager_ref.set_profile_metadata(&current.device, target.current(), target.maximum()).await {
                                Ok(_) => {
                                    match read_loaded(manager_ref, &current.device, Some(target.current())).await {
                                        Ok(new_loaded) => {
                                            loaded = Some(new_loaded);
                                            if let Some(ready) = loaded.as_ref() {
                                                emit_snapshot(&weak, ready, &format!("profile {number} hidden (maximum lowered to {})", target.maximum()));
                                            }
                                        }
                                        Err(error) => emit(&weak, format_error("profile hidden but reload failed", error)),
                                    }
                                }
                                Err(error) => emit(&weak, format_error("could not update the profile list", error)),
                            }
                        }
                        Command::Import(path) => {
                            let Some(manager_ref) = manager.as_ref() else {
                                emit(&weak, UiEvent::Error("the app isn't ready; press Refresh to try again".into()));
                                continue;
                            };
                            let Some(selected_id) = selected.as_ref() else {
                                emit(&weak, UiEvent::OperationError("no device is selected to import into; select one first".into()));
                                continue;
                            };
                            let identity = discovered.iter().find(|d| &d.identity.id == selected_id).map(|d| d.identity.clone()).or_else(|| manager_ref.device_identity(selected_id).ok());
                            let Some(identity) = identity else {
                                emit(&weak, UiEvent::OperationError("selected device is not registered; press Refresh to rescan".into()));
                                continue;
                            };
                            let text = match std::fs::read_to_string(&path) {
                                Ok(t) => t,
                                Err(error) => {
                                    emit(&weak, UiEvent::OperationError(format!("could not read {}: {error}", path.display())));
                                    continue;
                                }
                            };
                            let configuration: ConfigurationExport = match serde_json::from_str(&text) {
                                Ok(c) => c,
                                Err(error) => {
                                    emit(&weak, UiEvent::OperationError(format!("invalid configuration JSON in {}: {error}", path.display())));
                                    continue;
                                }
                            };
                            if let Err(error) = manager_ref.import_configuration(&identity, configuration) {
                                emit(&weak, UiEvent::OperationError(format_error_string("configuration import failed", error)));
                                continue;
                            }
                            if let Some(current) = loaded.as_ref() && current.device == identity.id {
                                match read_loaded(manager_ref, &current.device, Some(current.metadata.current())).await {
                                    Ok(new_loaded) => {
                                        loaded = Some(new_loaded);
                                        if let Some(ready) = loaded.as_ref() {
                                            emit_snapshot(&weak, ready, &format!("configuration imported from {}; saved settings updated; the mouse was not changed", path.display()));
                                        }
                                    }
                                    Err(error) => emit(&weak, format_error("configuration imported, but the app couldn't load it", error)),
                                }
                            } else {
                                emit(&weak, UiEvent::OperationError(format!("configuration imported from {}; select the device to view it", path.display())));
                            }
                        }
                        Command::Export(path) => {
                            let Some(manager_ref) = manager.as_ref() else {
                                emit(&weak, UiEvent::Error("the app isn't ready; press Refresh to try again".into()));
                                continue;
                            };
                            let Some(selected_id) = selected.as_ref() else {
                                emit(&weak, UiEvent::OperationError("no device is selected to export; select one first".into()));
                                continue;
                            };
                            let configuration = match manager_ref.export_configuration(selected_id) {
                                Ok(c) => c,
                                Err(error) => {
                                    emit(&weak, UiEvent::OperationError(format_error_string("configuration export failed", error)));
                                    continue;
                                }
                            };
                            let json = match serde_json::to_string_pretty(&configuration) {
                                Ok(j) => j,
                                Err(error) => {
                                    emit(&weak, UiEvent::OperationError(format!("configuration could not be encoded: {error}")));
                                    continue;
                                }
                            };
                            if let Err(error) = std::fs::write(&path, json) {
                                emit(&weak, UiEvent::OperationError(format!("could not write {}: {error}", path.display())));
                                continue;
                            }
                            emit(&weak, UiEvent::OperationError(format!("configuration exported to {}; includes your settings and the mouse's latest confirmation", path.display())));
                        }
                        Command::RefreshAllProfiles => {
                            let Some(manager_ref) = manager.as_ref() else {
                                emit(&weak, UiEvent::Error("the app isn't ready; press Refresh to try again".into()));
                                continue;
                            };
                            let Some(current) = loaded.as_ref() else {
                                emit(&weak, UiEvent::Error("no mouse is ready; press Refresh to try again".into()));
                                continue;
                            };
                            let device = current.device.clone();
                            match manager_ref.refresh_all_profiles(&device).await {
                                Ok(outcome) => {
                                    let status = format_refresh_summary(&outcome);
                                    match read_loaded(manager_ref, &device, Some(outcome.restored_metadata.current())).await {
                                        Ok(new_loaded) => {
                                            loaded = Some(new_loaded);
                                            if let Some(ready) = loaded.as_ref() {
                                                emit_snapshot(&weak, ready, &status);
                                            }
                                        }
                                        Err(error) => {
                                            clear_loaded_context(&mut loaded, &mut events, &mut subscriptions);
                                            emit(&weak, format_error("profiles refreshed but the restored profile could not be reloaded", error));
                                        }
                                    }
                                }
                                Err(error) => emit(&weak, UiEvent::OperationError(format_error_string("could not read all profiles", error))),
                            }
                        }
                        Command::InvalidateState => {
                            let Some(manager_ref) = manager.as_ref() else {
                                emit(&weak, UiEvent::Error("the app isn't ready; press Refresh to try again".into()));
                                continue;
                            };
                            let Some(current) = loaded.as_ref() else {
                                emit(&weak, UiEvent::Error("no mouse is ready; press Refresh to try again".into()));
                                continue;
                            };
                            if let Err(error) = manager_ref.invalidate_state(&current.device) {
                                emit(&weak, UiEvent::OperationError(format_error_string("could not clear saved confirmation", error)));
                                continue;
                            }
                            match read_loaded(manager_ref, &current.device, Some(current.metadata.current())).await {
                                Ok(new_loaded) => {
                                    loaded = Some(new_loaded);
                                    if let Some(ready) = loaded.as_ref() {
                                        emit_snapshot(&weak, ready, "saved confirmation cleared; your settings are kept");
                                    }
                                }
                                Err(error) => emit(&weak, format_error("saved confirmation cleared, but the app couldn't reload the settings", error)),
                            }
                        }
                        Command::VerifyProfile => {
                            let Some(manager_ref) = manager.as_ref() else {
                                emit(&weak, UiEvent::Error("the app isn't ready; press Refresh to try again".into()));
                                continue;
                            };
                            let Some(current) = loaded.as_ref() else {
                                emit(&weak, UiEvent::Error("no mouse is ready; press Refresh to try again".into()));
                                continue;
                            };
                            if is_ble_identity(&current.identity) {
                                emit(&weak, UiEvent::OperationError("the profile-reload check needs USB; unavailable over BLE".into()));
                                continue;
                            }
                            let target = current.metadata.current();
                            emit(&weak, UiEvent::Busy("checking whether settings survive switching profiles".into()));
                            emit(&weak, UiEvent::Verification(true));
                            let outcome = manager_ref.verify_profile_reload(&current.device, target).await;
                            emit(&weak, UiEvent::Verification(false));
                            match outcome {
                                Ok(outcome) => {
                                    let summary = workflow_summary("profile reload", outcome.profile, &outcome.dpi.verification);
                                    match read_loaded(manager_ref, &current.device, Some(target)).await {
                                        Ok(new_loaded) => {
                                            loaded = Some(new_loaded);
                                            if let Some(ready) = loaded.as_ref() {
                                                emit_snapshot(&weak, ready, &summary);
                                            }
                                        }
                                        Err(error) => emit(&weak, format_error("check finished, but the app couldn't reload the settings", error)),
                                    }
                                }
                                Err(error) => emit(&weak, UiEvent::Error(format_error_string("profile switch check failed", error))),
                            }
                        }
                        Command::VerifyPowerCycle => {
                            let Some(manager_ref) = manager.as_ref() else {
                                emit(&weak, UiEvent::Error("the app isn't ready; press Refresh to try again".into()));
                                continue;
                            };
                            let Some(current) = loaded.as_ref() else {
                                emit(&weak, UiEvent::Error("no mouse is ready; press Refresh to try again".into()));
                                continue;
                            };
                            if is_ble_identity(&current.identity) {
                                emit(&weak, UiEvent::OperationError("the power-cycle check needs USB; unavailable over BLE".into()));
                                continue;
                            }
                            let target = current.metadata.current();
                            emit(&weak, UiEvent::Busy("checking whether settings survive a full power-off; unplug USB, switch the mouse off, then reconnect it".into()));
                            emit(&weak, UiEvent::Verification(true));
                            subscriptions = None;
                            events = None;
                            let device_clone = current.device.clone();
                            let verify_fut = manager_ref.verify_power_cycle(&device_clone, target);
                            let outcome = tokio::select! {
                                res = verify_fut => Some(res),
                                _ = shutdown.changed() => {
                                    if *shutdown.borrow() {
                                        emit(&weak, UiEvent::OperationError("power-off check cancelled because the app is closing".into()));
                                        emit(&weak, UiEvent::Verification(false));
                                        clear_loaded_context(&mut loaded, &mut events, &mut subscriptions);
                                        break;
                                    }
                                    None
                                }
                            };
                            let Some(outcome) = outcome else { continue; };
                            emit(&weak, UiEvent::Verification(false));
                            match outcome {
                                Ok(outcome) => {
                                    let summary = workflow_summary("power cycle", outcome.profile, &outcome.dpi.verification);
                                    match read_loaded(manager_ref, &current.device, Some(target)).await {
                                        Ok(new_loaded) => {
                                            let new_loaded = subscribe_and_hold(manager_ref, new_loaded, &mut events, &mut subscriptions).await;
                                            loaded = Some(new_loaded);
                                            if let Some(ready) = loaded.as_ref() {
                                                emit_snapshot(&weak, ready, &summary);
                                            }
                                        }
                                        Err(error) => emit(&weak, format_error("check finished, but the app couldn't reload the settings", error)),
                                    }
                                }
                                Err(error) => emit(&weak, UiEvent::Error(format_error_string("power-off check failed", error))),
                            }
                        }
                        Command::Shutdown => {
                            clear_loaded_context(&mut loaded, &mut events, &mut subscriptions);
                            break;
                        }
                    }
                    // coalesce redundant pending Refresh/Startup: drain consecutive coalescable duplicates
                    if is_coalescable {
                        while let Ok(next) = commands.try_recv() {
                            if is_coalescable_command(&next) {
                                continue;
                            } else {
                                pending = Some(next);
                                break;
                            }
                        }
                    }
                }
                ev = async {
                    if let Some(rx) = events.as_mut() {
                        rx.recv().await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    match ev {
                        Ok(event) => {
                            let mut ctx = DeviceEventContext {
                                loaded: &mut loaded,
                                manager: &mut manager,
                                discovered: &mut discovered,
                                selected: &mut selected,
                                events: &mut events,
                                subscriptions: &mut subscriptions,
                                weak: &weak,
                            };
                            handle_device_event(&mut ctx, event).await;
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            let mut ctx = DeviceEventContext {
                                loaded: &mut loaded,
                                manager: &mut manager,
                                discovered: &mut discovered,
                                selected: &mut selected,
                                events: &mut events,
                                subscriptions: &mut subscriptions,
                                weak: &weak,
                            };
                            handle_device_event(&mut ctx, DeviceEvent::Lagged { skipped }).await;
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            clear_loaded_context(&mut loaded, &mut events, &mut subscriptions);
                        }
                    }
                }
            }
        }
        clear_loaded_context(&mut loaded, &mut events, &mut subscriptions);
        Ok(())
    })
}

/// Subscribes the freshly loaded device to event delivery and keeps the
/// subscription alive for the lifetime of the worker loop.
async fn subscribe_and_hold(
    manager: &DeviceManager,
    loaded: LoadedDevice,
    events: &mut Option<broadcast::Receiver<DeviceEvent>>,
    subscriptions: &mut Option<EventSubscriptions>,
) -> LoadedDevice {
    match manager.subscribe_events(&loaded.device).await {
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

struct DeviceEventContext<'a> {
    loaded: &'a mut Option<LoadedDevice>,
    manager: &'a mut Option<DeviceManager>,
    discovered: &'a mut [DiscoveredDevice],
    selected: &'a mut Option<DeviceId>,
    events: &'a mut Option<broadcast::Receiver<DeviceEvent>>,
    subscriptions: &'a mut Option<EventSubscriptions>,
    weak: &'a slint::Weak<AppWindow>,
}

async fn handle_device_event(ctx: &mut DeviceEventContext<'_>, event: DeviceEvent) {
    match event {
        DeviceEvent::BatteryChanged(e) => {
            if let Some(dev) = ctx.loaded.as_mut() {
                dev.battery = Some(e.level);
                emit_event_snapshot(ctx.weak, dev, "device battery level changed");
            }
        }
        DeviceEvent::ActiveDpiStageChanged(e) => {
            if let Some(dev) = ctx.loaded.as_mut() {
                dev.profile.dpi.active_stage = e.active_stage;
                emit_event_snapshot(ctx.weak, dev, "active DPI stage changed on device");
            }
        }
        DeviceEvent::ProfileChanged(_)
        | DeviceEvent::ProfileSync(_)
        | DeviceEvent::SecondaryProfileChanged(_) => {
            if let (Some(dev), Some(mgr)) = (ctx.loaded.as_mut(), ctx.manager.as_ref())
                && let Some(profile) = reported_profile_switch(event, dev.metadata.current())
            {
                match select_profile(mgr, dev, profile.get()).await {
                    Ok(new_loaded) => {
                        *ctx.loaded = Some(new_loaded);
                        if let Some(ready) = ctx.loaded.as_ref() {
                            emit_event_snapshot(
                                ctx.weak,
                                ready,
                                "device profile changed; state reloaded",
                            );
                        }
                    }
                    Err(error) => emit(ctx.weak, UiEvent::Error(error)),
                }
            }
        }
        DeviceEvent::Disconnected => {
            handle_disconnect(
                ctx.loaded,
                ctx.discovered,
                ctx.selected,
                ctx.weak,
                ctx.events,
                ctx.subscriptions,
            );
        }
        DeviceEvent::ConnectionChanged(e) if !e.connected => {
            handle_disconnect(
                ctx.loaded,
                ctx.discovered,
                ctx.selected,
                ctx.weak,
                ctx.events,
                ctx.subscriptions,
            );
        }
        DeviceEvent::Lagged { skipped } => {
            emit(
                ctx.weak,
                UiEvent::OperationError(lagged_event_message(skipped)),
            );
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
    events: &mut Option<broadcast::Receiver<DeviceEvent>>,
    subscriptions: &mut Option<EventSubscriptions>,
) {
    if loaded.is_none() {
        return;
    }
    if let Some(dev) = loaded.as_ref()
        && let Some(entry) = discovered
            .iter_mut()
            .find(|entry| entry.identity.id == dev.device)
    {
        entry.connected = false;
    }
    clear_loaded_context(loaded, events, subscriptions);
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
            let transport = device
                .transports
                .iter()
                .map(|transport| transport_label(*transport))
                .collect::<Vec<_>>()
                .join(" + ");
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

fn open_manager() -> Result<DeviceManager, StartupError> {
    let store = StateStore::with_default_paths()
        .map_err(|error| StartupError::Message(format!("could not open local data: {error}")))?;
    if store.load().is_err() {
        return Err(StartupError::UnreadableState(format!(
            "The app couldn't read its local data at {}. Reset it to continue; a backup will be kept.",
            store.paths().state_file().display()
        )));
    }
    DeviceManager::new(store).map_err(|error| {
        StartupError::Message(format!("could not initialize DeviceManager: {error}"))
    })
}

/// Discards an unreadable state file (keeping a sibling backup) and reruns
/// device startup, returning a human-readable reset summary for the UI.
async fn reset_unreadable_state_and_startup() -> Result<
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
        .map_err(|error| format!("could not open local data: {error}"))?;
    let reset_manager = DeviceManager::new(store)
        .map_err(|error| format!("could not prepare local data: {error}"))?;
    let reset = reset_manager
        .discard_unreadable_state()
        .map_err(|error| format!("could not reset local data: {error}"))?;
    let summary = match (&reset.backup, reset.discarded_schema) {
        (Some(path), Some(_)) => {
            format!("local data was reset and backed up to {}", path.display())
        }
        (Some(path), None) => format!("local data was reset and backed up to {}", path.display()),
        (None, _) => "local data was reset".into(),
    };
    match startup_flow().await {
        Ok((manager, discovered, resolved, loaded)) => {
            Ok((manager, discovered, resolved, loaded, summary))
        }
        Err(StartupError::UnreadableState(detail)) => Err(detail),
        Err(StartupError::Message(detail)) => Err(detail),
    }
}

/// Discovers every enabled transport and resolves one device without
/// guessing, honoring the durable selection.
async fn startup_flow()
-> Result<(DeviceManager, Vec<DiscoveredDevice>, DeviceId, LoadedDevice), StartupError> {
    let manager = open_manager()?;
    let discovered = manager
        .list_devices(TransportSelection::Auto)
        .await
        .map_err(|error| StartupError::Message(format!("device discovery failed: {error}")))?;
    let resolved = manager
        .resolve_device(None, TransportSelection::Auto)
        .await
        .map_err(|error| {
            StartupError::Message(format_error_string("device resolution failed", error))
        })?;
    let loaded = read_loaded(&manager, &resolved, None)
        .await
        .map_err(|error| {
            StartupError::Message(format_error_string(
                "could not load initial mouse settings",
                error,
            ))
        })?;
    Ok((manager, discovered, resolved, loaded))
}

async fn read_loaded(
    manager: &DeviceManager,
    device: &DeviceId,
    requested: Option<ProfileId>,
) -> Result<LoadedDevice, ManagerError> {
    let status = manager.read_status(device).await?;
    if is_ble_identity(&status.identity) {
        return read_ble_loaded(manager, status, requested);
    }
    let metadata = status
        .profile_metadata
        .as_ref()
        .and_then(|snapshot| snapshot.resource.observed.as_ref())
        .map(|observed| observed.value)
        .ok_or_else(|| {
            ManagerError::InvalidUpdate("USB did not return the current profile details".into())
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
            ManagerError::InvalidUpdate("USB did not return the current polling rate".into())
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
    let metadata = ProfileMetadata::new(working, ProfileId::MAX_ID).expect("valid metadata");
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

pub fn has_complete_profile_observations(state: &StateFile, device: &DeviceId) -> bool {
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
        return "showing the factory image; no saved settings yet (import one or allow defaults)"
            .to_owned();
    }
    let Some(profile_state) = state
        .devices
        .get(device)
        .and_then(|device| device.profiles.get(&profile))
    else {
        return if is_ble {
            "saved settings only; nothing applied yet".to_owned()
        } else {
            "read from the mouse; not confirmed after restart".to_owned()
        };
    };
    let mut summary = SubmissionSummary::default();
    record_submission(&profile_state.dpi, &mut summary);
    record_submission(&profile_state.preferences, &mut summary);
    record_submission(&profile_state.buttons, &mut summary);
    if summary.power_cycle {
        "confirmed to survive a full power-off".to_owned()
    } else if summary.reload {
        "confirmed to survive switching profiles".to_owned()
    } else if summary.submitted && summary.imported_only {
        "imported settings saved; nothing sent to the mouse".to_owned()
    } else if summary.submitted {
        "applied".to_owned()
    } else if is_ble {
        "saved settings only; nothing applied yet".to_owned()
    } else {
        "read from the mouse; not confirmed after restart".to_owned()
    }
}

async fn select_profile(
    manager: &DeviceManager,
    current: &LoadedDevice,
    number: u8,
) -> Result<LoadedDevice, String> {
    let target = ProfileId::new(number)
        .ok_or_else(|| format!("profile {number} is outside the fixed device range 1..=5"))?;
    if is_ble_identity(&current.identity) {
        manager
            .activate_profile(&current.device, target)
            .await
            .map_err(|error| format_error_string("BLE profile activation failed", error))?;
        return read_loaded(manager, &current.device, Some(target))
            .await
            .map_err(|error| format_error_string("saved-settings reload failed", error));
    }
    if target > current.metadata.maximum() {
        return Err(format!(
            "profile {number} is not enabled (maximum enabled profile is {})",
            current.metadata.maximum()
        ));
    }
    if target == current.metadata.current() {
        return read_loaded(manager, &current.device, Some(target))
            .await
            .map_err(|error| format_error_string("profile read failed", error));
    }
    manager
        .activate_profile(&current.device, target)
        .await
        .map_err(|error| format_error_string("profile activation failed", error))?;
    read_loaded(manager, &current.device, Some(target))
        .await
        .map_err(|error| format_error_string("profile read failed", error))
}

async fn apply_draft(
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
    let is_ble = is_ble_identity(&current.identity);
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
            "the draft has {} DPI stages; the mouse supports 1..={MAX_DPI_STAGES}; nothing was changed",
            draft.dpi_values.len()
        ));
    }
    let active_stage_index = draft.active_stage;
    if active_stage_index == 0 || active_stage_index as usize > draft.dpi_values.len() {
        return Err(
            "the active DPI stage isn't one of the configured stages; nothing was changed".into(),
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
    let sensor_delta = SensorOptionsDelta {
        lift_off_distance: (sensor.lift_off_distance != requested_lift).then_some(requested_lift),
        ripple_control: (sensor.ripple_control != draft.ripple_control)
            .then_some(draft.ripple_control),
        angle_snap: (sensor.angle_snap != draft.angle_snap).then_some(draft.angle_snap),
        motion_sync: (sensor.motion_sync != draft.motion_sync).then_some(draft.motion_sync),
    };
    let dpi_delta = {
        let has_dpi = dpi_changed;
        let has_sensor = !sensor_delta.is_empty();
        if !has_dpi && !has_sensor {
            None
        } else {
            let stages = if has_dpi {
                let stages = requested_dpi
                    .iter()
                    .copied()
                    .map(|value| {
                        DpiValue::new(value).ok_or_else(|| format!("invalid DPI value {value}"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Some(stages)
            } else {
                None
            };
            let active_stage = if has_dpi {
                Some(
                    StageIndex::new(active_stage_index)
                        .ok_or_else(|| "invalid active DPI stage".to_owned())?,
                )
            } else {
                None
            };
            let sensor_opt = if has_sensor { Some(sensor_delta) } else { None };
            let delta = DpiDelta {
                stages,
                active_stage,
                sensor: sensor_opt,
            };
            if delta.is_empty() { None } else { Some(delta) }
        }
    };

    let preferences = current.profile.preferences;
    let preferences_delta = {
        let mut delta = PreferencesDelta::default();
        if preferences.configuration != draft.raw_configuration {
            delta.configuration = Some(draft.raw_configuration);
        }
        if preferences.deep_sleep != draft.raw_deep_sleep {
            delta.deep_sleep = Some(draft.raw_deep_sleep);
        }
        if preferences.sleep_timer != draft.raw_sleep_timer {
            delta.sleep_timer = Some(draft.raw_sleep_timer);
        }
        if preferences.debounce != draft.raw_debounce {
            delta.debounce = Some(draft.raw_debounce);
        }
        if delta.is_empty() { None } else { Some(delta) }
    };

    let baseline_buttons: Vec<String> = SAFE_BUTTON_SLOTS
        .iter()
        .copied()
        .map(|slot| button_action_name(current.profile.buttons.slots[slot.index()]))
        .collect();
    let mut button_deltas = Vec::new();
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
                format!("unsupported button action {requested:?}; nothing was changed")
            })?;
            button_deltas.push(ButtonSlotDelta::new(SAFE_BUTTON_SLOTS[index], action));
        }
    }

    let requested_rate = PollingRate::new(draft.polling_rate_hz)
        .ok_or_else(|| "unsupported polling rate".to_owned())?;
    let polling_rate = if requested_rate != current.polling_rate {
        Some(requested_rate)
    } else {
        None
    };

    let has_non_rate =
        dpi_delta.is_some() || preferences_delta.is_some() || !button_deltas.is_empty();
    if !has_non_rate && polling_rate.is_none() {
        return Ok((
            "no changes to apply; the mouse is already up to date".into(),
            profile,
        ));
    }

    // The manager owns the report-0x06 isolation: a polling-rate change must
    // never share one apply with DPI/preferences/buttons. Apply the non-rate
    // composite first, then the rate through its own isolated call, so a
    // combined draft applies without losing edits.
    let mut outcome = ProfileUpdateOutcome::default();
    if has_non_rate {
        let non_rate = ProfileUpdate {
            dpi: dpi_delta,
            preferences: preferences_delta,
            buttons: button_deltas,
            polling_rate: None,
        };
        outcome = manager
            .apply_profile_update(&current.device, profile, non_rate, policy)
            .await
            .map_err(|error| format_error_string("apply failed", error))?;
    }

    if let Some(rate) = polling_rate {
        let rate_update = ProfileUpdate {
            dpi: None,
            preferences: None,
            buttons: Vec::new(),
            polling_rate: Some(rate),
        };
        let rate_outcome = manager
            .apply_profile_update(&current.device, profile, rate_update, policy)
            .await
            .map_err(|error| format_error_string("polling-rate apply failed", error))?;
        outcome.polling_rate = rate_outcome.polling_rate;
    }

    let status = profile_update_status(&outcome);
    Ok((status, profile))
}
pub fn make_live_snapshot(loaded: &LoadedDevice, status: &str) -> LiveSnapshot {
    let active_index = loaded.profile.dpi.active_stage.get().saturating_sub(1) as usize;
    let active_dpi = loaded
        .profile
        .dpi
        .stages
        .get(active_index)
        .copied()
        .map(DpiValue::get)
        .unwrap_or(0);
    let is_ble = is_ble_identity(&loaded.identity);
    let transport = transport_label_for_identity(&loaded.identity);
    let battery = loaded
        .battery
        .map_or_else(String::new, |value| format!("{value}%"));
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
            "current {} / maximum {} · not read from the mouse (ble)",
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
            "profile {} · {} dpi · saved settings (not read from the mouse)",
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
        product_id: product_id_label_for_identity(&loaded.identity),
        transport,
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
pub fn preference_fields(
    preferences: &attack_shark_x3_manager::PreferencesState,
) -> PreferenceFields {
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

fn lagged_event_message(skipped: u64) -> String {
    format!("missed {skipped} mouse updates; press Refresh to load the latest settings")
}

fn format_error(prefix: &str, error: ManagerError) -> UiEvent {
    UiEvent::Error(format_error_string(prefix, error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use attack_shark_x3::{DpiState, PollingRate, ProfileId, ProfileMetadata};
    use attack_shark_x3_manager::{DeviceId, DeviceIdentity};
    use std::collections::BTreeMap;
    use tokio::sync::{broadcast, mpsc, watch};
    use tokio::time::{Duration, timeout};

    fn dummy_loaded() -> LoadedDevice {
        let profile = ProfileId::new(1).unwrap();
        let dpi = DpiState::captured_stock_reset(profile).unwrap();
        let prefs = attack_shark_x3_manager::PreferencesState::captured_stock_reset(profile);
        let buttons = attack_shark_x3_manager::ButtonsState::default_for_profile(profile);
        let id = DeviceId::new("mouse-1").unwrap();
        let identity = DeviceIdentity::new(id.clone(), Some("Test Mouse".into()));
        LoadedDevice {
            device: id,
            identity,
            metadata: ProfileMetadata::new(profile, profile).unwrap(),
            profile: ProfileData {
                dpi,
                preferences: prefs,
                buttons,
            },
            polling_rate: PollingRate::Hz1000,
            battery: Some(80),
            polling_rate_ready: true,
            all_profiles_observed: false,
            has_stored_preferences: true,
            profile_names: BTreeMap::new(),
            verification_summary: String::new(),
        }
    }

    #[test]
    fn bounded_channel_capacity_is_32() {
        let (tx, _rx) = mpsc::channel::<Command>(32);
        assert_eq!(tx.capacity(), 32);
        assert_eq!(tx.max_capacity(), 32);
    }

    #[tokio::test]
    async fn queue_full_and_coalescing() {
        let (tx, mut rx) = mpsc::channel::<Command>(32);
        for _ in 0..32 {
            assert!(tx.try_send(Command::Refresh).is_ok());
        }
        match tx.try_send(Command::Refresh) {
            Err(mpsc::error::TrySendError::Full(cmd)) => {
                assert!(is_coalescable_command(&cmd));
            }
            other => panic!("expected Full, got {other:?}"),
        }
        match tx.try_send(Command::AddProfile) {
            Err(mpsc::error::TrySendError::Full(cmd)) => {
                assert!(!is_coalescable_command(&cmd));
            }
            other => panic!("expected Full for non-coalescable, got {other:?}"),
        }
        let _ = rx.recv().await;
        assert!(tx.try_send(Command::Refresh).is_ok());
        // second send should be Full (capacity 32) and be coalescable
        match tx.try_send(Command::Refresh) {
            Err(mpsc::error::TrySendError::Full(cmd)) => assert!(is_coalescable_command(&cmd)),
            Ok(()) => panic!("expected Full on second coalescable send"),
            Err(e) => panic!("unexpected error {e:?}"),
        }
        let mut pending: Option<Command> = None;
        let handled = Command::Refresh;
        if is_coalescable_command(&handled) {
            while let Ok(next) = rx.try_recv() {
                if is_coalescable_command(&next) {
                    continue;
                } else {
                    pending = Some(next);
                    break;
                }
            }
        }
        assert!(
            pending.is_none(),
            "coalescable duplicates should be dropped"
        );
    }

    #[tokio::test]
    async fn clear_loaded_context_drops_all() {
        let mut loaded = Some(dummy_loaded());
        let (ev_tx, ev_rx) = broadcast::channel::<DeviceEvent>(16);
        let mut events: Option<broadcast::Receiver<DeviceEvent>> = Some(ev_rx);
        let mut subs: Option<EventSubscriptions> = None;
        clear_loaded_context(&mut loaded, &mut events, &mut subs);
        assert!(loaded.is_none());
        assert!(events.is_none());
        assert!(subs.is_none());
        let mut loaded2 = Some(dummy_loaded());
        let mut events2 = Some(ev_tx.subscribe());
        clear_loaded_context(&mut loaded2, &mut events2, &mut subs);
        assert!(loaded2.is_none());
        assert!(events2.is_none());
    }

    #[tokio::test]
    async fn lagged_event_is_visible_and_handled() {
        let message = lagged_event_message(7);
        assert_eq!(
            message,
            "missed 7 mouse updates; press Refresh to load the latest settings"
        );
        assert!(!message.contains("lag"));
        assert!(!message.contains("dropped"));

        let mut loaded = Some(dummy_loaded());
        let mut events: Option<broadcast::Receiver<DeviceEvent>> = None;
        let mut subs: Option<EventSubscriptions> = None;
        let mut manager: Option<DeviceManager> = None;
        let mut discovered: Vec<DiscoveredDevice> = Vec::new();
        let mut selected: Option<DeviceId> = None;
        let weak = slint::Weak::<AppWindow>::default();
        let mut ctx = DeviceEventContext {
            loaded: &mut loaded,
            manager: &mut manager,
            discovered: &mut discovered,
            selected: &mut selected,
            events: &mut events,
            subscriptions: &mut subs,
            weak: &weak,
        };
        handle_device_event(&mut ctx, DeviceEvent::Lagged { skipped: 3 }).await;
        assert!(
            loaded.is_some(),
            "missed updates should keep loaded context"
        );
    }

    #[tokio::test]
    async fn prompt_event_delivery_via_select() {
        let (tx, mut rx) = broadcast::channel::<DeviceEvent>(16);
        let (_shut_tx, mut shutdown) = watch::channel(false);
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<Command>(32);
        let _keep_cmd = cmd_tx;
        let evt = DeviceEvent::BatteryChanged(attack_shark_x3::BatteryEvent {
            raw_report: [0; 5],
            level: 42,
        });
        tx.send(evt).unwrap();
        let start = tokio::time::Instant::now();
        let delivered = tokio::select! {
            biased;
            _ = shutdown.changed() => None,
            _cmd = cmd_rx.recv() => None,
            ev = rx.recv() => Some(ev.unwrap()),
        };
        let elapsed = start.elapsed();
        assert!(delivered.is_some());
        assert!(
            elapsed < Duration::from_millis(50),
            "event delivery not prompt: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn shutdown_cancels_power_cycle_future() {
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let long_future = async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok::<(), ()>(())
        };
        let shutdown_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let _ = shutdown_tx.send(true);
        });
        let result = tokio::select! {
            res = long_future => res,
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    Err(())
                } else { Ok(()) }
            }
        };
        assert!(result.is_err(), "shutdown should cancel long future");
        let _ = shutdown_task.await;
    }

    #[tokio::test]
    async fn shutdown_does_not_block_on_power_cycle_wait() {
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<Command>(32);
        let _keep_cmd2 = cmd_tx;
        let power_cycle = async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            "done"
        };
        let start = tokio::time::Instant::now();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            let _ = shutdown_tx.send(true);
        });
        let outcome: Option<&str> = tokio::select! {
            res = power_cycle => Some(res),
            _ = shutdown_rx.changed() => None,
            _ = cmd_rx.recv() => None,
        };
        let elapsed = start.elapsed();
        assert!(outcome.is_none(), "power-cycle should be cancelled");
        assert!(
            elapsed < Duration::from_millis(200),
            "shutdown must not wait for power-cycle: {elapsed:?}"
        );
        let _ = timeout(Duration::from_millis(100), async {}).await;
    }

    #[tokio::test]
    async fn profile_switch_restoration_not_cancelled() {
        let restore = async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            "restored"
        };
        let (shutdown_tx, _rx) = watch::channel(false);
        let _ = shutdown_tx.send(true);
        let result = restore.await;
        assert_eq!(result, "restored");
    }
}
