use slint::{Model, ModelRc, VecModel};

use crate::presentation::{CeremonyPrompt, DPI_LABELS, clamp_dpi_value, dpi_bounds};
use crate::worker::{LiveProfile, LiveSnapshot, UnassociatedRow as WorkerUnassociatedRow};
use crate::{AppWindow, BindingRow, DeviceRow, DpiStage, ProfileRow, UnassociatedRow};

pub fn polling_rate_text(snapshot: &LiveSnapshot) -> String {
    if snapshot.is_ble {
        "not available over BLE".to_owned()
    } else if snapshot.polling_rate_ready {
        format!("{} hz", snapshot.polling_rate_hz)
    } else {
        format!("{} hz · rate write unavailable", snapshot.polling_rate_hz)
    }
}

/// Resets every live-readback field shown by the UI, used when a worker
/// operation fails and no device state can be displayed. The discovered
/// device list is left untouched so the user can still switch devices.
pub fn clear_live_state(ui: &AppWindow) {
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
    ui.set_baseline_choice_wired(0);
    ui.set_baseline_choice_receiver(1);
    ui.set_allow_explicit_defaults(false);
    ui.set_all_profiles_observed(false);
    ui.set_verification_running(false);
    ui.set_verification_instruction("".into());
    ui.set_last_enabled_profile(-1);
    ui.set_battery_text("".into());
    ui.set_device_name("Attack Shark X3".into());
    ui.set_device_subtitle("no device selected".into());
    ui.set_header_editing_name(false);
    ui.set_device_id_text("not available".into());
    ui.set_device_product_text("not available".into());
    ui.set_profile_summary("no live profile".into());
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
    refresh_lmb_break_warning(ui);
}

pub fn replace_dpi_model(
    model: &ModelRc<DpiStage>,
    stages: &[(u16, bool)],
    logarithmic: bool,
    min: f32,
    max: f32,
) {
    let Some(model) = model.as_any().downcast_ref::<VecModel<DpiStage>>() else {
        return;
    };
    let built: Vec<DpiStage> = stages
        .iter()
        .copied()
        .enumerate()
        .map(|(index, (value, active))| {
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
            stage
        })
        .collect();
    model.set_vec(built);
}

pub fn replace_binding_model(model: &ModelRc<BindingRow>, bindings: &[(String, String, String)]) {
    let Some(model) = model.as_any().downcast_ref::<VecModel<BindingRow>>() else {
        return;
    };
    let built: Vec<BindingRow> = bindings
        .iter()
        .map(|(button, location, action)| binding(button, location, action))
        .collect();
    model.set_vec(built);
}

/// Recomputes the LMB safety state from the live bindings model:
/// `lmb-break-warning`: 0 = clicking intact, 1 = LMB remapped but click
/// lives elsewhere, 2 = nothing left-clicks (edge-triggered: only when the
/// loaded baseline left-clicked). `no-left-click-now`: true whenever nothing
/// left-clicks right now, even for deliberate setups (drives the banner).
/// Both forced off while warnings are disabled.
pub fn refresh_lmb_break_warning(ui: &AppWindow) {
    use slint::Model;
    let model = ui.get_bindings();
    let Some(model) = model.as_any().downcast_ref::<VecModel<BindingRow>>() else {
        return;
    };
    if !ui.get_warn_no_left_click() {
        ui.set_lmb_break_warning(0);
        ui.set_no_left_click_now(false);
        return;
    }
    let mut lmb_original = String::from("left click");
    let mut lmb_draft = String::from("left click");
    let mut owned: Vec<(String, String)> = Vec::new();
    for row in 0..model.row_count() {
        let Some(item) = model.row_data(row) else {
            continue;
        };
        if item.button.as_str() == "lmb" {
            lmb_original = item.original_action.to_string();
            lmb_draft = item.action.to_string();
        }
        owned.push((item.button.to_string(), item.action.to_string()));
    }
    let pairs: Vec<(&str, &str)> = owned
        .iter()
        .map(|(button, action)| (button.as_str(), action.as_str()))
        .collect();
    let warning = crate::presentation::lmb_break_warning(&lmb_original, &lmb_draft, &pairs);
    ui.set_lmb_break_warning(match warning {
        None => 0,
        Some(crate::presentation::LmbBreak::Remapped) => 1,
        Some(crate::presentation::LmbBreak::NothingClicks) => 2,
    });
    ui.set_lmb_break_action(lmb_draft.into());
    let clicking_exists = owned.iter().any(|(_, action)| action == "left click");
    ui.set_no_left_click_now(!clicking_exists && !owned.is_empty());
}

