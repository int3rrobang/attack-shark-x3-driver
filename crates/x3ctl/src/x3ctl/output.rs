use crate::args::OutputFormat;
use serde::Serialize;

// ---------------------------------------------------------------------------
// Output handle
// ---------------------------------------------------------------------------

/// An output handle that selects the rendering strategy for a single command
/// invocation.  All printing goes through this handle; parser/command logic
/// never calls `println!` or `eprintln!` directly.
use crate::wire;
pub struct Output {
    fmt: OutputFormat,
}

impl Output {
    #[must_use]
    pub fn new(fmt: OutputFormat) -> Self {
        Self { fmt }
    }

    #[must_use]
    pub fn format(&self) -> OutputFormat {
        self.fmt
    }

    // -- structured ---------------------------------------------------------

    /// Emit a serializable value.  In human mode this is a no-op; callers
    /// should use the `human_*` methods for their specific formatting.  In
    /// JSON mode the value is pretty-printed to stdout.  In quiet mode the
    /// value is silently dropped.
    pub fn json<T: Serialize>(&self, value: &T) {
        if self.fmt == OutputFormat::Json {
            let json = serde_json::to_string_pretty(value).unwrap_or_default();
            println!("{json}");
        }
    }

    // -- human-specific lines -----------------------------------------------

    /// Print a line that only appears in human mode.
    pub fn human(&self, line: &str) {
        if self.fmt == OutputFormat::Human {
            println!("{line}");
        }
    }

    /// Print a key-value pair that only appears in human mode.
    pub fn human_kv(&self, key: &str, value: &str) {
        if self.fmt == OutputFormat::Human {
            println!("  {key}: {value}");
        }
    }

    /// Print a section header in human mode.
    pub fn human_section(&self, title: &str) {
        if self.fmt == OutputFormat::Human {
            println!("{title}");
        }
    }

    // -- hex dump (debug / packet commands) ---------------------------------

    /// Print a labelled hex dump.  In human mode each byte is printed as a
    /// two-digit hex pair; in JSON mode it is emitted as a `{"label":…,
    /// "hex":…, "bytes":[…]}` object; in quiet mode only the hex string is
    /// printed.
    pub fn hex_dump(&self, label: &str, bytes: &[u8]) {
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        match self.fmt {
            OutputFormat::Human => {
                println!("{label}:");
                for chunk in hex.as_bytes().chunks(32) {
                    let line = std::str::from_utf8(chunk).unwrap_or("<utf8 error>");
                    println!("  {line}");
                }
            }
            OutputFormat::Json => {
                let obj = serde_json::json!({
                    "label": label,
                    "hex": hex,
                    "bytes": bytes,
                });
                println!("{}", serde_json::to_string_pretty(&obj).unwrap());
            }
            OutputFormat::Quiet => {
                println!("{hex}");
            }
        }
    }

    // -- error --------------------------------------------------------------

    /// Print an error message to stderr (human/quiet) or emit a JSON error
    /// object to stdout (JSON mode).
    pub fn error(&self, msg: &str) {
        match self.fmt {
            OutputFormat::Json => {
                let err = serde_json::json!({ "error": msg });
                println!("{}", serde_json::to_string(&err).unwrap());
            }
            _ => {
                eprintln!("error: {msg}");
            }
        }
    }

    // -- response rendering -------------------------------------------------

    /// Render a successful broker response.
    pub fn render_response(&self, response: &wire::Response) {
        use wire::ResponseResult;
        match &response.result {
            ResponseResult::Ok(data) => {
                self.render_data(data, response.provenance);
            }
            ResponseResult::Err { code, message } => {
                let msg = format!("{:?}: {}", code, message);
                self.error(&msg);
            }
        }
    }

