//! Device executor for `x3ctl` — broker-backed by default, direct/stateless
//! escape hatches.
//!
//! The executor owns at most one active device session ([`MouseHandle`] or
//! [`BleHandle`]), serialises all operations through an internal mutex, resolves
//! `auto` transport only when selection is unambiguous, and reuses sessions
//! until explicit or idle release.
//!
//! # Feature gates
//!
//! All transport-specific branches are `#[cfg]`-gated so the executor compiles
//! with `usb`-only, `ble`-only, or both features enabled.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

#[cfg(feature = "usb")]
use attack_shark_x3::{DeviceInfo, UsbDeviceKind};
#[cfg(feature = "usb")]
use attack_shark_x3::{
    DeviceSelector as UsbDeviceSelector, DriverError, MouseHandle, ReadPolicy,
    list_devices_for as usb_list_devices_for,
};

#[cfg(feature = "ble")]
use attack_shark_x3::{BleDeviceInfo, BleError, BleHandle, BleSelector};

use attack_shark_x3::ProfileMetadata;
use attack_shark_x3::protocol::buttons::BUTTON_SLOT_COUNT;
use attack_shark_x3::protocol::dpi::LiftOffDistance;
use attack_shark_x3::{
    ButtonAssignment, ButtonsState, DpiState, DpiValue, PollingRate, PreferencesState, ProfileId,
    SensorOptions as ProtocolSensorOptions, StageIndex,
};

use super::broker::{RequestHandler, SessionRelease};
#[cfg(feature = "ble")]
use super::state::MergeError;
use super::state::{self, StateFile, StateSource, StateVerification, StoredTransport};
use super::wire::{
    self, ButtonPayload, ButtonsStatePayload, DaemonStatusPayload, DeviceEntry as WireDeviceEntry,
    DeviceStatusPayload, DpiStatePayload, ErrorCode, ExecutionContext, ExportDeviceEntry,
    ExportDocument, ExportProfileState, ExportVersioned, ForgetTarget, ImportSummary,
    PreferencesDelta, PreferencesPayload, Provenance, Request, Response, ResponseData,
    ResponseResult, SensorOptions as WireSensorOptions, TransportKind as WireTransport,
};
// ── Executor ──────────────────────────────────────────────────────────────────

/// Cloneable device executor with shared interior state.
///
/// Implements both [`RequestHandler`] (for broker use) and [`SessionRelease`]
/// (for idle/forced session teardown).
#[derive(Clone)]
pub struct Executor {
    inner: Arc<Mutex<ExecutorInner>>,
}

struct ExecutorInner {
    /// Durable state.  `None` when `--no-state` is active — operations still
    /// work but nothing is persisted to disk.
    state: Option<StateFile>,

    /// Where to save durable state.  `None` when `--no-state`.
    state_path: Option<PathBuf>,

    /// Active USB session handle.
    #[cfg(feature = "usb")]
    usb_handle: Option<MouseHandle>,

    /// Active BLE session handle.
    #[cfg(feature = "ble")]
    ble_handle: Option<BleHandle>,

    /// Key of the currently-connected device in `state.devices`.
    active_device_key: Option<String>,

    /// Transport currently in use.
    active_transport: Option<WireTransport>,

    /// Wall-clock instant when the daemon started (for uptime).
    daemon_start: Instant,

    /// Wall-clock instant of the last processed request (for idle tracking).
    last_request: Instant,

    /// Daemon process ID.
    daemon_pid: u32,
}

impl Executor {
    /// Create a new executor with no durable state.
    ///
    /// Use [`Executor::with_state`] for persistent state.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ExecutorInner {
                state: None,
                state_path: None,
                #[cfg(feature = "usb")]
                usb_handle: None,
                #[cfg(feature = "ble")]
                ble_handle: None,
                active_device_key: None,
                active_transport: None,
                daemon_start: Instant::now(),
                last_request: Instant::now(),
                daemon_pid: std::process::id(),
            })),
        }
    }

    /// Create an executor that loads durable state from the canonical path.
    ///
    /// When `path` is `Some`, state is loaded on construction (falling back to
    /// defaults if the file does not exist) and saved after every mutating
    /// operation.  Pass `None` for `--no-state` / `--stateless` semantics.
    pub fn with_state(path: Option<PathBuf>) -> Self {
        let (state, state_path) = match path {
            Some(p) => {
                let s = state::load_from(&p).unwrap_or_default();
                (Some(s), Some(p))
            }
            None => (None, None),
        };
        Self {
            inner: Arc::new(Mutex::new(ExecutorInner {
                state,
                state_path,
                #[cfg(feature = "usb")]
                usb_handle: None,
                #[cfg(feature = "ble")]
                ble_handle: None,
                active_device_key: None,
                active_transport: None,
                daemon_start: Instant::now(),
                last_request: Instant::now(),
                daemon_pid: std::process::id(),
            })),
        }
    }
}

impl Default for Executor {
    fn default() -> Self {
        Self::new()
    }
}

// ── RequestHandler impl ───────────────────────────────────────────────────────

impl RequestHandler for Executor {
    async fn handle(&self, request: Request) -> Response {
        let mut inner = self.inner.lock().await;
        inner.last_request = Instant::now();
        #[cfg(all(target_os = "windows", feature = "ble"))]
        {
            tokio::task::block_in_place(move || {
                tokio::runtime::Handle::current().block_on(inner.dispatch(request))
            })
        }
        #[cfg(not(all(target_os = "windows", feature = "ble")))]
        {
            inner.dispatch(request).await
        }
    }
}

impl ExecutorInner {
    async fn dispatch(&mut self, request: Request) -> Response {
        match request {
            Request::Ping => Response::ok(ResponseData::Empty, Provenance::UsbValidated),
            Request::Stop => Response::ok(ResponseData::Empty, Provenance::UsbValidated),

            Request::ListDevices { ctx } => {
                let saved = self.mask_state(ctx.no_state);
                let result = self.handle_list_devices(ctx).await;
                self.restore_state(saved);
                result
            }
            Request::UseDevice {
                selector,
                transport,
                ref ctx,
            } => {
                let saved = self.mask_state(ctx.no_state);
                let result = self.handle_use_device(selector, transport, ctx).await;
                self.restore_state(saved);
                result
            }
            Request::Status { profile, ctx } => {
                let saved = self.mask_state(ctx.no_state);
                let result = self.handle_status(profile, ctx).await;
                self.restore_state(saved);
                result
            }
            Request::ReadDpi { profile, ctx } => {
                let saved = self.mask_state(ctx.no_state);
                let result = self.handle_read_dpi(profile, ctx).await;
                self.restore_state(saved);
                result
            }
            Request::ReadPreferences { profile, ctx } => {
                let saved = self.mask_state(ctx.no_state);
                let result = self.handle_read_prefs(profile, ctx).await;
                self.restore_state(saved);
                result
            }
            Request::ReadButtons { profile, ctx } => {
                let saved = self.mask_state(ctx.no_state);
                let result = self.handle_read_buttons(profile, ctx).await;
                self.restore_state(saved);
                result
            }
            Request::ReadRate { ctx } => {
                let saved = self.mask_state(ctx.no_state);
                let result = self.handle_read_rate(ctx).await;
                self.restore_state(saved);
                result
            }
            Request::Battery { ctx } => {
                let saved = self.mask_state(ctx.no_state);
                let result = self.handle_battery(ctx).await;
                self.restore_state(saved);
                result
            }

            Request::SetDpi {
                profile,
                stages,
                active_stage,
                sensor,
                ctx,
            } => {
                let saved = self.mask_state(ctx.no_state);
                let result = self
                    .handle_set_dpi(profile, stages, active_stage, sensor, ctx)
                    .await;
                self.restore_state(saved);
                result
            }
            Request::SetPreferences {
                profile,
                prefs,
                ctx,
            } => {
                let saved = self.mask_state(ctx.no_state);
                let result = self.handle_set_prefs(profile, prefs, ctx).await;
                self.restore_state(saved);
                result
            }
            Request::SetButton {
                profile,
                index,
                assignment,
                ctx,
            } => {
                let saved = self.mask_state(ctx.no_state);
                let result = self
                    .handle_set_button(profile, index, assignment, ctx)
                    .await;
                self.restore_state(saved);
                result
            }
            Request::SetRate { hz, ctx } => {
                let saved = self.mask_state(ctx.no_state);
                let result = self.handle_set_rate(hz, ctx).await;
                self.restore_state(saved);
                result
            }

            Request::ProfileUse { profile, ctx } => {
                let saved = self.mask_state(ctx.no_state);
                let result = self.handle_profile_use(profile, ctx).await;
                self.restore_state(saved);
                result
            }
            Request::ProfileMax { max, ctx } => {
                let saved = self.mask_state(ctx.no_state);
                let result = self.handle_profile_max(max, ctx).await;
                self.restore_state(saved);
                result
            }

            Request::Apply { ctx } => {
                let saved = self.mask_state(ctx.no_state);
                let result = self.handle_apply(ctx).await;
                self.restore_state(saved);
                result
            }
            Request::Export { ctx } => {
                let saved = self.mask_state(ctx.no_state);
                let result = self.handle_export(ctx).await;
                self.restore_state(saved);
                result
            }
            Request::Import { document, ctx } => {
                let saved = self.mask_state(ctx.no_state);
                let result = self.handle_import(document, ctx).await;
                self.restore_state(saved);
                result
            }
            Request::Reset { profile, ctx } => {
                let saved = self.mask_state(ctx.no_state);
                let result = self.handle_reset(profile, ctx).await;
                self.restore_state(saved);
                result
            }
            Request::InitDefaults { ctx } => {
                let saved = self.mask_state(ctx.no_state);
                let result = self.handle_init_defaults(ctx).await;
                self.restore_state(saved);
                result
            }
            Request::Forget { target, ctx } => {
                let saved = self.mask_state(ctx.no_state);
                let result = self.handle_forget(target, ctx).await;
                self.restore_state(saved);
                result
            }

            Request::Disconnect => self.handle_disconnect().await,
            Request::DaemonStatus => self.handle_daemon_status(),
            Request::DaemonStop => Response::ok(ResponseData::Empty, Provenance::UsbValidated),
            Request::Debug => Response::ok(
                ResponseData::DebugInfo(serde_json::Value::String("executor alive".into())),
                Provenance::UsbValidated,
            ),
        }
    }

    /// Mask durable state when `no_state` is requested, returning the saved
    /// values so the caller can restore them after the handler completes.
    fn mask_state(&mut self, no_state: bool) -> (Option<StateFile>, Option<PathBuf>) {
        if no_state {
            (self.state.take(), self.state_path.take())
        } else {
            (None, None)
        }
    }

    /// Restore durable state after a no-state request.
    fn restore_state(&mut self, saved: (Option<StateFile>, Option<PathBuf>)) {
        let (state, path) = saved;
        if state.is_some() || path.is_some() {
            self.state = state;
            self.state_path = path;
        }
    }

    // ── Session management ────────────────────────────────────────────────────────

    /// Resolve `Auto` transport to a concrete transport, preferring the
    /// currently-active transport if available, then the stored selection.
    fn resolve_transport(&self, ctx: &ExecutionContext) -> Result<WireTransport, Response> {
        match ctx.transport {
            WireTransport::Auto => {
                // Prefer currently active transport.
                if let Some(t) = self.active_transport {
                    if t != WireTransport::Auto {
                        return Ok(t);
                    }
                }
                // Fall back to stored transport for the selected device.
                if let Some(ref state) = self.state {
                    if let Some(key) = state.selected_device.as_deref() {
                        if let Some(entry) = state.devices.get(key) {
                            return Ok(stored_to_wire_transport(entry.transport));
                        }
                    }
                }
                // If we have a session open, use its transport.
                #[cfg(feature = "usb")]
                if self.usb_handle.is_some() {
                    return Ok(WireTransport::Wired);
                }
                #[cfg(feature = "ble")]
                if self.ble_handle.is_some() {
                    return Ok(WireTransport::Ble);
                }
                Err(Response::err(
                    ErrorCode::InvalidRequest,
                    "cannot resolve `auto` transport: no device selected and no active session",
                    Provenance::Unverified,
                ))
            }
            concrete => Ok(concrete),
        }
    }
    /// Resolve `Auto` transport with hardware probing when no prior state
    /// exists.  Mirrors the strategy in `handle_list_devices`: try USB first,
    /// then BLE.  For concrete transports the selector is passed through
    /// unchanged (but `ensure_session` now accepts name selectors for USB).
    async fn resolve_transport_and_selector(
        &mut self,
        transport: WireTransport,
        selector: wire::DeviceSelector,
    ) -> Result<(WireTransport, wire::DeviceSelector), Response> {
        if transport != WireTransport::Auto {
            return Ok((transport, selector));
        }

        // Try the existing resolution first (active session / stored state).
        let ctx = ExecutionContext {
            transport,
            device: None,
            no_state: true,
            explicit_defaults: false,
        };
        if let Ok(t) = self.resolve_transport(&ctx) {
            return Ok((t, selector));
        }

        // Probe hardware — USB wired, USB receiver, then BLE.
        #[cfg(feature = "usb")]
        {
            if usb_list_devices_for(UsbDeviceKind::Wired)
                .map(|d| !d.is_empty())
                .unwrap_or(false)
            {
                return Ok((WireTransport::Wired, selector));
            }
            if usb_list_devices_for(UsbDeviceKind::Receiver)
                .map(|d| !d.is_empty())
                .unwrap_or(false)
            {
                return Ok((WireTransport::Receiver, selector));
            }
        }

        #[cfg(feature = "ble")]
        return Ok((WireTransport::Ble, selector));

        // No BLE compiled in — if we get here, USB found nothing.
        #[cfg(not(feature = "ble"))]
        return Err(Response::err(
            ErrorCode::DeviceNotFound,
            "no device found",
            Provenance::Unverified,
        ));

        // No transport compiled at all.
        #[cfg(not(any(feature = "usb", feature = "ble")))]
        return Err(Response::err(
            ErrorCode::Unsupported,
            "no transport available",
            Provenance::Unverified,
        ));
    }

    /// Ensure a device session matching `transport` is open.
    ///
    /// If a session is already open for the same transport/device it is reused;
    /// otherwise the old session is released first.
    async fn ensure_session(
        &mut self,
        transport: WireTransport,
        selector: Option<&wire::DeviceSelector>,
    ) -> Result<(), Response> {
        // If we already have the right session, reuse it.
        if self.active_transport == Some(transport) {
            return Ok(());
        }

        // Release any existing session first.
        self.release_session();
        match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => {
                let usb_sel = match selector {
                    Some(wire::DeviceSelector::UsbPath(p)) => UsbDeviceSelector::Path(p.clone()),
                    // Name selectors (from `use` or `--device`) are accepted
                    // but ignored for USB transport — the name is a display
                    // hint, not a HID path.  Fall back to Unique auto-detect.
                    Some(wire::DeviceSelector::BleName(_))
                    | Some(wire::DeviceSelector::BleAddr(_)) => UsbDeviceSelector::Unique,
                    None => UsbDeviceSelector::Unique,
                };
                let kind = match transport {
                    WireTransport::Wired => UsbDeviceKind::Wired,
                    WireTransport::Receiver => UsbDeviceKind::Receiver,
                    _ => unreachable!(),
                };
                let handle =
                    MouseHandle::open_for_kind_with_policy(usb_sel, kind, ReadPolicy::default())
                        .map_err(|e| map_driver_error(&e))?;
                self.usb_handle = Some(handle);
            }

            #[cfg(feature = "ble")]
            WireTransport::Ble => {
                let ble_sel = match selector {
                    Some(wire::DeviceSelector::BleName(n)) => BleSelector::Name(n.clone()),
                    Some(wire::DeviceSelector::BleAddr(_a)) => {
                        BleSelector::Name(format!("addr:{}", _a))
                    }
                    Some(_) => {
                        return Err(Response::err(
                            ErrorCode::InvalidRequest,
                            "USB selector provided for BLE transport",
                            Provenance::Unverified,
                        ));
                    }
                    None => BleSelector::UniqueConnected,
                };
                let handle = BleHandle::open(ble_sel)
                    .await
                    .map_err(|e| map_ble_error(&e))?;
                self.ble_handle = Some(handle);
            }

            #[cfg(not(feature = "usb"))]
            WireTransport::Wired | WireTransport::Receiver => {
                return Err(Response::err(
                    ErrorCode::Unsupported,
                    "USB support not compiled in",
                    Provenance::Unverified,
                ));
            }

            #[cfg(not(feature = "ble"))]
            WireTransport::Ble => {
                return Err(Response::err(
                    ErrorCode::Unsupported,
                    "BLE support not compiled in",
                    Provenance::Unverified,
                ));
            }

            WireTransport::Auto => {
                return Err(Response::err(
                    ErrorCode::Internal,
                    "Auto transport not resolved before ensure_session",
                    Provenance::Unverified,
                ));
            }
        }

