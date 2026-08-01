use std::{rc::Rc, time::Duration};

use slint::{ComponentHandle, Model, ModelRc, Timer, VecModel};

slint::include_modules!();

const BINDING_ACTIONS: [&str; 7] = [
    "left click",
    "right click",
    "middle click",
    "dpi cycle",
    "profile cycle",
    "forward",
    "backward",
];

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;

    let dpi_stages = Rc::new(VecModel::from(vec![
        dpi_stage("01", "800", "work", 96, 98, 104, true),
        dpi_stage("02", "1600", "everyday", 112, 114, 120, false),
        dpi_stage("03", "2400", "fast", 128, 130, 136, false),
        dpi_stage("04", "3200", "precision", 144, 146, 152, false),
        dpi_stage("05", "5000", "high", 96, 98, 104, false),
        dpi_stage("06", "26000", "maximum", 72, 74, 80, false),
    ]));
    ui.set_dpi_stages(ModelRc::from(dpi_stages.clone()));

    let bindings = Rc::new(VecModel::from(vec![
        binding("lmb", "primary", "left click"),
        binding("rmb", "secondary", "right click"),
        binding("wheel", "middle", "middle click"),
        binding("dpi", "top button", "dpi cycle"),
        binding("forward", "side upper", "forward"),
        binding("back", "side lower", "profile cycle"),
    ]));
    ui.set_bindings(ModelRc::from(bindings.clone()));

    let profiles = Rc::new(VecModel::from(vec![
        profile("office", "balanced", true),
        profile("competitive", "low latency", false),
        profile("editing", "precision", false),
    ]));
    ui.set_profiles(ModelRc::from(profiles.clone()));

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
        let profiles = profiles.clone();
        ui.on_select_profile(move |selected| {
            for row in 0..profiles.row_count() {
                if let Some(mut profile) = profiles.row_data(row) {
                    profile.active = row == selected as usize;
                    profiles.set_row_data(row, profile);
                }
            }
            if let Some(ui) = weak.upgrade() {
                ui.set_selected_profile(selected);
                ui.set_status_text(format!("profile {} selected in preview", selected + 1).into());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let dpi_stages = dpi_stages.clone();
        ui.on_select_dpi(move |selected| {
            for row in 0..dpi_stages.row_count() {
                if let Some(mut stage) = dpi_stages.row_data(row) {
                    stage.active = row == selected as usize;
                    dpi_stages.set_row_data(row, stage);
                }
            }
            if let Some(ui) = weak.upgrade() {
                ui.set_active_dpi(selected);
                ui.set_dirty(true);
                ui.set_status_text("dpi draft changed — save to apply".into());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let bindings = bindings.clone();
        ui.on_cycle_binding(move |row| {
            let row = row as usize;
            let Some(mut item) = bindings.row_data(row) else {
                return;
            };
            let current = BINDING_ACTIONS
                .iter()
                .position(|action| *action == item.action.as_str())
                .unwrap_or(0);
            item.action = BINDING_ACTIONS[(current + 1) % BINDING_ACTIONS.len()].into();
            bindings.set_row_data(row, item);
            if let Some(ui) = weak.upgrade() {
                ui.set_dirty(true);
                ui.set_status_text("button draft changed — save to apply".into());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let profiles = profiles.clone();
        ui.on_add_profile(move || {
            if profiles.row_count() >= 5 {
                if let Some(ui) = weak.upgrade() {
                    ui.set_status_text("all five device profile slots are already in use".into());
                }
                return;
            }
            let number = profiles.row_count() + 1;
            profiles.push(profile(&format!("profile {number}"), "new profile", false));
            if let Some(ui) = weak.upgrade() {
                ui.set_dirty(true);
                ui.set_status_text(format!("profile {number} added to the draft").into());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let profiles = profiles.clone();
        ui.on_rename_profile(move |row, name| {
            let row = row as usize;
            let name = name.trim();
            if name.is_empty() {
                if let Some(ui) = weak.upgrade() {
                    ui.set_status_text("profile names cannot be empty".into());
                }
                return;
            }
            if let Some(mut profile) = profiles.row_data(row) {
                profile.name = name.into();
                profiles.set_row_data(row, profile);
                if let Some(ui) = weak.upgrade() {
                    ui.set_dirty(true);
                    ui.set_status_text("profile renamed in the draft".into());
                }
            }
        });
    }

    {
        let weak = ui.as_weak();
        let profiles = profiles.clone();
        ui.on_hide_profile(move |row| {
            let row = row as usize;
            let visible_count = profiles
                .iter()
                .filter(|profile| profile.visible)
                .count();
            if visible_count <= 1 {
                if let Some(ui) = weak.upgrade() {
                    ui.set_status_text("at least one profile must remain visible".into());
                }
                return;
            }
            let Some(mut hidden) = profiles.row_data(row) else {
                return;
            };
            hidden.visible = false;
            let was_active = hidden.active;
            hidden.active = false;
            profiles.set_row_data(row, hidden);

            let mut replacement = None;
            if was_active {
                for index in 0..profiles.row_count() {
                    if let Some(mut candidate) = profiles.row_data(index)
                        && candidate.visible
                    {
                        candidate.active = true;
                        profiles.set_row_data(index, candidate);
                        replacement = Some(index as i32);
                        break;
                    }
                }
            }
            if let Some(ui) = weak.upgrade() {
                if let Some(index) = replacement {
                    ui.set_selected_profile(index);
                }
                ui.set_dirty(true);
                ui.set_status_text("profile hidden from the workspace".into());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let profiles = profiles.clone();
        ui.on_show_hidden_profiles(move || {
            let mut restored = 0;
            for row in 0..profiles.row_count() {
                if let Some(mut profile) = profiles.row_data(row)
                    && !profile.visible
                {
                    profile.visible = true;
                    profiles.set_row_data(row, profile);
                    restored += 1;
                }
            }
            if let Some(ui) = weak.upgrade() {
                if restored > 0 {
                    ui.set_dirty(true);
                    ui.set_status_text("hidden profiles restored to the workspace".into());
                } else {
                    ui.set_status_text("no profiles are hidden".into());
                }
            }
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_toggle_theme(move || {
            if let Some(ui) = weak.upgrade() {
                let theme = ui.global::<Theme>();
                theme.set_dark_mode(!theme.get_dark_mode());
            }
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_select_transport(move || {
            if let Some(ui) = weak.upgrade() {
                let receiver = ui.get_transport_label() == "usb wired";
                ui.set_transport_label(if receiver { "2.4g receiver" } else { "usb wired" }.into());
                ui.set_battery_text(if receiver { "82%" } else { "unavailable" }.into());
                ui.set_status_text("demo transport changed".into());
            }
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_request_save(move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            ui.set_dirty(false);
            ui.set_status_text("applied and immediately verified — demo mode".into());
            let weak = ui.as_weak();
            Timer::single_shot(Duration::from_millis(2200), move || {
                if let Some(ui) = weak.upgrade() {
                    ui.set_status_text("interactive design preview — hardware is untouched".into());
                }
            });
        });
    }

    {
        let weak = ui.as_weak();
        ui.on_request_reset(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_dirty(false);
                ui.set_status_text("draft changes reset".into());
            }
        });
    }

    ui.run()
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

fn profile(name: &str, subtitle: &str, active: bool) -> ProfileRow {
    ProfileRow {
        name: name.into(),
        subtitle: subtitle.into(),
        active,
        visible: true,
    }
}

fn page_status(page: i32) -> &'static str {
    match page {
        0 => "live device summary",
        1 => "click a binding to preview safe actions",
        2 => "select a dpi stage to edit the draft",
        3 => "performance settings preview",
        4 => "device identity and verification",
        _ => "interactive design preview",
    }
}