pub fn replace_profile_model(model: &ModelRc<ProfileRow>, profiles: &[LiveProfile], is_ble: bool) {
    let Some(model) = model.as_any().downcast_ref::<VecModel<ProfileRow>>() else {
        return;
    };
    let built: Vec<ProfileRow> = profiles
        .iter()
        .map(|profile| {
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
            ProfileRow {
                name: profile.name.clone().into(),
                subtitle: subtitle.into(),
                active: profile.current,
                visible: true,
                enabled: profile.enabled,
            }
        })
        .collect();
    model.set_vec(built);
}

/// Laconic relative time for candidate last-seen stamps (`2h ago`).
fn relative_seen(seen_unix_seconds: i64) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let delta = (now - seen_unix_seconds).max(0);
    if delta < 60 {
        "just now".to_owned()
    } else if delta < 3_600 {
        format!("{}m ago", delta / 60)
    } else if delta < 86_400 {
        format!("{}h ago", delta / 3_600)
    } else {
        format!("{}d ago", delta / 86_400)
    }
}

pub fn apply_event(ui: &AppWindow, event: crate::worker::UiEvent) {
    use crate::worker::UiEvent;
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
        UiEvent::Unassociated(rows) => {
            let unassociated = ui.get_unassociated();
            let Some(model) = unassociated
                .as_any()
                .downcast_ref::<VecModel<UnassociatedRow>>()
            else {
                return;
            };
            let built: Vec<UnassociatedRow> = rows
                .into_iter()
                .map(|entry: WorkerUnassociatedRow| UnassociatedRow {
                    name: entry.name.into(),
                    detail: entry.detail.into(),
                    can_add: entry.can_add,
                    can_restore: entry.can_restore,
                    can_adopt: entry.can_adopt,
                    can_associate: entry.can_associate,
                })
                .collect();
            model.set_vec(built);
        }
        UiEvent::AskPerEndpoint(rows) => {
            ui.set_busy(false);
            let Some(first) = rows.first() else {
                return;
            };
            ui.set_pending_ask_kind(match first.kind {
                crate::worker::PendingKind::NewOrMine => 2,
                crate::worker::PendingKind::Which => 1,
            });
            ui.set_pending_endpoint_label(
                format!("{} · {}", first.transport_label, first.endpoint_label).into(),
            );
            let names: Vec<slint::SharedString> = first
                .candidates
                .iter()
                .map(|entry| {
                    let name = entry.display_name.as_deref().unwrap_or("saved mouse");
                    match (
                        entry.last_transport.as_deref(),
                        entry.last_seen_unix_seconds,
                    ) {
                        (Some(transport), Some(seen)) => {
                            format!("{name} — last on {transport} {}", relative_seen(seen)).into()
                        }
                        _ => name.into(),
                    }
                })
                .collect();
            ui.set_pending_candidates(slint::ModelRc::from(std::rc::Rc::new(
                slint::VecModel::from(names),
            )));
            ui.set_status_text("which mouse is this?".into());
            ui.invoke_show_pending_ask();
        }
        UiEvent::DemoMode { entering, scenario } => {
            ui.set_demo_active(entering);
            if entering {
                ui.set_sim_scenario(scenario.clone().into());
                // A re-entry after dismiss must re-raise the strip: each
                // demo session starts undisguised, collapsing only by click.
                ui.set_demo_banner_dismissed(false);
                ui.set_status_text(
                    "no compatible mouse found — showing a simulated mouse; connect yours".into(),
                );
            } else {
                ui.set_sim_scenario("".into());
            }
        }
        UiEvent::AutoAttached { keep_stored, .. } => {
            ui.set_busy(false);
            ui.set_status_text(
                if keep_stored {
                    "kept your saved settings"
                } else {
                    "took the mouse's settings"
                }
                .into(),
            );
        }
        UiEvent::CeremonyProgress(view) => {
            ui.set_ceremony_active(true);
            ui.set_ceremony_title(view.title.clone().into());
            ui.set_ceremony_instruction(view.instruction.clone().into());
            ui.set_ceremony_detail(view.detail.clone().into());
            ui.set_ceremony_prompt(match view.prompt {
                CeremonyPrompt::None => 0,
                CeremonyPrompt::Reconnect => 1,
                CeremonyPrompt::Stamp => 2,
                CeremonyPrompt::Adopt => 3,
                CeremonyPrompt::Associate => 4,
                CeremonyPrompt::Done => 5,
            });
            ui.set_ceremony_prompt_label(view.prompt_label.clone().into());
            ui.set_ceremony_can_cancel(view.can_cancel);
            ui.set_ceremony_migration(view.migration_offered);
            ui.set_ceremony_error(view.failed.clone().unwrap_or_default().into());
            ui.invoke_show_ceremony();
            ui.set_busy(false);
            ui.set_status_text(view.instruction.clone().into());
        }
        UiEvent::CeremonyError(error) => {
            ui.set_busy(false);
            if ui.get_ceremony_active() {
                ui.set_ceremony_error(error.into());
            } else {
                ui.set_status_text(error.into());
            }
        }
        UiEvent::Devices(entries) => {
            let devices = ui.get_devices();
            let Some(model) = devices.as_any().downcast_ref::<VecModel<DeviceRow>>() else {
                return;
            };
            let built: Vec<DeviceRow> = entries
                .into_iter()
                .map(|entry| DeviceRow {
                    id: entry.id.into(),
                    name: entry.name.into(),
                    detail: entry.detail.into(),
                    selected: entry.selected,
                    connected: entry.connected,
                })
                .collect();
            model.set_vec(built);
        }
        UiEvent::Snapshot(snapshot) => apply_snapshot(ui, &snapshot, false),
        UiEvent::EventSnapshot(snapshot) => apply_snapshot(ui, &snapshot, true),
        UiEvent::BatteryChanged(level) => {
            ui.set_battery_text(format!("{level}%").into());
        }
    }
}