    /// Render the payload of a successful response.
    fn render_data(&self, data: &wire::ResponseData, provenance: wire::Provenance) {
        use wire::ResponseData;
        // Always emit structured JSON in JSON mode.
        self.json(&serde_json::json!({
            "provenance": serde_json::to_value(provenance).unwrap_or_default(),
            "data": serde_json::to_value(data).unwrap_or_default(),
        }));

        match data {
            ResponseData::Empty => {
                self.human("OK");
                self.render_provenance(provenance);
            }
            ResponseData::DeviceList(devices) => self.render_device_list(devices, provenance),
            ResponseData::DpiRead(dpi) => self.render_dpi(dpi, provenance),
            ResponseData::PreferencesRead(prefs) => self.render_preferences(prefs, provenance),
            ResponseData::ButtonsRead(buttons) => {
                self.human_kv("Profile", &buttons.profile.to_string());
                self.render_buttons(&buttons.slots, provenance);
            }
            ResponseData::RateRead(rate) => self.render_rate(rate, provenance),
            ResponseData::ProfileMetaRead(meta) => self.render_profile_meta(meta, provenance),
            ResponseData::BatteryLevel(pct) => self.render_battery(*pct, provenance),
            ResponseData::DpiWritten(summary) => {
                self.human(&format!(
                    "DPI written: profile {}, {} stages, active stage {}",
                    summary.profile, summary.stage_count, summary.active_stage
                ));
                self.render_provenance(provenance);
            }
            ResponseData::PreferencesWritten(summary) => {
                self.human(&format!("Preferences written: profile {}", summary.profile));
                self.render_provenance(provenance);
            }
            ResponseData::ButtonWritten(summary) => {
                self.human(&format!(
                    "Button written: profile {}, slot {}",
                    summary.profile, summary.index
                ));
                self.render_provenance(provenance);
            }
            ResponseData::RateWritten(summary) => {
                self.human(&format!("Polling rate set to {} Hz", summary.hz));
                self.render_provenance(provenance);
            }
            ResponseData::DeviceStatus(status) => self.render_device_status(status, provenance),
            ResponseData::ExportedState(doc) => {
                if self.fmt == OutputFormat::Human {
                    println!("{}", serde_json::to_string_pretty(doc).unwrap_or_default());
                }
                self.render_provenance(provenance);
            }
            ResponseData::ImportedSummary(summary) => {
                self.human(&format!(
                    "Imported {} device(s), {} profile(s)",
                    summary.device_count, summary.profiles_imported
                ));
                self.render_provenance(provenance);
            }
            ResponseData::ResetDone { profile } => {
                self.human(&format!(
                    "Reset complete: profile {profile} restored to factory defaults"
                ));
                self.render_provenance(provenance);
            }
            ResponseData::DaemonInfo(info) => self.render_daemon_info(info, provenance),
            ResponseData::DebugInfo(val) => {
                if self.fmt == OutputFormat::Human {
                    println!("{}", serde_json::to_string_pretty(val).unwrap_or_default());
                }
            }
        }
    }

    // -- individual renderers -----------------------------------------------

    fn render_device_list(&self, devices: &[wire::DeviceEntry], provenance: wire::Provenance) {
        if devices.is_empty() {
            self.human("No X3 devices found.");
            return;
        }
        self.human_section("Devices:");
        for d in devices {
            let label = d.product.as_deref().unwrap_or(&d.path);
            self.human_kv(
                label,
                &format!(
                    "{:?} (vid:{:04x} pid:{:04x})",
                    d.transport, d.vendor_id, d.product_id
                ),
            );
        }
        self.render_provenance(provenance);
    }

    fn render_dpi(&self, dpi: &wire::DpiStatePayload, provenance: wire::Provenance) {
        self.human_section("DPI");
        self.human_kv("Profile", &dpi.profile.to_string());
        self.human_kv("Active stage", &dpi.active_stage.to_string());
        self.human_section("  Stages:");
        for (i, &hz) in dpi.stages.iter().enumerate() {
            let marker = if (i + 1) as u8 == dpi.active_stage {
                " *"
            } else {
                ""
            };
            self.human_kv(
                &format!("    Stage {}{}", i + 1, marker),
                &format!("{hz} DPI"),
            );
        }
        self.human_section("  Sensor:");
        let lod_str = match dpi.sensor.lift_off_distance {
            1 => "1 mm (low)",
            2 => "2 mm (high)",
            n => &format!("{n}"),
        };
        self.human_kv("    Lift-off distance", lod_str);
        self.human_kv("    Ripple control", on_off(dpi.sensor.ripple_control));
        self.human_kv("    Angle snap", on_off(dpi.sensor.angle_snap));
        self.human_kv("    Motion sync", on_off(dpi.sensor.motion_sync));
        self.render_provenance(provenance);
    }