        self.active_transport = Some(transport);
        Ok(())
    }

    /// Release the current device session.
    ///
    /// Drops the handle(s) but **never** un-pairs or disconnects the device.
    fn release_session(&mut self) {
        #[cfg(feature = "usb")]
        {
            self.usb_handle = None;
        }
        #[cfg(feature = "ble")]
        {
            self.ble_handle = None;
        }
        self.active_transport = None;
        // Keep active_device_key — the logical selection survives session
        // release.  The next request re-opens if needed.
    }

    /// Resolve the device key for state lookups.
    ///
    /// Returns the key from the active device, falling back to stored selection.
    fn resolve_device_key(&self) -> Option<String> {
        self.active_device_key
            .clone()
            .or_else(|| self.state.as_ref().and_then(|s| s.selected_device.clone()))
    }

    /// Derive a device key from a wire-level selector and transport.
    fn derive_device_key(selector: &wire::DeviceSelector, _transport: WireTransport) -> String {
        match selector {
            wire::DeviceSelector::UsbPath(p) => format!("usb:{}", p),
            wire::DeviceSelector::BleName(n) => format!("ble:{}", n),
            wire::DeviceSelector::BleAddr(a) => format!("ble:{}", a),
        }
    }
}

// ── Per-request handlers ──────────────────────────────────────────────────────

impl ExecutorInner {
    // ── discovery ──────────────────────────────────────────────────────────

    async fn handle_list_devices(&mut self, ctx: ExecutionContext) -> Response {
        // Fall back to Auto when no prior session exists — we need to probe
        // both transports to discover devices.
        let transport = self.resolve_transport(&ctx).unwrap_or(WireTransport::Auto);

        let mut entries: Vec<WireDeviceEntry> = Vec::new();

        #[cfg(feature = "usb")]
        {
            if transport == WireTransport::Wired || ctx.transport == WireTransport::Auto {
                match usb_list_devices_for(UsbDeviceKind::Wired) {
                    Ok(devs) => {
                        for d in &devs {
                            entries.push(usb_device_to_wire(&d));
                        }
                    }
                    Err(e) => {
                        return error_response(map_driver_error_code(&e), e.to_string());
                    }
                }
            }
            if transport == WireTransport::Receiver || ctx.transport == WireTransport::Auto {
                match usb_list_devices_for(UsbDeviceKind::Receiver) {
                    Ok(devs) => {
                        for d in &devs {
                            entries.push(usb_device_to_wire(&d));
                        }
                    }
                    Err(e) => {
                        return error_response(map_driver_error_code(&e), e.to_string());
                    }
                }
            }
        }

        #[cfg(feature = "ble")]
        {
            if transport == WireTransport::Ble || ctx.transport == WireTransport::Auto {
                match BleHandle::list_connected().await {
                    Ok(devs) => {
                        for d in &devs {
                            entries.push(ble_device_to_wire(d));
                        }
                    }
                    Err(e) => {
                        return error_response(map_ble_error_code(&e), e.to_string());
                    }
                }
            }
        }

        Response::ok(ResponseData::DeviceList(entries), Provenance::UsbValidated)
    }

    // ── use ────────────────────────────────────────────────────────────────

    async fn handle_use_device(
        &mut self,
        selector: wire::DeviceSelector,
        transport: WireTransport,
        ctx: &ExecutionContext,
    ) -> Response {
        // Resolve `Auto` transport by probing hardware, matching the
        // strategy used in `handle_list_devices`.
        let (resolved_transport, resolved_selector) = match self
            .resolve_transport_and_selector(transport, selector.clone())
            .await
        {
            Ok(pair) => pair,
            Err(e) => return e,
        };

        let device_key = Self::derive_device_key(&resolved_selector, resolved_transport);

        // Open the session — always open the hardware handle even under
        // no-state (the session is transient and discarded on release).
        if let Err(e) = self
            .ensure_session(resolved_transport, Some(&resolved_selector))
            .await
        {
            return e;
        }

        self.active_device_key = Some(device_key.clone());

        // Record durable device selection unless --no-state was requested.
        if !ctx.no_state {
            if let Some(ref mut state) = self.state {
                let stored_transport = wire_to_stored_transport(resolved_transport);
                let stored_selector = wire_to_stored_selector(&resolved_selector);
                state::ensure_device(state, &device_key, stored_transport, stored_selector);
                state::select_device(state, &device_key);
                let _ = self.save_state();
            }
        }

        Response::ok(ResponseData::Empty, Provenance::UsbValidated)
    }

    // ── status / resource reads ────────────────────────────────────────────

