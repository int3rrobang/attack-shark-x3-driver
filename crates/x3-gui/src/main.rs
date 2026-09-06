use std::{error::Error, thread};

use attack_shark_x3::{DebounceMs, SleepTimer};
use attack_shark_x3_manager::{DeviceId, IdentityCeremonyAction, IdentityCeremonyKind, ProfileId};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use tokio::sync::{mpsc, watch};

mod app_settings;
mod generated_ui;
mod presentation;
mod projection;
mod worker;

pub use generated_ui::{
    AppWindow, BindingRow, DeviceRow, DpiStage, ProfileRow, Theme, UnassociatedRow,
};

use crate::presentation::{
    BINDING_ACTIONS, MAX_DPI_STAGES, RAW_PREFERENCE_CONFIGURATION, RAW_PREFERENCE_DEBOUNCE,
    RAW_PREFERENCE_DEEP_SLEEP, RAW_PREFERENCE_SLEEP_TIMER, VERIFICATION_INSTRUCTION_POWER_CYCLE,
    VERIFICATION_INSTRUCTION_PROFILE_RELOAD, configured_product_label, format_dpi_setting,
    page_status, parse_dpi_range,
};
use crate::presentation::{
    canonical_debounce_ms, canonical_deep_sleep_minutes, canonical_sleep_minutes,
    dirty_draft_message, format_byte, parse_raw_byte, parse_raw_preference_draft,
    raw_preference_field_label, round_dpi_step, validated_binding_action,
};
use crate::projection::{
    append_dpi_stage, dpi_from_ratio, refresh_button_change_summary, refresh_deep_sleep_display,
    refresh_dpi_ratios, remove_dpi_stage, set_active_dpi_stage, update_single_dpi_ratio,
};
use crate::worker::{BaselineChoice, Command, Draft, VerificationChoice, worker_main};

/// Action codes the unassociated-connection rows send through
/// `unassociated-action`; they select which ceremony the row starts.
const UNASSOCIATED_ADD: i32 = 0;
const UNASSOCIATED_RESTORE: i32 = 1;
const UNASSOCIATED_ADOPT: i32 = 2;
const UNASSOCIATED_ASSOCIATE: i32 = 3;

fn main() -> Result<(), Box<dyn Error>> {
    let ui = AppWindow::new()?;
    let dpi_stages = std::rc::Rc::new(VecModel::from(Vec::<DpiStage>::new()));
    let bindings = std::rc::Rc::new(VecModel::from(Vec::<BindingRow>::new()));
    let profiles = std::rc::Rc::new(VecModel::from(Vec::<ProfileRow>::new()));
    let devices = std::rc::Rc::new(VecModel::from(Vec::<DeviceRow>::new()));
    let unassociated = std::rc::Rc::new(VecModel::from(Vec::<UnassociatedRow>::new()));
    ui.set_dpi_stages(ModelRc::from(dpi_stages.clone()));
    ui.set_bindings(ModelRc::from(bindings.clone()));
    ui.set_profiles(ModelRc::from(profiles.clone()));
    ui.set_devices(ModelRc::from(devices.clone()));
    ui.set_unassociated(ModelRc::from(unassociated));
    let binding_actions = std::rc::Rc::new(VecModel::from(
        BINDING_ACTIONS
            .iter()
            .map(|action| slint::SharedString::from(*action))
            .collect::<Vec<_>>(),
    ));
    ui.set_binding_actions(ModelRc::from(binding_actions));
    // Load GUI preferences before initial Slint properties; malformed/
    // unknown configs are backed up and defaults are used with a restrained
    // warning shown in the status text.
    let (gui_prefs, gui_warning) = app_settings::load_gui_preferences();
    {
        let theme = ui.global::<Theme>();
        let appearance = match gui_prefs.appearance {
            1 => slint::language::ColorScheme::Light,
            2 => slint::language::ColorScheme::Dark,
            _ => slint::language::ColorScheme::Unknown,
        };
        theme.set_appearance(appearance);
    }
    let custom_name = gui_prefs.custom_product_name.clone();
    let choice = gui_prefs.product_name_choice;
    ui.set_product_name_choice(choice);
    ui.set_custom_product_name(custom_name.clone().into());
    ui.set_product_name(configured_product_label(choice, custom_name.as_str()).into());
    ui.set_dpi_log_scale(gui_prefs.dpi_log_scale);
    ui.set_dpi_min(gui_prefs.dpi_min);
    ui.set_dpi_max(gui_prefs.dpi_max);
    ui.set_dpi_min_text(format_dpi_setting(gui_prefs.dpi_min).into());
    ui.set_dpi_max_text(format_dpi_setting(gui_prefs.dpi_max).into());
    ui.set_current_page(gui_prefs.last_page);
    ui.set_hardware_ready(false);
    ui.set_busy(true);
    ui.set_lifecycle_text("starting device discovery".into());
    let initial_status = if let Some(warning) = gui_warning {
        warning
    } else {
        "discovering wired, receiver, and BLE devices".to_owned()
    };
    ui.set_status_text(initial_status.into());
    ui.set_transport_label("no device selected".into());
    ui.set_battery_text("".into());
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
    ui.set_validation_choice(gui_prefs.validation_choice);
    ui.set_baseline_choice_wired(gui_prefs.baseline_choice_wired);
    ui.set_baseline_choice_receiver(gui_prefs.baseline_choice_receiver);
    ui.set_allow_explicit_defaults(gui_prefs.allow_explicit_defaults);
    ui.set_ble_device(false);
    ui.set_verification_running(false);
    ui.set_last_enabled_profile(-1);

    let (commands, receiver) = mpsc::channel(32);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let weak = ui.as_weak();
    let worker = thread::Builder::new()
        .name("x3-gui-manager".into())
        .spawn(move || worker_main(receiver, shutdown_rx, weak))?;
    let _ = queue_command(&ui, &commands, Command::Startup);

    install_callbacks(
        &ui,
        commands.clone(),
        dpi_stages.clone(),
        bindings.clone(),
        profiles.clone(),
    );
    let run_result = ui.run();
    let _ = shutdown_tx.send(true);
    let _ = commands.try_send(Command::Shutdown);
    drop(commands);
    let worker_result = worker
        .join()
        .map_err(|_| std::io::Error::other("x3 manager worker terminated unexpectedly"))?;
    run_result?;
    worker_result.map_err(|_| std::io::Error::other("x3 manager worker failed"))?;
    Ok(())
}