    fn render_preferences(&self, prefs: &wire::PreferencesPayload, provenance: wire::Provenance) {
        self.human_section("Preferences");
        self.human_kv("Profile", &prefs.profile.to_string());
        self.human_kv("Light mode", &format!("0x{:02x}", prefs.light_mode));
        self.human_kv("Configuration", &format!("0x{:02x}", prefs.configuration));
        self.human_kv("Deep sleep", &format!("0x{:02x}", prefs.deep_sleep));
        self.human_kv(
            "Host color",
            &format!(
                "#{:02x}{:02x}{:02x}",
                prefs.host_color[0], prefs.host_color[1], prefs.host_color[2]
            ),
        );
        self.human_kv("Sleep timer", &format!("{} min", prefs.sleep_timer));
        self.human_kv("Debounce", &format!("0x{:02x}", prefs.debounce));
        self.render_provenance(provenance);
    }

    fn render_buttons(&self, buttons: &[wire::ButtonPayload], provenance: wire::Provenance) {
        self.human_section("Buttons");
        for (i, btn) in buttons.iter().enumerate() {
            self.human_kv(
                &format!("  Slot {}", i),
                &format!(
                    "action=0x{:02x} modifier=0x{:02x} key_code=0x{:02x}",
                    btn.action, btn.modifier, btn.key_code
                ),
            );
        }
        self.render_provenance(provenance);
    }

    fn render_rate(&self, rate: &wire::RatePayload, provenance: wire::Provenance) {
        self.human_kv("Polling rate", &format!("{} Hz", rate.hz));
        if self.fmt == OutputFormat::Quiet {
            println!("{}", rate.hz);
        }
        self.render_provenance(provenance);
    }

    fn render_profile_meta(
        &self,
        meta: &wire::ProfileMetadataPayload,
        provenance: wire::Provenance,
    ) {
        self.human_section("Profile metadata");
        self.human_kv("Current profile", &meta.current.to_string());
        self.human_kv("Maximum profile", &meta.maximum.to_string());
        self.render_provenance(provenance);
    }

    fn render_battery(&self, pct: u8, provenance: wire::Provenance) {
        self.human_kv("Battery", &format!("{pct}%"));
        if self.fmt == OutputFormat::Quiet {
            println!("{pct}");
        }
        self.render_provenance(provenance);
    }

    fn render_device_status(
        &self,
        status: &wire::DeviceStatusPayload,
        provenance: wire::Provenance,
    ) {
        self.human_section("Device Status");
        self.human_kv("Active profile", &status.profile.to_string());
        self.human_kv("Max profile", &status.profile_max.to_string());

        if let Some(dpi) = &status.dpi {
            self.render_dpi(dpi, provenance);
        }
        if let Some(prefs) = &status.prefs {
            self.render_preferences(prefs, provenance);
        }
        if let Some(buttons) = &status.buttons {
            self.render_buttons(buttons, provenance);
        }
        if let Some(hz) = status.rate_hz {
            self.human_kv("Polling rate", &format!("{hz} Hz"));
        }
        if let Some(pct) = status.battery {
            self.human_kv("Battery", &format!("{pct}%"));
        }
        self.render_provenance(provenance);
    }

    fn render_daemon_info(&self, info: &wire::DaemonStatusPayload, provenance: wire::Provenance) {
        self.human_section("Daemon");
        self.human_kv("PID", &info.pid.to_string());
        self.human_kv("Uptime", &format!("{} s", info.uptime_secs));
        self.human_kv("Connected", yes_no(info.connected));
        if let Some(path) = &info.device_path {
            self.human_kv("Device", path);
        }
        if let Some(transport) = &info.transport {
            self.human_kv("Transport", &format!("{:?}", transport));
        }
        self.human_kv("Idle", &format!("{} s", info.idle_secs));
        self.render_provenance(provenance);
    }

    // -- provenance ---------------------------------------------------------

    fn render_provenance(&self, provenance: wire::Provenance) {
        let note = provenance_note(provenance);
        if self.fmt == OutputFormat::Human {
            println!("  [{note}]");
        }
    }
}

// -- helpers ---------------------------------------------------------------

/// Human-readable provenance tag for CLI output.
fn provenance_note(p: wire::Provenance) -> &'static str {
    match p {
        wire::Provenance::UsbValidated => "applied, readback confirmed via USB",
        wire::Provenance::BleAcknowledged => {
            "firmware ACK accepted; application/persistence unverified"
        }
        wire::Provenance::Cached => "cached — not device readback",
        wire::Provenance::Unverified => "unverified — not confirmed by device",
    }
}

fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}