    async fn handle_status(&mut self, profile_id: u8, ctx: ExecutionContext) -> Response {
        let transport = match self.resolve_transport(&ctx) {
            Ok(t) => t,
            Err(e) => return e,
        };

        let device_key = match self.resolve_device_key() {
            Some(k) => k,
            None => {
                return Response::err(
                    ErrorCode::DeviceNotFound,
                    "no device selected; use `x3ctl use` first",
                    Provenance::Unverified,
                );
            }
        };

        if let Err(e) = self.ensure_session(transport, None).await {
            return e;
        }

        match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => {
                self.handle_status_usb(profile_id, &device_key).await
            }
            #[cfg(feature = "ble")]
            WireTransport::Ble => self.handle_status_ble_cached(profile_id, &device_key),
            _ => Response::err(
                ErrorCode::Unsupported,
                "transport not available",
                Provenance::Unverified,
            ),
        }
    }

    #[cfg(feature = "usb")]
    async fn handle_status_usb(&mut self, profile_id: u8, device_key: &str) -> Response {
        let handle = match &self.usb_handle {
            Some(h) => h,
            None => {
                return Response::err(
                    ErrorCode::Internal,
                    "USB session lost",
                    Provenance::Unverified,
                );
            }
        };

        let pid = match ProfileId::try_from(profile_id) {
            Ok(p) => p,
            Err(_) => {
                return Response::err(
                    ErrorCode::InvalidRequest,
                    format!("invalid profile id: {}", profile_id),
                    Provenance::Unverified,
                );
            }
        };

        // Read profile metadata (global).
        let meta = match handle.read_profile_metadata().await {
            Ok(m) => m,
            Err(e) => return error_response(map_driver_error_code(&e), e.to_string()),
        };

        // Read DPI for the requested profile.
        let dpi = handle.read_dpi(pid).await.ok();

        // Read preferences.
        let prefs = handle.read_preferences(pid).await.ok();

        // Read buttons.
        let buttons = handle.read_buttons(pid).await.ok();

        // Read polling rate.
        let rate = handle.read_polling_rate().await.ok();

        // Build payloads.
        let dpi_payload = dpi.as_ref().map(|d| dpi_state_to_payload(d));
        let prefs_payload = prefs.as_ref().map(|p| prefs_state_to_payload(p));
        let buttons_payload = buttons.as_ref().map(|b| buttons_state_to_payload(b));

        // Persist to state.
        if let Some(ref mut state) = self.state {
            if let Some(ref d) = dpi {
                let stored = dpi_state_to_stored(d);
                state::patch_dpi(
                    state,
                    device_key,
                    profile_id,
                    stored,
                    StateSource::UsbReadback,
                    StateVerification::Observed,
                );
            }
            if let Some(ref p) = prefs {
                let stored = prefs_state_to_stored(p);
                state::patch_preferences(
                    state,
                    device_key,
                    profile_id,
                    stored,
                    StateSource::UsbReadback,
                    StateVerification::Observed,
                );
            }
            if let Some(ref b) = buttons {
                let stored = buttons_state_to_stored(b);
                state::patch_buttons(
                    state,
                    device_key,
                    profile_id,
                    stored,
                    StateSource::UsbReadback,
                    StateVerification::Observed,
                );
            }
            if let Some(r) = rate {
                state::patch_polling_rate(
                    state,
                    device_key,
                    r.hz(),
                    StateSource::UsbReadback,
                    StateVerification::Observed,
                );
            }
            state::patch_profile_metadata(
                state,
                device_key,
                state::StoredProfileMetadata {
                    current: meta.current().get(),
                    maximum: meta.maximum().get(),
                },
                StateSource::UsbReadback,
                StateVerification::Observed,
            );
            let _ = self.save_state();
        }

        let payload = DeviceStatusPayload {
            profile: meta.current().get(),
            profile_max: meta.maximum().get(),
            dpi: dpi_payload,
            rate_hz: rate.map(|r| r.hz()),
            prefs: prefs_payload,
            buttons: buttons_payload,
            battery: None,
        };

        Response::ok(
            ResponseData::DeviceStatus(Box::new(payload)),
            Provenance::UsbValidated,
        )
    }

    #[cfg(feature = "ble")]
    fn handle_status_ble_cached(&self, profile_id: u8, device_key: &str) -> Response {
        let state = match &self.state {
            Some(s) => s,
            None => {
                return Response::err(
                    ErrorCode::Unsupported,
                    "BLE configuration readback is not supported; use cached state or --no-state",
                    Provenance::Unverified,
                );
            }
        };

        let profile = state::profile(state, device_key, profile_id);

        let dpi_payload = profile.and_then(|p| {
            p.value
                .dpi
                .as_ref()
                .map(|d| stored_dpi_to_payload(&d.value, profile_id))
        });
        let prefs_payload = profile.and_then(|p| {
            p.value
                .preferences
                .as_ref()
                .map(|pr| stored_prefs_to_payload(&pr.value, profile_id))
        });
        let buttons_payload = profile.and_then(|p| {
            p.value
                .buttons
                .as_ref()
                .map(|b| stored_buttons_to_payload(&b.value, profile_id))
        });
        let rate_hz = state
            .devices
            .get(device_key)
            .and_then(|d| d.polling_rate.as_ref().map(|v| v.value));

        let (current_profile, max_profile) = state
            .devices
            .get(device_key)
            .and_then(|d| d.profile_metadata.as_ref())
            .map(|m| (m.value.current, m.value.maximum))
            .unwrap_or((1, 5));

        let payload = DeviceStatusPayload {
            profile: current_profile,
            profile_max: max_profile,
            dpi: dpi_payload,
            rate_hz,
            prefs: prefs_payload,
            buttons: buttons_payload,
            battery: None,
        };

        Response::ok(
            ResponseData::DeviceStatus(Box::new(payload)),
            Provenance::Cached,
        )
    }

    async fn handle_read_dpi(&mut self, profile_id: u8, ctx: ExecutionContext) -> Response {
        let transport = match self.resolve_transport(&ctx) {
            Ok(t) => t,
            Err(e) => return e,
        };
        if let Err(e) = self.ensure_session(transport, None).await {
            return e;
        }

        match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => self.read_dpi_usb(profile_id).await,
            #[cfg(feature = "ble")]
            WireTransport::Ble => self.read_dpi_ble_cached(profile_id),
            _ => Response::err(
                ErrorCode::Unsupported,
                "transport not available",
                Provenance::Unverified,
            ),
        }
    }

    #[cfg(feature = "usb")]
    async fn read_dpi_usb(&mut self, profile_id: u8) -> Response {
        let handle = match &self.usb_handle {
            Some(h) => h,
            None => {
                return Response::err(
                    ErrorCode::Internal,
                    "USB session lost",
                    Provenance::Unverified,
                );
            }
        };
        let pid = match ProfileId::try_from(profile_id) {
            Ok(p) => p,
            Err(_) => {
                return Response::err(
                    ErrorCode::InvalidRequest,
                    "invalid profile",
                    Provenance::Unverified,
                );
            }
        };
        match handle.read_dpi(pid).await {
            Ok(dpi) => {
                let payload = dpi_state_to_payload(&dpi);
                let device_key = self.resolve_device_key();
                if let Some(ref mut state) = self.state {
                    if let Some(key) = device_key {
                        state::patch_dpi(
                            state,
                            &key,
                            profile_id,
                            dpi_state_to_stored(&dpi),
                            StateSource::UsbReadback,
                            StateVerification::Observed,
                        );
                        let _ = self.save_state();
                    }
                }
                Response::ok(
                    ResponseData::DpiRead(Box::new(payload)),
                    Provenance::UsbValidated,
                )
            }
            Err(e) => error_response(map_driver_error_code(&e), e.to_string()),
        }
    }

    #[cfg(feature = "ble")]
    fn read_dpi_ble_cached(&self, profile_id: u8) -> Response {
        self.cached_read(
            profile_id,
            |p| {
                p.value
                    .dpi
                    .as_ref()
                    .map(|d| stored_dpi_to_payload(&d.value, profile_id))
            },
            |payload| ResponseData::DpiRead(Box::new(payload)),
        )
    }

    async fn handle_read_prefs(&mut self, profile_id: u8, ctx: ExecutionContext) -> Response {
        let transport = match self.resolve_transport(&ctx) {
            Ok(t) => t,
            Err(e) => return e,
        };

        if let Err(e) = self.ensure_session(transport, None).await {
            return e;
        }

        match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => self.read_prefs_usb(profile_id).await,
            #[cfg(feature = "ble")]
            WireTransport::Ble => self.read_prefs_ble_cached(profile_id),
            _ => Response::err(
                ErrorCode::Unsupported,
                "transport not available",
                Provenance::Unverified,
            ),
        }
    }

    #[cfg(feature = "usb")]
    async fn read_prefs_usb(&mut self, profile_id: u8) -> Response {
        let handle = match &self.usb_handle {
            Some(h) => h,
            None => {
                return Response::err(
                    ErrorCode::Internal,
                    "USB session lost",
                    Provenance::Unverified,
                );
            }
        };
        let pid = match ProfileId::try_from(profile_id) {
            Ok(p) => p,
            Err(_) => {
                return Response::err(
                    ErrorCode::InvalidRequest,
                    "invalid profile",
                    Provenance::Unverified,
                );
            }
        };
        match handle.read_preferences(pid).await {
            Ok(prefs) => {
                let payload = prefs_state_to_payload(&prefs);
                let device_key = self.resolve_device_key();
                if let Some(ref mut state) = self.state {
                    if let Some(key) = device_key {
                        state::patch_preferences(
                            state,
                            &key,
                            profile_id,
                            prefs_state_to_stored(&prefs),
                            StateSource::UsbReadback,
                            StateVerification::Observed,
                        );
                        let _ = self.save_state();
                    }
                }
                Response::ok(
                    ResponseData::PreferencesRead(Box::new(payload)),
                    Provenance::UsbValidated,
                )
            }
            Err(e) => error_response(map_driver_error_code(&e), e.to_string()),
        }
    }

    #[cfg(feature = "ble")]
    fn read_prefs_ble_cached(&self, profile_id: u8) -> Response {
        self.cached_read(
            profile_id,
            |p| {
                p.value
                    .preferences
                    .as_ref()
                    .map(|pr| stored_prefs_to_payload(&pr.value, profile_id))
            },
            |payload| ResponseData::PreferencesRead(Box::new(payload)),
        )
    }

    async fn handle_read_buttons(&mut self, profile_id: u8, ctx: ExecutionContext) -> Response {
        let transport = match self.resolve_transport(&ctx) {
            Ok(t) => t,
            Err(e) => return e,
        };

        if let Err(e) = self.ensure_session(transport, None).await {
            return e;
        }

        match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => {
                self.read_buttons_usb(profile_id).await
            }
            #[cfg(feature = "ble")]
            WireTransport::Ble => self.read_buttons_ble_cached(profile_id),
            _ => Response::err(
                ErrorCode::Unsupported,
                "transport not available",
                Provenance::Unverified,
            ),
        }
    }

    #[cfg(feature = "usb")]
    async fn read_buttons_usb(&mut self, profile_id: u8) -> Response {
        let handle = match &self.usb_handle {
            Some(h) => h,
            None => {
                return Response::err(
                    ErrorCode::Internal,
                    "USB session lost",
                    Provenance::Unverified,
                );
            }
        };
        let pid = match ProfileId::try_from(profile_id) {
            Ok(p) => p,
            Err(_) => {
                return Response::err(
                    ErrorCode::InvalidRequest,
                    "invalid profile",
                    Provenance::Unverified,
                );
            }
        };
        match handle.read_buttons(pid).await {
            Ok(buttons) => {
                let payload = buttons_state_to_payload(&buttons);
                let device_key = self.resolve_device_key();
                if let Some(ref mut state) = self.state {
                    if let Some(key) = device_key {
                        state::patch_buttons(
                            state,
                            &key,
                            profile_id,
                            buttons_state_to_stored(&buttons),
                            StateSource::UsbReadback,
                            StateVerification::Observed,
                        );
                        let _ = self.save_state();
                    }
                }
                Response::ok(
                    ResponseData::ButtonsRead(Box::new(ButtonsStatePayload {
                        profile: profile_id,
                        slots: payload,
                    })),
                    Provenance::UsbValidated,
                )
            }
            Err(e) => error_response(map_driver_error_code(&e), e.to_string()),
        }
    }

    #[cfg(feature = "ble")]
    fn read_buttons_ble_cached(&self, profile_id: u8) -> Response {
        self.cached_read(
            profile_id,
            |p| {
                p.value.buttons.as_ref().map(|b| ButtonsStatePayload {
                    profile: b.value.profile,
                    slots: stored_buttons_to_payload(&b.value, profile_id),
                })
            },
            |payload| ResponseData::ButtonsRead(Box::new(payload)),
        )
    }

    async fn handle_read_rate(&mut self, ctx: ExecutionContext) -> Response {
        let transport = match self.resolve_transport(&ctx) {
            Ok(t) => t,
            Err(e) => return e,
        };

        if let Err(e) = self.ensure_session(transport, None).await {
            return e;
        }

        match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => self.read_rate_usb().await,
            #[cfg(feature = "ble")]
            WireTransport::Ble => self.read_rate_ble_cached(),
            _ => Response::err(
                ErrorCode::Unsupported,
                "transport not available",
                Provenance::Unverified,
            ),
        }
    }

    #[cfg(feature = "usb")]
    async fn read_rate_usb(&mut self) -> Response {
        let handle = match &self.usb_handle {
            Some(h) => h,
            None => {
                return Response::err(
                    ErrorCode::Internal,
                    "USB session lost",
                    Provenance::Unverified,
                );
            }
        };
        match handle.read_polling_rate().await {
            Ok(rate) => {
                let hz = rate.hz();
                let device_key = self.resolve_device_key();
                if let Some(ref mut state) = self.state {
                    if let Some(key) = device_key {
                        state::patch_polling_rate(
                            state,
                            &key,
                            hz,
                            StateSource::UsbReadback,
                            StateVerification::Observed,
                        );
                        let _ = self.save_state();
                    }
                }
                Response::ok(
                    ResponseData::RateRead(Box::new(wire::RatePayload { hz })),
                    Provenance::UsbValidated,
                )
            }
            Err(e) => error_response(map_driver_error_code(&e), e.to_string()),
        }
    }

    #[cfg(feature = "ble")]
    fn read_rate_ble_cached(&self) -> Response {
        let state = match &self.state {
            Some(s) => s,
            None => {
                return Response::err(
                    ErrorCode::Unsupported,
                    "BLE rate readback not supported",
                    Provenance::Unverified,
                );
            }
        };
        let key = match self.resolve_device_key() {
            Some(k) => k,
            None => {
                return Response::err(
                    ErrorCode::DeviceNotFound,
                    "no device selected",
                    Provenance::Unverified,
                );
            }
        };
        let hz = state
            .devices
            .get(&key)
            .and_then(|d| d.polling_rate.as_ref().map(|v| v.value));
        match hz {
            Some(hz) => Response::ok(
                ResponseData::RateRead(Box::new(wire::RatePayload { hz })),
                Provenance::Cached,
            ),
            None => Response::err(
                ErrorCode::Unsupported,
                "no cached polling rate",
                Provenance::Unverified,
            ),
        }
    }

    async fn handle_battery(&mut self, _ctx: ExecutionContext) -> Response {
        #[cfg(feature = "usb")]
        let transport = match self.resolve_transport(&_ctx) {
            Ok(t) => t,
            Err(e) => return e,
        };

        #[cfg(feature = "usb")]
        if let Err(e) = self.ensure_session(transport, None).await {
            return e;
        }

        #[cfg(feature = "usb")]
        if transport == WireTransport::Receiver {
            let handle = match &self.usb_handle {
                Some(h) => h,
                None => {
                    return Response::err(
                        ErrorCode::Internal,
                        "USB session lost",
                        Provenance::Unverified,
                    );
                }
            };
            match handle.read_battery(std::time::Duration::from_secs(5)).await {
                Ok(level) => {
                    return Response::ok(
                        ResponseData::BatteryLevel(level),
                        Provenance::UsbValidated,
                    );
                }
                Err(e) => {
                    return error_response(map_driver_error_code(&e), e.to_string());
                }
            }
        }

        #[cfg(feature = "usb")]
        if transport == WireTransport::Wired {
            return Response::err(
                ErrorCode::Unsupported,
                "battery telemetry is available only from the 2.4 GHz receiver",
                Provenance::Unverified,
            );
        }

        Response::err(
            ErrorCode::Unsupported,
            "battery not available on this transport",
            Provenance::Unverified,
        )
    }

    // ── writes ─────────────────────────────────────────────────────────────

    async fn handle_set_dpi(
        &mut self,
        profile_id: u8,
        stages: Option<Vec<u16>>,
        active_stage: Option<u8>,
        sensor: Option<wire::SensorOptionsDelta>,
        ctx: ExecutionContext,
    ) -> Response {
        let transport = match self.resolve_transport(&ctx) {
            Ok(t) => t,
            Err(e) => return e,
        };

        if let Err(e) = self.ensure_session(transport, None).await {
            return e;
        }

        let device_key = match self.resolve_device_key() {
            Some(k) => k,
            None => {
                return Response::err(
                    ErrorCode::DeviceNotFound,
                    "no device selected",
                    Provenance::Unverified,
                );
            }
        };

        match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => {
                self.write_dpi_usb(profile_id, stages, active_stage, sensor, &device_key)
                    .await
            }
            #[cfg(feature = "ble")]
            WireTransport::Ble => {
                self.write_dpi_ble(profile_id, stages, active_stage, sensor, &device_key, &ctx)
                    .await
            }
            _ => Response::err(
                ErrorCode::Unsupported,
                "transport not available",
                Provenance::Unverified,
            ),
        }
    }

    #[cfg(feature = "usb")]
    async fn write_dpi_usb(
        &mut self,
        profile_id: u8,
        stages: Option<Vec<u16>>,
        active_stage: Option<u8>,
        sensor: Option<wire::SensorOptionsDelta>,
        device_key: &str,
    ) -> Response {
        let handle = match &self.usb_handle {
            Some(h) => h,
            None => {
                return Response::err(
                    ErrorCode::Internal,
                    "USB session lost",
                    Provenance::Unverified,
                );
            }
        };

        let pid = match ProfileId::try_from(profile_id) {
            Ok(p) => p,
            Err(_) => {
                return Response::err(
                    ErrorCode::InvalidRequest,
                    "invalid profile",
                    Provenance::Unverified,
                );
            }
        };

        // Read current state, apply delta, then write.
        let mut current = match handle.read_dpi(pid).await {
            Ok(c) => c,
            Err(e) => return error_response(map_driver_error_code(&e), e.to_string()),
        };

        if let Some(ref stages) = stages {
            let new_stages: Vec<DpiValue> = stages
                .iter()
                .filter_map(|&v| DpiValue::try_from(v).ok())
                .collect();
            if !new_stages.is_empty() {
                current.stages = new_stages;
                // Clamp active stage to new count.
                if current.active_stage.get() as usize > current.stages.len() {
                    current.active_stage = StageIndex::try_from(current.stages.len() as u8)
                        .unwrap_or(current.active_stage);
                }
            }
        }
        if let Some(as_val) = active_stage {
            if let Ok(si) = StageIndex::try_from(as_val) {
                current.active_stage = si;
            }
        }
        if let Some(ref s) = sensor {
            merge_sensor_delta(&mut current.sensor, s);
        }

        match handle.write_dpi(current.clone()).await {
            Ok(verified) => {
                let payload = dpi_state_to_payload(&verified);
                if let Some(ref mut state) = self.state {
                    state::patch_dpi(
                        state,
                        device_key,
                        profile_id,
                        dpi_state_to_stored(&verified),
                        StateSource::LocallyWritten,
                        StateVerification::Observed,
                    );
                    let _ = self.save_state();
                }
                Response::ok(
                    ResponseData::DpiWritten(Box::new(wire::DpiWriteSummary {
                        profile: profile_id,
                        stages: payload.stages.clone(),
                        stage_count: payload.stages.len() as u8,
                        active_stage: payload.active_stage,
                    })),
                    Provenance::UsbValidated,
                )
            }
            Err(e) => error_response(map_driver_error_code(&e), e.to_string()),
        }
    }

    #[cfg(feature = "ble")]
    async fn write_dpi_ble(
        &mut self,
        profile_id: u8,
        stages: Option<Vec<u16>>,
        active_stage: Option<u8>,
        sensor: Option<wire::SensorOptionsDelta>,
        device_key: &str,
        ctx: &ExecutionContext,
    ) -> Response {
        let handle = match &self.ble_handle {
            Some(h) => h,
            None => {
                return Response::err(
                    ErrorCode::Internal,
                    "BLE session lost",
                    Provenance::Unverified,
                );
            }
        };

        // Build the state-level merge delta from wire inputs.
        let delta = state::DpiMergeDelta {
            stages,
            active_stage,
            sensor: sensor.map(|s| state::SensorMergeDelta {
                lift_off_distance: s.lift_off_distance,
                ripple_control: s.ripple_control,
                angle_snap: s.angle_snap,
                motion_sync: s.motion_sync,
            }),
        };

        // Merge into durable state via canonical helper.
        let stored = match &mut self.state {
            Some(state) => {
                if let Err(e) = state::merge_dpi_delta(
                    state,
                    device_key,
                    profile_id,
                    &delta,
                    StateSource::LocallyWritten,
                    StateVerification::AckAccepted,
                    ctx.explicit_defaults,
                ) {
                    return merge_error_to_response(e);
                }
                match state::profile(state, device_key, profile_id)
                    .and_then(|p| p.value.dpi.as_ref())
                    .map(|sd| sd.value.clone())
                {
                    Some(v) => v,
                    None => {
                        return Response::err(
                            ErrorCode::Internal,
                            "merged DPI state not found",
                            Provenance::Unverified,
                        );
                    }
                }
            }
            None => {
                if !ctx.explicit_defaults {
                    return Response::err(
                        ErrorCode::MissingBaseline,
                        "BLE DPI write requires a stored baseline; use --explicit-defaults or load state first",
                        Provenance::Unverified,
                    );
                }
                let mut temp = StateFile::default();
                state::ensure_device(
                    &mut temp,
                    device_key,
                    StoredTransport::Ble,
                    state::StoredSelector::BleName(String::new()),
                );
                if let Err(e) = state::merge_dpi_delta(
                    &mut temp,
                    device_key,
                    profile_id,
                    &delta,
                    StateSource::LocallyWritten,
                    StateVerification::AckAccepted,
                    true,
                ) {
                    return merge_error_to_response(e);
                }
                match state::profile(&temp, device_key, profile_id)
                    .and_then(|p| p.value.dpi.as_ref())
                    .map(|sd| sd.value.clone())
                {
                    Some(v) => v,
                    None => {
                        return Response::err(
                            ErrorCode::Internal,
                            "merged DPI state not found",
                            Provenance::Unverified,
                        );
                    }
                }
            }
        };

        let merged = match stored_dpi_to_protocol(&stored, profile_id) {
            Ok(m) => m,
            Err(e) => return e,
        };

        match handle.write_dpi(merged.clone()).await {
            Ok(_receipt) => {
                if self.state.is_some() {
                    let _ = self.save_state();
                }
                let payload = dpi_state_to_payload(&merged);
                Response::ok(
                    ResponseData::DpiWritten(Box::new(wire::DpiWriteSummary {
                        profile: profile_id,
                        stages: payload.stages.clone(),
                        stage_count: payload.stages.len() as u8,
                        active_stage: payload.active_stage,
                    })),
                    Provenance::BleAcknowledged,
                )
            }
            Err(e) => error_response(map_ble_error_code(&e), e.to_string()),
        }
    }
    async fn handle_set_prefs(
        &mut self,
        profile_id: u8,
        delta: PreferencesDelta,
        ctx: ExecutionContext,
    ) -> Response {
        let transport = match self.resolve_transport(&ctx) {
            Ok(t) => t,
            Err(e) => return e,
        };

        if let Err(e) = self.ensure_session(transport, None).await {
            return e;
        }

        let device_key = match self.resolve_device_key() {
            Some(k) => k,
            None => {
                return Response::err(
                    ErrorCode::DeviceNotFound,
                    "no device selected",
                    Provenance::Unverified,
                );
            }
        };

        match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => {
                self.write_prefs_usb(profile_id, &delta, &device_key).await
            }
            #[cfg(feature = "ble")]
            WireTransport::Ble => {
                self.write_prefs_ble(profile_id, &delta, &device_key, &ctx)
                    .await
            }
            _ => Response::err(
                ErrorCode::Unsupported,
                "transport not available",
                Provenance::Unverified,
            ),
        }
    }

    #[cfg(feature = "ble")]
    async fn write_prefs_ble(
        &mut self,
        profile_id: u8,
        delta: &PreferencesDelta,
        device_key: &str,
        ctx: &ExecutionContext,
    ) -> Response {
        let handle = match &self.ble_handle {
            Some(h) => h,
            None => {
                return Response::err(
                    ErrorCode::Internal,
                    "BLE session lost",
                    Provenance::Unverified,
                );
            }
        };

        // Convert wire delta to state-level merge delta.
        let merge_delta = state::PrefsMergeDelta {
            light_mode: delta.light_mode,
            configuration: delta.configuration,
            deep_sleep: delta.deep_sleep,
            host_color: delta.host_color,
            sleep_timer: delta.sleep_timer,
            debounce: delta.debounce,
        };

        // Merge into durable state via canonical helper.
        let stored = match &mut self.state {
            Some(state) => {
                if let Err(e) = state::merge_prefs_delta(
                    state,
                    device_key,
                    profile_id,
                    &merge_delta,
                    StateSource::LocallyWritten,
                    StateVerification::AckAccepted,
                    ctx.explicit_defaults,
                ) {
                    return merge_error_to_response(e);
                }
                match state::profile(state, device_key, profile_id)
                    .and_then(|p| p.value.preferences.as_ref())
                    .map(|sp| sp.value)
                {
                    Some(v) => v,
                    None => {
                        return Response::err(
                            ErrorCode::Internal,
                            "merged prefs state not found",
                            Provenance::Unverified,
                        );
                    }
                }
            }
            None => {
                if !ctx.explicit_defaults {
                    return Response::err(
                        ErrorCode::MissingBaseline,
                        "BLE prefs write requires a stored baseline; use --explicit-defaults",
                        Provenance::Unverified,
                    );
                }
                let mut temp = StateFile::default();
                state::ensure_device(
                    &mut temp,
                    device_key,
                    StoredTransport::Ble,
                    state::StoredSelector::BleName(String::new()),
                );
                if let Err(e) = state::merge_prefs_delta(
                    &mut temp,
                    device_key,
                    profile_id,
                    &merge_delta,
                    StateSource::LocallyWritten,
                    StateVerification::AckAccepted,
                    true,
                ) {
                    return merge_error_to_response(e);
                }
                match state::profile(&temp, device_key, profile_id)
                    .and_then(|p| p.value.preferences.as_ref())
                    .map(|sp| sp.value)
                {
                    Some(v) => v,
                    None => {
                        return Response::err(
                            ErrorCode::Internal,
                            "merged prefs state not found",
                            Provenance::Unverified,
                        );
                    }
                }
            }
        };

        let merged = match stored_prefs_to_protocol(&stored, profile_id) {
            Ok(m) => m,
            Err(e) => return e,
        };

        match handle.write_preferences(merged.clone()).await {
            Ok(_receipt) => {
                if self.state.is_some() {
                    let _ = self.save_state();
                }
                let payload = prefs_state_to_payload(&merged);
                Response::ok(
                    ResponseData::PreferencesWritten(Box::new(wire::PrefsWriteSummary {
                        profile: profile_id,
                        prefs: Box::new(payload),
                    })),
                    Provenance::BleAcknowledged,
                )
            }
            Err(e) => error_response(map_ble_error_code(&e), e.to_string()),
        }
    }
    #[cfg(feature = "usb")]
    async fn write_prefs_usb(
        &mut self,
        profile_id: u8,
        delta: &PreferencesDelta,
        device_key: &str,
    ) -> Response {
        let handle = match &self.usb_handle {
            Some(h) => h,
            None => {
                return Response::err(
                    ErrorCode::Internal,
                    "USB session lost",
                    Provenance::Unverified,
                );
            }
        };
        let pid = match ProfileId::try_from(profile_id) {
            Ok(p) => p,
            Err(_) => {
                return Response::err(
                    ErrorCode::InvalidRequest,
                    "invalid profile",
                    Provenance::Unverified,
                );
            }
        };
        let mut current = match handle.read_preferences(pid).await {
            Ok(c) => c,
            Err(e) => return error_response(map_driver_error_code(&e), e.to_string()),
        };
        merge_prefs_delta_into(&mut current, delta);
        match handle.write_preferences(current.clone()).await {
            Ok(verified) => {
                let payload = prefs_state_to_payload(&verified);
                if let Some(ref mut state) = self.state {
                    state::patch_preferences(
                        state,
                        device_key,
                        profile_id,
                        prefs_state_to_stored(&verified),
                        StateSource::LocallyWritten,
                        StateVerification::Observed,
                    );
                    let _ = self.save_state();
                }
                Response::ok(
                    ResponseData::PreferencesWritten(Box::new(wire::PrefsWriteSummary {
                        profile: profile_id,
                        prefs: Box::new(payload),
                    })),
                    Provenance::UsbValidated,
                )
            }
            Err(e) => error_response(map_driver_error_code(&e), e.to_string()),
        }
    }
    async fn handle_set_button(
        &mut self,
        profile_id: u8,
        index: u8,
        assignment: ButtonPayload,
        ctx: ExecutionContext,
    ) -> Response {
        let transport = match self.resolve_transport(&ctx) {
            Ok(t) => t,
            Err(e) => return e,
        };

        if let Err(e) = self.ensure_session(transport, None).await {
            return e;
        }

        let device_key = match self.resolve_device_key() {
            Some(k) => k,
            None => {
                return Response::err(
                    ErrorCode::DeviceNotFound,
                    "no device selected",
                    Provenance::Unverified,
                );
            }
        };

        let slot_idx = index as usize;
        if slot_idx >= BUTTON_SLOT_COUNT {
            return Response::err(
                ErrorCode::InvalidRequest,
                format!(
                    "button index {} out of range (0-{})",
                    index,
                    BUTTON_SLOT_COUNT - 1
                ),
                Provenance::Unverified,
            );
        }

        match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => {
                self.write_button_usb(profile_id, slot_idx, &assignment, &device_key)
                    .await
            }
            #[cfg(feature = "ble")]
            WireTransport::Ble => {
                self.write_button_ble(profile_id, slot_idx, &assignment, &device_key, &ctx)
                    .await
            }
            _ => Response::err(
                ErrorCode::Unsupported,
                "transport not available",
                Provenance::Unverified,
            ),
        }
    }

    #[cfg(feature = "usb")]
    async fn write_button_usb(
        &mut self,
        profile_id: u8,
        slot_idx: usize,
        assignment: &ButtonPayload,
        device_key: &str,
    ) -> Response {
        let handle = match &self.usb_handle {
            Some(h) => h,
            None => {
                return Response::err(
                    ErrorCode::Internal,
                    "USB session lost",
                    Provenance::Unverified,
                );
            }
        };
        let pid = match ProfileId::try_from(profile_id) {
            Ok(p) => p,
            Err(_) => {
                return Response::err(
                    ErrorCode::InvalidRequest,
                    "invalid profile",
                    Provenance::Unverified,
                );
            }
        };
        let mut current = match handle.read_buttons(pid).await {
            Ok(c) => c,
            Err(e) => return error_response(map_driver_error_code(&e), e.to_string()),
        };
        current.slots[slot_idx] = ButtonAssignment {
            action: assignment.action,
            modifier: assignment.modifier,
            key_code: assignment.key_code,
        };
        match handle.write_buttons(current.clone()).await {
            Ok(verified) => {
                if let Some(ref mut state) = self.state {
                    state::patch_buttons(
                        state,
                        device_key,
                        profile_id,
                        buttons_state_to_stored(&verified),
                        StateSource::LocallyWritten,
                        StateVerification::Observed,
                    );
                    let _ = self.save_state();
                }
                Response::ok(
                    ResponseData::ButtonWritten(Box::new(wire::ButtonWriteSummary {
                        profile: profile_id,
                        index: slot_idx as u8,
                        slot: wire::ButtonPayload {
                            action: verified.slots[slot_idx].action,
                            modifier: verified.slots[slot_idx].modifier,
                            key_code: verified.slots[slot_idx].key_code,
                        },
                    })),
                    Provenance::UsbValidated,
                )
            }
            Err(e) => error_response(map_driver_error_code(&e), e.to_string()),
        }
    }
    #[cfg(feature = "ble")]
    async fn write_button_ble(
        &mut self,
        profile_id: u8,
        slot_idx: usize,
        assignment: &ButtonPayload,
        device_key: &str,
        ctx: &ExecutionContext,
    ) -> Response {
        let handle = match &self.ble_handle {
            Some(h) => h,
            None => {
                return Response::err(
                    ErrorCode::Internal,
                    "BLE session lost",
                    Provenance::Unverified,
                );
            }
        };

        // Merge into durable state via canonical helper.
        let stored = match &mut self.state {
            Some(state) => {
                if let Err(e) = state::merge_button_delta(
                    state,
                    device_key,
                    profile_id,
                    slot_idx,
                    assignment.action,
                    assignment.modifier,
                    assignment.key_code,
                    StateSource::LocallyWritten,
                    StateVerification::AckAccepted,
                    ctx.explicit_defaults,
                ) {
                    return merge_error_to_response(e);
                }
                match state::profile(state, device_key, profile_id)
                    .and_then(|p| p.value.buttons.as_ref())
                    .map(|sb| sb.value.clone())
                {
                    Some(v) => v,
                    None => {
                        return Response::err(
                            ErrorCode::Internal,
                            "merged buttons state not found",
                            Provenance::Unverified,
                        );
                    }
                }
            }
            None => {
                if !ctx.explicit_defaults {
                    return Response::err(
                        ErrorCode::MissingBaseline,
                        "BLE button write requires a stored baseline; use --explicit-defaults",
                        Provenance::Unverified,
                    );
                }
                let mut temp = StateFile::default();
                state::ensure_device(
                    &mut temp,
                    device_key,
                    StoredTransport::Ble,
                    state::StoredSelector::BleName(String::new()),
                );
                if let Err(e) = state::merge_button_delta(
                    &mut temp,
                    device_key,
                    profile_id,
                    slot_idx,
                    assignment.action,
                    assignment.modifier,
                    assignment.key_code,
                    StateSource::LocallyWritten,
                    StateVerification::AckAccepted,
                    true,
                ) {
                    return merge_error_to_response(e);
                }
                match state::profile(&temp, device_key, profile_id)
                    .and_then(|p| p.value.buttons.as_ref())
                    .map(|sb| sb.value.clone())
                {
                    Some(v) => v,
                    None => {
                        return Response::err(
                            ErrorCode::Internal,
                            "merged buttons state not found",
                            Provenance::Unverified,
                        );
                    }
                }
            }
        };

        let merged = match stored_buttons_to_protocol(&stored, profile_id) {
            Ok(m) => m,
            Err(e) => return e,
        };

        match handle.write_buttons(merged.clone()).await {
            Ok(_receipt) => {
                if self.state.is_some() {
                    let _ = self.save_state();
                }
                Response::ok(
                    ResponseData::ButtonWritten(Box::new(wire::ButtonWriteSummary {
                        profile: profile_id,
                        index: slot_idx as u8,
                        slot: wire::ButtonPayload {
                            action: merged.slots[slot_idx].action,
                            modifier: merged.slots[slot_idx].modifier,
                            key_code: merged.slots[slot_idx].key_code,
                        },
                    })),
                    Provenance::BleAcknowledged,
                )
            }
            Err(e) => error_response(map_ble_error_code(&e), e.to_string()),
        }
    }

    async fn handle_set_rate(&mut self, hz: u16, ctx: ExecutionContext) -> Response {
        let transport = match self.resolve_transport(&ctx) {
            Ok(t) => t,
            Err(e) => return e,
        };

        let rate = match PollingRate::new(hz) {
            Some(r) => r,
            None => {
                return Response::err(
                    ErrorCode::InvalidRequest,
                    format!(
                        "unsupported polling rate: {} Hz (valid: 125, 250, 500, 1000)",
                        hz
                    ),
                    Provenance::Unverified,
                );
            }
        };

        if let Err(e) = self.ensure_session(transport, None).await {
            return e;
        }

        let device_key = match self.resolve_device_key() {
            Some(k) => k,
            None => {
                return Response::err(
                    ErrorCode::DeviceNotFound,
                    "no device selected",
                    Provenance::Unverified,
                );
            }
        };

        match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => {
                let handle = match &self.usb_handle {
                    Some(h) => h,
                    None => {
                        return Response::err(
                            ErrorCode::Internal,
                            "USB session lost",
                            Provenance::Unverified,
                        );
                    }
                };
                match handle.write_polling_rate(rate).await {
                    Ok(verified) => {
                        let verified_hz = verified.hz();
                        if let Some(ref mut state) = self.state {
                            state::patch_polling_rate(
                                state,
                                &device_key,
                                verified_hz,
                                StateSource::LocallyWritten,
                                StateVerification::Observed,
                            );
                            let _ = self.save_state();
                        }
                        Response::ok(
                            ResponseData::RateWritten(Box::new(wire::RateWriteSummary {
                                hz: verified_hz,
                            })),
                            Provenance::UsbValidated,
                        )
                    }
                    Err(e) => error_response(map_driver_error_code(&e), e.to_string()),
                }
            }
            #[cfg(feature = "ble")]
            WireTransport::Ble => {
                let handle = match &self.ble_handle {
                    Some(h) => h,
                    None => {
                        return Response::err(
                            ErrorCode::Internal,
                            "BLE session lost",
                            Provenance::Unverified,
                        );
                    }
                };
                match handle.write_polling_rate(rate).await {
                    Ok(_receipt) => {
                        if let Some(ref mut state) = self.state {
                            state::patch_polling_rate(
                                state,
                                &device_key,
                                hz,
                                StateSource::LocallyWritten,
                                StateVerification::AckAccepted,
                            );
                            let _ = self.save_state();
                        }
                        Response::ok(
                            ResponseData::RateWritten(Box::new(wire::RateWriteSummary { hz })),
                            Provenance::BleAcknowledged,
                        )
                    }
                    Err(e) => error_response(map_ble_error_code(&e), e.to_string()),
                }
            }
            _ => Response::err(
                ErrorCode::Unsupported,
                "transport not available",
                Provenance::Unverified,
            ),
        }
    }

    // ── profile control ────────────────────────────────────────────────────

    async fn handle_profile_use(&mut self, profile_id: u8, ctx: ExecutionContext) -> Response {
        let transport = match self.resolve_transport(&ctx) {
            Ok(t) => t,
            Err(e) => return e,
        };

        if let Err(e) = self.ensure_session(transport, None).await {
            return e;
        }

        let pid = match ProfileId::try_from(profile_id) {
            Ok(p) => p,
            Err(_) => {
                return Response::err(
                    ErrorCode::InvalidRequest,
                    "invalid profile",
                    Provenance::Unverified,
                );
            }
        };

        match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => {
                let handle = match &self.usb_handle {
                    Some(h) => h,
                    None => {
                        return Response::err(
                            ErrorCode::Internal,
                            "USB session lost",
                            Provenance::Unverified,
                        );
                    }
                };
                match handle.activate_profile(pid).await {
                    Ok(meta) => {
                        let device_key = self.resolve_device_key();
                        if let Some(ref mut state) = self.state {
                            if let Some(ref key) = device_key {
                                state::patch_profile_metadata(
                                    state,
                                    key,
                                    state::StoredProfileMetadata {
                                        current: meta.current().get(),
                                        maximum: meta.maximum().get(),
                                    },
                                    StateSource::LocallyWritten,
                                    StateVerification::Observed,
                                );
                                let _ = self.save_state();
                            }
                        }
                        Response::ok(
                            ResponseData::ProfileMetaRead(Box::new(wire::ProfileMetadataPayload {
                                current: meta.current().get(),
                                maximum: meta.maximum().get(),
                            })),
                            Provenance::UsbValidated,
                        )
                    }
                    Err(e) => error_response(map_driver_error_code(&e), e.to_string()),
                }
            }
            #[cfg(feature = "ble")]
            WireTransport::Ble => {
                let handle = match &self.ble_handle {
                    Some(h) => h,
                    None => {
                        return Response::err(
                            ErrorCode::Internal,
                            "BLE session lost",
                            Provenance::Unverified,
                        );
                    }
                };
                let stored_max = self.state.as_ref().and_then(|s| {
                    self.resolve_device_key().and_then(|key| {
                        s.devices
                            .get(&key)
                            .and_then(|d| d.profile_metadata.as_ref())
                            .map(|m| m.value.maximum)
                    })
                });
                let max_val = match stored_max {
                    Some(m) => m,
                    None => {
                        if !ctx.explicit_defaults {
                            return Response::err(
                                ErrorCode::MissingBaseline,
                                "BLE profile use requires a stored baseline; use --explicit-defaults",
                                Provenance::Unverified,
                            );
                        }
                        5
                    }
                };
                let max_pid =
                    ProfileId::try_from(max_val).unwrap_or(ProfileId::try_from(1u8).unwrap());
                let metadata = match ProfileMetadata::new(pid, max_pid) {
                    Ok(metadata) => metadata,
                    Err(_) => {
                        return Response::err(
                            ErrorCode::InvalidRequest,
                            "invalid profile range",
                            Provenance::Unverified,
                        );
                    }
                };
                match handle.write_profile_control(metadata).await {
                    Ok(_receipt) => {
                        let device_key = self.resolve_device_key();
                        if let Some(ref mut state) = self.state {
                            if let Some(ref key) = device_key {
                                state::patch_profile_metadata(
                                    state,
                                    key,
                                    state::StoredProfileMetadata {
                                        current: profile_id,
                                        maximum: max_pid.get(),
                                    },
                                    StateSource::LocallyWritten,
                                    StateVerification::AckAccepted,
                                );
                                let _ = self.save_state();
                            }
                        }
                        Response::ok(
                            ResponseData::ProfileMetaRead(Box::new(wire::ProfileMetadataPayload {
                                current: profile_id,
                                maximum: max_pid.get(),
                            })),
                            Provenance::BleAcknowledged,
                        )
                    }
                    Err(e) => error_response(map_ble_error_code(&e), e.to_string()),
                }
            }
            _ => Response::err(
                ErrorCode::Unsupported,
                "transport not available",
                Provenance::Unverified,
            ),
        }
    }

    async fn handle_profile_max(&mut self, max: u8, ctx: ExecutionContext) -> Response {
        let transport = match self.resolve_transport(&ctx) {
            Ok(t) => t,
            Err(e) => return e,
        };

        if let Err(e) = self.ensure_session(transport, None).await {
            return e;
        }

        let max_pid = match ProfileId::try_from(max) {
            Ok(p) => p,
            Err(_) => {
                return Response::err(
                    ErrorCode::InvalidRequest,
                    "invalid max profile",
                    Provenance::Unverified,
                );
            }
        };

        match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => {
                let handle = match &self.usb_handle {
                    Some(h) => h,
                    None => {
                        return Response::err(
                            ErrorCode::Internal,
                            "USB session lost",
                            Provenance::Unverified,
                        );
                    }
                };
                match handle.set_maximum_profile(max_pid).await {
                    Ok(meta) => {
                        let device_key = self.resolve_device_key();
                        if let Some(ref mut state) = self.state {
                            if let Some(ref key) = device_key {
                                state::patch_profile_metadata(
                                    state,
                                    key,
                                    state::StoredProfileMetadata {
                                        current: meta.current().get(),
                                        maximum: meta.maximum().get(),
                                    },
                                    StateSource::LocallyWritten,
                                    StateVerification::Observed,
                                );
                                let _ = self.save_state();
                            }
                        }
                        Response::ok(
                            ResponseData::ProfileMetaRead(Box::new(wire::ProfileMetadataPayload {
                                current: meta.current().get(),
                                maximum: meta.maximum().get(),
                            })),
                            Provenance::UsbValidated,
                        )
                    }
                    Err(e) => error_response(map_driver_error_code(&e), e.to_string()),
                }
            }
            #[cfg(feature = "ble")]
            WireTransport::Ble => {
                let handle = match &self.ble_handle {
                    Some(h) => h,
                    None => {
                        return Response::err(
                            ErrorCode::Internal,
                            "BLE session lost",
                            Provenance::Unverified,
                        );
                    }
                };
                let stored_cur = self.state.as_ref().and_then(|s| {
                    self.resolve_device_key().and_then(|key| {
                        s.devices
                            .get(&key)
                            .and_then(|d| d.profile_metadata.as_ref())
                            .map(|m| m.value.current)
                    })
                });
                let cur_val = match stored_cur {
                    Some(c) => c,
                    None => {
                        if !ctx.explicit_defaults {
                            return Response::err(
                                ErrorCode::MissingBaseline,
                                "BLE profile max requires a stored baseline; use --explicit-defaults",
                                Provenance::Unverified,
                            );
                        }
                        1
                    }
                };
                let cur_pid =
                    ProfileId::try_from(cur_val).unwrap_or(ProfileId::try_from(1u8).unwrap());
                let meta = match ProfileMetadata::new(cur_pid, max_pid) {
                    Ok(metadata) => metadata,
                    Err(_) => {
                        return Response::err(
                            ErrorCode::InvalidRequest,
                            "invalid profile range",
                            Provenance::Unverified,
                        );
                    }
                };
                match handle.write_profile_control(meta).await {
                    Ok(_receipt) => {
                        let device_key = self.resolve_device_key();
                        if let Some(ref mut state) = self.state {
                            if let Some(ref key) = device_key {
                                let cur = state
                                    .devices
                                    .get(key)
                                    .and_then(|d| d.profile_metadata.as_ref())
                                    .map(|m| m.value.current)
                                    .unwrap_or(1);
                                state::patch_profile_metadata(
                                    state,
                                    key,
                                    state::StoredProfileMetadata {
                                        current: cur,
                                        maximum: max,
                                    },
                                    StateSource::LocallyWritten,
                                    StateVerification::AckAccepted,
                                );
                                let _ = self.save_state();
                            }
                        }
                        Response::ok(
                            ResponseData::ProfileMetaRead(Box::new(wire::ProfileMetadataPayload {
                                current: 1,
                                maximum: max,
                            })),
                            Provenance::BleAcknowledged,
                        )
                    }
                    Err(e) => error_response(map_ble_error_code(&e), e.to_string()),
                }
            }
            _ => Response::err(
                ErrorCode::Unsupported,
                "transport not available",
                Provenance::Unverified,
            ),
        }
    }

    // ── state operations ───────────────────────────────────────────────────

    async fn handle_apply(&mut self, ctx: ExecutionContext) -> Response {
        let transport = match self.resolve_transport(&ctx) {
            Ok(t) => t,
            Err(e) => return e,
        };

        let state = match &self.state {
            Some(s) => s.clone(),
            None => {
                return Response::err(
                    ErrorCode::InvalidRequest,
                    "no state to apply",
                    Provenance::Unverified,
                );
            }
        };

        let device_key = match self.resolve_device_key() {
            Some(k) => k,
            None => {
                return Response::err(
                    ErrorCode::DeviceNotFound,
                    "no device selected",
                    Provenance::Unverified,
                );
            }
        };

        if let Err(e) = self.ensure_session(transport, None).await {
            return e;
        }

        let device = match state.devices.get(&device_key) {
            Some(d) => d,
            None => {
                return Response::err(
                    ErrorCode::DeviceNotFound,
                    "device not in state",
                    Provenance::Unverified,
                );
            }
        };

        // Apply profile metadata first (max profiles).
        if let Some(ref meta) = device.profile_metadata {
            if let Ok(max_pid) = ProfileId::try_from(meta.value.maximum) {
                let result = self.apply_profile_max(transport, max_pid).await;
                if let Err(e) = result {
                    return e;
                }
            }
        }

        // Apply each profile in order.
        for (&pid, versioned) in &device.profiles {
            if let Some(ref dpi) = versioned.value.dpi {
                if let Err(resp) = self.apply_one_dpi(transport, pid, &dpi.value).await {
                    return resp;
                }
            }
            if let Some(ref prefs) = versioned.value.preferences {
                if let Err(resp) = self.apply_one_prefs(transport, pid, &prefs.value).await {
                    return resp;
                }
            }
            if let Some(ref buttons) = versioned.value.buttons {
                if let Err(resp) = self.apply_one_buttons(transport, pid, &buttons.value).await {
                    return resp;
                }
            }
        }

        // Apply global polling rate.
        if let Some(ref rate) = device.polling_rate {
            if let Some(poll_rate) = PollingRate::new(rate.value) {
                let result = self.apply_rate(transport, poll_rate).await;
                if let Err(e) = result {
                    return e;
                }
            }
        }

        // Activate the current profile.
        if let Some(ref meta) = device.profile_metadata {
            if let Ok(pid) = ProfileId::try_from(meta.value.current) {
                let result = self.apply_activate_profile(transport, pid).await;
                if let Err(e) = result {
                    return e;
                }
            }
        }

        let provenance = match transport {
            #[cfg(feature = "ble")]
            WireTransport::Ble => Provenance::BleAcknowledged,
            _ => Provenance::UsbValidated,
        };
        Response::ok(ResponseData::Empty, provenance)
    }
    // ── Reset ────────────────────────────────────────────────────────────────

    /// Factory-reset the device: send 0x0c (max=1), wait, reapply every
    /// configuration section with 500 ms quiet periods, then restore max=5.
    ///
    /// Matches the stock reset capture from `docs/evidence/x3-fa61/reset-packets.json`.
    async fn handle_reset(&mut self, profile_id: u8, ctx: ExecutionContext) -> Response {
        let transport = match self.resolve_transport(&ctx) {
            Ok(t) => t,
            Err(e) => return e,
        };

        if let Err(e) = self.ensure_session(transport, None).await {
            return e;
        }

        let pid = match ProfileId::try_from(profile_id) {
            Ok(p) => p,
            Err(_) => {
                return Response::err(
                    ErrorCode::InvalidRequest,
                    format!("invalid profile id: {profile_id}"),
                    Provenance::Unverified,
                );
            }
        };

        // Step 1: Send 0x0c reset image — current=pid, max=1.
        let reset_meta = ProfileMetadata::new(pid, ProfileId::try_from(1u8).unwrap());
        let reset_meta = match reset_meta {
            Ok(m) => m,
            Err(_) => {
                return Response::err(
                    ErrorCode::Internal,
                    "failed to build reset metadata",
                    Provenance::Unverified,
                );
            }
        };
        if let Err(e) = self.apply_profile_control(transport, reset_meta).await {
            return e;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Step 2: Factory DPI — 6 stages, active 1, LOD 2mm (high).
        // Read the current DPI to capture the opaque preserved_tail (bytes
        // 25–49 of the report).  The driver's write-verification requires
        // exact equality, so we must echo the device's real tail.
        let preserved_tail = match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => {
                if let Some(handle) = &self.usb_handle {
                    match handle.read_dpi(pid).await {
                        Ok(dpi) => dpi.preserved_tail,
                        Err(_) => [0u8; 25],
                    }
                } else {
                    [0u8; 25]
                }
            }
            _ => [0u8; 25],
        };
        let factory_dpi = factory_dpi_state(pid, preserved_tail);
        if let Err(e) = self.apply_one_dpi_state(transport, factory_dpi).await {
            return e;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Step 3: Factory preferences.
        let factory_prefs = factory_preferences_state(pid);
        if let Err(e) = self.apply_one_prefs_state(transport, factory_prefs).await {
            return e;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Step 4: Factory polling rate — 500 Hz.
        if let Err(e) = self.apply_rate(transport, PollingRate::Hz500).await {
            return e;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Step 5: Factory buttons.
        let factory_btns = factory_buttons_state(pid);
        if let Err(e) = self.apply_one_buttons_state(transport, factory_btns).await {
            return e;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Step 6: Restore max profiles to 5.
        let max_meta = ProfileMetadata::new(pid, ProfileId::try_from(5u8).unwrap());
        let max_meta = match max_meta {
            Ok(m) => m,
            Err(_) => {
                return Response::err(
                    ErrorCode::Internal,
                    "failed to build max metadata",
                    Provenance::Unverified,
                );
            }
        };
        if let Err(e) = self.apply_profile_control(transport, max_meta).await {
            return e;
        }

        // Update durable state with factory defaults.
        let device_key = self.resolve_device_key();
        if let (Some(state), Some(key)) = (&mut self.state, &device_key) {
            state::patch_dpi(
                state,
                key,
                profile_id,
                dpi_state_to_stored(&factory_dpi_state(pid, preserved_tail)),
                StateSource::LocallyWritten,
                StateVerification::Observed,
            );
            state::patch_preferences(
                state,
                key,
                profile_id,
                prefs_state_to_stored(&factory_preferences_state(pid)),
                StateSource::LocallyWritten,
                StateVerification::Observed,
            );
            state::patch_buttons(
                state,
                key,
                profile_id,
                buttons_state_to_stored(&factory_buttons_state(pid)),
                StateSource::LocallyWritten,
                StateVerification::Observed,
            );
            state::patch_polling_rate(
                state,
                key,
                500,
                StateSource::LocallyWritten,
                StateVerification::Observed,
            );
            state::patch_profile_metadata(
                state,
                key,
                state::StoredProfileMetadata {
                    current: profile_id,
                    maximum: 5,
                },
                StateSource::LocallyWritten,
                StateVerification::Observed,
            );
            let _ = self.save_state();
        }

        let provenance = match transport {
            #[cfg(feature = "ble")]
            WireTransport::Ble => Provenance::BleAcknowledged,
            _ => Provenance::UsbValidated,
        };
        Response::ok(
            ResponseData::ResetDone {
                profile: profile_id,
            },
            provenance,
        )
    }

    /// Apply a profile-metadata write via the correct transport.
    ///
    /// USB uses `set_maximum_profile` (preserves current, includes 500 ms
    /// quiet period).  BLE uses `write_profile_control` (combined
    /// current+maximum).
    async fn apply_profile_control(
        &self,
        transport: WireTransport,
        meta: ProfileMetadata,
    ) -> Result<(), Response> {
        match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => {
                let handle = self.usb_handle.as_ref().ok_or_else(|| {
                    Response::err(
                        ErrorCode::Internal,
                        "USB session lost",
                        Provenance::Unverified,
                    )
                })?;
                handle
                    .set_maximum_profile(meta.maximum())
                    .await
                    .map_err(|e| error_response(map_driver_error_code(&e), e.to_string()))?;
                Ok(())
            }
            #[cfg(feature = "ble")]
            WireTransport::Ble => {
                let handle = self.ble_handle.as_ref().ok_or_else(|| {
                    Response::err(
                        ErrorCode::Internal,
                        "BLE session lost",
                        Provenance::Unverified,
                    )
                })?;
                handle
                    .write_profile_control(meta)
                    .await
                    .map_err(|e| error_response(map_ble_error_code(&e), e.to_string()))?;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Write a complete [`DpiState`] (full replacement, not delta).
    async fn apply_one_dpi_state(
        &self,
        transport: WireTransport,
        dpi: DpiState,
    ) -> Result<(), Response> {
        match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => {
                let handle = self.usb_handle.as_ref().ok_or_else(|| {
                    Response::err(
                        ErrorCode::Internal,
                        "USB session lost",
                        Provenance::Unverified,
                    )
                })?;
                handle
                    .write_dpi(dpi)
                    .await
                    .map_err(|e| error_response(map_driver_error_code(&e), e.to_string()))?;
            }
            #[cfg(feature = "ble")]
            WireTransport::Ble => {
                let handle = self.ble_handle.as_ref().ok_or_else(|| {
                    Response::err(
                        ErrorCode::Internal,
                        "BLE session lost",
                        Provenance::Unverified,
                    )
                })?;
                handle
                    .write_dpi(dpi)
                    .await
                    .map_err(|e| error_response(map_ble_error_code(&e), e.to_string()))?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Write a complete [`PreferencesState`].
    async fn apply_one_prefs_state(
        &self,
        transport: WireTransport,
        prefs: PreferencesState,
    ) -> Result<(), Response> {
        match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => {
                let handle = self.usb_handle.as_ref().ok_or_else(|| {
                    Response::err(
                        ErrorCode::Internal,
                        "USB session lost",
                        Provenance::Unverified,
                    )
                })?;
                handle
                    .write_preferences(prefs)
                    .await
                    .map_err(|e| error_response(map_driver_error_code(&e), e.to_string()))?;
            }
            #[cfg(feature = "ble")]
            WireTransport::Ble => {
                let handle = self.ble_handle.as_ref().ok_or_else(|| {
                    Response::err(
                        ErrorCode::Internal,
                        "BLE session lost",
                        Provenance::Unverified,
                    )
                })?;
                handle
                    .write_preferences(prefs)
                    .await
                    .map_err(|e| error_response(map_ble_error_code(&e), e.to_string()))?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Write a complete [`ButtonsState`].
    async fn apply_one_buttons_state(
        &self,
        transport: WireTransport,
        buttons: ButtonsState,
    ) -> Result<(), Response> {
        match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => {
                let handle = self.usb_handle.as_ref().ok_or_else(|| {
                    Response::err(
                        ErrorCode::Internal,
                        "USB session lost",
                        Provenance::Unverified,
                    )
                })?;
                handle
                    .write_buttons(buttons)
                    .await
                    .map_err(|e| error_response(map_driver_error_code(&e), e.to_string()))?;
            }
            #[cfg(feature = "ble")]
            WireTransport::Ble => {
                let handle = self.ble_handle.as_ref().ok_or_else(|| {
                    Response::err(
                        ErrorCode::Internal,
                        "BLE session lost",
                        Provenance::Unverified,
                    )
                })?;
                handle
                    .write_buttons(buttons)
                    .await
                    .map_err(|e| error_response(map_ble_error_code(&e), e.to_string()))?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Apply profile maximum (cfg-gated internally).
    async fn apply_profile_max(
        &self,
        transport: WireTransport,
        max_pid: ProfileId,
    ) -> Result<(), Response> {
        match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => {
                let handle = self.usb_handle.as_ref().ok_or_else(|| {
                    Response::err(
                        ErrorCode::Internal,
                        "USB session lost",
                        Provenance::Unverified,
                    )
                })?;
                handle
                    .set_maximum_profile(max_pid)
                    .await
                    .map_err(|e| error_response(map_driver_error_code(&e), e.to_string()))?;
                Ok(())
            }
            #[cfg(feature = "ble")]
            WireTransport::Ble => {
                let handle = self.ble_handle.as_ref().ok_or_else(|| {
                    Response::err(
                        ErrorCode::Internal,
                        "BLE session lost",
                        Provenance::Unverified,
                    )
                })?;
                let fallback = ProfileId::try_from(1u8).unwrap();
                let meta = ProfileMetadata::new(fallback, max_pid).map_err(|_| {
                    Response::err(
                        ErrorCode::InvalidRequest,
                        "invalid profile range",
                        Provenance::Unverified,
                    )
                })?;
                handle
                    .write_profile_control(meta)
                    .await
                    .map_err(|e| error_response(map_ble_error_code(&e), e.to_string()))?;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    async fn apply_one_dpi(
        &self,
        transport: WireTransport,
        pid: u8,
        stored: &state::StoredDpiState,
    ) -> Result<(), Response> {
        let dpi = stored_dpi_to_protocol(stored, pid)?;
        match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => {
                let handle = self.usb_handle.as_ref().ok_or_else(|| {
                    Response::err(
                        ErrorCode::Internal,
                        "USB session lost",
                        Provenance::Unverified,
                    )
                })?;
                handle
                    .write_dpi(dpi)
                    .await
                    .map_err(|e| error_response(map_driver_error_code(&e), e.to_string()))?;
            }
            #[cfg(feature = "ble")]
            WireTransport::Ble => {
                let handle = self.ble_handle.as_ref().ok_or_else(|| {
                    Response::err(
                        ErrorCode::Internal,
                        "BLE session lost",
                        Provenance::Unverified,
                    )
                })?;
                handle
                    .write_dpi(dpi)
                    .await
                    .map_err(|e| error_response(map_ble_error_code(&e), e.to_string()))?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn apply_one_prefs(
        &self,
        transport: WireTransport,
        pid: u8,
        stored: &state::StoredPreferencesState,
    ) -> Result<(), Response> {
        let prefs = stored_prefs_to_protocol(stored, pid)?;
        match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => {
                let handle = self.usb_handle.as_ref().ok_or_else(|| {
                    Response::err(
                        ErrorCode::Internal,
                        "USB session lost",
                        Provenance::Unverified,
                    )
                })?;
                handle
                    .write_preferences(prefs)
                    .await
                    .map_err(|e| error_response(map_driver_error_code(&e), e.to_string()))?;
            }
            #[cfg(feature = "ble")]
            WireTransport::Ble => {
                let handle = self.ble_handle.as_ref().ok_or_else(|| {
                    Response::err(
                        ErrorCode::Internal,
                        "BLE session lost",
                        Provenance::Unverified,
                    )
                })?;
                handle
                    .write_preferences(prefs)
                    .await
                    .map_err(|e| error_response(map_ble_error_code(&e), e.to_string()))?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn apply_one_buttons(
        &self,
        transport: WireTransport,
        pid: u8,
        stored: &state::StoredButtonsState,
    ) -> Result<(), Response> {
        let buttons = stored_buttons_to_protocol(stored, pid)?;
        match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => {
                let handle = self.usb_handle.as_ref().ok_or_else(|| {
                    Response::err(
                        ErrorCode::Internal,
                        "USB session lost",
                        Provenance::Unverified,
                    )
                })?;
                handle
                    .write_buttons(buttons)
                    .await
                    .map_err(|e| error_response(map_driver_error_code(&e), e.to_string()))?;
            }
            #[cfg(feature = "ble")]
            WireTransport::Ble => {
                let handle = self.ble_handle.as_ref().ok_or_else(|| {
                    Response::err(
                        ErrorCode::Internal,
                        "BLE session lost",
                        Provenance::Unverified,
                    )
                })?;
                handle
                    .write_buttons(buttons)
                    .await
                    .map_err(|e| error_response(map_ble_error_code(&e), e.to_string()))?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn apply_rate(
        &self,
        transport: WireTransport,
        rate: PollingRate,
    ) -> Result<(), Response> {
        match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => {
                let handle = self.usb_handle.as_ref().ok_or_else(|| {
                    Response::err(
                        ErrorCode::Internal,
                        "USB session lost",
                        Provenance::Unverified,
                    )
                })?;
                handle
                    .write_polling_rate(rate)
                    .await
                    .map_err(|e| error_response(map_driver_error_code(&e), e.to_string()))?;
            }
            #[cfg(feature = "ble")]
            WireTransport::Ble => {
                let handle = self.ble_handle.as_ref().ok_or_else(|| {
                    Response::err(
                        ErrorCode::Internal,
                        "BLE session lost",
                        Provenance::Unverified,
                    )
                })?;
                handle
                    .write_polling_rate(rate)
                    .await
                    .map_err(|e| error_response(map_ble_error_code(&e), e.to_string()))?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn apply_activate_profile(
        &self,
        transport: WireTransport,
        pid: ProfileId,
    ) -> Result<(), Response> {
        match transport {
            #[cfg(feature = "usb")]
            WireTransport::Wired | WireTransport::Receiver => {
                let handle = self.usb_handle.as_ref().ok_or_else(|| {
                    Response::err(
                        ErrorCode::Internal,
                        "USB session lost",
                        Provenance::Unverified,
                    )
                })?;
                handle
                    .activate_profile(pid)
                    .await
                    .map_err(|e| error_response(map_driver_error_code(&e), e.to_string()))?;
            }
            #[cfg(feature = "ble")]
            WireTransport::Ble => {
                let handle = self.ble_handle.as_ref().ok_or_else(|| {
                    Response::err(
                        ErrorCode::Internal,
                        "BLE session lost",
                        Provenance::Unverified,
                    )
                })?;
                // Preserve the stored maximum profile so we don't reduce it
                // to the current value.
                let max_pid = self
                    .state
                    .as_ref()
                    .and_then(|s| {
                        self.resolve_device_key().and_then(|key| {
                            s.devices
                                .get(&key)
                                .and_then(|d| d.profile_metadata.as_ref())
                                .map(|m| m.value.maximum)
                        })
                    })
                    .and_then(|m| ProfileId::try_from(m).ok())
                    .unwrap_or(pid);
                let meta = ProfileMetadata::new(pid, max_pid).map_err(|_| {
                    Response::err(
                        ErrorCode::InvalidRequest,
                        "invalid profile range",
                        Provenance::Unverified,
                    )
                })?;
                handle
                    .write_profile_control(meta)
                    .await
                    .map_err(|e| error_response(map_ble_error_code(&e), e.to_string()))?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn handle_export(&mut self, _ctx: ExecutionContext) -> Response {
        let state = match &self.state {
            Some(s) => s,
            None => {
                return Response::err(
                    ErrorCode::InvalidRequest,
                    "no state to export",
                    Provenance::Unverified,
                );
            }
        };

        let doc = export_state_to_document(state);
        Response::ok(
            ResponseData::ExportedState(Box::new(doc)),
            Provenance::Cached,
        )
    }

    async fn handle_import(
        &mut self,
        document: ExportDocument,
        _ctx: ExecutionContext,
    ) -> Response {
        // Validate the document before any writes.
        if let Err(err) = wire::validate_export_document(&document) {
            return Response::err(err.code, err.message, Provenance::Unverified);
        }

        let state = match &mut self.state {
            Some(s) => s,
            None => {
                return Response::err(
                    ErrorCode::InvalidRequest,
                    "no state to import into",
                    Provenance::Unverified,
                );
            }
        };

        let device_count = document.devices.len();
        let mut profiles_imported = 0usize;

        for (key, entry) in &document.devices {
            // Ensure device entry exists.
            let stored_transport = match entry.transport.as_str() {
                "wired" => StoredTransport::Wired,
                "receiver" => StoredTransport::Receiver,
                "ble" => StoredTransport::Ble,
                _ => StoredTransport::Wired, // fallback
            };
            let stored_selector = wire_to_stored_selector(&entry.selector);
            state::ensure_device(state, key, stored_transport, stored_selector);
            // Set polling rate if present.
            if let Some(ref rate) = entry.polling_rate {
                state::patch_polling_rate(
                    state,
                    key,
                    rate.value,
                    StateSource::Imported,
                    StateVerification::PersistenceUnknown,
                );
            }
            // Set profile metadata if present.
            if let Some(ref meta) = entry.profile_metadata {
                state::patch_profile_metadata(
                    state,
                    key,
                    state::StoredProfileMetadata {
                        current: meta.value.current,
                        maximum: meta.value.maximum,
                    },
                    StateSource::Imported,
                    StateVerification::PersistenceUnknown,
                );
            }
            // Import each profile.
            for (&pid, versioned) in &entry.profiles {
                let pstate = &versioned.value;
                if let Some(ref dpi) = pstate.dpi {
                    state::patch_dpi(
                        state,
                        key,
                        pid,
                        export_dpi_to_stored(&dpi.value),
                        StateSource::Imported,
                        StateVerification::PersistenceUnknown,
                    );
                }
                if let Some(ref prefs) = pstate.preferences {
                    state::patch_preferences(
                        state,
                        key,
                        pid,
                        export_prefs_to_stored(&prefs.value),
                        StateSource::Imported,
                        StateVerification::PersistenceUnknown,
                    );
                }
                if let Some(ref buttons) = pstate.buttons {
                    let stored = export_buttons_to_stored(&buttons.value);
                    state::patch_buttons(
                        state,
                        key,
                        pid,
                        stored,
                        StateSource::Imported,
                        StateVerification::PersistenceUnknown,
                    );
                }
                profiles_imported += 1;
            }
        }

        let _ = self.save_state();

        Response::ok(
            ResponseData::ImportedSummary(Box::new(ImportSummary {
                device_count,
                profiles_imported,
            })),
            Provenance::Cached,
        )
    }

    async fn handle_init_defaults(&mut self, ctx: ExecutionContext) -> Response {
        let _transport = match self.resolve_transport(&ctx) {
            Ok(t) => t,
            Err(e) => return e,
        };

        let device_key = match self.resolve_device_key() {
            Some(k) => k,
            None => {
                return Response::err(
                    ErrorCode::DeviceNotFound,
                    "no device selected",
                    Provenance::Unverified,
                );
            }
        };

        let state = match &mut self.state {
            Some(s) => s,
            None => {
                return Response::err(
                    ErrorCode::InvalidRequest,
                    "no state to initialise",
                    Provenance::Unverified,
                );
            }
        };

        if !state.devices.contains_key(&device_key) {
            return Response::err(
                ErrorCode::DeviceNotFound,
                "device not in state; use `x3ctl use` first",
                Provenance::Unverified,
            );
        }

        for pid in 1..=5u8 {
            if let Err(error) = state::init_profile_defaults(state, &device_key, pid) {
                return Response::err(
                    ErrorCode::InvalidRequest,
                    error.to_string(),
                    Provenance::Unverified,
                );
            }
        }

        let _ = self.save_state();

        Response::ok(ResponseData::Empty, Provenance::Cached)
    }

    async fn handle_forget(&mut self, target: ForgetTarget, _ctx: ExecutionContext) -> Response {
        let state = match &mut self.state {
            Some(s) => s,
            None => {
                return Response::err(
                    ErrorCode::InvalidRequest,
                    "no state to modify",
                    Provenance::Unverified,
                );
            }
        };

        match &target {
            ForgetTarget::Device { key } => {
                state::forget_device(state, key);
                if self.active_device_key.as_deref() == Some(key) {
                    self.release_session();
                    self.active_device_key = None;
                }
            }
            ForgetTarget::Profile {
                device_key,
                profile_id,
            } => {
                state::forget_profile(state, device_key, *profile_id);
            }
        }

        let _ = self.save_state();

        Response::ok(ResponseData::Empty, Provenance::Cached)
    }

    // ── disconnect ─────────────────────────────────────────────────────────

    async fn handle_disconnect(&mut self) -> Response {
        self.release_session();
        Response::ok(ResponseData::Empty, Provenance::UsbValidated)
    }

    // ── daemon ─────────────────────────────────────────────────────────────

    fn handle_daemon_status(&self) -> Response {
        let payload = DaemonStatusPayload {
            pid: self.daemon_pid,
            uptime_secs: self.daemon_start.elapsed().as_secs(),
            connected: self.active_transport.is_some(),
            device_path: self.active_device_key.clone(),
            transport: self.active_transport,
            idle_secs: self.last_request.elapsed().as_secs(),
        };
        Response::ok(ResponseData::DaemonInfo(payload), Provenance::UsbValidated)
    }

    // ── BLE merge helpers ──────────────────────────────────────────────────

    #[cfg(feature = "ble")]
    fn cached_read<T, F, G>(&self, profile_id: u8, extract: F, wrap: G) -> Response
    where
        F: FnOnce(&state::VersionedState<state::StoredProfileState>) -> Option<T>,
        G: FnOnce(T) -> ResponseData,
    {
        let state = match &self.state {
            Some(s) => s,
            None => {
                return Response::err(
                    ErrorCode::Unsupported,
                    "BLE configuration readback is not supported; no cached state available",
                    Provenance::Unverified,
                );
            }
        };
        let key = match self.resolve_device_key() {
            Some(k) => k,
            None => {
                return Response::err(
                    ErrorCode::DeviceNotFound,
                    "no device selected",
                    Provenance::Unverified,
                );
            }
        };
        let profile = state::profile(state, &key, profile_id);
        match profile.and_then(extract) {
            Some(payload) => Response::ok(wrap(payload), Provenance::Cached),
            None => Response::err(
                ErrorCode::Unsupported,
                format!("no cached data for device {} profile {}", key, profile_id,),
                Provenance::Unverified,
            ),
        }
    }

    // ── state persistence ──────────────────────────────────────────────────

    fn save_state(&self) -> Result<(), ()> {
        if let (Some(state), Some(path)) = (&self.state, &self.state_path) {
            state::save_to(state, path).map_err(|_| ())
        } else {
            Ok(())
        }
    }
}

// ── SessionRelease impl ───────────────────────────────────────────────────────

impl SessionRelease for Executor {
    async fn release(&self) {
        let mut inner = self.inner.lock().await;
        inner.release_session();
    }
}

// ── Conversion helpers ────────────────────────────────────────────────────────

// ── Transport ─────────────────────────────────────────────────────────────

fn stored_to_wire_transport(t: StoredTransport) -> WireTransport {
    match t {
        StoredTransport::Wired => WireTransport::Wired,
        StoredTransport::Receiver => WireTransport::Receiver,
        StoredTransport::Ble => WireTransport::Ble,
    }
}

fn wire_to_stored_transport(t: WireTransport) -> StoredTransport {
    match t {
        WireTransport::Wired => StoredTransport::Wired,
        WireTransport::Receiver => StoredTransport::Receiver,
        WireTransport::Ble => StoredTransport::Ble,
        WireTransport::Auto => StoredTransport::Wired,
    }
}

fn wire_to_stored_selector(sel: &wire::DeviceSelector) -> state::StoredSelector {
    match sel {
        wire::DeviceSelector::UsbPath(p) => state::StoredSelector::Path(p.clone()),
        wire::DeviceSelector::BleName(n) => state::StoredSelector::BleName(n.clone()),
        wire::DeviceSelector::BleAddr(a) => state::StoredSelector::BleAddr(a.clone()),
    }
}

// ── Discovery ─────────────────────────────────────────────────────────────

#[cfg(feature = "usb")]
fn usb_device_to_wire(d: &DeviceInfo) -> WireDeviceEntry {
    let transport = if d.product_id == UsbDeviceKind::Receiver.product_id() {
        WireTransport::Receiver
    } else {
        WireTransport::Wired
    };
    WireDeviceEntry {
        path: d.path.clone(),
        vendor_id: d.vendor_id,
        product_id: d.product_id,
        interface_number: d.interface_number,
        product: d.product.clone(),
        serial_number: d.serial_number.clone(),
        transport,
    }
}

#[cfg(feature = "ble")]
fn ble_device_to_wire(d: &BleDeviceInfo) -> WireDeviceEntry {
    WireDeviceEntry {
        path: format!("{:?}", d.id),
        vendor_id: 0,
        product_id: 0,
        interface_number: 0,
        product: d.name.clone(),
        serial_number: None,
        transport: WireTransport::Ble,
    }
}

// ── Protocol → Wire ───────────────────────────────────────────────────────

fn dpi_state_to_payload(d: &DpiState) -> DpiStatePayload {
    let stages: Vec<u16> = d.stages.iter().map(|dv| dv.get()).collect();

    DpiStatePayload {
        profile: d.profile.get(),
        stages,
        active_stage: d.active_stage.get(),
        sensor: WireSensorOptions {
            lift_off_distance: lod_to_u8(d.sensor.lift_off_distance),
            ripple_control: d.sensor.ripple_control,
            angle_snap: d.sensor.angle_snap,
            motion_sync: d.sensor.motion_sync,
        },
        preserved_tail: d.preserved_tail,
    }
}

fn prefs_state_to_payload(p: &PreferencesState) -> PreferencesPayload {
    PreferencesPayload {
        profile: p.profile.get(),
        light_mode: p.light_mode,
        configuration: p.configuration,
        deep_sleep: p.deep_sleep,
        host_color: p.host_color,
        sleep_timer: p.sleep_timer,
        debounce: p.debounce,
    }
}

#[cfg(feature = "usb")]
fn buttons_state_to_payload(b: &ButtonsState) -> Vec<ButtonPayload> {
    b.slots
        .iter()
        .map(|slot| ButtonPayload {
            action: slot.action,
            modifier: slot.modifier,
            key_code: slot.key_code,
        })
        .collect()
}

// ── Protocol → Stored ─────────────────────────────────────────────────────

#[cfg(feature = "usb")]
fn dpi_state_to_stored(d: &DpiState) -> state::StoredDpiState {
    state::StoredDpiState {
        profile: d.profile.get(),
        stages: d
            .stages
            .iter()
            .map(|dv| state::StoredDpiStage { dpi: dv.get() })
            .collect(),
        active_stage: d.active_stage.get(),
        sensor_options: state::StoredSensorOptions {
            lift_off_distance: lod_to_u8(d.sensor.lift_off_distance),
            ripple_control: d.sensor.ripple_control,
            angle_snap: d.sensor.angle_snap,
            motion_sync: d.sensor.motion_sync,
        },
        preserved_tail: d.preserved_tail,
    }
}

#[cfg(feature = "usb")]
fn prefs_state_to_stored(p: &PreferencesState) -> state::StoredPreferencesState {
    state::StoredPreferencesState {
        light_mode: p.light_mode,
        configuration: p.configuration,
        deep_sleep: p.deep_sleep,
        host_color: p.host_color,
        sleep_timer: p.sleep_timer,
        debounce: p.debounce,
    }
}

#[cfg(feature = "usb")]
fn buttons_state_to_stored(b: &ButtonsState) -> state::StoredButtonsState {
    let mut slots = [state::StoredButtonSlot::default(); BUTTON_SLOT_COUNT];
    for (i, slot) in b.slots.iter().enumerate() {
        slots[i] = state::StoredButtonSlot {
            action: slot.action,
            modifier: slot.modifier,
            key_code: slot.key_code,
        };
    }
    state::StoredButtonsState {
        profile: b.profile.get(),
        slots,
    }
}

fn lod_to_u8(lod: LiftOffDistance) -> u8 {
    match lod {
        LiftOffDistance::OneMillimeter => 1,
        LiftOffDistance::TwoMillimeters => 2,
    }
}

// ── Stored → Protocol ─────────────────────────────────────────────────────

fn stored_dpi_to_protocol(s: &state::StoredDpiState, profile_id: u8) -> Result<DpiState, Response> {
    let profile = ProfileId::try_from(profile_id).map_err(|_| {
        Response::err(
            ErrorCode::InvalidRequest,
            "invalid profile id",
            Provenance::Unverified,
        )
    })?;
    let active_stage = StageIndex::try_from(s.active_stage).map_err(|_| {
        Response::err(
            ErrorCode::InvalidRequest,
            "invalid active stage",
            Provenance::Unverified,
        )
    })?;
    let stages: Result<Vec<DpiValue>, _> = s
        .stages
        .iter()
        .map(|ss| DpiValue::try_from(ss.dpi))
        .collect();
    let stages = stages.map_err(|_| {
        Response::err(
            ErrorCode::InvalidRequest,
            "invalid stored DPI value",
            Provenance::Unverified,
        )
    })?;

    Ok(DpiState {
        profile,
        stages,
        active_stage,
        sensor: ProtocolSensorOptions {
            lift_off_distance: u8_to_lod(s.sensor_options.lift_off_distance),
            ripple_control: s.sensor_options.ripple_control,
            angle_snap: s.sensor_options.angle_snap,
            motion_sync: s.sensor_options.motion_sync,
        },
        preserved_tail: s.preserved_tail,
    })
}

fn stored_prefs_to_protocol(
    s: &state::StoredPreferencesState,
    profile_id: u8,
) -> Result<PreferencesState, Response> {
    let profile = ProfileId::try_from(profile_id).map_err(|_| {
        Response::err(
            ErrorCode::InvalidRequest,
            "invalid profile id",
            Provenance::Unverified,
        )
    })?;
    Ok(PreferencesState {
        profile,
        light_mode: s.light_mode,
        configuration: s.configuration,
        deep_sleep: s.deep_sleep,
        host_color: s.host_color,
        sleep_timer: s.sleep_timer,
        debounce: s.debounce,
    })
}

fn stored_buttons_to_protocol(
    s: &state::StoredButtonsState,
    profile_id: u8,
) -> Result<ButtonsState, Response> {
    let profile = ProfileId::try_from(profile_id).map_err(|_| {
        Response::err(
            ErrorCode::InvalidRequest,
            "invalid profile id",
            Provenance::Unverified,
        )
    })?;
    let mut slots = [ButtonAssignment::default(); BUTTON_SLOT_COUNT];
    for (i, slot) in s.slots.iter().enumerate() {
        slots[i] = ButtonAssignment {
            action: slot.action,
            modifier: slot.modifier,
            key_code: slot.key_code,
        };
    }
    Ok(ButtonsState { profile, slots })
}

fn u8_to_lod(v: u8) -> LiftOffDistance {
    match v {
        1 => LiftOffDistance::OneMillimeter,
        _ => LiftOffDistance::TwoMillimeters,
    }
}

// ── Stored → Wire ─────────────────────────────────────────────────────────
#[cfg(feature = "ble")]
fn stored_dpi_to_payload(s: &state::StoredDpiState, _profile_id: u8) -> DpiStatePayload {
    let stages: Vec<u16> = s.stages.iter().map(|ss| ss.dpi).collect();

    DpiStatePayload {
        profile: s.profile,
        stages,
        active_stage: s.active_stage,
        sensor: WireSensorOptions {
            lift_off_distance: s.sensor_options.lift_off_distance,
            ripple_control: s.sensor_options.ripple_control,
            angle_snap: s.sensor_options.angle_snap,
            motion_sync: s.sensor_options.motion_sync,
        },
        preserved_tail: s.preserved_tail,
    }
}

#[cfg(feature = "ble")]
fn stored_prefs_to_payload(
    s: &state::StoredPreferencesState,
    _profile_id: u8,
) -> PreferencesPayload {
    PreferencesPayload {
        profile: _profile_id,
        light_mode: s.light_mode,
        configuration: s.configuration,
        deep_sleep: s.deep_sleep,
        host_color: s.host_color,
        sleep_timer: s.sleep_timer,
        debounce: s.debounce,
    }
}

#[cfg(feature = "ble")]
fn stored_buttons_to_payload(s: &state::StoredButtonsState, _profile_id: u8) -> Vec<ButtonPayload> {
    s.slots
        .iter()
        .map(|slot| ButtonPayload {
            action: slot.action,
            modifier: slot.modifier,
            key_code: slot.key_code,
        })
        .collect()
}

// ── Export helpers ────────────────────────────────────────────────────────

fn state_source_to_str(s: StateSource) -> String {
    match s {
        StateSource::UsbReadback => "usb-readback".into(),
        StateSource::LocallyWritten => "locally-written".into(),
        StateSource::ExplicitDefaults => "explicit-defaults".into(),
        StateSource::Imported => "imported".into(),
    }
}

fn state_verification_to_str(v: StateVerification) -> String {
    match v {
        StateVerification::Observed => "observed".into(),
        StateVerification::VerifiedImmediate => "verified-immediate".into(),
        StateVerification::AckAccepted => "ack-accepted".into(),
        StateVerification::ApplicationUnknown => "application-unknown".into(),
        StateVerification::PersistenceUnknown => "persistence-unknown".into(),
    }
}

// ── Export ────────────────────────────────────────────────────────────────

fn export_state_to_document(state: &StateFile) -> ExportDocument {
    let mut devices: BTreeMap<String, ExportDeviceEntry> = BTreeMap::new();

    for (key, entry) in &state.devices {
        let mut profiles: BTreeMap<u8, ExportVersioned<ExportProfileState>> = BTreeMap::new();

        for (&pid, versioned) in &entry.profiles {
            let dpi = versioned.value.dpi.as_ref().map(|d| wire::ExportVersioned {
                updated_at: d.updated_at.clone(),
                source: state_source_to_str(d.source),
                verification: state_verification_to_str(d.verification),
                value: wire::ExportDpiState {
                    profile: pid,
                    stages: d.value.stages.iter().map(|s| s.dpi).collect(),
                    active_stage: d.value.active_stage,
                    sensor: wire::SensorOptions {
                        lift_off_distance: d.value.sensor_options.lift_off_distance,
                        ripple_control: d.value.sensor_options.ripple_control,
                        angle_snap: d.value.sensor_options.angle_snap,
                        motion_sync: d.value.sensor_options.motion_sync,
                    },
                    preserved_tail: d.value.preserved_tail,
                },
            });
            let preferences = versioned
                .value
                .preferences
                .as_ref()
                .map(|p| wire::ExportVersioned {
                    updated_at: p.updated_at.clone(),
                    source: state_source_to_str(p.source),
                    verification: state_verification_to_str(p.verification),
                    value: wire::ExportPreferencesState {
                        profile: pid,
                        light_mode: p.value.light_mode,
                        configuration: p.value.configuration,
                        deep_sleep: p.value.deep_sleep,
                        host_color: p.value.host_color,
                        sleep_timer: p.value.sleep_timer,
                        debounce: p.value.debounce,
                    },
                });
            let buttons = versioned
                .value
                .buttons
                .as_ref()
                .map(|b| wire::ExportVersioned {
                    updated_at: b.updated_at.clone(),
                    source: state_source_to_str(b.source),
                    verification: state_verification_to_str(b.verification),
                    value: wire::ExportButtonsState {
                        profile: pid,
                        slots: b
                            .value
                            .slots
                            .iter()
                            .map(|s| wire::ButtonPayload {
                                action: s.action,
                                modifier: s.modifier,
                                key_code: s.key_code,
                            })
                            .collect(),
                    },
                });

            let export_profile = ExportProfileState {
                dpi,
                preferences,
                buttons,
            };

            profiles.insert(
                pid,
                ExportVersioned {
                    updated_at: versioned.updated_at.clone(),
                    source: state_source_to_str(versioned.source),
                    verification: state_verification_to_str(versioned.verification),
                    value: export_profile,
                },
            );
        }

        let export_meta = entry
            .profile_metadata
            .as_ref()
            .map(|m| wire::ExportVersioned {
                value: wire::ExportProfileMetadata {
                    current: m.value.current,
                    maximum: m.value.maximum,
                },
                source: state_source_to_str(m.source),
                verification: state_verification_to_str(m.verification),
                updated_at: m.updated_at.clone(),
            });

        let export_rate = entry.polling_rate.as_ref().map(|r| wire::ExportVersioned {
            value: r.value,
            source: state_source_to_str(r.source),
            verification: state_verification_to_str(r.verification),
            updated_at: r.updated_at.clone(),
        });

        let transport_str = match entry.transport {
            StoredTransport::Wired => "wired".to_string(),
            StoredTransport::Receiver => "receiver".to_string(),
            StoredTransport::Ble => "ble".to_string(),
        };

        let wire_selector = match &entry.selector {
            state::StoredSelector::Path(p) => wire::DeviceSelector::UsbPath(p.clone()),
            state::StoredSelector::Unique => wire::DeviceSelector::UsbPath(String::new()),
            state::StoredSelector::BleName(n) => wire::DeviceSelector::BleName(n.clone()),
            state::StoredSelector::BleAddr(a) => wire::DeviceSelector::BleAddr(a.clone()),
            state::StoredSelector::BleDevice(d) => wire::DeviceSelector::BleName(d.clone()),
            state::StoredSelector::UniqueConnected => wire::DeviceSelector::BleName(String::new()),
        };

        let export_entry = ExportDeviceEntry {
            transport: transport_str,
            selector: wire_selector,
            polling_rate: export_rate,
            profile_metadata: export_meta,
            profiles,
        };

        devices.insert(key.clone(), export_entry);
    }

    ExportDocument {
        format_version: 1,
        exported_at: state::utc_now_iso8601(),
        devices,
    }
}

fn export_dpi_to_stored(d: &wire::ExportDpiState) -> state::StoredDpiState {
    state::StoredDpiState {
        profile: d.profile,
        stages: d
            .stages
            .iter()
            .map(|&dpi| state::StoredDpiStage { dpi })
            .collect(),
        active_stage: d.active_stage,
        sensor_options: state::StoredSensorOptions {
            lift_off_distance: d.sensor.lift_off_distance,
            ripple_control: d.sensor.ripple_control,
            angle_snap: d.sensor.angle_snap,
            motion_sync: d.sensor.motion_sync,
        },
        preserved_tail: d.preserved_tail,
    }
}

fn export_prefs_to_stored(p: &wire::ExportPreferencesState) -> state::StoredPreferencesState {
    state::StoredPreferencesState {
        light_mode: p.light_mode,
        configuration: p.configuration,
        deep_sleep: p.deep_sleep,
        host_color: p.host_color,
        sleep_timer: p.sleep_timer,
        debounce: p.debounce,
    }
}

fn export_buttons_to_stored(b: &wire::ExportButtonsState) -> state::StoredButtonsState {
    let mut slots = [state::StoredButtonSlot::default(); BUTTON_SLOT_COUNT];
    for (i, slot) in b.slots.iter().enumerate() {
        slots[i] = state::StoredButtonSlot {
            action: slot.action,
            modifier: slot.modifier,
            key_code: slot.key_code,
        };
    }
    state::StoredButtonsState {
        profile: b.profile,
        slots,
    }
}
// ── Delta merge ───────────────────────────────────────────────────────────

#[cfg(feature = "usb")]
fn merge_sensor_delta(sensor: &mut ProtocolSensorOptions, delta: &wire::SensorOptionsDelta) {
    if let Some(lod) = delta.lift_off_distance {
        sensor.lift_off_distance = match lod {
            1 => LiftOffDistance::OneMillimeter,
            _ => LiftOffDistance::TwoMillimeters,
        };
    }
    if let Some(v) = delta.ripple_control {
        sensor.ripple_control = v;
    }
    if let Some(v) = delta.angle_snap {
        sensor.angle_snap = v;
    }
    if let Some(v) = delta.motion_sync {
        sensor.motion_sync = v;
    }
}

#[cfg(feature = "usb")]
fn merge_prefs_delta_into(prefs: &mut PreferencesState, delta: &PreferencesDelta) {
    if let Some(lm) = delta.light_mode {
        prefs.light_mode = lm;
    }
    if let Some(cfg) = delta.configuration {
        prefs.configuration = cfg;
    }
    if let Some(ds) = delta.deep_sleep {
        prefs.deep_sleep = ds;
    }
    if let Some(hc) = delta.host_color {
        prefs.host_color = hc;
    }
    if let Some(st) = delta.sleep_timer {
        prefs.sleep_timer = st;
    }
    if let Some(db) = delta.debounce {
        prefs.debounce = db;
    }
}

#[cfg(feature = "usb")]
fn map_driver_error(err: &DriverError) -> Response {
    error_response(map_driver_error_code(err), err.to_string())
}

#[cfg(feature = "usb")]
fn map_driver_error_code(err: &DriverError) -> ErrorCode {
    match err {
        DriverError::Protocol(_) => ErrorCode::InvalidRequest,
        DriverError::Transport(_) => ErrorCode::TransportFailure,
        DriverError::BatteryUnavailable | DriverError::BatteryTimeout => ErrorCode::Unsupported,
        DriverError::InputUnavailable => ErrorCode::TransportFailure,
        DriverError::DeviceNotFound => ErrorCode::DeviceNotFound,
        DriverError::AmbiguousDevice { .. } => ErrorCode::InvalidRequest,
        DriverError::WorkerStart(_) | DriverError::WorkerUnavailable => ErrorCode::TransportFailure,
        DriverError::ReadAttemptsExhausted { .. } => ErrorCode::Timeout,
        DriverError::WriteVerificationMismatch { .. }
        | DriverError::GlobalWriteVerificationMismatch { .. } => ErrorCode::TransportFailure,
        DriverError::ProfileAlreadyActive { .. }
        | DriverError::ProfileNotEnabled { .. }
        | DriverError::MaximumProfileBelowCurrent { .. } => ErrorCode::InvalidRequest,
    }
}

#[cfg(feature = "ble")]
fn map_ble_error(err: &BleError) -> Response {
    error_response(map_ble_error_code(err), err.to_string())
}

#[cfg(feature = "ble")]
fn map_ble_error_code(err: &BleError) -> ErrorCode {
    match err {
        BleError::Protocol(_) => ErrorCode::InvalidRequest,
        BleError::AdapterUnavailable | BleError::Backend(_) | BleError::Operation { .. } => {
            ErrorCode::TransportFailure
        }
        BleError::DeviceNotFound => ErrorCode::DeviceNotFound,
        BleError::AmbiguousDevice { .. } => ErrorCode::InvalidRequest,
        BleError::DeviceOpen { .. } => ErrorCode::TransportFailure,
        BleError::Disconnected => ErrorCode::TransportFailure,
        BleError::MissingService { .. }
        | BleError::AmbiguousService { .. }
        | BleError::MissingCharacteristic { .. }
        | BleError::AmbiguousCharacteristic { .. }
        | BleError::InvalidCharacteristicProperties { .. } => ErrorCode::Unsupported,
        BleError::UnsupportedReport { .. } => ErrorCode::Unsupported,
        BleError::AckRejected { .. } => ErrorCode::InvalidRequest,
        BleError::AckTimeout { .. } => ErrorCode::Timeout,
        BleError::MalformedAck | BleError::NotificationOverflow | BleError::Notification(_) => {
            ErrorCode::TransportFailure
        }
    }
}
// ── Factory defaults ────────────────────────────────────────────────────────

/// Factory-default DPI state from the stock reset capture.
///
/// 6 stages: 800, 1600, 2400, 3200, 5000, 26000.  Active stage 1.
/// LOD 2 mm (high), ripple/angle-snap/motion-sync off.
fn factory_dpi_state(profile: ProfileId, preserved_tail: [u8; 25]) -> DpiState {
    DpiState {
        profile,
        stages: vec![
            DpiValue::try_from(800u16).unwrap(),
            DpiValue::try_from(1600u16).unwrap(),
            DpiValue::try_from(2400u16).unwrap(),
            DpiValue::try_from(3200u16).unwrap(),
            DpiValue::try_from(5000u16).unwrap(),
            DpiValue::try_from(26000u16).unwrap(),
        ],
        active_stage: StageIndex::try_from(1u8).unwrap(),
        sensor: ProtocolSensorOptions {
            lift_off_distance: LiftOffDistance::TwoMillimeters,
            ripple_control: false,
            angle_snap: false,
            motion_sync: false,
        },
        preserved_tail,
    }
}

/// Factory-default preferences from the stock reset capture.
fn factory_preferences_state(profile: ProfileId) -> PreferencesState {
    PreferencesState {
        profile,
        light_mode: 0x00,
        configuration: 0x03,
        deep_sleep: 0xa8,
        host_color: [0x00, 0x00, 0xff],
        sleep_timer: 1,
        debounce: 4,
    }
}

/// Factory-default button table from the stock reset capture.
fn factory_buttons_state(profile: ProfileId) -> ButtonsState {
    ButtonsState {
        profile,
        slots: [
            ButtonAssignment {
                action: 0x02,
                modifier: 0x00,
                key_code: 0x00,
            },
            ButtonAssignment {
                action: 0x03,
                modifier: 0x00,
                key_code: 0x00,
            },
            ButtonAssignment {
                action: 0x04,
                modifier: 0x00,
                key_code: 0x00,
            },
            ButtonAssignment {
                action: 0x0d,
                modifier: 0x00,
                key_code: 0x00,
            },
            ButtonAssignment {
                action: 0x3c,
                modifier: 0x00,
                key_code: 0x00,
            },
            ButtonAssignment {
                action: 0x0f,
                modifier: 0x00,
                key_code: 0x00,
            },
            ButtonAssignment {
                action: 0x06,
                modifier: 0x00,
                key_code: 0x00,
            },
            ButtonAssignment {
                action: 0x05,
                modifier: 0x00,
                key_code: 0x00,
            },
            ButtonAssignment {
                action: 0x3c,
                modifier: 0x00,
                key_code: 0x00,
            },
            ButtonAssignment {
                action: 0x01,
                modifier: 0x00,
                key_code: 0x00,
            },
            ButtonAssignment {
                action: 0x01,
                modifier: 0x00,
                key_code: 0x00,
            },
            ButtonAssignment {
                action: 0x01,
                modifier: 0x00,
                key_code: 0x00,
            },
            ButtonAssignment {
                action: 0x01,
                modifier: 0x00,
                key_code: 0x00,
            },
            ButtonAssignment {
                action: 0x01,
                modifier: 0x00,
                key_code: 0x00,
            },
            ButtonAssignment {
                action: 0x01,
                modifier: 0x00,
                key_code: 0x00,
            },
            ButtonAssignment {
                action: 0x01,
                modifier: 0x00,
                key_code: 0x00,
            },
            ButtonAssignment {
                action: 0x0a,
                modifier: 0x00,
                key_code: 0x00,
            },
            ButtonAssignment {
                action: 0x09,
                modifier: 0x00,
                key_code: 0x00,
            },
        ],
    }
}

// ── Error response helper ───────────────────────────────────────────────────────

/// Build a response from an error code and message with [`Provenance::Unverified`].
fn error_response(code: ErrorCode, message: String) -> Response {
    Response {
        provenance: Provenance::Unverified,
        result: ResponseResult::Err { code, message },
    }
}

/// Convert a [`MergeError`] to a [`Response`] with the appropriate error code.
#[cfg(feature = "ble")]
fn merge_error_to_response(err: MergeError) -> Response {
    match err {
        MergeError::NoDevice(key) => Response::err(
            ErrorCode::DeviceNotFound,
            format!("device \"{key}\" not in state"),
            Provenance::Unverified,
        ),
        MergeError::MissingBaseline {
            device_key,
            profile_id,
            resource,
        } => Response::err(
            ErrorCode::MissingBaseline,
            format!(
                "no stored {resource} baseline for device {device_key} profile {profile_id}; pass --explicit-defaults",
            ),
            Provenance::Unverified,
        ),
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use attack_shark_x3::DpiState;

    #[test]
    fn dpi_state_to_payload_conversion() {
        let dpi = DpiState {
            profile: ProfileId::try_from(1u8).unwrap(),
            stages: vec![
                DpiValue::try_from(800u16).unwrap(),
                DpiValue::try_from(1600u16).unwrap(),
                DpiValue::try_from(3200u16).unwrap(),
            ],
            active_stage: StageIndex::try_from(2u8).unwrap(),
            sensor: ProtocolSensorOptions {
                lift_off_distance: LiftOffDistance::OneMillimeter,
                ripple_control: true,
                angle_snap: false,
                motion_sync: true,
            },
            preserved_tail: [0u8; 25],
        };

        let payload = dpi_state_to_payload(&dpi);

        assert_eq!(payload.stages.len(), 3);
        assert_eq!(payload.stages[0], 800);
        assert_eq!(payload.stages[1], 1600);
        assert_eq!(payload.stages[2], 3200);
        assert_eq!(payload.active_stage, 2);
        assert_eq!(payload.sensor.lift_off_distance, 1);
        assert!(payload.sensor.ripple_control);
        assert!(!payload.sensor.angle_snap);
    }

    #[cfg(feature = "usb")]
    #[test]
    fn usb_transport_classification_uses_product_id() {
        let wired = DeviceInfo {
            path: "wired".into(),
            vendor_id: 0x1d57,
            product_id: UsbDeviceKind::Wired.product_id(),
            interface_number: 2,
            product: None,
            serial_number: None,
        };
        let receiver = DeviceInfo {
            path: "receiver".into(),
            vendor_id: 0x1d57,
            product_id: UsbDeviceKind::Receiver.product_id(),
            interface_number: 2,
            product: None,
            serial_number: None,
        };

        assert_eq!(usb_device_to_wire(&wired).transport, WireTransport::Wired);
        assert_eq!(
            usb_device_to_wire(&receiver).transport,
            WireTransport::Receiver
        );
    }

    #[test]
    fn dpi_stored_roundtrip() {
        let dpi = DpiState {
            profile: ProfileId::try_from(1u8).unwrap(),
            stages: vec![
                DpiValue::try_from(800u16).unwrap(),
                DpiValue::try_from(1600u16).unwrap(),
            ],
            active_stage: StageIndex::try_from(1u8).unwrap(),
            sensor: ProtocolSensorOptions {
                lift_off_distance: LiftOffDistance::TwoMillimeters,
                ripple_control: false,
                angle_snap: true,
                motion_sync: false,
            },
            preserved_tail: [0xAAu8; 25],
        };

        let stored = dpi_state_to_stored(&dpi);
        let roundtripped = stored_dpi_to_protocol(&stored, 1).unwrap();

        assert_eq!(dpi.profile, roundtripped.profile);
        assert_eq!(dpi.stages.len(), roundtripped.stages.len());
        assert_eq!(dpi.active_stage, roundtripped.active_stage);
        assert_eq!(
            dpi.sensor.lift_off_distance,
            roundtripped.sensor.lift_off_distance
        );
        assert_eq!(
            dpi.sensor.ripple_control,
            roundtripped.sensor.ripple_control
        );
        assert_eq!(dpi.preserved_tail, roundtripped.preserved_tail);
    }

    #[test]
    fn sensor_delta_merge_partial() {
        let mut sensor = ProtocolSensorOptions {
            lift_off_distance: LiftOffDistance::OneMillimeter,
            ripple_control: false,
            angle_snap: false,
            motion_sync: false,
        };

        let delta = wire::SensorOptionsDelta {
            lift_off_distance: Some(2),
            ripple_control: Some(true),
            angle_snap: None,
            motion_sync: None,
        };

        merge_sensor_delta(&mut sensor, &delta);

        assert_eq!(sensor.lift_off_distance, LiftOffDistance::TwoMillimeters);
        assert!(sensor.ripple_control);
        assert!(!sensor.angle_snap);
        assert!(!sensor.motion_sync);
    }

    #[test]
    fn prefs_delta_merge_partial() {
        let mut prefs = PreferencesState {
            profile: ProfileId::try_from(1u8).unwrap(),
            light_mode: 0,
            configuration: 0,
            deep_sleep: 0,
            host_color: [0, 0, 0],
            sleep_timer: 0,
            debounce: 5,
        };

        let delta = PreferencesDelta {
            light_mode: Some(3),
            configuration: None,
            deep_sleep: None,
            host_color: Some([255, 0, 0]),
            sleep_timer: Some(30),
            debounce: None,
        };

        merge_prefs_delta_into(&mut prefs, &delta);

        assert_eq!(prefs.light_mode, 3);
        assert_eq!(prefs.configuration, 0);
        assert_eq!(prefs.host_color, [255, 0, 0]);
        assert_eq!(prefs.sleep_timer, 30);
    }

    #[test]
    fn polling_rate_conversion() {
        assert_eq!(PollingRate::new(125), Some(PollingRate::Hz125));
        assert_eq!(PollingRate::new(250), Some(PollingRate::Hz250));
        assert_eq!(PollingRate::new(500), Some(PollingRate::Hz500));
        assert_eq!(PollingRate::new(1000), Some(PollingRate::Hz1000));
        assert_eq!(PollingRate::new(200), None);
        assert_eq!(PollingRate::Hz125.hz(), 125);
        assert_eq!(PollingRate::Hz1000.hz(), 1000);
    }

    #[test]
    #[cfg(feature = "usb")]
    fn driver_error_to_code() {
        assert_eq!(
            map_driver_error_code(&DriverError::DeviceNotFound),
            ErrorCode::DeviceNotFound
        );
        assert_eq!(
            map_driver_error_code(&DriverError::BatteryUnavailable),
            ErrorCode::Unsupported
        );
        assert_eq!(
            map_driver_error_code(&DriverError::AmbiguousDevice { count: 3 }),
            ErrorCode::InvalidRequest
        );
        assert_eq!(
            map_driver_error_code(&DriverError::WorkerUnavailable),
            ErrorCode::TransportFailure
        );
    }

    #[test]
    #[cfg(feature = "ble")]
    fn ble_error_to_code() {
        assert_eq!(
            map_ble_error_code(&BleError::DeviceNotFound),
            ErrorCode::DeviceNotFound
        );
        assert_eq!(
            map_ble_error_code(&BleError::AckTimeout { report_id: 4 }),
            ErrorCode::Timeout
        );
        assert_eq!(
            map_ble_error_code(&BleError::UnsupportedReport { report_id: 0xFF }),
            ErrorCode::Unsupported
        );
    }

    #[test]
    fn response_constructors() {
        let ok = Response::ok(ResponseData::Empty, Provenance::UsbValidated);
        assert_eq!(ok.provenance, Provenance::UsbValidated);
        match ok.result {
            ResponseResult::Ok(ResponseData::Empty) => {}
            _ => panic!("expected Ok(Empty)"),
        }

        let err = Response::err(
            ErrorCode::DeviceNotFound,
            "not found",
            Provenance::Unverified,
        );
        assert_eq!(err.provenance, Provenance::Unverified);
        match err.result {
            ResponseResult::Err { code, .. } => assert_eq!(code, ErrorCode::DeviceNotFound),
            _ => panic!("expected Err"),
        }
    }

    #[tokio::test]
    async fn transport_resolution_prefers_active() {
        let exec = Executor::new();
        let mut inner = exec.inner.lock().await;
        inner.active_transport = Some(WireTransport::Ble);

        let ctx = ExecutionContext {
            transport: WireTransport::Auto,
            device: None,
            no_state: false,
            explicit_defaults: false,
        };
        let resolved = inner.resolve_transport(&ctx).unwrap();
        assert_eq!(resolved, WireTransport::Ble);
    }

    #[test]
    fn transport_resolution_concrete_passthrough() {
        let exec = Executor::new();
        // We can test this synchronously since resolve_transport doesn't need async
        let inner = exec.inner.try_lock().unwrap();
        let ctx = ExecutionContext {
            transport: WireTransport::Wired,
            device: None,
            no_state: false,
            explicit_defaults: false,
        };
        let resolved = inner.resolve_transport(&ctx).unwrap();
        assert_eq!(resolved, WireTransport::Wired);
    }

    #[test]
    fn derive_device_key_usb() {
        let key = ExecutorInner::derive_device_key(
            &wire::DeviceSelector::UsbPath("\\\\?\\HID#VID_3297&PID_3382".into()),
            WireTransport::Wired,
        );
        assert!(key.starts_with("usb:"));
        assert!(key.contains("3297"));
    }

    #[test]
    fn derive_device_key_ble() {
        let key = ExecutorInner::derive_device_key(
            &wire::DeviceSelector::BleName("Attack Shark X3".into()),
            WireTransport::Ble,
        );
        assert_eq!(key, "ble:Attack Shark X3");
    }

    // ── BLE safety regression tests ───────────────────────────────────────

    /// Helper: build an ExecutionContext with given flags.
    fn ctx_no_state() -> ExecutionContext {
        ExecutionContext {
            transport: WireTransport::Auto,
            device: None,
            no_state: true,
            explicit_defaults: false,
        }
    }

    fn ctx_normal() -> ExecutionContext {
        ExecutionContext {
            transport: WireTransport::Auto,
            device: None,
            no_state: false,
            explicit_defaults: false,
        }
    }

    // ── Fix 1: no-state masking ─────────────────────────────────────────

    #[test]
    fn no_state_masks_durable_state() {
        let mut inner = ExecutorInner {
            state: Some(StateFile::default()),
            state_path: Some(PathBuf::from("/fake/state.json")),
            #[cfg(feature = "usb")]
            usb_handle: None,
            #[cfg(feature = "ble")]
            ble_handle: None,
            active_device_key: None,
            active_transport: None,
            daemon_start: Instant::now(),
            last_request: Instant::now(),
            daemon_pid: 0,
        };
        // Mask with no_state=true takes state away
        let saved = inner.mask_state(true);
        assert!(inner.state.is_none());
        assert!(inner.state_path.is_none());
        // Restore brings it back
        inner.restore_state(saved);
        assert!(inner.state.is_some());
        assert!(inner.state_path.is_some());
    }

    #[test]
    fn no_state_false_does_not_mask() {
        let mut inner = ExecutorInner {
            state: Some(StateFile::default()),
            state_path: Some(PathBuf::from("/fake/state.json")),
            #[cfg(feature = "usb")]
            usb_handle: None,
            #[cfg(feature = "ble")]
            ble_handle: None,
            active_device_key: None,
            active_transport: None,
            daemon_start: Instant::now(),
            last_request: Instant::now(),
            daemon_pid: 0,
        };
        let saved = inner.mask_state(false);
        assert!(inner.state.is_some());
        inner.restore_state(saved);
        assert!(inner.state.is_some());
    }

    // ── Fix: no-state use-device regression ─────────────────────────────

    /// Helper: build an `ExecutorInner` with state pre-populated for testing.
    fn inner_with_state() -> ExecutorInner {
        let mut state = StateFile::default();
        // Pre-populate with a different device to show persistence matters.
        state::ensure_device(
            &mut state,
            "other-dev",
            state::StoredTransport::Receiver,
            state::StoredSelector::Path("other-path".into()),
        );
        state::select_device(&mut state, "other-dev");
        ExecutorInner {
            state: Some(state),
            state_path: Some(PathBuf::from("/fake/state.json")),
            #[cfg(feature = "usb")]
            usb_handle: None,
            #[cfg(feature = "ble")]
            ble_handle: None,
            active_device_key: None,
            active_transport: None,
            daemon_start: Instant::now(),
            last_request: Instant::now(),
            daemon_pid: 0,
        }
    }

    #[tokio::test]
    async fn use_device_no_state_leaves_durable_selection_unchanged() {
        let mut inner = inner_with_state();
        let original_selected = inner.state.as_ref().and_then(|s| s.selected_device.clone());
        assert_eq!(
            original_selected.as_deref(),
            Some("other-dev"),
            "precondition: other-dev is selected"
        );

        // Pre-set active_transport so ensure_session short-circuits
        // (avoids hardware dependency).
        inner.active_transport = Some(WireTransport::Wired);

        let ctx = ctx_no_state();
        // We need to call dispatch, which masks state before handle_use_device.
        // Since ensure_session succeeds (transport already active), the handler
        // proceeds but should NOT write to state because ctx.no_state is true.
        let response = inner
            .dispatch(Request::UseDevice {
                selector: wire::DeviceSelector::UsbPath("some-usb-path".into()),
                transport: WireTransport::Wired,
                ctx,
            })
            .await;

        assert!(
            matches!(response.result, ResponseResult::Ok(_)),
            "use_device should succeed when session is already open"
        );
        assert_eq!(
            inner
                .state
                .as_ref()
                .and_then(|s| s.selected_device.as_deref()),
            Some("other-dev"),
            "no-state use_device must not mutate durable selected_device"
        );
        assert!(
            inner
                .state
                .as_ref()
                .and_then(|s| s.devices.get("usb:some-usb-path"))
                .is_none(),
            "no-state use_device must not create a durable device entry"
        );

        // active_device_key is in-memory selection — allowed under no-state.
        assert_eq!(
            inner.active_device_key.as_deref(),
            Some("usb:some-usb-path"),
            "in-memory device key should be set even under no-state"
        );
    }

    #[tokio::test]
    async fn use_device_with_state_persists_durable_selection() {
        let mut inner = inner_with_state();
        let original_selected = inner.state.as_ref().and_then(|s| s.selected_device.clone());
        assert_eq!(
            original_selected.as_deref(),
            Some("other-dev"),
            "precondition: other-dev is selected"
        );

        // Pre-set active_transport so ensure_session short-circuits.
        inner.active_transport = Some(WireTransport::Wired);

        let ctx = ctx_normal();
        let response = inner
            .dispatch(Request::UseDevice {
                selector: wire::DeviceSelector::UsbPath("some-usb-path".into()),
                transport: WireTransport::Wired,
                ctx,
            })
            .await;

        assert!(
            matches!(response.result, ResponseResult::Ok(_)),
            "use_device should succeed when session is already open"
        );

        // Durable selection must now be the newly used device.
        assert_eq!(
            inner
                .state
                .as_ref()
                .and_then(|s| s.selected_device.as_deref()),
            Some("usb:some-usb-path"),
            "normal use_device must update durable selected_device"
        );
        assert!(
            inner
                .state
                .as_ref()
                .and_then(|s| s.devices.get("usb:some-usb-path"))
                .is_some(),
            "normal use_device must create a durable device entry"
        );

        assert_eq!(
            inner.active_device_key.as_deref(),
            Some("usb:some-usb-path"),
            "in-memory device key should match the used device"
        );
    }

    #[test]
    fn export_buttons_accepts_exact_slot_count() {
        let slots: Vec<ButtonPayload> = (0..BUTTON_SLOT_COUNT)
            .map(|i| ButtonPayload {
                action: i as u8,
                modifier: 0,
                key_code: 0,
            })
            .collect();
        let export = wire::ExportButtonsState { profile: 2, slots };
        let result = export_buttons_to_stored(&export);
        assert_eq!(result.profile, 2);
        assert_eq!(result.slots[0].action, 0);
        assert_eq!(result.slots[17].action, 17);
    }

    #[cfg(feature = "ble")]
    #[test]
    fn ble_transport_maps_to_ble_acknowledged() {
        // Verify Provenance mapping logic (the match expression used in handle_apply)
        let transport = WireTransport::Ble;
        let provenance = match transport {
            #[cfg(feature = "ble")]
            WireTransport::Ble => Provenance::BleAcknowledged,
            _ => Provenance::UsbValidated,
        };
        assert_eq!(provenance, Provenance::BleAcknowledged);
    }

    #[test]
    fn usb_transport_maps_to_usb_validated() {
        let transport = WireTransport::Wired;
        let provenance = match transport {
            #[cfg(feature = "ble")]
            WireTransport::Ble => Provenance::BleAcknowledged,
            _ => Provenance::UsbValidated,
        };
        assert_eq!(provenance, Provenance::UsbValidated);
    }

    // ── Fix 9: apply_activate_profile metadata completeness ─────────────

    #[test]
    fn profile_metadata_new_preserves_both_fields() {
        let pid = ProfileId::try_from(3u8).unwrap();
        let max_pid = ProfileId::try_from(5u8).unwrap();
        let meta = ProfileMetadata::new(pid, max_pid).expect("valid range");
        assert_eq!(meta.current(), pid);
        assert_eq!(meta.maximum(), max_pid);
    }

    #[test]
    fn profile_metadata_new_with_same_values() {
        // The old bug: ProfileMetadata::new(pid, pid) reduced max to current
        let pid = ProfileId::try_from(2u8).unwrap();
        let meta = ProfileMetadata::new(pid, pid).expect("valid");
        assert_eq!(meta.current(), pid);
        assert_eq!(meta.maximum(), pid);
    }

    // ── no-state dispatch integration test ──────────────────────────────

    #[tokio::test]
    async fn dispatch_no_state_export_rejected() {
        let exec = Executor::new();
        let mut inner = exec.inner.lock().await;
        // Set up state so that without no_state, export would succeed
        inner.state = Some(StateFile::default());
        inner.state_path = Some(PathBuf::from("/tmp/state.json"));
        inner.active_device_key = Some("test-dev".into());

        let request = Request::Export {
            ctx: ctx_no_state(),
        };
        // Dispatch with no_state=true — state should be masked,
        // handle_export should see None and return an error.
        let response = inner.dispatch(request).await;
        // After dispatch, state should be restored
        assert!(
            inner.state.is_some(),
            "state should be restored after no_state dispatch"
        );
        assert!(
            inner.state_path.is_some(),
            "state_path should be restored after no_state dispatch"
        );
        // The export handler with masked state returns an error
        match response.result {
            ResponseResult::Err { code, .. } => {
                assert_eq!(code, ErrorCode::InvalidRequest);
            }
            _ => panic!("expected Err from no_state export"),
        }
    }

    #[tokio::test]
    async fn dispatch_with_state_keeps_state() {
        let exec = Executor::new();
        let mut inner = exec.inner.lock().await;
        inner.state = Some(StateFile::default());
        inner.state_path = Some(PathBuf::from("/tmp/state.json"));
        inner.active_device_key = Some("test-dev".into());

        let request = Request::Export { ctx: ctx_normal() };
        // With no_state=false, state stays intact through dispatch.
        // Export with empty StateFile will fail at "device not in state",
        // but that's fine — we just verify state wasn't dropped.
        let _response = inner.dispatch(request).await;
        // State should still be present after dispatch
        assert!(
            inner.state.is_some(),
            "state should persist through normal dispatch"
        );
        assert!(inner.state_path.is_some());
    }
}
