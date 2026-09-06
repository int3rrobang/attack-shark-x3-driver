use attack_shark_x3::{DebounceMs, DeepSleepMinutes, DpiValue, SleepTimer};
use attack_shark_x3_manager::{
    DeviceEndpoint, DeviceEvent, DeviceIdentity, FullProfileRefreshOutcome, IdentityCeremonyKind,
    IdentityCeremonyProgress, IdentityCeremonyStage, IdentityResolution, LiftOffDistance,
    ManagerError, ProfileId, ProfileMetadata, ProfileResourceKind, ProfileUpdateOutcome,
    SafeButtonAction, SafeButtonSlot, TransportKind, UnassociatedReason, Verification,
};

pub const BINDING_ACTIONS: [&str; 49] = [
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
pub const SAFE_BUTTON_SLOTS: [SafeButtonSlot; 6] = [
    SafeButtonSlot::Left,
    SafeButtonSlot::Right,
    SafeButtonSlot::Middle,
    SafeButtonSlot::Dpi,
    SafeButtonSlot::Forward,
    SafeButtonSlot::Backward,
];
pub const MAX_DPI_STAGES: usize = 8;
// Protocol DPI limits are owned by `DpiValue`; the GUI keeps only display-range
// policy and log-scale helpers. These aliases delegate to the manager's typed
// validation so no wire formula is duplicated.
pub const DPI_MIN: f32 = DpiValue::MIN as f32;
pub const DPI_MAX: f32 = DpiValue::MAX as f32;
pub const DPI_STEP: f32 = DpiValue::STEP as f32;
pub const DEFAULT_DPI_DISPLAY_MIN: f32 = 200.0;
pub const DEFAULT_DPI_DISPLAY_MAX: f32 = 3_200.0;
pub const DPI_LABELS: [&str; MAX_DPI_STAGES] = [
    "stage 01", "stage 02", "stage 03", "stage 04", "stage 05", "stage 06", "stage 07", "stage 08",
];
pub const RAW_PREFERENCE_CONFIGURATION: i32 = 0;
pub const RAW_PREFERENCE_DEEP_SLEEP: i32 = 1;
pub const RAW_PREFERENCE_SLEEP_TIMER: i32 = 2;
pub const RAW_PREFERENCE_DEBOUNCE: i32 = 3;

pub const VERIFICATION_INSTRUCTION_PROFILE_RELOAD: &str = "Keep the mouse connected. This check switches your active profile twice to test it. Don't unplug or turn it off.";
pub const VERIFICATION_INSTRUCTION_POWER_CYCLE: &str = "Unplug USB, turn the mouse off, wait for it to disappear, then turn it on and reconnect. The check waits for each step.";

pub const DPI_VALUES_APPEND: [&str; MAX_DPI_STAGES] = [
    "800", "1200", "1600", "2000", "2400", "3200", "12000", "26000",
];

/// Centralized endpoint helpers: never unwrap, never panic on missing endpoint.
/// The selected endpoint is `preferred_transport` with deterministic fallback
/// (Wired -> Receiver -> BLE), exactly `DeviceIdentity::selected_endpoint()`.
pub fn selected_endpoint(identity: &DeviceIdentity) -> Option<&DeviceEndpoint> {
    identity.selected_endpoint()
}

pub fn selected_transport(identity: &DeviceIdentity) -> Option<TransportKind> {
    selected_endpoint(identity).map(|endpoint| endpoint.transport)
}

pub fn is_ble_identity(identity: &DeviceIdentity) -> bool {
    matches!(selected_transport(identity), Some(TransportKind::Ble))
}

#[allow(dead_code)]
pub fn is_receiver_identity(identity: &DeviceIdentity) -> bool {
    matches!(selected_transport(identity), Some(TransportKind::Receiver))
}

pub fn transport_label_for_identity(identity: &DeviceIdentity) -> String {
    match selected_transport(identity) {
        Some(transport) => transport_label(transport).to_owned(),
        None => "disconnected".to_owned(),
    }
}

pub fn transport_label(transport: TransportKind) -> &'static str {
    match transport {
        TransportKind::Wired => "usb wired",
        TransportKind::Receiver => "2.4g receiver",
        TransportKind::Ble => "ble",
    }
}