/// Applies a snapshot to the UI. Event-driven snapshots preserve an open
/// draft (see `snapshot_preserves_draft`); explicit operation/load snapshots
/// replace every model and clear the draft.
pub fn apply_snapshot(ui: &AppWindow, snapshot: &LiveSnapshot, event_driven: bool) {
    // Use presentation helper for draft preservation logic
    if crate::presentation::snapshot_preserves_draft(event_driven, ui.get_dirty()) {
        apply_snapshot_preserving_draft(ui, snapshot);
    } else {
        apply_snapshot_replacing(ui, snapshot);
    }
}

/// Applies an explicit operation/load snapshot: every live field and the
/// editable models are replaced from the device state and the draft is
/// cleared.
pub fn apply_snapshot_replacing(ui: &AppWindow, snapshot: &LiveSnapshot) {
    ui.set_busy(false);
    ui.set_hardware_ready(true);
    ui.set_lifecycle_text(
        if snapshot.is_ble {
            "connected — your saved settings (not read from the mouse)"
        } else {
            "connected — settings read from the mouse"
        }
        .into(),
    );
    ui.set_status_text(snapshot.status.clone().into());
    ui.set_transport_label(snapshot.transport.clone().into());
    ui.set_battery_text(snapshot.battery.clone().into());
    ui.set_device_name(snapshot.device_name.clone().into());
    ui.set_device_subtitle(format!("{} · {}", snapshot.stable_id, snapshot.transport).into());
    ui.set_device_id_text(snapshot.stable_id.clone().into());
    ui.set_device_product_text(snapshot.product_id.clone().into());
    ui.set_white_shell(snapshot.shell == attack_shark_x3_manager::ShellColor::White);
    ui.set_profile_summary(snapshot.profile_summary.clone().into());
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
        ui.set_baseline_choice_wired(1);
        ui.set_baseline_choice_receiver(1);
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
    refresh_lmb_break_warning(ui);
}