fn install_callbacks(
    ui: &AppWindow,
    commands: mpsc::Sender<Command>,
    dpi_stages: std::rc::Rc<VecModel<DpiStage>>,
    bindings: std::rc::Rc<VecModel<BindingRow>>,
    profiles: std::rc::Rc<VecModel<ProfileRow>>,
) {
    {
        let weak = ui.as_weak();
        ui.on_navigate(move |page| {
            if let Some(ui) = weak.upgrade() {
                let page = page.clamp(0, 5);
                ui.set_current_page(page);
                ui.set_status_text(page_status(page).into());
                persist_gui_preferences(&ui);
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
                persist_gui_preferences(&ui);
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
            if row < 0 {
                return;
            }
            let Some(device) = ui.get_devices().row_data(row as usize) else {
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
            if selected < 0 {
                return;
            }
            let Some(row) = profiles.row_data(selected as usize) else {
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
                ui.set_status_text(
                    "save or discard the current draft before adding a profile".into(),
                );
                return;
            }
            if ui.get_ble_device() {
                ui.set_status_text(
                    "adding a profile is unavailable over BLE; connect by USB".into(),
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
                ui.set_status_text(
                    "save or discard the current draft before hiding a profile".into(),
                );
                return;
            }
            if ui.get_ble_device() {
                ui.set_status_text(
                    "hiding a profile is unavailable over BLE; connect by USB".into(),
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
            if row >= dpi_stages.row_count() {
                return;
            }
            let logarithmic = ui.get_dpi_log_scale();
            let min = ui.get_dpi_min();
            let max = ui.get_dpi_max();
            let dpi = dpi_from_ratio(ratio, logarithmic, min, max);
            let dpi = round_dpi_step(dpi) as f32;
            set_active_dpi_stage(&dpi_stages, row);
            if let Some(mut stage) = dpi_stages.row_data(row) {
                stage.active = true;
                stage.dpi = dpi;
                stage.value = format_dpi_setting(dpi).into();
                dpi_stages.set_row_data(row, stage);
            }
            update_single_dpi_ratio(&dpi_stages, row, logarithmic, min, max);
            ui.set_active_dpi(row as i32);
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
                persist_gui_preferences(&ui);
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
            persist_gui_preferences(&ui);
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
                    "fix the raw configuration value before changing deep sleep (the last valid value is kept)"
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
                    "fix the raw configuration value before changing deep sleep (the last valid value is kept)"
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
                        "raw preference settings shown; typed fields update from raw values"
                    } else {
                        "raw preference settings hidden"
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
                    "raw preference values must be one or two hex digits (for example 0a or 0x0a)"
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
            ui.set_status_text("raw preference value changed in the draft — save to apply".into());
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_set_baseline_choice_wired(move |choice| {
            if let Some(ui) = weak.upgrade() {
                let choice = choice.clamp(0, 1);
                ui.set_baseline_choice_wired(choice);
                if ui.get_ble_device() {
                    ui.set_status_text("BLE always starts from your last saved settings".into());
                } else if choice == 1 {
                    ui.set_status_text(
                        "next USB write will start from your last saved settings".into(),
                    );
                } else {
                    ui.set_status_text(
                        "next USB write will start from the mouse's current settings".into(),
                    );
                }
                persist_gui_preferences(&ui);
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_set_baseline_choice_receiver(move |choice| {
            if let Some(ui) = weak.upgrade() {
                let choice = choice.clamp(0, 1);
                ui.set_baseline_choice_receiver(choice);
                if choice == 1 {
                    ui.set_status_text(
                        "next dongle write will start from your last saved settings".into(),
                    );
                } else {
                    ui.set_status_text(
                        "next dongle write will start from the mouse's current settings".into(),
                    );
                }
                persist_gui_preferences(&ui);
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
                        "explicit defaults allowed when no saved settings exist"
                    } else {
                        "explicit defaults off; missing saved settings will fail safely"
                    }
                    .into(),
                );
                persist_gui_preferences(&ui);
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
            if let Some(message) = dirty_draft_message(ui.get_dirty(), "reading all profiles") {
                ui.set_status_text(message.into());
                return;
            }
            if ui.get_ble_device() {
                ui.set_status_text(
                    "reading all profiles needs USB; it's unavailable over BLE".into(),
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
            if let Some(message) = dirty_draft_message(ui.get_dirty(), "reading all profiles") {
                ui.set_status_text(message.into());
                return;
            }
            if ui.get_ble_device() {
                ui.set_status_text(
                    "reading all profiles needs USB; it's unavailable over BLE".into(),
                );
                return;
            }
            ui.set_busy(true);
            ui.set_lifecycle_text("reading all profiles".into());
            ui.set_status_text(
                "reading all five profiles; your current settings will be restored…".into(),
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
                dirty_draft_message(ui.get_dirty(), "clearing saved confirmation")
            {
                ui.set_status_text(message.into());
                return;
            }
            ui.set_busy(true);
            ui.set_lifecycle_text("clearing saved confirmation".into());
            ui.set_status_text("clearing saved confirmation; your settings are kept…".into());
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
                dirty_draft_message(ui.get_dirty(), "running the profile-reload check")
            {
                ui.set_status_text(message.into());
                return;
            }
            if ui.get_ble_device() {
                ui.set_status_text(
                    "the profile-reload check needs USB; unavailable over BLE".into(),
                );
                return;
            }
            ui.set_busy(true);
            ui.set_lifecycle_text("running the profile-reload check".into());
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
                dirty_draft_message(ui.get_dirty(), "running the power-cycle check")
            {
                ui.set_status_text(message.into());
                return;
            }
            if ui.get_ble_device() {
                ui.set_status_text(
                    "the power-cycle check needs USB; unavailable over BLE".into(),
                );
                return;
            }
            ui.set_busy(true);
            ui.set_lifecycle_text("running the power-cycle check".into());
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
        let commands = commands.clone();
        ui.on_request_add_mouse(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_busy() || ui.get_ceremony_active() {
                if ui.get_ceremony_active() {
                    ui.set_status_text(
                        "finish or cancel the current setup before starting another one".into(),
                    );
                }
                return;
            }
            if let Some(message) = dirty_draft_message(ui.get_dirty(), "setting up another mouse") {
                ui.set_status_text(message.into());
                return;
            }
            ui.set_status_text("starting setup…".into());
            queue_command(
                &ui,
                &commands,
                Command::BeginCeremony {
                    kind: IdentityCeremonyKind::AddMouse,
                    target: None,
                    subject: None,
                },
            );
        });
    }
    {
        let weak = ui.as_weak();
        let commands = commands.clone();
        ui.on_unassociated_action(move |row, action| {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_busy() || ui.get_ceremony_active() {
                ui.set_status_text(
                    "finish or cancel the current setup before starting another one".into(),
                );
                return;
            }
            if let Some(message) = dirty_draft_message(ui.get_dirty(), "setting up another mouse") {
                ui.set_status_text(message.into());
                return;
            }
            if row < 0 {
                return;
            }
            match action {
                UNASSOCIATED_ADD => {
                    ui.set_status_text("starting setup…".into());
                    queue_command(
                        &ui,
                        &commands,
                        Command::BeginCeremony {
                            kind: IdentityCeremonyKind::AddMouse,
                            target: None,
                            subject: Some(row as usize),
                        },
                    );
                }
                UNASSOCIATED_ADOPT => {
                    ui.set_status_text("starting adoption…".into());
                    queue_command(
                        &ui,
                        &commands,
                        Command::BeginCeremony {
                            kind: IdentityCeremonyKind::ForeignAdoption,
                            target: None,
                            subject: Some(row as usize),
                        },
                    );
                }
                UNASSOCIATED_RESTORE => {
                    ui.set_pending_unassociated_row(row);
                    ui.invoke_show_restore_picker();
                }
                UNASSOCIATED_ASSOCIATE => {
                    ui.set_pending_unassociated_row(row);
                    ui.invoke_show_associate_picker();
                }
                _ => {
                    ui.set_status_text("that action isn't available".into());
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let commands = commands.clone();
        ui.on_begin_restore(move |target| {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_busy() || ui.get_ceremony_active() {
                return;
            }
            let row = ui.get_pending_unassociated_row();
            if row < 0 {
                ui.set_status_text("choose the unrecognized mouse first".into());
                return;
            }
            let target = match DeviceId::new(target.as_str()) {
                Ok(id) => id,
                Err(_) => {
                    ui.set_status_text("that saved mouse isn't valid".into());
                    return;
                }
            };
            ui.set_status_text("starting restore…".into());
            queue_command(
                &ui,
                &commands,
                Command::BeginCeremony {
                    kind: IdentityCeremonyKind::Restore,
                    target: Some(target),
                    subject: Some(row as usize),
                },
            );
        });
    }
    {
        let weak = ui.as_weak();
        let commands = commands.clone();
        ui.on_begin_associate(move |target| {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_busy() || ui.get_ceremony_active() {
                return;
            }
            let row = ui.get_pending_unassociated_row();
            if row < 0 {
                ui.set_status_text("choose the bluetooth mouse first".into());
                return;
            }
            let target = match DeviceId::new(target.as_str()) {
                Ok(id) => id,
                Err(_) => {
                    ui.set_status_text("that saved mouse isn't valid".into());
                    return;
                }
            };
            ui.set_status_text("starting pairing…".into());
            queue_command(
                &ui,
                &commands,
                Command::BeginCeremony {
                    kind: IdentityCeremonyKind::BleAssociation,
                    target: Some(target),
                    subject: Some(row as usize),
                },
            );
        });
    }
    {
        let weak = ui.as_weak();
        let commands = commands.clone();
        ui.on_ceremony_primary(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_busy() {
                return;
            }
            match ui.get_ceremony_prompt() {
                // Reconnect
                1 => {
                    queue_command(
                        &ui,
                        &commands,
                        Command::CeremonyAction {
                            action: IdentityCeremonyAction::Reconnected,
                            endpoint: None,
                            target: None,
                        },
                    );
                }
                // Stamp
                2 => {
                    queue_command(
                        &ui,
                        &commands,
                        Command::CeremonyAction {
                            action: IdentityCeremonyAction::Stamp,
                            endpoint: None,
                            target: None,
                        },
                    );
                }
                // Adopt
                3 => {
                    queue_command(
                        &ui,
                        &commands,
                        Command::CeremonyAction {
                            action: IdentityCeremonyAction::Adopt,
                            endpoint: None,
                            target: None,
                        },
                    );
                }
                // Associate
                4 => {
                    queue_command(
                        &ui,
                        &commands,
                        Command::CeremonyAction {
                            action: IdentityCeremonyAction::Associate,
                            endpoint: None,
                            target: None,
                        },
                    );
                }
                // Done / None: close the ceremony surface.
                _ => {
                    ui.invoke_close_ceremony();
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let commands = commands.clone();
        ui.on_ceremony_cancel(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_busy() {
                return;
            }
            queue_command(
                &ui,
                &commands,
                Command::CeremonyAction {
                    action: IdentityCeremonyAction::Cancel,
                    endpoint: None,
                    target: None,
                },
            );
        });
    }
    {
        let weak = ui.as_weak();
        let commands = commands.clone();
        ui.on_ceremony_migration_changed(move |accept| {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_busy() {
                return;
            }
            queue_command(
                &ui,
                &commands,
                Command::CeremonyAction {
                    action: if accept {
                        IdentityCeremonyAction::AcceptMigration
                    } else {
                        IdentityCeremonyAction::SkipMigration
                    },
                    endpoint: None,
                    target: None,
                },
            );
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
                ui.set_status_text("that button action is not supported".into());
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
        ui.on_select_appearance(move |choice| {
            if let Some(ui) = weak.upgrade() {
                let choice = choice.clamp(0, 2);
                let theme = ui.global::<Theme>();
                let appearance = match choice {
                    1 => slint::language::ColorScheme::Light,
                    2 => slint::language::ColorScheme::Dark,
                    _ => slint::language::ColorScheme::Unknown,
                };
                theme.set_appearance(appearance);
                ui.set_status_text("theme updated".into());
                persist_gui_preferences(&ui);
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
                persist_gui_preferences(&ui);
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
                    persist_gui_preferences(&ui);
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
                        "BLE saves confirm delivery; the mouse can't read the setting back".into(),
                    );
                } else {
                    ui.set_status_text(if choice == 1 {
                        "next USB save will be confirmed by the mouse".into()
                    } else {
                        "next USB save will confirm delivery only".into()
                    });
                }
                persist_gui_preferences(&ui);
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
                // No frontend polling isolation: the manager's single
                // `apply_profile_update` owns transport safety and the
                // polling-rate isolation check. The draft just records the
                // choice; any mixed-rate or BLE error surfaces on Save.
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
                            "raw {} value is not valid hex (one or two digits); the last valid value is kept — fix it before saving",
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
                baseline: {
                    if ui.get_ble_device() {
                        BaselineChoice::Stored
                    } else if ui.get_transport_label().as_str().contains("receiver") {
                        if ui.get_baseline_choice_receiver() == 1 {
                            BaselineChoice::Stored
                        } else {
                            BaselineChoice::Live
                        }
                    } else if ui.get_baseline_choice_wired() == 1 {
                        BaselineChoice::Stored
                    } else {
                        BaselineChoice::Live
                    }
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
                queue_command(&ui, &commands, Command::Refresh);
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
        ui.on_close_app(move || {
            let _ = slint::quit_event_loop();
        });
    }
}

fn is_coalescable(command: &Command) -> bool {
    matches!(command, Command::Startup | Command::Refresh)
}

fn queue_command(ui: &AppWindow, commands: &mpsc::Sender<Command>, command: Command) -> bool {
    let coalescable = is_coalescable(&command);
    match commands.try_send(command) {
        Ok(()) => true,
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) if coalescable => {
            ui.set_status_text("refresh already queued — coalesced".into());
            true
        }
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            ui.set_status_text("command queue is full; try again shortly".into());
            false
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            ui.set_busy(false);
            ui.set_hardware_ready(false);
            ui.set_status_text("manager worker is unavailable; restart the application".into());
            false
        }
    }
}

fn current_gui_preferences(ui: &AppWindow) -> app_settings::GuiPreferences {
    let appearance = match ui.global::<Theme>().get_appearance() {
        slint::language::ColorScheme::Light => 1,
        slint::language::ColorScheme::Dark => 2,
        _ => 0,
    };
    app_settings::GuiPreferences {
        schema_version: app_settings::GUI_PREFERENCES_SCHEMA_VERSION,
        appearance,
        product_name_choice: ui.get_product_name_choice(),
        custom_product_name: ui.get_custom_product_name().to_string(),
        dpi_min: ui.get_dpi_min(),
        dpi_max: ui.get_dpi_max(),
        dpi_log_scale: ui.get_dpi_log_scale(),
        last_page: ui.get_current_page(),
        validation_choice: ui.get_validation_choice(),
        baseline_choice_wired: ui.get_baseline_choice_wired(),
        baseline_choice_receiver: ui.get_baseline_choice_receiver(),
        allow_explicit_defaults: ui.get_allow_explicit_defaults(),
    }
}

fn persist_gui_preferences(ui: &AppWindow) {
    let prefs = current_gui_preferences(ui).normalized();
    // Coalesced atomic write; errors are best-effort for GUI prefs (no state lock).
    let _ = app_settings::save_gui_preferences(&prefs);
}