pub fn product_id_label_for_identity(identity: &DeviceIdentity) -> String {
    let endpoint = selected_endpoint(identity);
    let vid = endpoint
        .and_then(|ep| ep.vendor_id)
        .map_or_else(|| "unknown".to_owned(), |id| format!("{id:04x}"));
    let pid = endpoint
        .and_then(|ep| ep.product_id)
        .map_or_else(|| "unknown".to_owned(), |id| format!("{id:04x}"));
    format!("{vid}:{pid}")
}

pub fn format_error_string(prefix: &str, error: ManagerError) -> String {
    let message = match error {
        ManagerError::Protocol { .. } => "that change isn't valid".to_owned(),
        ManagerError::DeviceOperationBusy { .. } => {
            "the mouse is busy; wait a moment and try again".to_owned()
        }
        ManagerError::MissingBaseline { resource, .. } => {
            format!("saved {resource} settings are needed for this change")
        }
        other => other.to_string(),
    };
    format!("{prefix}: {message}")
}

/// Which action the identity-ceremony popup's primary button performs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CeremonyPrompt {
    /// No primary action; the popup only informs.
    None,
    /// The user reconnected or presented the physical mouse.
    Reconnect,
    /// Assign the generated token and write the watermark.
    Stamp,
    /// Explicitly confirm adoption of the observed foreign token.
    Adopt,
    /// Pair the presented BLE endpoint with the target logical mouse.
    Associate,
    /// The ceremony finished (complete, cancelled, or failed); close it.
    Done,
}

/// User-facing title of a physical-identity ceremony kind.
pub fn ceremony_title(kind: IdentityCeremonyKind) -> &'static str {
    match kind {
        IdentityCeremonyKind::InitialEnrollment | IdentityCeremonyKind::AddMouse => {
            "add another mouse"
        }
        IdentityCeremonyKind::Restore => "restore a saved mouse",
        IdentityCeremonyKind::ForeignAdoption => "adopt this mouse",
        IdentityCeremonyKind::BleAssociation => "pair a bluetooth mouse",
    }
}

/// Success line shown when a ceremony completes.
pub fn ceremony_success_copy(kind: IdentityCeremonyKind) -> &'static str {
    match kind {
        IdentityCeremonyKind::InitialEnrollment => {
            "Both mice are set up and each keeps its own identity."
        }
        IdentityCeremonyKind::AddMouse => "The mouse was added.",
        IdentityCeremonyKind::Restore => "The saved mouse is restored to this mouse.",
        IdentityCeremonyKind::ForeignAdoption => "The mouse was added.",
        IdentityCeremonyKind::BleAssociation => "The bluetooth mouse is paired.",
    }
}

/// The step instruction shown for one ceremony progress report. Each line
/// names the physical action the user must perform; internal ceremony terms
/// stay in the engineering vocabulary.
pub fn ceremony_instruction(progress: &IdentityCeremonyProgress) -> String {
    let kind = progress.kind;
    match &progress.stage {
        IdentityCeremonyStage::Ready => "Ready.".into(),
        IdentityCeremonyStage::AwaitingReconnect => match kind {
            IdentityCeremonyKind::InitialEnrollment if progress.step == Some(1) => {
                "Reconnect the mouse you're adding.".into()
            }
            IdentityCeremonyKind::InitialEnrollment => {
                "Now reconnect your other mouse.".into()
            }
            IdentityCeremonyKind::AddMouse => "Reconnect the mouse you want to add.".into(),
            IdentityCeremonyKind::Restore => {
                "Disconnect the saved mouse if it's connected, then reconnect the mouse you're assigning to it.".into()
            }
            IdentityCeremonyKind::ForeignAdoption => {
                "This mouse was set up on another computer. Make sure it's connected, then continue.".into()
            }
            IdentityCeremonyKind::BleAssociation => {
                "Make sure the bluetooth mouse is on and nearby, then pair it.".into()
            }
        },
        IdentityCeremonyStage::Capturing => {
            "Reading the mouse and capturing its settings. Keep it connected.".into()
        }
        IdentityCeremonyStage::Stamping => {
            "The app is setting up this mouse. Keep it connected.".into()
        }
        IdentityCeremonyStage::Verified => "The mouse confirmed its identity.".into(),
        IdentityCeremonyStage::Complete => ceremony_success_copy(kind).into(),
        IdentityCeremonyStage::Cancelled => "Setup was cancelled. Nothing was changed.".into(),
        IdentityCeremonyStage::Failed { .. } => "Setup stopped. Nothing was changed.".into(),
    }
}