/// Applies an event-driven snapshot while a draft is open: the editable
/// models and every draft property are preserved, only safe readback
/// telemetry and status are refreshed, and the user is warned that the
/// external change may make the next Save fail until discard/reload.
pub fn apply_snapshot_preserving_draft(ui: &AppWindow, snapshot: &LiveSnapshot) {
    ui.set_busy(false);
    ui.set_hardware_ready(true);
    ui.set_lifecycle_text(
        if snapshot.is_ble {
            "connected — your saved settings (not read from the mouse)"
        } else {
            "connected — settings read from the mouse"
        }
        .into(),
    );
    ui.set_status_text(crate::presentation::event_snapshot_draft_warning(&snapshot.status).into());
    ui.set_transport_label(snapshot.transport.clone().into());
    ui.set_battery_text(snapshot.battery.clone().into());
    ui.set_device_name(snapshot.device_name.clone().into());
    ui.set_device_subtitle(format!("{} · {}", snapshot.stable_id, snapshot.transport).into());
    ui.set_device_id_text(snapshot.stable_id.clone().into());
    ui.set_device_product_text(snapshot.product_id.clone().into());
    ui.set_white_shell(snapshot.shell == attack_shark_x3_manager::ShellColor::White);
    ui.set_profile_summary(snapshot.profile_summary.clone().into());
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
}

pub fn dpi_from_ratio(ratio: f32, logarithmic: bool, min: f32, max: f32) -> f32 {
    let (min, max) = dpi_bounds(min, max);
    let ratio = ratio.clamp(0.0, 1.0);
    let raw = if logarithmic {
        min * (max / min).powf(ratio)
    } else {
        min + (max - min) * ratio
    };
    clamp_dpi_value(raw, min, max)
}
pub fn dpi_ratio(dpi: f32, logarithmic: bool, min: f32, max: f32) -> f32 {
    let (min, max) = dpi_bounds(min, max);
    let dpi = clamp_dpi_value(dpi, min, max);
    let ratio = if logarithmic {
        (dpi / min).ln() / (max / min).ln()
    } else {
        (dpi - min) / (max - min)
    };
    ratio.clamp(0.0, 1.0)
}
pub fn refresh_dpi_ratios(stages: &VecModel<DpiStage>, logarithmic: bool, min: f32, max: f32) {
    for row in 0..stages.row_count() {
        if let Some(mut stage) = stages.row_data(row) {
            stage.ratio = dpi_ratio(stage.dpi, logarithmic, min, max);
            stages.set_row_data(row, stage);
        }
    }
}

pub fn update_single_dpi_ratio(
    stages: &VecModel<DpiStage>,
    row: usize,
    logarithmic: bool,
    min: f32,
    max: f32,
) {
    if let Some(mut stage) = stages.row_data(row) {
        stage.ratio = dpi_ratio(stage.dpi, logarithmic, min, max);
        stages.set_row_data(row, stage);
    }
}
pub fn dpi_stage(
    index: &str,
    value: &str,
    label: &str,
    red: u8,
    green: u8,
    blue: u8,
    active: bool,
) -> DpiStage {
    use crate::presentation::DPI_MIN;
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
pub fn binding(button: &str, location: &str, action: &str) -> BindingRow {
    BindingRow {
        button: button.into(),
        location: location.into(),
        action: action.into(),
        original_action: action.into(),
        editable: true,
    }
}

pub fn default_dpi_stage(index: usize, active: bool) -> DpiStage {
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
        crate::presentation::DPI_VALUES_APPEND[index],
        DPI_LABELS[index],
        red,
        green,
        blue,
        active,
    )
}

/// Appends one draft stage (never active), returning false when the ladder is
/// already at the device maximum of eight stages.
pub fn append_dpi_stage(stages: &VecModel<DpiStage>) -> bool {
    use crate::presentation::MAX_DPI_STAGES;
    if stages.row_count() >= MAX_DPI_STAGES {
        return false;
    }
    stages.push(default_dpi_stage(stages.row_count(), false));
    true
}