/// Label for the ceremony popup's primary action button.
pub fn ceremony_prompt_label(prompt: CeremonyPrompt, kind: IdentityCeremonyKind) -> &'static str {
    match prompt {
        CeremonyPrompt::None => "",
        CeremonyPrompt::Reconnect => "I've reconnected the mouse",
        CeremonyPrompt::Stamp => match kind {
            IdentityCeremonyKind::Restore => "restore this mouse",
            _ => "add this mouse",
        },
        CeremonyPrompt::Adopt => "adopt this mouse",
        CeremonyPrompt::Associate => "pair this bluetooth mouse",
        CeremonyPrompt::Done => "done",
    }
}

/// Secondary line for the ceremony popup: step position and the logical
/// mouse being processed when known.
pub fn ceremony_detail(progress: &IdentityCeremonyProgress, target_name: Option<&str>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let (Some(step), Some(total)) = (progress.step, progress.total_steps)
        && total > 1
    {
        parts.push(format!("step {step} of {total}"));
    }
    if let Some(name) = target_name {
        parts.push(name.to_owned());
    }
    parts.join(" · ")
}

/// Display name for an unassociated connection, distinct from logical mice.
pub fn unassociated_name(endpoint: &DeviceEndpoint) -> String {
    endpoint
        .display_name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| match endpoint.transport {
            TransportKind::Ble => "Bluetooth mouse".to_owned(),
            _ => "Unmarked mouse".to_owned(),
        })
}

/// Why an unassociated connection cannot be attached to a logical mouse.
pub fn unassociated_reason_copy(
    endpoint: &DeviceEndpoint,
    reason: UnassociatedReason,
) -> &'static str {
    match reason {
        UnassociatedReason::Absent if endpoint.transport == TransportKind::Ble => {
            "bluetooth, not paired yet"
        }
        UnassociatedReason::Absent => "no saved identity",
        UnassociatedReason::Malformed => "its identity can't be read",
        UnassociatedReason::Unsupported { .. } => "newer identity format — update the app",
        UnassociatedReason::Unknown => "set up on another computer",
        UnassociatedReason::Duplicate => "identity conflict",
        UnassociatedReason::Reserved => "reserved identity",
        UnassociatedReason::ReservedByJournal => "reserved by a running setup",
    }
}

/// Detail line for one unassociated connection, leading with its transport.
pub fn unassociated_detail(endpoint: &DeviceEndpoint, resolution: &IdentityResolution) -> String {
    let transport = transport_label(endpoint.transport);
    match resolution {
        IdentityResolution::Resolved { .. } => format!("{transport} · known mouse"),
        IdentityResolution::Unassociated { reason, .. } => {
            format!(
                "{transport} · {}",
                unassociated_reason_copy(endpoint, *reason)
            )
        }
    }
}

pub fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