pub fn set_active_dpi_stage(stages: &VecModel<DpiStage>, selected: usize) {
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
pub fn remove_dpi_stage(stages: &VecModel<DpiStage>, row: usize, active: usize) -> Option<usize> {
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

pub fn renumber_dpi_stages(stages: &VecModel<DpiStage>) {
    for row in 0..stages.row_count() {
        if let Some(mut stage) = stages.row_data(row) {
            stage.index = format!("{:02}", row + 1).into();
            stages.set_row_data(row, stage);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use attack_shark_x3_manager::ProfileId;
    use slint::VecModel;

    use crate::worker::preference_fields;
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
        use crate::presentation::MAX_DPI_STAGES;
        let stages = VecModel::from(Vec::<DpiStage>::new());
        for expected in 1..=MAX_DPI_STAGES {
            assert!(append_dpi_stage(&stages));
            assert_eq!(stages.row_count(), expected);
        }
        assert!(!append_dpi_stage(&stages));
        assert_eq!(stages.row_count(), MAX_DPI_STAGES);
    }

    #[test]
    fn dpi_slider_snaps_to_fifty_and_clamps_to_default_display_range() {
        use crate::presentation::{DEFAULT_DPI_DISPLAY_MAX, DEFAULT_DPI_DISPLAY_MIN};
        let min = DEFAULT_DPI_DISPLAY_MIN;
        let max = DEFAULT_DPI_DISPLAY_MAX;
        for logarithmic in [false, true] {
            let low = dpi_from_ratio(-1.0, logarithmic, min, max);
            let middle = dpi_from_ratio(0.5, logarithmic, min, max);
            let high = dpi_from_ratio(2.0, logarithmic, min, max);
            assert_eq!(low, min);
            assert_eq!(high, max);
            assert_eq!(middle as u32 % crate::presentation::DPI_STEP as u32, 0);
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
    fn logarithmic_slider_spends_more_travel_on_low_dpi_values() {
        let linear = dpi_from_ratio(0.5, false, 200.0, 3_200.0);
        let logarithmic = dpi_from_ratio(0.5, true, 200.0, 3_200.0);
        assert!(logarithmic > 200.0);
        assert!(logarithmic < linear);
    }

    #[test]
    fn loaded_bindings_baseline_original_action_to_the_loaded_action() {
        let row = binding("lmb", "primary", "left click");
        assert_eq!(row.action.as_str(), "left click");
        assert_eq!(row.original_action.as_str(), "left click");
        assert_eq!(row.action, row.original_action);
    }

    #[test]
    fn stock_reset_preferences_decode_through_the_typed_helpers() {
        let preferences = attack_shark_x3_manager::PreferencesState::captured_stock_reset(
            ProfileId::new(1).expect("1"),
        );
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
        let preferences = attack_shark_x3_manager::PreferencesState::new(
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
        let preferences = attack_shark_x3_manager::PreferencesState::new(
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
    fn clear_model_is_linear_single_reset() {
        let model = VecModel::from(vec![
            binding("a", "loc", "act"),
            binding("b", "loc", "act2"),
        ]);
        assert_eq!(model.row_count(), 2);
        model.set_vec(Vec::new());
        assert_eq!(model.row_count(), 0);
        model.set_vec(Vec::new());
        assert_eq!(model.row_count(), 0);
    }

    #[test]
    fn replace_dpi_model_batches_via_set_vec() {
        use slint::{Model, ModelRc};
        let backing = std::rc::Rc::new(VecModel::default());
        let rc: ModelRc<DpiStage> = ModelRc::from(backing.clone());
        replace_dpi_model(&rc, &[(400, true), (800, false)], false, 200.0, 3200.0);
        assert_eq!(backing.row_count(), 2);
        assert_eq!(backing.row_data(0).unwrap().value.as_str(), "400");
        assert!(backing.row_data(0).unwrap().active);
        assert!(!backing.row_data(1).unwrap().active);
        assert_eq!(backing.row_data(1).unwrap().index.as_str(), "02");
        // Replacing with a different size truncates via single reset, not repeated remove(0).
        replace_dpi_model(&rc, &[(1200, false)], true, 200.0, 3200.0);
        assert_eq!(backing.row_count(), 1);
        assert_eq!(backing.row_data(0).unwrap().value.as_str(), "1200");
        // Empty replacement clears to zero rows linearly.
        replace_dpi_model(&rc, &[], false, 200.0, 3200.0);
        assert_eq!(backing.row_count(), 0);
    }

    #[test]
    fn replace_binding_model_batches_linearly() {
        use slint::{Model, ModelRc};
        let backing = std::rc::Rc::new(VecModel::default());
        let rc: ModelRc<BindingRow> = ModelRc::from(backing.clone());
        replace_binding_model(
            &rc,
            &[
                (
                    "lmb".to_string(),
                    "primary".to_string(),
                    "left click".to_string(),
                ),
                (
                    "rmb".to_string(),
                    "secondary".to_string(),
                    "right click".to_string(),
                ),
            ],
        );
        assert_eq!(backing.row_count(), 2);
        assert_eq!(backing.row_data(0).unwrap().button.as_str(), "lmb");
        replace_binding_model(&rc, &[]);
        assert_eq!(backing.row_count(), 0);
    }

    #[test]
    fn replace_profile_model_batches_and_maps_subtitles() {
        use crate::worker::LiveProfile;
        use slint::{Model, ModelRc};
        let backing = std::rc::Rc::new(VecModel::default());
        let rc: ModelRc<ProfileRow> = ModelRc::from(backing.clone());
        let profiles = vec![
            LiveProfile {
                name: "p1".into(),
                current: true,
                enabled: true,
            },
            LiveProfile {
                name: "p2".into(),
                current: false,
                enabled: false,
            },
        ];
        replace_profile_model(&rc, &profiles, false);
        assert_eq!(backing.row_count(), 2);
        assert_eq!(backing.row_data(0).unwrap().subtitle.as_str(), "available");
        assert_eq!(backing.row_data(1).unwrap().subtitle.as_str(), "hidden");
        replace_profile_model(&rc, &profiles, true);
        assert_eq!(
            backing.row_data(0).unwrap().subtitle.as_str(),
            "saved settings"
        );
        assert_eq!(backing.row_data(1).unwrap().subtitle.as_str(), "hidden");
        replace_profile_model(&rc, &[], false);
        assert_eq!(backing.row_count(), 0);
    }

    #[test]
    fn single_row_ratio_update_leaves_other_ratios_untouched() {
        let stages = VecModel::from(
            vec![
                dpi_stage("01", "400", "low", 96, 98, 104, true),
                dpi_stage("02", "800", "mid", 112, 114, 120, false),
                dpi_stage("03", "1600", "high", 128, 130, 136, false),
            ]
            .into_iter()
            .map(|mut s| {
                s.ratio = dpi_ratio(s.dpi, false, 200.0, 3200.0);
                s
            })
            .collect::<Vec<_>>(),
        );
        let before: Vec<f32> = (0..stages.row_count())
            .map(|r| stages.row_data(r).unwrap().ratio)
            .collect();
        // Change only row 1's DPI and recompute its ratio alone.
        if let Some(mut stage) = stages.row_data(1) {
            stage.dpi = 1200.0;
            stage.value = "1200".into();
            stages.set_row_data(1, stage);
        }
        update_single_dpi_ratio(&stages, 1, false, 200.0, 3200.0);
        let after: Vec<f32> = (0..stages.row_count())
            .map(|r| stages.row_data(r).unwrap().ratio)
            .collect();
        assert!((before[0] - after[0]).abs() < f32::EPSILON);
        assert!((before[2] - after[2]).abs() < f32::EPSILON);
        assert!((after[1] - dpi_ratio(1200.0, false, 200.0, 3200.0)).abs() < 0.0001);
        assert_ne!(before[1], after[1]);
    }
}