/// Pure tail rule for enabling a profile: the maximum is raised by one.
/// Returns `None` when every slot is already enabled.
pub fn add_tail_metadata(metadata: ProfileMetadata) -> Option<ProfileMetadata> {
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
pub fn hide_tail_metadata(metadata: ProfileMetadata, number: u8) -> Option<ProfileMetadata> {
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

pub fn lift_off_choice_value(choice: u8) -> LiftOffDistance {
    match choice {
        0 => LiftOffDistance::OneMillimeter,
        _ => LiftOffDistance::TwoMillimeters,
    }
}

pub fn format_byte(value: u8) -> String {
    format!("{value:02x}")
}

/// Parses a one- or two-digit hex byte, tolerating an optional `0x` prefix.
pub fn parse_raw_byte(text: &str) -> Option<u8> {
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
pub struct RawPreferenceBytes {
    pub configuration: u8,
    pub deep_sleep: u8,
    pub sleep_timer: u8,
    pub debounce: u8,
}

/// Parses all four raw preference text fields at Save time so invalid text
/// can never serialize as a zero byte. Returns the failing field index (one
/// of the `RAW_PREFERENCE_*` constants) when any displayed text is invalid;
/// the model keeps the last valid byte until that field is corrected.
pub fn parse_raw_preference_draft(
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

pub fn raw_preference_field_label(field: i32) -> &'static str {
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
pub fn dirty_draft_message(dirty: bool, action: &str) -> Option<String> {
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
pub fn reported_profile_switch(event: DeviceEvent, loaded_current: ProfileId) -> Option<ProfileId> {
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

pub fn shortcut_preset_name(
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

pub fn button_action_name(assignment: attack_shark_x3_manager::ButtonAssignment) -> String {
    if let Some(name) = shortcut_preset_name(assignment) {
        return name.to_owned();
    }
    if assignment.modifier != 0 || assignment.key_code != 0 {
        return "unsupported assignment".to_owned();
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
        _ => "unsupported assignment",
    }
    .to_owned()
}

pub fn safe_button_action(name: &str) -> Option<SafeButtonAction> {
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

pub fn validated_binding_action(_current: &str, requested: &str) -> Option<String> {
    let action = safe_button_action(requested)?;
    Some(button_action_name(action.to_assignment()))
}

pub fn configured_product_label(choice: i32, custom: &str) -> String {
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
pub fn canonical_debounce_ms(value: f64) -> Option<DebounceMs> {
    if !value.is_finite() {
        return None;
    }
    let rounded = ((value / 2.0).round() * 2.0).clamp(4.0, 50.0) as u8;
    DebounceMs::new(rounded)
}

/// Rounds a user-entered sleep duration to the nearest supported half-minute
/// step and returns the manager-owned typed value.
pub fn canonical_sleep_minutes(value: f64) -> Option<SleepTimer> {
    if !value.is_finite() {
        return None;
    }
    let half_minutes = (value * 2.0).round().clamp(1.0, 60.0) as u8;
    SleepTimer::new(half_minutes)
}

/// Rounds a user-entered deep-sleep duration to the nearest whole minute.
pub fn canonical_deep_sleep_minutes(value: f64) -> Option<DeepSleepMinutes> {
    if !value.is_finite() {
        return None;
    }
    let minutes = value.clamp(1.0, 60.0).round() as u8;
    DeepSleepMinutes::new(minutes)
}

pub fn round_dpi_step(value: f32) -> u16 {
    if !value.is_finite() {
        return DpiValue::MIN;
    }
    let step = DpiValue::STEP as f32;
    let min_step = DpiValue::MIN as f32 / step;
    let max_step = DpiValue::MAX as f32 / step;
    let step_index = (value / step).round().clamp(min_step, max_step);
    (step_index * step) as u16
}

pub fn parse_dpi_setting(text: &str) -> Option<f32> {
    let value = text.trim().parse::<f32>().ok()?;
    if !value.is_finite() || !(DpiValue::MIN as f32..=DpiValue::MAX as f32).contains(&value) {
        return None;
    }
    Some(round_dpi_step(value) as f32)
}
pub fn parse_dpi_range(minimum: &str, maximum: &str) -> Option<(f32, f32)> {
    let minimum = parse_dpi_setting(minimum)?;
    let maximum = parse_dpi_setting(maximum)?;
    (minimum < maximum).then_some((minimum, maximum))
}

pub fn format_dpi_setting(value: f32) -> String {
    (round_dpi_step(value) as u32).to_string()
}
pub fn dpi_bounds(min: f32, max: f32) -> (f32, f32) {
    let min = (round_dpi_step(min) as f32).clamp(
        DpiValue::MIN as f32,
        DpiValue::MAX as f32 - DpiValue::STEP as f32,
    );
    let max = (round_dpi_step(max) as f32).clamp(min + DpiValue::STEP as f32, DpiValue::MAX as f32);
    (min, max)
}
pub fn clamp_dpi_value(dpi: f32, min: f32, max: f32) -> f32 {
    let (min, max) = dpi_bounds(min, max);
    (round_dpi_step(dpi) as f32).clamp(min, max)
}

pub fn page_status(page: i32) -> &'static str {
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

pub fn verification_summary(verification: &Verification) -> String {
    use attack_shark_x3_manager::{ApplicationVerification, PersistenceVerification};
    match (verification.application, verification.persistence.clone()) {
        (ApplicationVerification::ReadbackVerified, PersistenceVerification::Unknown) => {
            "confirmed by the mouse".to_owned()
        }
        (
            ApplicationVerification::ReadbackVerified,
            PersistenceVerification::ProfileReloadVerified { .. },
        ) => "confirmed by the mouse and survives switching profiles".to_owned(),
        (
            ApplicationVerification::ReadbackVerified,
            PersistenceVerification::PowerCycleVerified { .. },
        ) => "confirmed by the mouse and survives a full power-off".to_owned(),
        (ApplicationVerification::Acknowledged, PersistenceVerification::Unknown) => {
            "applied (device acknowledged)".to_owned()
        }
        (
            ApplicationVerification::Acknowledged,
            PersistenceVerification::ProfileReloadVerified { .. },
        ) => "applied (device acknowledged) and survives switching profiles".to_owned(),
        (
            ApplicationVerification::Acknowledged,
            PersistenceVerification::PowerCycleVerified { .. },
        ) => "applied (device acknowledged) and survives a full power-off".to_owned(),
        (ApplicationVerification::Mismatch, _) => {
            "the mouse couldn't confirm the change".to_owned()
        }
        (ApplicationVerification::NotSent, _) => "not yet confirmed".to_owned(),
    }
}

pub fn workflow_summary(workflow: &str, profile: ProfileId, verification: &Verification) -> String {
    format!(
        "{} for profile {} — {}",
        workflow,
        profile.get(),
        verification_summary(verification)
    )
}

pub fn format_refresh_summary(outcome: &FullProfileRefreshOutcome) -> String {
    let mut parts = Vec::new();
    parts.push(format!(
        "restored profile {} with profiles enabled through {}",
        outcome.restored_metadata.current().get(),
        outcome.restored_metadata.maximum().get()
    ));
    if outcome.temporarily_expanded {
        parts.push(format!(
            "temporarily enabled profiles through {}",
            ProfileId::MAX
        ));
    }
    for (profile, resources) in &outcome.drift {
        let names: Vec<&str> = resources.iter().map(profile_resource_name).collect();
        parts.push(format!(
            "profile {} differs from the saved settings: {}",
            profile.get(),
            names.join(", ")
        ));
    }
    if parts.len() == 1 {
        parts.push("saved settings match all profiles".to_owned());
    }
    parts.push(if outcome.profile_metadata_drift {
        "the saved profile list differs from the mouse".to_owned()
    } else {
        "the saved profile list matches the mouse".to_owned()
    });
    parts.push("not yet confirmed after restart".to_owned());
    parts.join("; ")
}

pub fn profile_resource_name(resource: &ProfileResourceKind) -> &'static str {
    match resource {
        ProfileResourceKind::Dpi => "DPI",
        ProfileResourceKind::Preferences => "preferences",
        ProfileResourceKind::Buttons => "buttons",
        ProfileResourceKind::PollingRate => "polling rate",
    }
}

/// Builds the user-facing status line for a composite `ProfileUpdate` outcome.
/// Consumes `ProfileUpdateOutcome` exactly as the manager's single `apply_profile_update`
/// call produces it: each present field contributes its verification summary,
/// and the whole string keeps the existing result-first phrasing.
pub fn profile_update_status(outcome: &ProfileUpdateOutcome) -> String {
    let mut confirmations = Vec::new();
    if let Some(write) = &outcome.dpi {
        confirmations.push(verification_summary(&write.verification));
    }
    if let Some(write) = &outcome.preferences {
        confirmations.push(verification_summary(&write.verification));
    }
    if let Some(write) = &outcome.buttons {
        confirmations.push(verification_summary(&write.verification));
    }
    if let Some(write) = &outcome.polling_rate {
        confirmations.push(verification_summary(&write.verification));
    }
    if confirmations.is_empty() {
        return "nothing was changed; the mouse is already up to date".to_owned();
    }
    format!(
        "applied: {} · not yet confirmed after restart; run a check in Advanced",
        confirmations.join("; ")
    )
}

/// Status-line warning shown when an event-driven snapshot arrives while a
/// draft is open: the draft is preserved, but the external change may make
/// the next Save fail until the user discards or reloads.
pub fn event_snapshot_draft_warning(status: &str) -> String {
    format!(
        "{status} — external state changed while a draft was open; the draft was preserved, but Save may be rejected until you discard / reload"
    )
}

/// True when an incoming snapshot must preserve the draft: any event-driven
/// snapshot preserves UI state (e.g. an open button-assignment popup) and
/// only refreshes telemetry. Explicit operations (refresh, apply, select,
/// discard, reload) may always replace the models and clear the draft.
pub fn snapshot_preserves_draft(event_driven: bool, _dirty: bool) -> bool {
    event_driven
}

#[cfg(test)]
mod tests {
    use super::*;
    use attack_shark_x3_manager::{BatteryEvent, ProfileChangedEvent};

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
            dirty_draft_message(true, "running the power-cycle check").as_deref(),
            Some("save or discard the current draft before running the power-cycle check")
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
            reported_profile_switch(
                attack_shark_x3_manager::DeviceEvent::SecondaryProfileChanged(event),
                loaded
            ),
            Some(profile_three)
        );
        assert_eq!(
            reported_profile_switch(
                attack_shark_x3_manager::DeviceEvent::ProfileSync(event),
                loaded
            ),
            Some(profile_three)
        );
        assert_eq!(
            reported_profile_switch(
                attack_shark_x3_manager::DeviceEvent::SecondaryProfileChanged(event),
                profile_three
            ),
            None
        );
        assert_eq!(
            reported_profile_switch(
                attack_shark_x3_manager::DeviceEvent::BatteryChanged(BatteryEvent {
                    raw_report: [0x03, 0, 0, 0, 0],
                    level: 8,
                }),
                loaded,
            ),
            None
        );
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

        let full =
            ProfileMetadata::new(ProfileId::MIN_ID, ProfileId::MAX_ID).expect("valid metadata");
        assert!(add_tail_metadata(full).is_none());
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
    fn dpi_settings_round_to_fifty_and_reject_invalid_values() {
        assert_eq!(parse_dpi_setting("224"), Some(200.0));
        assert_eq!(parse_dpi_setting("226"), Some(250.0));
        assert_eq!(parse_dpi_setting("75"), Some(100.0));
        assert_eq!(parse_dpi_setting("49"), None);
        assert_eq!(parse_dpi_setting("26001"), None);
        assert_eq!(parse_dpi_setting("nan"), None);
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
    fn modified_or_unknown_button_assignments_are_not_exposed_as_safe_actions() {
        let modified = attack_shark_x3_manager::ButtonAssignment::new(0x02, 1, 0);
        let unknown = attack_shark_x3_manager::ButtonAssignment::new(0xff, 0, 0);
        assert_eq!(button_action_name(modified), "unsupported assignment");
        assert_eq!(button_action_name(unknown), "unsupported assignment");
        assert!(safe_button_action("unsupported assignment").is_none());
    }

    #[test]
    fn dropdown_selection_overrides_an_unsupported_current_assignment_with_a_safe_action() {
        let forward = crate::projection::binding("forward", "side upper", "unsupported assignment");
        assert!(forward.editable);
        assert_eq!(
            validated_binding_action(forward.action.as_str(), "right click").as_deref(),
            Some("right click")
        );
        assert!(validated_binding_action(forward.action.as_str(), "not a safe action").is_none());
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
        assert!(snapshot_preserves_draft(true, true));
        assert!(snapshot_preserves_draft(true, false));
        assert!(!snapshot_preserves_draft(false, true));
        assert!(!snapshot_preserves_draft(false, false));
    }

    #[test]
    fn refresh_summary_describes_results_without_internal_terms() {
        use std::collections::BTreeMap;
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
        assert!(summary.contains("restored profile 2 with profiles enabled through 3"));
        assert!(summary.contains("temporarily enabled profiles through 5"));
        assert!(
            summary.contains("profile 2 differs from the saved settings: polling rate, buttons")
        );
        assert!(summary.contains("not yet confirmed after restart"));
        assert!(!contains_blocklist_token(&summary));
    }

    #[test]
    fn dpi_constants_delegate_to_typed_value() {
        assert_eq!(DPI_MIN as u16, DpiValue::MIN);
        assert_eq!(DPI_MAX as u16, DpiValue::MAX);
        assert_eq!(DPI_STEP as u16, DpiValue::STEP);
        // Round-tripped validation uses the typed range, not a duplicated literal.
        assert_eq!(DpiValue::new(50).unwrap().get(), 50);
        assert!(DpiValue::new(51).is_none());
        assert!(DpiValue::new(26_001).is_none());
    }

    #[test]
    fn composite_profile_update_is_one_typed_operation() {
        use attack_shark_x3::{DpiValue, StageIndex};
        use attack_shark_x3_manager::{
            ButtonSlotDelta, PollingRate, SafeButtonAction, SafeButtonSlot,
        };
        use attack_shark_x3_manager::{DpiDelta, PreferencesDelta, ProfileUpdate};
        // Pure conversion: the GUI builds exactly one ProfileUpdate from the draft diff.
        let dpi = DpiDelta {
            stages: Some(vec![
                DpiValue::new(800).unwrap(),
                DpiValue::new(1600).unwrap(),
            ]),
            active_stage: Some(StageIndex::new(1).unwrap()),
            sensor: None,
        };
        let prefs = PreferencesDelta {
            configuration: Some(0x03),
            debounce: Some(0x04),
            ..Default::default()
        };
        let buttons = vec![ButtonSlotDelta::new(
            SafeButtonSlot::Forward,
            SafeButtonAction::Forward,
        )];
        let update = ProfileUpdate {
            dpi: Some(dpi),
            preferences: Some(prefs),
            buttons,
            polling_rate: None,
        };
        assert!(!update.is_empty());
        assert!(update.has_non_rate());
        assert!(!update.has_polling());
        // Polling is isolated by the manager; combining is rejected as one error.
        let mixed = ProfileUpdate {
            dpi: Some(DpiDelta {
                active_stage: Some(StageIndex::new(1).unwrap()),
                ..Default::default()
            }),
            polling_rate: Some(PollingRate::Hz1000),
            ..Default::default()
        };
        assert!(mixed.has_polling() && mixed.has_non_rate());
    }

    #[test]
    fn profile_update_status_consumes_outcome_with_result_first_phrasing() {
        use attack_shark_x3::{DpiState, DpiValue, ProfileId, StageIndex};
        use attack_shark_x3_manager::{
            ApplicationVerification, PersistenceVerification, PreferencesState, Verification,
            WriteOutcome,
        };
        let profile = ProfileId::new(1).unwrap();
        let dpi_state = DpiState::new(
            profile,
            vec![DpiValue::new(800).unwrap()],
            StageIndex::new(1).unwrap(),
            [0xff; 25],
        )
        .unwrap()
        .with_sensor(attack_shark_x3::SensorOptions {
            lift_off_distance: attack_shark_x3::LiftOffDistance::OneMillimeter,
            ripple_control: false,
            angle_snap: false,
            motion_sync: false,
        });
        let prefs_state = PreferencesState {
            profile,
            light_mode: 0x02,
            configuration: 0x03,
            deep_sleep: 0xa8,
            host_color: [0, 0, 0],
            sleep_timer: 1,
            debounce: 0x04,
        };
        // Simulate a composite outcome with two resources.
        let outcome = ProfileUpdateOutcome {
            dpi: Some(WriteOutcome {
                desired: dpi_state,
                observed: None,
                verification: Verification {
                    application: ApplicationVerification::Acknowledged,
                    persistence: PersistenceVerification::Unknown,
                },
            }),
            preferences: Some(WriteOutcome {
                desired: prefs_state,
                observed: None,
                verification: Verification {
                    application: ApplicationVerification::ReadbackVerified,
                    persistence: PersistenceVerification::Unknown,
                },
            }),
            buttons: None,
            polling_rate: None,
        };
        let status = profile_update_status(&outcome);
        // Result-first phrasing retained from the sequential version.
        assert!(status.starts_with("applied:"));
        assert!(status.contains("not yet confirmed after restart"));
        assert!(status.contains("applied (device acknowledged)"));
        assert!(status.contains("confirmed by the mouse"));
        assert!(!contains_blocklist_token(&status));
    }

    #[test]
    fn confidence_summaries_avoid_blocklist_vocabulary() {
        use attack_shark_x3_manager::{
            ApplicationVerification, PersistenceVerification, Verification,
        };
        let timestamp = attack_shark_x3_manager::Timestamp { unix_seconds: 1 };
        let cases = [
            (
                Verification {
                    application: ApplicationVerification::ReadbackVerified,
                    persistence: PersistenceVerification::Unknown,
                },
                "readback verified / unknown",
            ),
            (
                Verification {
                    application: ApplicationVerification::Acknowledged,
                    persistence: PersistenceVerification::PowerCycleVerified {
                        verified_at: timestamp,
                    },
                },
                "acknowledged / power-cycle",
            ),
            (
                Verification {
                    application: ApplicationVerification::Mismatch,
                    persistence: PersistenceVerification::ProfileReloadVerified {
                        verified_at: timestamp,
                    },
                },
                "mismatch / reload",
            ),
            (
                Verification {
                    application: ApplicationVerification::NotSent,
                    persistence: PersistenceVerification::Unknown,
                },
                "not sent / unknown",
            ),
        ];
        for (verification, label) in cases {
            let summary = verification_summary(&verification);
            assert!(
                !contains_blocklist_token(&summary),
                "{label}: verification_summary leaked: {summary:?}"
            );
            let workflow =
                workflow_summary("profile reload", ProfileId::new(1).unwrap(), &verification);
            assert!(
                !contains_blocklist_token(&workflow),
                "{label}: workflow_summary leaked: {workflow:?}"
            );
        }
    }

    #[test]
    fn manager_errors_use_concise_copy_but_keep_debug_detail() {
        use attack_shark_x3::ProtocolError;
        use std::{path::PathBuf, time::Duration};

        let protocol = ManagerError::Protocol {
            operation: "profile metadata packet",
            source: ProtocolError::InvalidProfile { value: 99 },
        };
        let protocol_debug = format!("{protocol:?}");
        let protocol_copy = format_error_string("could not apply settings", protocol);
        assert_eq!(
            protocol_copy,
            "could not apply settings: that change isn't valid"
        );
        assert!(!contains_blocklist_token(&protocol_copy));
        assert!(protocol_debug.contains("Protocol"));
        assert!(protocol_debug.contains("InvalidProfile"));

        let busy = ManagerError::DeviceOperationBusy {
            device: attack_shark_x3_manager::DeviceId::new("mouse-1").unwrap(),
            operation: "write packet",
            timeout: Duration::from_secs(1),
            path: PathBuf::from("technical-lock-path"),
        };
        let busy_debug = format!("{busy:?}");
        let busy_copy = format_error_string("could not save", busy);
        assert_eq!(
            busy_copy,
            "could not save: the mouse is busy; wait a moment and try again"
        );
        assert!(!contains_blocklist_token(&busy_copy));
        assert!(busy_debug.contains("DeviceOperationBusy"));
        assert!(busy_debug.contains("technical-lock-path"));
    }

    fn contains_blocklist_token(text: &str) -> bool {
        const BLOCKLIST: &[&str] = &[
            "readback",
            "persistence",
            "baseline",
            "observed",
            "desired",
            "drift",
            "evidence",
            "preflight",
            "submission",
            "schema",
            "unverified",
            "was attempted",
            "definitive",
            "packet",
            "byte",
            "ack",
        ];
        let lower = text.to_ascii_lowercase();
        for token in BLOCKLIST {
            if token.contains(' ') {
                if lower.contains(token) {
                    return true;
                }
                continue;
            }
            let mut from = 0;
            while let Some(pos) = lower[from..].find(token) {
                let abs = from + pos;
                let before = abs == 0 || !lower.as_bytes()[abs - 1].is_ascii_alphanumeric();
                let end = abs + token.len();
                let after = end >= lower.len() || !lower.as_bytes()[end].is_ascii_alphanumeric();
                if before && after {
                    return true;
                }
                from = abs + token.len();
            }
        }
        false
    }
}
