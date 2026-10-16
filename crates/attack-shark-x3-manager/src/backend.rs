use std::time::Duration;
#[cfg(test)]
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
#[cfg(feature = "usb")]
use attack_shark_x3::MouseHandle;
#[cfg(any(feature = "usb", feature = "ble"))]
use attack_shark_x3::driver::ProfileSnapshot;
#[cfg(feature = "ble")]
use attack_shark_x3::{BleDeviceId, BleHandle, BleSelector};
use attack_shark_x3::{
    ButtonsState, DpiState, InputEvent, PollingRate, PreferencesState, ProfileId, ProfileMetadata,
    TransportKind,
};
#[cfg(feature = "usb")]
use attack_shark_x3::{DeviceSelector, UsbDeviceKind, list_devices_for};
use tokio::sync::broadcast;

use crate::device::DeviceLocator;
#[cfg(feature = "ble")]
use crate::error::StateError;
use crate::{
    device::{DeviceEndpoint, TransportSelection},
    error::ManagerError,
    operation::{DiscoveredEndpoint, VerificationMethod},
};

/// The result shape of a typed session write.
///
/// A transport-verified write is acknowledged at acceptance and carries no
/// readback payload. A readback-verified write carries the state confirmed by
/// a fresh post-write read; USB can provide this and BLE cannot, so BLE
/// readback requests are rejected up front.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SessionWrite<T> {
    ReadbackVerified(T),
    Acknowledged,
}

/// Optional decoded report-`0x03` input stream owned by one open session.
///
/// BLE configuration sessions do not expose this stream. USB sessions expose
/// the low-level HID event receiver directly and preserve its transport
/// semantics for the manager event bridge.
pub(crate) struct SessionEvents {
    pub(crate) input: Option<broadcast::Receiver<InputEvent>>,
}

#[async_trait(?Send)]
pub(crate) trait SessionFactory: Send + Sync {
    async fn list(
        &self,
        selection: TransportSelection,
    ) -> Result<Vec<DiscoveredEndpoint>, ManagerError>;

    async fn open(&self, endpoint: &DeviceEndpoint)
    -> Result<Box<dyn DeviceSession>, ManagerError>;
}

#[async_trait(?Send)]
pub(crate) trait DeviceSession: Send + Sync {
    fn transport(&self) -> TransportKind;

    async fn read_profile_metadata(&self) -> Result<ProfileMetadata, ManagerError>;
    async fn read_profile(&self, profile: ProfileId) -> Result<ProfileSnapshot, ManagerError>;
    async fn read_dpi(&self, profile: ProfileId) -> Result<DpiState, ManagerError>;
    async fn read_preferences(&self, profile: ProfileId) -> Result<PreferencesState, ManagerError>;
    async fn read_buttons(&self, profile: ProfileId) -> Result<ButtonsState, ManagerError>;
    /// Reads the live polling rate; the supplied alias is a wire side effect and never identifies rate content.
    ///
    /// Report `0x06` skips the profile loader: the selector byte is an alias
    /// whose value is recorded as a side effect while the returned rate is
    /// from the current live image.
    async fn read_live_polling_rate(&self, alias: ProfileId) -> Result<PollingRate, ManagerError>;
    async fn write_dpi(
        &self,
        state: DpiState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<DpiState>, ManagerError>;
    async fn write_preferences(
        &self,
        state: PreferencesState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<PreferencesState>, ManagerError>;
    async fn write_buttons(
        &self,
        state: ButtonsState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<ButtonsState>, ManagerError>;
    /// Submits a polling-rate report through an `unchecked` driver primitive.
    ///
    /// Report `0x06` skips the profile loader and its deferred writer may
    /// persist the complete live image into the target alias, so this method
    /// provides no live-image safety precondition. Callers must resolve the
    /// safe target image themselves before reaching it.
    async fn write_polling_rate_unchecked(
        &self,
        profile: ProfileId,
        rate: PollingRate,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<PollingRate>, ManagerError>;
    async fn write_profile_metadata(
        &self,
        metadata: ProfileMetadata,
    ) -> Result<SessionWrite<ProfileMetadata>, ManagerError>;

    async fn read_battery(&self, timeout: Duration) -> Result<u8, ManagerError>;
    fn subscribe_events(&self) -> SessionEvents;
}

/// The production transport factory. It is crate-visible only so resource
/// operations can inject it without exposing low-level handles publicly.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RealSessionFactory;

#[cfg(feature = "usb")]
fn list_usb_devices(kind: UsbDeviceKind) -> Result<Vec<DiscoveredEndpoint>, ManagerError> {
    list_devices_for(kind)?
        .into_iter()
        .map(|info| {
            Ok(DiscoveredEndpoint {
                endpoint: DeviceEndpoint::usb(
                    kind.transport_kind(),
                    info.vendor_id,
                    info.product_id,
                    info.serial_number.as_deref(),
                    &info.path,
                    info.product.as_deref(),
                )?,
                connected: true,
            })
        })
        .collect()
}

#[cfg(feature = "ble")]
async fn list_ble_devices() -> Result<Vec<DiscoveredEndpoint>, ManagerError> {
    let devices = BleHandle::list_connected().await?;
    devices
        .into_iter()
        .map(|info| {
            let stable_id = serde_json::to_string(&info.id)
                .map_err(|error| ManagerError::State(StateError::Serde(error)))?;
            Ok(DiscoveredEndpoint {
                endpoint: DeviceEndpoint::ble(&stable_id, info.name.as_deref())?,
                connected: info.connected,
            })
        })
        .collect()
}

#[cfg(feature = "usb")]
async fn open_usb_session(
    endpoint: &DeviceEndpoint,
    kind: UsbDeviceKind,
) -> Result<Box<dyn DeviceSession>, ManagerError> {
    let DeviceLocator::UsbPath(path) = &endpoint.locator else {
        debug_assert!(false, "USB open expected UsbPath locator");
        return Err(ManagerError::InvalidUpdate(format!(
            "USB open expected UsbPath locator for transport {:?}, got {:?}",
            endpoint.transport, endpoint.locator
        )));
    };
    debug_assert_eq!(
        endpoint.transport,
        kind.transport_kind(),
        "USB transport mismatch"
    );
    let handle = MouseHandle::open_for_kind(DeviceSelector::path(path.clone()), kind)?;
    Ok(Box::new(UsbSession {
        handle,
        transport: kind.transport_kind(),
    }))
}

#[cfg(feature = "ble")]
async fn open_ble_session(
    endpoint: &DeviceEndpoint,
) -> Result<Box<dyn DeviceSession>, ManagerError> {
    let DeviceLocator::BlePlatformId(serialized_id) = &endpoint.locator else {
        debug_assert!(false, "BLE open expected BlePlatformId locator");
        return Err(ManagerError::InvalidUpdate(format!(
            "BLE open expected BlePlatformId locator for transport {:?}, got {:?}",
            endpoint.transport, endpoint.locator
        )));
    };
    debug_assert_eq!(
        endpoint.transport,
        TransportKind::Ble,
        "BLE transport mismatch"
    );
    let id: BleDeviceId = serde_json::from_str(serialized_id)
        .map_err(|error| ManagerError::State(StateError::Serde(error)))?;
    let handle = BleHandle::open(BleSelector::Device(id)).await?;
    Ok(Box::new(BleSession { handle }))
}

/// Sim discovery endpoints registered while `X3_SIM` is set.
///
/// Sim endpoints are indistinguishable `DiscoveredEndpoint`s flowing through
/// the existing discovery/ceremony/resolution/snapshot paths. Env unset
/// (or blank) means no endpoints: zero behavior change and no special-casing
/// downstream. A malformed `X3_SIM` value fails discovery loudly via
/// `ManagerError::InvalidUpdate` naming the variable.
fn sim_discovered_endpoints(
    selection: TransportSelection,
) -> Result<Vec<DiscoveredEndpoint>, ManagerError> {
    use attack_shark_x3::sim::sim_spec_from_env;
    let spec = sim_spec_from_env().map_err(|gap| ManagerError::InvalidUpdate(format!("{gap}")))?;
    sim_endpoints_for_spec(spec, selection)
}

/// Registers the endpoints of one parsed `X3_SIM` spec: `None` registers
/// nothing (env unset = hardware only).
fn sim_endpoints_for_spec(
    spec: Option<attack_shark_x3::sim::SimSpec>,
    selection: TransportSelection,
) -> Result<Vec<DiscoveredEndpoint>, ManagerError> {
    use attack_shark_x3::TransportKind;
    use attack_shark_x3::sim::{SIM_MODEL_TAG, SIM_REPLAY_TAG, SimReplay, SimSpec};
    let Some(spec) = spec else {
        return Ok(Vec::new());
    };
    let transports = match &spec {
        SimSpec::Replay { scenario } => SimReplay::endpoint_transports(scenario)
            .map_err(|gap| ManagerError::InvalidUpdate(format!("{gap}")))?,
        SimSpec::Model { preset } => crate::sim_model::SimModel::endpoint_transports(preset)
            .map_err(|gap| ManagerError::InvalidUpdate(format!("{gap}")))?,
    };
    let (tag, name) = match &spec {
        SimSpec::Replay { scenario } => (SIM_REPLAY_TAG, scenario.clone()),
        SimSpec::Model { preset } => (SIM_MODEL_TAG, preset.clone()),
    };
    let wanted = |transport: TransportKind| match selection {
        TransportSelection::Auto => true,
        TransportSelection::Exact(exact) => exact == transport,
    };
    let multiple = transports.len() > 1;
    let mut out = Vec::new();
    for (index, transport) in transports.into_iter().enumerate() {
        if !wanted(transport) {
            continue;
        }
        let display = if multiple {
            format!("{tag} {name} {}", index + 1)
        } else {
            format!("{tag} {name}")
        };
        // Locator format `sim:{tag}:{index}`; the tag is exactly the backend's
        // `tag()` string (enforced by both backends' tag tests), so frontends
        // recover the tag from the same value the backend reports.
        let locator = format!("sim:{tag}:{index}");
        let endpoint = match transport {
            TransportKind::Ble => DeviceEndpoint::ble(&locator, Some(display.as_str())),
            usb => DeviceEndpoint::usb(
                usb,
                0x1d57,
                if usb == TransportKind::Receiver {
                    0xfa60
                } else {
                    0xfa61
                },
                None,
                locator.as_str(),
                Some(display.as_str()),
            ),
        }?;
        out.push(DiscoveredEndpoint {
            endpoint,
            connected: true,
        });
    }
    Ok(out)
}

/// The tag a sim locator carries, exactly as the backend's `tag()` reported
/// it at registration; `None` for hardware locators.
pub fn sim_tag_for_endpoint(endpoint: &DeviceEndpoint) -> Option<&str> {
    let locator = match &endpoint.locator {
        DeviceLocator::UsbPath(path) => path.as_str(),
        DeviceLocator::BlePlatformId(id) => id.as_str(),
    };
    let mut parts = locator.strip_prefix("sim:")?.split(':');
    let tag = parts.next()?;
    (!tag.is_empty()).then_some(tag)
}

fn sim_locator_index(endpoint: &DeviceEndpoint) -> Option<usize> {
    let locator = match &endpoint.locator {
        DeviceLocator::UsbPath(path) => path.as_str(),
        DeviceLocator::BlePlatformId(id) => id.as_str(),
    };
    locator
        .strip_prefix("sim:")?
        .rsplit(':')
        .next()?
        .parse()
        .ok()
}

/// Sim endpoints use reserved `sim:` locators, so hardware paths never
/// collide and no transport special-casing leaks into the manager's
/// ceremony/resolution/snapshot paths.
fn is_sim_endpoint(endpoint: &DeviceEndpoint) -> bool {
    sim_tag_for_endpoint(endpoint).is_some()
}

/// Stable synthetic identity for one sim endpoint: `SIM:` + scenario bytes +
/// endpoint index. Deterministic across restarts (re-enrollment resolves to
/// the same mouse), distinct per scenario/endpoint (multi-endpoint ambiguity
/// stays testable), and unmistakable for hardware (real tokens are random).
fn sim_token_for(name: &str, index: usize) -> attack_shark_x3::PhysicalId {
    let mut token = [0x53, 0x49, 0x4d, 0x3a, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let bytes = name.as_bytes();
    let copy = bytes.len().min(11);
    token[4..4 + copy].copy_from_slice(&bytes[..copy]);
    token[15] = index as u8;
    attack_shark_x3::PhysicalId::from_token_bytes(token)
}

/// Opens a sim endpoint through the backend named by `X3_SIM`. Hardware
/// locators return `None` and flow on untouched.
fn open_sim_session(
    endpoint: &DeviceEndpoint,
) -> Result<Option<Box<dyn DeviceSession>>, attack_shark_x3::sim::SimGap> {
    use attack_shark_x3::sim::sim_spec_from_env;
    let spec = sim_spec_from_env()?;
    open_sim_session_for_spec(spec, endpoint)
}

/// Opens a sim endpoint through the backend named by `spec`. Hardware
/// locators return `None` and flow on untouched.
fn open_sim_session_for_spec(
    spec: Option<attack_shark_x3::sim::SimSpec>,
    endpoint: &DeviceEndpoint,
) -> Result<Option<Box<dyn DeviceSession>>, attack_shark_x3::sim::SimGap> {
    use attack_shark_x3::sim::{SimGap, SimSpec};
    if !is_sim_endpoint(endpoint) {
        return Ok(None);
    }
    let index = sim_locator_index(endpoint)
        .ok_or_else(|| SimGap("sim: malformed sim locator".to_owned()))?;
    let spec = spec.ok_or_else(|| SimGap("sim: endpoint needs X3_SIM set".to_owned()))?;
    // A locator registered by one backend can never be served by the other:
    // `X3_SIM` changing after discovery means a stale row, reported loudly.
    let expected = match &spec {
        SimSpec::Replay { .. } => attack_shark_x3::sim::SIM_REPLAY_TAG,
        SimSpec::Model { .. } => attack_shark_x3::sim::SIM_MODEL_TAG,
    };
    let registered = sim_tag_for_endpoint(endpoint).unwrap_or_default();
    if registered != expected {
        return Err(SimGap(format!(
            "sim: endpoint registered by {registered} doesn't match {expected} from X3_SIM; \
             refresh discovery"
        )));
    }
    let transport = endpoint.transport;
    match spec {
        SimSpec::Replay { scenario } => {
            use attack_shark_x3::sim::SimBackend;
            let backend = attack_shark_x3::sim::SimReplay::load_endpoint(&scenario, index)?;
            debug_assert_eq!(backend.transport(), transport);
            let token = sim_token_for(&scenario, index);
            Ok(Some(Box::new(SimReplaySession {
                backend: std::sync::Mutex::new(backend),
                token,
            }) as Box<dyn DeviceSession>))
        }
        SimSpec::Model { preset } => {
            let slot = sim_model_slot(&preset, transport)?;
            let token = sim_token_for(&preset, index);
            Ok(Some(Box::new(SimModelSession {
                slot,
                transport,
                token,
            }) as Box<dyn DeviceSession>))
        }
    }
}

/// Process-wide model state, so a preset keeps its images, battery, and write
/// count across session opens exactly like a physical mouse keeps its
/// configuration.
struct SimModelSlot {
    model: crate::sim_model::SimModel,
    /// Host-side images for transports that cannot read (BLE): an entry is
    /// written whenever the host stores an image, so a BLE write merges
    /// against the host's knowledge instead of a refused read.
    host_images: std::collections::BTreeMap<u8, attack_shark_x3::sim::ProfileImage>,
}

/// One shared slot per `X3_SIM` model preset and transport; a preset
/// declares a single endpoint, so a given device state is never split.
type SimModelSlots = std::sync::Mutex<
    std::collections::BTreeMap<
        (String, TransportKind),
        std::sync::Arc<std::sync::Mutex<SimModelSlot>>,
    >,
>;

static SIM_MODEL_SLOTS: std::sync::LazyLock<SimModelSlots> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::BTreeMap::new()));

fn sim_model_slot(
    preset: &str,
    transport: TransportKind,
) -> Result<std::sync::Arc<std::sync::Mutex<SimModelSlot>>, attack_shark_x3::sim::SimGap> {
    use attack_shark_x3::sim::SimBackend;
    let key = (preset.to_owned(), transport);
    let mut slots = SIM_MODEL_SLOTS.lock().expect("sim model registry poisoned");
    if let Some(slot) = slots.get(&key) {
        return Ok(slot.clone());
    }
    let model = crate::sim_model::SimModel::load(preset)?.with_transport(transport);
    // The preset's own images are the host-side baseline: they are readable
    // directly from the preset (a wired reference instance), which is what
    // lets a BLE write merge without a refused readback.
    let reference = crate::sim_model::SimModel::load(preset)?;
    let mut host_images = std::collections::BTreeMap::new();
    if reference.transport() != TransportKind::Ble {
        for profile in ProfileId::MIN..=ProfileId::MAX {
            if let Ok(image) = reference.read_profile(profile) {
                host_images.insert(profile, image);
            }
        }
    }
    let slot = std::sync::Arc::new(std::sync::Mutex::new(SimModelSlot { model, host_images }));
    slots.insert(key, slot.clone());
    Ok(slot)
}

/// Base image for a write: the model's own image where the transport can
/// read, the recorded host image otherwise. BLE therefore merges against
/// what the host last stored (hardware BLE behaves the same way: the host
/// keeps the baseline, the device never serves a readback).
fn sim_base_image(
    slot: &SimModelSlot,
    profile: ProfileId,
    transport: TransportKind,
) -> Result<attack_shark_x3::sim::ProfileImage, ManagerError> {
    use attack_shark_x3::sim::SimBackend;
    if let Some(image) = slot.host_images.get(&profile.get()) {
        return Ok(image.clone());
    }
    slot.model
        .read_profile(profile.get())
        .map_err(|gap| sim_gap_error(gap, transport))
}

/// `DeviceSession` over the in-memory model backend.
///
/// Transport rules stay real: BLE refuses reads (like hardware) and
/// acknowledges writes; wired serves and readback-verifies them.
struct SimModelSession {
    slot: std::sync::Arc<std::sync::Mutex<SimModelSlot>>,
    transport: TransportKind,
    token: attack_shark_x3::PhysicalId,
}

impl SimModelSession {
    fn image(
        &self,
        profile: ProfileId,
    ) -> Result<attack_shark_x3::sim::ProfileImage, ManagerError> {
        use attack_shark_x3::sim::SimBackend;
        let slot = self.slot.lock().expect("sim model slot poisoned");
        let mut image = slot
            .model
            .read_profile(profile.get())
            .map_err(|gap| sim_gap_error(gap, self.transport))?;
        image.0.overlay_physical_id(self.token);
        Ok(image)
    }

    /// Merges one resource into the profile image, stores it, and returns the
    /// accepted image. Gaps (BLE read refusal, invalid write) stay loud.
    fn store(
        &self,
        profile: ProfileId,
        mutate: impl FnOnce(&mut attack_shark_x3::sim::ProfileImage),
    ) -> Result<attack_shark_x3::sim::ProfileImage, ManagerError> {
        use attack_shark_x3::sim::SimBackend;
        let mut slot = self.slot.lock().expect("sim model slot poisoned");
        let mut image = sim_base_image(&slot, profile, self.transport)?;
        mutate(&mut image);
        slot.model
            .write_profile(profile.get(), &image)
            .map_err(|gap| sim_gap_error(gap, self.transport))?;
        slot.host_images.insert(profile.get(), image.clone());
        Ok(image)
    }
}

#[async_trait(?Send)]
impl DeviceSession for SimModelSession {
    fn transport(&self) -> TransportKind {
        self.transport
    }

    async fn read_profile_metadata(&self) -> Result<ProfileMetadata, ManagerError> {
        let slot = self.slot.lock().expect("sim model slot poisoned");
        let current = ProfileId::try_from(slot.model.active_profile()).unwrap_or(ProfileId::MIN_ID);
        ProfileMetadata::new(current, ProfileId::MAX_ID).map_err(|error| ManagerError::Protocol {
            operation: "sim profile metadata",
            source: error,
        })
    }

    async fn read_profile(&self, profile: ProfileId) -> Result<ProfileSnapshot, ManagerError> {
        let image = self.image(profile)?;
        Ok(ProfileSnapshot {
            persistent_metadata: self.read_profile_metadata().await?,
            target_profile: profile,
            dpi: image.0,
            preferences: image.1,
            buttons: image.2,
        })
    }

    async fn read_dpi(&self, profile: ProfileId) -> Result<DpiState, ManagerError> {
        self.image(profile).map(|image| image.0)
    }

    async fn read_preferences(&self, profile: ProfileId) -> Result<PreferencesState, ManagerError> {
        self.image(profile).map(|image| image.1)
    }

    async fn read_buttons(&self, profile: ProfileId) -> Result<ButtonsState, ManagerError> {
        self.image(profile).map(|image| image.2)
    }

    async fn read_live_polling_rate(&self, _alias: ProfileId) -> Result<PollingRate, ManagerError> {
        Ok(PollingRate::Hz1000)
    }

    async fn write_dpi(
        &self,
        state: DpiState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<DpiState>, ManagerError> {
        let stored = self.store(state.profile, |image| image.0 = state)?;
        Ok(sim_write_outcome(stored.0, verification))
    }

    async fn write_preferences(
        &self,
        state: PreferencesState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<PreferencesState>, ManagerError> {
        let stored = self.store(state.profile, |image| image.1 = state)?;
        Ok(sim_write_outcome(stored.1, verification))
    }

    async fn write_buttons(
        &self,
        state: ButtonsState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<ButtonsState>, ManagerError> {
        let stored = self.store(state.profile, |image| image.2 = state)?;
        Ok(sim_write_outcome(stored.2, verification))
    }

    async fn write_polling_rate_unchecked(
        &self,
        _profile: ProfileId,
        rate: PollingRate,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<PollingRate>, ManagerError> {
        Ok(sim_write_outcome(rate, verification))
    }

    async fn write_profile_metadata(
        &self,
        metadata: ProfileMetadata,
    ) -> Result<SessionWrite<ProfileMetadata>, ManagerError> {
        let mut slot = self.slot.lock().expect("sim model slot poisoned");
        slot.model
            .set_active_profile(metadata.current().get())
            .map_err(|gap| sim_gap_error(gap, self.transport))?;
        drop(slot);
        Ok(sim_write_outcome(metadata, VerificationMethod::Transport))
    }

    async fn read_battery(&self, _timeout: Duration) -> Result<u8, ManagerError> {
        use attack_shark_x3::sim::SimBackend;
        let slot = self.slot.lock().expect("sim model slot poisoned");
        slot.model
            .battery()
            .ok_or(ManagerError::UnsupportedOperation {
                operation: "read_battery",
                transport: self.transport,
            })
    }

    fn subscribe_events(&self) -> SessionEvents {
        SessionEvents { input: None }
    }
}

/// Readback verification where the transport provides one; BLE acknowledges.
fn sim_write_outcome<T>(value: T, verification: VerificationMethod) -> SessionWrite<T> {
    match verification {
        VerificationMethod::Transport => SessionWrite::Acknowledged,
        VerificationMethod::Readback => SessionWrite::ReadbackVerified(value),
    }
}

/// `DeviceSession` over a [`SimBackend`] replay.
///
/// Profile snapshots are rebuilt from the served triple plus metadata that
/// marks profile 1 current (max 5): ceremony and resolution paths consume
/// the same types as hardware snapshots, untouched. Transport rules stay
/// real: BLE refuses reads via gaps exactly like hardware, and writes verify
/// against the scenario's recorded post-write image or gap loudly naming the
/// missing capture. The backend is per-session: replay holds no live state.
struct SimReplaySession {
    backend: std::sync::Mutex<attack_shark_x3::sim::SimReplay>,
    token: attack_shark_x3::PhysicalId,
}

impl SimReplaySession {
    fn read_image(
        &self,
        profile: ProfileId,
    ) -> Result<attack_shark_x3::sim::ProfileImage, ManagerError> {
        use attack_shark_x3::sim::SimBackend;
        // Transport is read before taking the lock: the lock is not reentrant.
        let transport = self.transport();
        let token = self.token;
        let backend = self.backend.lock().expect("sim replay poisoned");
        let mut image = backend
            .read_profile(profile.get())
            .map_err(|gap| sim_gap_error(gap, transport))?;
        image.0.overlay_physical_id(token);
        Ok(image)
    }

    /// Applies one resource change and submits the full image, so the replay
    /// backend can verify it against the recorded post-write capture.
    fn store(
        &self,
        profile: ProfileId,
        mutate: impl FnOnce(&mut attack_shark_x3::sim::ProfileImage),
    ) -> Result<attack_shark_x3::sim::ProfileImage, ManagerError> {
        use attack_shark_x3::sim::SimBackend;
        // Transport is read before taking the lock: the lock is not reentrant.
        let transport = self.transport();
        let mut backend = self.backend.lock().expect("sim replay poisoned");
        let mut image = backend
            .read_profile(profile.get())
            .map_err(|gap| sim_gap_error(gap, transport))?;
        mutate(&mut image);
        backend
            .write_profile(profile.get(), &image)
            .map_err(|gap| sim_gap_error(gap, transport))?;
        Ok(image)
    }

    /// A behavior this scenario recorded no capture for: never guessed.
    fn missing_capture(&self, detail: &str) -> ManagerError {
        sim_gap_error(
            attack_shark_x3::sim::SimGap(format!("sim-replay: no recorded capture for {detail}")),
            self.transport(),
        )
    }
}

#[async_trait(?Send)]
impl DeviceSession for SimReplaySession {
    fn transport(&self) -> TransportKind {
        use attack_shark_x3::sim::SimBackend;
        self.backend
            .lock()
            .expect("sim replay poisoned")
            .transport()
    }

    async fn read_profile_metadata(&self) -> Result<ProfileMetadata, ManagerError> {
        ProfileMetadata::new(ProfileId::MIN_ID, ProfileId::MAX_ID).map_err(|error| {
            ManagerError::Protocol {
                operation: "sim profile metadata",
                source: error,
            }
        })
    }

    async fn read_profile(&self, profile: ProfileId) -> Result<ProfileSnapshot, ManagerError> {
        let image = self.read_image(profile)?;
        Ok(ProfileSnapshot {
            persistent_metadata: self.read_profile_metadata().await?,
            target_profile: profile,
            dpi: image.0,
            preferences: image.1,
            buttons: image.2,
        })
    }

    async fn read_dpi(&self, profile: ProfileId) -> Result<DpiState, ManagerError> {
        self.read_image(profile).map(gap_image_dpi)
    }

    async fn read_preferences(&self, profile: ProfileId) -> Result<PreferencesState, ManagerError> {
        self.read_image(profile).map(gap_image_preferences)
    }

    async fn read_buttons(&self, profile: ProfileId) -> Result<ButtonsState, ManagerError> {
        self.read_image(profile).map(gap_image_buttons)
    }

    async fn read_live_polling_rate(&self, _alias: ProfileId) -> Result<PollingRate, ManagerError> {
        Ok(PollingRate::Hz1000)
    }

    async fn write_dpi(
        &self,
        state: DpiState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<DpiState>, ManagerError> {
        let accepted = self.store(state.profile, |image| image.0 = state)?;
        Ok(sim_write_outcome(accepted.0, verification))
    }

    async fn write_preferences(
        &self,
        state: PreferencesState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<PreferencesState>, ManagerError> {
        let accepted = self.store(state.profile, |image| image.1 = state)?;
        Ok(sim_write_outcome(accepted.1, verification))
    }

    async fn write_buttons(
        &self,
        state: ButtonsState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<ButtonsState>, ManagerError> {
        let accepted = self.store(state.profile, |image| image.2 = state)?;
        Ok(sim_write_outcome(accepted.2, verification))
    }

    async fn write_polling_rate_unchecked(
        &self,
        _profile: ProfileId,
        _rate: PollingRate,
        _verification: VerificationMethod,
    ) -> Result<SessionWrite<PollingRate>, ManagerError> {
        Err(self.missing_capture("polling rate"))
    }

    async fn write_profile_metadata(
        &self,
        _metadata: ProfileMetadata,
    ) -> Result<SessionWrite<ProfileMetadata>, ManagerError> {
        Err(self.missing_capture("profile activation"))
    }

    async fn read_battery(&self, _timeout: Duration) -> Result<u8, ManagerError> {
        use attack_shark_x3::sim::SimBackend;
        let transport = self.transport();
        let level = self.backend.lock().expect("sim replay poisoned").battery();
        level.ok_or(unsupported_sim_write("read_battery", transport))
    }

    fn subscribe_events(&self) -> SessionEvents {
        SessionEvents { input: None }
    }
}

/// Maps a [`SimGap`] onto manager errors by the session's transport: a BLE
/// read refusal becomes `UnsupportedOperation` exactly like hardware, so the
/// manager's USB/BLE gates treat sim transports as they treat real ones;
/// every other gap is a loud `InvalidUpdate` naming the missing capture.
fn sim_gap_error(gap: attack_shark_x3::sim::SimGap, transport: TransportKind) -> ManagerError {
    match transport {
        TransportKind::Ble => ManagerError::UnsupportedOperation {
            operation: "read_profile",
            transport: TransportKind::Ble,
        },
        _ => ManagerError::InvalidUpdate(format!("{gap}")),
    }
}

fn gap_image_dpi(image: attack_shark_x3::sim::ProfileImage) -> DpiState {
    image.0
}

fn gap_image_preferences(image: attack_shark_x3::sim::ProfileImage) -> PreferencesState {
    image.1
}

fn gap_image_buttons(image: attack_shark_x3::sim::ProfileImage) -> ButtonsState {
    image.2
}

fn unsupported_sim_write(operation: &'static str, transport: TransportKind) -> ManagerError {
    ManagerError::UnsupportedOperation {
        operation,
        transport,
    }
}

#[async_trait(?Send)]
impl SessionFactory for RealSessionFactory {
    async fn list(
        &self,
        selection: TransportSelection,
    ) -> Result<Vec<DiscoveredEndpoint>, ManagerError> {
        match selection {
            TransportSelection::Auto => {
                #[cfg(any(feature = "usb", feature = "ble"))]
                {
                    let mut devices = Vec::new();
                    #[cfg(feature = "usb")]
                    {
                        devices.extend(list_usb_devices(UsbDeviceKind::Wired)?);
                        devices.extend(list_usb_devices(UsbDeviceKind::Receiver)?);
                    }
                    #[cfg(feature = "ble")]
                    devices.extend(list_ble_devices().await?);
                    devices.extend(sim_discovered_endpoints(selection)?);
                    Ok(devices)
                }
                #[cfg(not(any(feature = "usb", feature = "ble")))]
                {
                    Ok(Vec::new())
                }
            }
            TransportSelection::Exact(TransportKind::Wired) => {
                #[cfg(feature = "usb")]
                {
                    let mut devices = list_usb_devices(UsbDeviceKind::Wired)?;
                    devices.extend(sim_discovered_endpoints(selection)?);
                    Ok(devices)
                }
                #[cfg(not(feature = "usb"))]
                {
                    sim_discovered_endpoints(selection)
                }
            }
            TransportSelection::Exact(TransportKind::Receiver) => {
                #[cfg(feature = "usb")]
                {
                    let mut devices = list_usb_devices(UsbDeviceKind::Receiver)?;
                    devices.extend(sim_discovered_endpoints(selection)?);
                    Ok(devices)
                }
                #[cfg(not(feature = "usb"))]
                {
                    sim_discovered_endpoints(selection)
                }
            }
            TransportSelection::Exact(TransportKind::Ble) => {
                #[cfg(feature = "ble")]
                {
                    let mut devices = list_ble_devices().await?;
                    devices.extend(sim_discovered_endpoints(selection)?);
                    Ok(devices)
                }
                #[cfg(not(feature = "ble"))]
                {
                    sim_discovered_endpoints(selection)
                }
            }
        }
    }

    async fn open(
        &self,
        endpoint: &DeviceEndpoint,
    ) -> Result<Box<dyn DeviceSession>, ManagerError> {
        if is_sim_endpoint(endpoint)
            && let Some(session) =
                open_sim_session(endpoint).map_err(|gap: attack_shark_x3::sim::SimGap| {
                    ManagerError::InvalidUpdate(format!("{gap}"))
                })?
        {
            return Ok(session);
        }
        match endpoint.transport {
            TransportKind::Wired => {
                #[cfg(feature = "usb")]
                return open_usb_session(endpoint, UsbDeviceKind::Wired).await;
                #[cfg(not(feature = "usb"))]
                Err(ManagerError::UnsupportedOperation {
                    operation: "open",
                    transport: TransportKind::Wired,
                })
            }
            TransportKind::Receiver => {
                #[cfg(feature = "usb")]
                return open_usb_session(endpoint, UsbDeviceKind::Receiver).await;
                #[cfg(not(feature = "usb"))]
                Err(ManagerError::UnsupportedOperation {
                    operation: "open",
                    transport: TransportKind::Receiver,
                })
            }
            TransportKind::Ble => {
                #[cfg(feature = "ble")]
                return open_ble_session(endpoint).await;
                #[cfg(not(feature = "ble"))]
                Err(ManagerError::UnsupportedOperation {
                    operation: "open",
                    transport: TransportKind::Ble,
                })
            }
        }
    }
}

#[cfg(feature = "usb")]
struct UsbSession {
    handle: MouseHandle,
    transport: TransportKind,
}

#[cfg(feature = "usb")]
#[async_trait(?Send)]
impl DeviceSession for UsbSession {
    fn transport(&self) -> TransportKind {
        self.transport
    }

    async fn read_profile_metadata(&self) -> Result<ProfileMetadata, ManagerError> {
        Ok(self.handle.read_profile_metadata().await?)
    }

    async fn read_profile(&self, profile: ProfileId) -> Result<ProfileSnapshot, ManagerError> {
        Ok(self.handle.read_profile(profile).await?)
    }

    async fn read_dpi(&self, profile: ProfileId) -> Result<DpiState, ManagerError> {
        Ok(self.handle.read_dpi(profile).await?)
    }

    async fn read_preferences(&self, profile: ProfileId) -> Result<PreferencesState, ManagerError> {
        Ok(self.handle.read_preferences(profile).await?)
    }

    async fn read_buttons(&self, profile: ProfileId) -> Result<ButtonsState, ManagerError> {
        Ok(self.handle.read_buttons(profile).await?)
    }

    async fn read_live_polling_rate(&self, alias: ProfileId) -> Result<PollingRate, ManagerError> {
        Ok(self.handle.read_live_polling_rate(alias).await?)
    }

    async fn write_dpi(
        &self,
        state: DpiState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<DpiState>, ManagerError> {
        match verification {
            VerificationMethod::Transport => {
                self.handle.send_dpi(state).await?;
                Ok(SessionWrite::Acknowledged)
            }
            VerificationMethod::Readback => Ok(SessionWrite::ReadbackVerified(
                self.handle.write_dpi(state).await?,
            )),
        }
    }

    async fn write_preferences(
        &self,
        state: PreferencesState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<PreferencesState>, ManagerError> {
        match verification {
            VerificationMethod::Transport => {
                self.handle.send_preferences(state).await?;
                Ok(SessionWrite::Acknowledged)
            }
            VerificationMethod::Readback => Ok(SessionWrite::ReadbackVerified(
                self.handle.write_preferences(state).await?,
            )),
        }
    }

    async fn write_buttons(
        &self,
        state: ButtonsState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<ButtonsState>, ManagerError> {
        match verification {
            VerificationMethod::Transport => {
                self.handle.send_buttons(state).await?;
                Ok(SessionWrite::Acknowledged)
            }
            VerificationMethod::Readback => Ok(SessionWrite::ReadbackVerified(
                self.handle.write_buttons(state).await?,
            )),
        }
    }

    async fn write_polling_rate_unchecked(
        &self,
        profile: ProfileId,
        rate: PollingRate,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<PollingRate>, ManagerError> {
        match verification {
            VerificationMethod::Transport => {
                self.handle
                    .send_polling_rate_unchecked(profile, rate)
                    .await?;
                Ok(SessionWrite::Acknowledged)
            }
            VerificationMethod::Readback => Ok(SessionWrite::ReadbackVerified(
                self.handle
                    .write_polling_rate_unchecked(profile, rate)
                    .await?,
            )),
        }
    }

    async fn write_profile_metadata(
        &self,
        metadata: ProfileMetadata,
    ) -> Result<SessionWrite<ProfileMetadata>, ManagerError> {
        let mut actual = self.handle.read_profile_metadata().await?;

        // Raise the maximum before activating a profile that is not currently
        // enabled. Lowering it waits until the target profile is active so the
        // low-level driver's current-profile invariant is never violated.
        if metadata.maximum().get() > actual.maximum().get() {
            actual = self.handle.set_maximum_profile(metadata.maximum()).await?;
        }
        if metadata.current() != actual.current() {
            actual = self.handle.activate_profile(metadata.current()).await?;
        }
        if metadata.maximum() != actual.maximum() {
            actual = self.handle.set_maximum_profile(metadata.maximum()).await?;
        }

        Ok(SessionWrite::ReadbackVerified(actual))
    }

    async fn read_battery(&self, timeout: Duration) -> Result<u8, ManagerError> {
        Ok(self.handle.read_battery(timeout).await?)
    }

    fn subscribe_events(&self) -> SessionEvents {
        SessionEvents {
            input: Some(self.handle.subscribe_input_events()),
        }
    }
}

#[cfg(feature = "ble")]
struct BleSession {
    handle: BleHandle,
}

#[cfg(feature = "ble")]
impl BleSession {
    fn unsupported<T>(operation: &'static str) -> Result<T, ManagerError> {
        Err(ManagerError::UnsupportedOperation {
            operation,
            transport: TransportKind::Ble,
        })
    }
}

#[cfg(feature = "ble")]
#[async_trait(?Send)]
impl DeviceSession for BleSession {
    fn transport(&self) -> TransportKind {
        TransportKind::Ble
    }

    async fn read_profile_metadata(&self) -> Result<ProfileMetadata, ManagerError> {
        Self::unsupported("read_profile_metadata")
    }

    async fn read_profile(&self, _profile: ProfileId) -> Result<ProfileSnapshot, ManagerError> {
        Self::unsupported("read_profile")
    }

    async fn read_dpi(&self, _profile: ProfileId) -> Result<DpiState, ManagerError> {
        Self::unsupported("read_dpi")
    }

    async fn read_preferences(
        &self,
        _profile: ProfileId,
    ) -> Result<PreferencesState, ManagerError> {
        Self::unsupported("read_preferences")
    }

    async fn read_buttons(&self, _profile: ProfileId) -> Result<ButtonsState, ManagerError> {
        Self::unsupported("read_buttons")
    }

    async fn read_live_polling_rate(&self, _alias: ProfileId) -> Result<PollingRate, ManagerError> {
        Self::unsupported("read_live_polling_rate")
    }

    async fn write_dpi(
        &self,
        state: DpiState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<DpiState>, ManagerError> {
        match verification {
            VerificationMethod::Transport => {
                self.handle.write_dpi(state).await?;
                Ok(SessionWrite::Acknowledged)
            }
            VerificationMethod::Readback => Self::unsupported("write_dpi"),
        }
    }

    async fn write_preferences(
        &self,
        state: PreferencesState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<PreferencesState>, ManagerError> {
        match verification {
            VerificationMethod::Transport => {
                self.handle.write_preferences(state).await?;
                Ok(SessionWrite::Acknowledged)
            }
            VerificationMethod::Readback => Self::unsupported("write_preferences"),
        }
    }

    async fn write_buttons(
        &self,
        state: ButtonsState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<ButtonsState>, ManagerError> {
        match verification {
            VerificationMethod::Transport => {
                self.handle.write_buttons(state).await?;
                Ok(SessionWrite::Acknowledged)
            }
            VerificationMethod::Readback => Self::unsupported("write_buttons"),
        }
    }

    async fn write_polling_rate_unchecked(
        &self,
        profile: ProfileId,
        rate: PollingRate,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<PollingRate>, ManagerError> {
        match verification {
            VerificationMethod::Transport => {
                // BLE has no readback path; an accepted application ACK is
                // the strongest evidence available to the manager.
                self.handle
                    .write_polling_rate_unchecked(profile, rate)
                    .await?;
                Ok(SessionWrite::Acknowledged)
            }
            VerificationMethod::Readback => Self::unsupported("write_polling_rate_unchecked"),
        }
    }

    async fn write_profile_metadata(
        &self,
        metadata: ProfileMetadata,
    ) -> Result<SessionWrite<ProfileMetadata>, ManagerError> {
        self.handle.write_profile_control(metadata).await?;
        Ok(SessionWrite::Acknowledged)
    }

    async fn read_battery(&self, _timeout: Duration) -> Result<u8, ManagerError> {
        Self::unsupported("read_battery")
    }

    fn subscribe_events(&self) -> SessionEvents {
        SessionEvents { input: None }
    }
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ScriptedWrite {
    Dpi(DpiState),
    Preferences(PreferencesState),
    Buttons(ButtonsState),
    PollingRate(ProfileId, PollingRate),
    ProfileMetadata(ProfileMetadata),
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct ScriptedFakeSession {
    transport: TransportKind,
    /// Default evidence for session writes that carry no per-call method
    /// (profile metadata): USB readback evidence, BLE transport ACK. The four
    /// resource writes take their `VerificationMethod` per call instead.
    verification: VerificationMethod,
    metadata: Arc<Mutex<Option<ProfileMetadata>>>,
    profiles: Arc<Mutex<BTreeMap<ProfileId, ProfileSnapshot>>>,
    profile_sequences: Arc<Mutex<BTreeMap<ProfileId, VecDeque<ProfileSnapshot>>>>,
    last_profiles: Arc<Mutex<BTreeMap<ProfileId, ProfileSnapshot>>>,
    profile_reads: Arc<Mutex<usize>>,
    dpi_reads: Arc<Mutex<usize>>,
    /// Per-profile polling rates; the live rate is `polling_rates[live_profile]`.
    polling_rates: Arc<Mutex<BTreeMap<ProfileId, PollingRate>>>,
    /// The currently live/working profile image; report `0x06` skips the loader
    /// so polling reads return `polling_rates[live_profile]` regardless of alias.
    live_profile: Arc<Mutex<ProfileId>>,
    /// Last alias supplied to `read_live_polling_rate`; wire side effect, never content.
    last_polling_alias: Arc<Mutex<Option<ProfileId>>>,
    battery: Arc<Mutex<Option<u8>>>,
    writes: Arc<Mutex<Vec<ScriptedWrite>>>,
    metadata_write_failure: Arc<Mutex<Option<ProfileMetadata>>>,
    input_events: broadcast::Sender<InputEvent>,
}

#[cfg(test)]
fn lock_scripted<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value.lock().expect("scripted fake state lock poisoned")
}

#[cfg(test)]
impl ScriptedFakeSession {
    fn new(transport: TransportKind, verification: VerificationMethod) -> Self {
        let (input_events, _) = broadcast::channel(16);
        Self {
            transport,
            verification,
            metadata: Arc::new(Mutex::new(None)),
            profiles: Arc::new(Mutex::new(BTreeMap::new())),
            profile_sequences: Arc::new(Mutex::new(BTreeMap::new())),
            last_profiles: Arc::new(Mutex::new(BTreeMap::new())),
            profile_reads: Arc::new(Mutex::new(0)),
            dpi_reads: Arc::new(Mutex::new(0)),
            polling_rates: Arc::new(Mutex::new(BTreeMap::new())),
            live_profile: Arc::new(Mutex::new(ProfileId::MIN_ID)),
            last_polling_alias: Arc::new(Mutex::new(None)),
            battery: Arc::new(Mutex::new(None)),
            writes: Arc::new(Mutex::new(Vec::new())),
            metadata_write_failure: Arc::new(Mutex::new(None)),
            input_events,
        }
    }

    pub(crate) fn usb() -> Self {
        Self::new(TransportKind::Wired, VerificationMethod::Readback)
    }

    pub(crate) fn ble() -> Self {
        Self::new(TransportKind::Ble, VerificationMethod::Transport)
    }

    pub(crate) fn with_metadata(self, metadata: ProfileMetadata) -> Self {
        *lock_scripted(&self.metadata) = Some(metadata);
        *lock_scripted(&self.live_profile) = metadata.current();
        self
    }

    pub(crate) fn with_profile(self, snapshot: ProfileSnapshot) -> Self {
        lock_scripted(&self.profiles).insert(snapshot.target_profile, snapshot);
        self
    }
    pub(crate) fn with_profile_sequence(self, sequence: Vec<ProfileSnapshot>) -> Self {
        {
            let mut queues = lock_scripted(&self.profile_sequences);
            let mut last_profiles = lock_scripted(&self.last_profiles);
            for snapshot in sequence {
                let profile = snapshot.target_profile;
                queues.entry(profile).or_default().push_back(snapshot);
                last_profiles.remove(&profile);
            }
        }
        self
    }
    pub(crate) fn fail_metadata_write_for(self, metadata: ProfileMetadata) -> Self {
        *lock_scripted(&self.metadata_write_failure) = Some(metadata);
        self
    }

    /// Sets the same polling rate for every profile (1..=5); convenience for legacy single-rate tests.
    pub(crate) fn with_polling_rate(self, rate: PollingRate) -> Self {
        {
            let mut rates = lock_scripted(&self.polling_rates);
            for id in 1..=5 {
                if let Ok(profile) = ProfileId::try_from(id) {
                    rates.insert(profile, rate);
                }
            }
        }
        self
    }

    /// Sets a per-profile polling rate.
    pub(crate) fn with_polling_rate_for(self, profile: ProfileId, rate: PollingRate) -> Self {
        lock_scripted(&self.polling_rates).insert(profile, rate);
        self
    }

    /// Forces the live/working profile; subsequent `read_live_polling_rate` returns this profile's rate.
    pub(crate) fn with_live_profile(self, profile: ProfileId) -> Self {
        *lock_scripted(&self.live_profile) = profile;
        self
    }

    /// Returns the last alias supplied to `read_live_polling_rate`, if any.
    pub(crate) fn last_polling_alias(&self) -> Option<ProfileId> {
        *lock_scripted(&self.last_polling_alias)
    }

    pub(crate) fn profile_read_count(&self) -> usize {
        *lock_scripted(&self.profile_reads)
    }

    pub(crate) fn dpi_read_count(&self) -> usize {
        *lock_scripted(&self.dpi_reads)
    }

    pub(crate) fn with_battery(self, level: u8) -> Self {
        *lock_scripted(&self.battery) = Some(level);
        self
    }
    /// Snapshot of every hardware write the fake has performed, in order.
    pub(crate) fn writes(&self) -> Vec<ScriptedWrite> {
        lock_scripted(&self.writes).clone()
    }

    fn unsupported<T>(&self, operation: &'static str) -> Result<T, ManagerError> {
        Err(ManagerError::UnsupportedOperation {
            operation,
            transport: self.transport,
        })
    }

    fn write_outcome<T>(&self, value: T, verification: VerificationMethod) -> SessionWrite<T> {
        match verification {
            VerificationMethod::Transport => SessionWrite::Acknowledged,
            VerificationMethod::Readback => SessionWrite::ReadbackVerified(value),
        }
    }

    fn readback_unsupported(&self, operation: &'static str) -> ManagerError {
        ManagerError::UnsupportedOperation {
            operation,
            transport: self.transport,
        }
    }

    fn is_ble(&self) -> bool {
        self.transport == TransportKind::Ble
    }
}

#[cfg(test)]
#[async_trait(?Send)]
impl DeviceSession for ScriptedFakeSession {
    fn transport(&self) -> TransportKind {
        self.transport
    }

    async fn read_profile_metadata(&self) -> Result<ProfileMetadata, ManagerError> {
        if self.is_ble() {
            return self.unsupported("read_profile_metadata");
        }
        lock_scripted(&self.metadata)
            .as_ref()
            .copied()
            .ok_or(ManagerError::MissingBaseline {
                resource: "profile metadata",
                profile: None,
            })
    }

    async fn read_profile(&self, profile: ProfileId) -> Result<ProfileSnapshot, ManagerError> {
        if self.is_ble() {
            return self.unsupported("read_profile");
        }
        *lock_scripted(&self.profile_reads) += 1;
        let queued = lock_scripted(&self.profile_sequences)
            .get_mut(&profile)
            .and_then(VecDeque::pop_front);
        let mut snapshot = if let Some(snapshot) = queued {
            lock_scripted(&self.last_profiles).insert(profile, snapshot.clone());
            snapshot
        } else if let Some(snapshot) = lock_scripted(&self.last_profiles).get(&profile).cloned() {
            snapshot
        } else {
            lock_scripted(&self.profiles).get(&profile).cloned().ok_or(
                ManagerError::MissingBaseline {
                    resource: "profile",
                    profile: Some(profile),
                },
            )?
        };
        if let Some(metadata) = lock_scripted(&self.metadata).as_ref().copied() {
            snapshot.persistent_metadata = metadata;
        }
        *lock_scripted(&self.live_profile) = profile;
        Ok(snapshot)
    }

    async fn read_dpi(&self, profile: ProfileId) -> Result<DpiState, ManagerError> {
        if self.is_ble() {
            return self.unsupported("read_dpi");
        }
        *lock_scripted(&self.dpi_reads) += 1;
        let result = lock_scripted(&self.profiles)
            .get(&profile)
            .map(|snapshot| snapshot.dpi.clone())
            .ok_or(ManagerError::MissingBaseline {
                resource: "DPI",
                profile: Some(profile),
            })?;
        *lock_scripted(&self.live_profile) = profile;
        Ok(result)
    }

    async fn read_preferences(&self, profile: ProfileId) -> Result<PreferencesState, ManagerError> {
        if self.is_ble() {
            return self.unsupported("read_preferences");
        }
        let result = lock_scripted(&self.profiles)
            .get(&profile)
            .map(|snapshot| snapshot.preferences)
            .ok_or(ManagerError::MissingBaseline {
                resource: "preferences",
                profile: Some(profile),
            })?;
        *lock_scripted(&self.live_profile) = profile;
        Ok(result)
    }

    async fn read_buttons(&self, profile: ProfileId) -> Result<ButtonsState, ManagerError> {
        if self.is_ble() {
            return self.unsupported("read_buttons");
        }
        let result = lock_scripted(&self.profiles)
            .get(&profile)
            .map(|snapshot| snapshot.buttons)
            .ok_or(ManagerError::MissingBaseline {
                resource: "buttons",
                profile: Some(profile),
            })?;
        *lock_scripted(&self.live_profile) = profile;
        Ok(result)
    }

    async fn read_live_polling_rate(&self, alias: ProfileId) -> Result<PollingRate, ManagerError> {
        if self.is_ble() {
            return self.unsupported("read_live_polling_rate");
        }
        *lock_scripted(&self.last_polling_alias) = Some(alias);
        let live = *lock_scripted(&self.live_profile);
        lock_scripted(&self.polling_rates)
            .get(&live)
            .copied()
            .ok_or(ManagerError::MissingBaseline {
                resource: "polling rate",
                profile: Some(alias),
            })
    }

    async fn write_dpi(
        &self,
        state: DpiState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<DpiState>, ManagerError> {
        if self.is_ble() && verification == VerificationMethod::Readback {
            return Err(self.readback_unsupported("write_dpi"));
        }
        lock_scripted(&self.writes).push(ScriptedWrite::Dpi(state.clone()));
        if !self.is_ble()
            && let Some(snapshot) = lock_scripted(&self.profiles).get_mut(&state.profile)
        {
            snapshot.dpi = state.clone();
        }
        Ok(self.write_outcome(state, verification))
    }

    async fn write_preferences(
        &self,
        state: PreferencesState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<PreferencesState>, ManagerError> {
        if self.is_ble() && verification == VerificationMethod::Readback {
            return Err(self.readback_unsupported("write_preferences"));
        }
        lock_scripted(&self.writes).push(ScriptedWrite::Preferences(state));
        if !self.is_ble()
            && let Some(snapshot) = lock_scripted(&self.profiles).get_mut(&state.profile)
        {
            snapshot.preferences = state;
        }
        Ok(self.write_outcome(state, verification))
    }

    async fn write_buttons(
        &self,
        state: ButtonsState,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<ButtonsState>, ManagerError> {
        if self.is_ble() && verification == VerificationMethod::Readback {
            return Err(self.readback_unsupported("write_buttons"));
        }
        lock_scripted(&self.writes).push(ScriptedWrite::Buttons(state));
        if !self.is_ble()
            && let Some(snapshot) = lock_scripted(&self.profiles).get_mut(&state.profile)
        {
            snapshot.buttons = state;
        }
        Ok(self.write_outcome(state, verification))
    }

    async fn write_polling_rate_unchecked(
        &self,
        profile: ProfileId,
        rate: PollingRate,
        verification: VerificationMethod,
    ) -> Result<SessionWrite<PollingRate>, ManagerError> {
        if self.is_ble() && verification == VerificationMethod::Readback {
            return Err(self.readback_unsupported("write_polling_rate_unchecked"));
        }
        lock_scripted(&self.writes).push(ScriptedWrite::PollingRate(profile, rate));
        // Report `0x06` is a save alias: the deferred writer serializes the live
        // image into `profile`, so the target slot's rate becomes `rate`. The live
        // image's rate is also the new rate when the session's live profile is
        // the target; otherwise the live rate stays until the target is loaded.
        lock_scripted(&self.polling_rates).insert(profile, rate);
        Ok(self.write_outcome(rate, verification))
    }

    async fn write_profile_metadata(
        &self,
        metadata: ProfileMetadata,
    ) -> Result<SessionWrite<ProfileMetadata>, ManagerError> {
        if lock_scripted(&self.metadata_write_failure).as_ref() == Some(&metadata) {
            return Err(ManagerError::InvalidUpdate(
                "scripted profile metadata write failure".to_owned(),
            ));
        }
        lock_scripted(&self.writes).push(ScriptedWrite::ProfileMetadata(metadata));
        *lock_scripted(&self.metadata) = Some(metadata);
        // Real activation establishes the new live profile; mirror it.
        *lock_scripted(&self.live_profile) = metadata.current();
        Ok(self.write_outcome(metadata, self.verification))
    }

    async fn read_battery(&self, _timeout: Duration) -> Result<u8, ManagerError> {
        if self.is_ble() {
            return self.unsupported("read_battery");
        }
        lock_scripted(&self.battery)
            .as_ref()
            .copied()
            .ok_or(ManagerError::MissingBaseline {
                resource: "battery",
                profile: None,
            })
    }

    fn subscribe_events(&self) -> SessionEvents {
        if self.is_ble() {
            SessionEvents { input: None }
        } else {
            SessionEvents {
                input: Some(self.input_events.subscribe()),
            }
        }
    }
}

#[cfg(test)]
fn endpoint_key(endpoint: &DeviceEndpoint) -> String {
    match &endpoint.locator {
        DeviceLocator::UsbPath(path) => format!("{:?}:{}", endpoint.transport, path),
        DeviceLocator::BlePlatformId(id) => format!("{:?}:{}", endpoint.transport, id),
    }
}

#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct ScriptedFakeFactory {
    endpoints: Arc<Mutex<BTreeMap<String, DiscoveredEndpoint>>>,
    sessions: Arc<Mutex<BTreeMap<String, ScriptedFakeSession>>>,
    discovery_queue: Arc<Mutex<VecDeque<Vec<DiscoveredEndpoint>>>>,
    last_discovery: Arc<Mutex<Option<Vec<DiscoveredEndpoint>>>>,
    list_calls: Arc<Mutex<Vec<TransportSelection>>>,
}

#[cfg(test)]
impl ScriptedFakeFactory {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with_discovery_sequence(self, sequence: Vec<Vec<DiscoveredEndpoint>>) -> Self {
        *lock_scripted(&self.discovery_queue) = VecDeque::from(sequence);
        *lock_scripted(&self.last_discovery) = None;
        self
    }

    pub(crate) fn list_calls(&self) -> Vec<TransportSelection> {
        lock_scripted(&self.list_calls).clone()
    }

    pub(crate) fn with_endpoint(
        self,
        discovered: DiscoveredEndpoint,
        session: ScriptedFakeSession,
    ) -> Self {
        self.add_endpoint(discovered, session);
        self
    }

    /// Adds the first endpoint from a test identity.
    pub(crate) fn with_identity(
        self,
        identity: crate::device::DeviceIdentity,
        connected: bool,
        session: ScriptedFakeSession,
    ) -> Self {
        let endpoint = identity
            .endpoints
            .values()
            .next()
            .cloned()
            .expect("DeviceIdentity must contain at least one endpoint");
        self.add_endpoint(
            DiscoveredEndpoint {
                endpoint,
                connected,
            },
            session,
        );
        self
    }

    pub(crate) fn add_endpoint(
        &self,
        discovered: DiscoveredEndpoint,
        session: ScriptedFakeSession,
    ) {
        let key = endpoint_key(&discovered.endpoint);
        lock_scripted(&self.endpoints).insert(key.clone(), discovered);
        lock_scripted(&self.sessions).insert(key, session);
    }
}

#[cfg(test)]
#[async_trait(?Send)]
impl SessionFactory for ScriptedFakeFactory {
    async fn list(
        &self,
        selection: TransportSelection,
    ) -> Result<Vec<DiscoveredEndpoint>, ManagerError> {
        lock_scripted(&self.list_calls).push(selection);
        let candidates = if let Some(next) = lock_scripted(&self.discovery_queue).pop_front() {
            *lock_scripted(&self.last_discovery) = Some(next.clone());
            next
        } else if let Some(last) = lock_scripted(&self.last_discovery).clone() {
            last
        } else {
            lock_scripted(&self.endpoints).values().cloned().collect()
        };
        Ok(candidates
            .into_iter()
            .filter(|discovered| match selection {
                TransportSelection::Auto => true,
                TransportSelection::Exact(transport) => discovered.endpoint.transport == transport,
            })
            .collect())
    }

    async fn open(
        &self,
        endpoint: &DeviceEndpoint,
    ) -> Result<Box<dyn DeviceSession>, ManagerError> {
        let key = endpoint_key(endpoint);
        let session = lock_scripted(&self.sessions)
            .get(&key)
            .cloned()
            .ok_or_else(|| {
                ManagerError::InvalidUpdate(format!(
                    "no scripted session for endpoint {endpoint:?} (key {key})"
                ))
            })?;
        Ok(Box::new(session))
    }
}

#[cfg(test)]
mod tests {
    use super::{DeviceSession, ScriptedFakeSession, SessionWrite};
    use crate::device::{DeviceEndpoint, TransportSelection};
    use crate::error::ManagerError;
    use crate::operation::VerificationMethod;
    use attack_shark_x3::{
        ButtonAssignment, ButtonsState, DpiState, DpiValue, PollingRate, PreferencesState,
        ProfileId, StageIndex, TransportKind,
    };

    fn profile(value: u8) -> ProfileId {
        ProfileId::try_from(value).expect("test profile must be valid")
    }

    fn snapshot() -> attack_shark_x3::driver::ProfileSnapshot {
        use attack_shark_x3::ProfileMetadata;
        let dpi = DpiState::new(
            profile(2),
            vec![DpiValue::new(800).expect("valid dpi")],
            StageIndex::new(1).expect("valid stage"),
            [0; 25],
        )
        .expect("valid dpi state");
        let slots = [ButtonAssignment::default(); 18];
        attack_shark_x3::driver::ProfileSnapshot {
            persistent_metadata: ProfileMetadata::new(profile(2), profile(5))
                .expect("valid metadata"),
            target_profile: profile(2),
            dpi,
            preferences: PreferencesState::new(profile(2), 0, 0, 0, [0, 0, 0], 0, 0),
            buttons: ButtonsState::new(profile(2), slots),
        }
    }

    /// Values distinct from the baseline snapshot so state-update assertions
    /// prove the write landed rather than echoing the seeded baseline.
    fn written_resources() -> (DpiState, PreferencesState, ButtonsState) {
        let dpi = DpiState::new(
            profile(2),
            vec![DpiValue::new(1600).expect("valid dpi")],
            StageIndex::new(1).expect("valid stage"),
            [0; 25],
        )
        .expect("valid dpi state");
        let preferences = PreferencesState::new(profile(2), 1, 2, 3, [4, 5, 6], 7, 8);
        let mut slots = [ButtonAssignment::default(); 18];
        slots[0] = ButtonAssignment::new(0x01, 0x02, 0x03);
        (dpi, preferences, ButtonsState::new(profile(2), slots))
    }

    #[tokio::test]
    async fn usb_fake_routes_transport_to_acknowledged() {
        let usb = ScriptedFakeSession::usb().with_profile(snapshot());
        let (dpi, preferences, buttons) = written_resources();

        assert_eq!(
            usb.write_dpi(dpi, VerificationMethod::Transport)
                .await
                .expect("USB transport DPI write succeeds"),
            SessionWrite::Acknowledged
        );
        assert_eq!(
            usb.write_preferences(preferences, VerificationMethod::Transport)
                .await
                .expect("USB transport preferences write succeeds"),
            SessionWrite::Acknowledged
        );
        assert_eq!(
            usb.write_buttons(buttons, VerificationMethod::Transport)
                .await
                .expect("USB transport buttons write succeeds"),
            SessionWrite::Acknowledged
        );
        assert_eq!(
            usb.write_polling_rate_unchecked(
                profile(2),
                PollingRate::Hz1000,
                VerificationMethod::Transport
            )
            .await
            .expect("USB transport polling rate write succeeds"),
            SessionWrite::Acknowledged
        );
    }

    #[tokio::test]
    async fn usb_fake_routes_readback_to_readback_verified_and_updates_state() {
        let usb = ScriptedFakeSession::usb().with_profile(snapshot());
        let (dpi, preferences, buttons) = written_resources();

        assert_eq!(
            usb.write_dpi(dpi.clone(), VerificationMethod::Readback)
                .await
                .expect("USB readback DPI write succeeds"),
            SessionWrite::ReadbackVerified(dpi.clone())
        );
        assert_eq!(
            usb.write_preferences(preferences, VerificationMethod::Readback)
                .await
                .expect("USB readback preferences write succeeds"),
            SessionWrite::ReadbackVerified(preferences)
        );
        assert_eq!(
            usb.write_buttons(buttons, VerificationMethod::Readback)
                .await
                .expect("USB readback buttons write succeeds"),
            SessionWrite::ReadbackVerified(buttons)
        );
        assert_eq!(
            usb.write_polling_rate_unchecked(
                profile(2),
                PollingRate::Hz1000,
                VerificationMethod::Readback
            )
            .await
            .expect("USB readback polling rate write succeeds"),
            SessionWrite::ReadbackVerified(PollingRate::Hz1000)
        );

        // Successful writes land in the fake device state.
        assert_eq!(usb.read_dpi(profile(2)).await.expect("read back DPI"), dpi);
        assert_eq!(
            usb.read_preferences(profile(2))
                .await
                .expect("read back preferences"),
            preferences
        );
        assert_eq!(
            usb.read_buttons(profile(2))
                .await
                .expect("read back buttons"),
            buttons
        );
        // Polling is live: after the targeted reads above, live is profile 2.
        assert_eq!(
            usb.read_live_polling_rate(profile(2))
                .await
                .expect("read back polling rate"),
            PollingRate::Hz1000
        );
    }

    #[tokio::test]
    async fn ble_fake_accepts_transport_but_rejects_readback() {
        let ble = ScriptedFakeSession::ble();
        let (dpi, preferences, buttons) = written_resources();

        assert_eq!(
            ble.write_dpi(dpi, VerificationMethod::Transport)
                .await
                .expect("BLE transport DPI write succeeds"),
            SessionWrite::Acknowledged
        );
        assert_eq!(
            ble.write_preferences(preferences, VerificationMethod::Transport)
                .await
                .expect("BLE transport preferences write succeeds"),
            SessionWrite::Acknowledged
        );
        assert_eq!(
            ble.write_buttons(buttons, VerificationMethod::Transport)
                .await
                .expect("BLE transport buttons write succeeds"),
            SessionWrite::Acknowledged
        );
        assert_eq!(
            ble.write_polling_rate_unchecked(
                profile(2),
                PollingRate::Hz1000,
                VerificationMethod::Transport
            )
            .await
            .expect("BLE transport polling rate write succeeds"),
            SessionWrite::Acknowledged
        );

        let (dpi, preferences, buttons) = written_resources();
        assert!(matches!(
            ble.write_dpi(dpi, VerificationMethod::Readback).await,
            Err(ManagerError::UnsupportedOperation { .. })
        ));
        assert!(matches!(
            ble.write_preferences(preferences, VerificationMethod::Readback)
                .await,
            Err(ManagerError::UnsupportedOperation { .. })
        ));
        assert!(matches!(
            ble.write_buttons(buttons, VerificationMethod::Readback)
                .await,
            Err(ManagerError::UnsupportedOperation { .. })
        ));
        assert!(matches!(
            ble.write_polling_rate_unchecked(
                profile(2),
                PollingRate::Hz1000,
                VerificationMethod::Readback
            )
            .await,
            Err(ManagerError::UnsupportedOperation { .. })
        ));
    }

    #[tokio::test]
    async fn live_polling_alias_is_side_effect_not_content() {
        // Per-profile rates with live profile 1.
        let session = ScriptedFakeSession::usb()
            .with_profile(snapshot())
            .with_profile(attack_shark_x3::driver::ProfileSnapshot {
                target_profile: profile(1),
                persistent_metadata: attack_shark_x3::ProfileMetadata::new(profile(1), profile(5))
                    .expect("valid metadata"),
                dpi: snapshot().dpi.clone(),
                preferences: snapshot().preferences,
                buttons: snapshot().buttons,
            })
            .with_polling_rate_for(profile(1), PollingRate::Hz500)
            .with_polling_rate_for(profile(2), PollingRate::Hz1000)
            .with_live_profile(profile(1));
        // Standalone alias 2 returns live profile 1 rate and records alias.
        let rate_via_alias2 = session
            .read_live_polling_rate(profile(2))
            .await
            .expect("live polling rate via alias 2 must succeed");
        assert_eq!(
            rate_via_alias2,
            PollingRate::Hz500,
            "alias 2 must return live 1 rate, exposing cross-profile contamination"
        );
        assert_eq!(session.last_polling_alias(), Some(profile(2)));
        // Loading target 2 establishes it as live; alias 2 now returns profile 2 rate.
        session
            .read_profile(profile(2))
            .await
            .expect("loading profile 2 must succeed");
        let rate_after_load = session
            .read_live_polling_rate(profile(2))
            .await
            .expect("live polling after load must succeed");
        assert_eq!(rate_after_load, PollingRate::Hz1000);
        // Even alias 1 now returns live 2 rate.
        let rate_via_alias1 = session
            .read_live_polling_rate(profile(1))
            .await
            .expect("alias 1 must now return live 2 rate");
        assert_eq!(rate_via_alias1, PollingRate::Hz1000);
        assert_eq!(session.last_polling_alias(), Some(profile(1)));
    }

    #[tokio::test]
    async fn polling_write_updates_target_slot_and_live_visibility() {
        let session = ScriptedFakeSession::usb()
            .with_profile(snapshot())
            .with_profile(attack_shark_x3::driver::ProfileSnapshot {
                target_profile: profile(1),
                persistent_metadata: attack_shark_x3::ProfileMetadata::new(profile(1), profile(5))
                    .expect("valid metadata"),
                dpi: snapshot().dpi.clone(),
                preferences: snapshot().preferences,
                buttons: snapshot().buttons,
            })
            .with_polling_rate_for(profile(1), PollingRate::Hz1000)
            .with_polling_rate_for(profile(2), PollingRate::Hz500)
            .with_live_profile(profile(1));
        // Write alias 2 while live is 1: target slot updated, live unchanged.
        session
            .write_polling_rate_unchecked(
                profile(2),
                PollingRate::Hz250,
                VerificationMethod::Readback,
            )
            .await
            .expect("write alias 2 must succeed");
        let still_live1 = session
            .read_live_polling_rate(profile(2))
            .await
            .expect("alias 2 while live 1 must still return live 1 rate");
        assert_eq!(still_live1, PollingRate::Hz1000);
        // Load target 2: now live 2 returns the newly written rate.
        session.read_profile(profile(2)).await.expect("load 2");
        let now_live2 = session
            .read_live_polling_rate(profile(1))
            .await
            .expect("alias 1 while live 2 must return live 2 rate");
        assert_eq!(now_live2, PollingRate::Hz250);
        // Write via live 2 alias: immediate visibility.
        session
            .write_polling_rate_unchecked(
                profile(2),
                PollingRate::Hz500,
                VerificationMethod::Readback,
            )
            .await
            .expect("write alias 2 while live 2");
        let immediate = session
            .read_live_polling_rate(profile(2))
            .await
            .expect("must see new live rate");
        assert_eq!(immediate, PollingRate::Hz500);
        // Metadata activation updates live profile.
        let meta = attack_shark_x3::ProfileMetadata::new(profile(2), profile(5)).expect("metadata");
        session
            .write_profile_metadata(meta)
            .await
            .expect("metadata write");
        let after_meta = session
            .read_live_polling_rate(profile(1))
            .await
            .expect("after metadata activation live is 2");
        assert_eq!(after_meta, PollingRate::Hz500);
    }

    /// Discovery for one parsed `X3_SIM` spec, without touching the process
    /// environment (specs are injected, exactly as `sim_spec_from_env` yields
    /// them for the real variable).
    fn sim_endpoints(
        spec: Option<attack_shark_x3::sim::SimSpec>,
        selection: TransportSelection,
    ) -> Vec<crate::operation::DiscoveredEndpoint> {
        super::sim_endpoints_for_spec(spec, selection).expect("sim discovery")
    }

    fn replay_spec(scenario: &str) -> Option<attack_shark_x3::sim::SimSpec> {
        Some(attack_shark_x3::sim::SimSpec::Replay {
            scenario: scenario.to_owned(),
        })
    }

    fn model_spec(preset: &str) -> Option<attack_shark_x3::sim::SimSpec> {
        Some(attack_shark_x3::sim::SimSpec::Model {
            preset: preset.to_owned(),
        })
    }

    #[test]
    fn sim_env_unset_registers_no_endpoints() {
        assert!(sim_endpoints(None, TransportSelection::Auto).is_empty());
    }

    #[test]
    fn sim_tokens_are_stable_distinct_and_decodable() {
        use attack_shark_x3::{WatermarkDecode, decode_watermark};
        let tuned = super::sim_token_for("tuned", 0);
        let tuned_again = super::sim_token_for("tuned", 0);
        let stock = super::sim_token_for("stock", 0);
        let tuned_second = super::sim_token_for("tuned", 1);
        assert_eq!(tuned, tuned_again, "stable across restarts");
        assert_ne!(tuned, stock, "distinct per scenario");
        assert_ne!(tuned, tuned_second, "distinct per endpoint");
        assert_eq!(
            decode_watermark(&tuned.to_watermark_bytes()),
            WatermarkDecode::Valid(tuned),
            "served tails must authenticate"
        );
        assert_ne!(tuned.token_bytes(), [0u8; 16], "never reserved");
    }

    #[test]
    fn sim_replay_scenario_registers_one_endpoint_with_backend_tag() {
        use attack_shark_x3::sim::{SIM_REPLAY_TAG, SimBackend, SimReplay};
        let endpoints = sim_endpoints(replay_spec("stock"), TransportSelection::Auto);
        assert_eq!(endpoints.len(), 1, "stock declares one wired endpoint");
        let endpoint = &endpoints[0].endpoint;
        assert_eq!(endpoint.transport, TransportKind::Wired);
        assert!(endpoint.is_coherent());
        let tag = super::sim_tag_for_endpoint(endpoint).expect("sim endpoint carries its tag");
        assert_eq!(tag, SIM_REPLAY_TAG);
        // The registered tag is exactly what the backend reports via tag().
        let backend = SimReplay::load("stock").expect("stock loads");
        assert_eq!(tag, backend.tag());
    }

    #[test]
    fn sim_model_preset_registers_one_endpoint_with_backend_tag() {
        use attack_shark_x3::sim::{SIM_MODEL_TAG, SimBackend};
        let endpoints = sim_endpoints(model_spec("tuned"), TransportSelection::Auto);
        assert_eq!(
            endpoints.len(),
            1,
            "model presets declare one wired endpoint"
        );
        let tag = super::sim_tag_for_endpoint(&endpoints[0].endpoint).expect("tagged");
        assert_eq!(tag, SIM_MODEL_TAG);
        assert_eq!(
            tag,
            crate::sim_model::SimModel::tuned().tag(),
            "registered tag equals the backend's tag()"
        );
    }

    #[test]
    fn sim_scenario_endpoints_follow_the_transport_selection() {
        let endpoints = sim_endpoints(replay_spec("two-ble-unknown"), TransportSelection::Auto);
        assert_eq!(endpoints.len(), 2, "two BLE endpoints register");
        assert!(
            endpoints
                .iter()
                .all(|entry| entry.endpoint.transport == TransportKind::Ble)
        );
        for entry in &endpoints {
            assert!(entry.endpoint.is_coherent());
            assert!(super::is_sim_endpoint(&entry.endpoint));
        }
        // A wired-only scan registers nothing from a BLE-only scenario, and
        // an exact BLE scan registers both.
        assert!(
            sim_endpoints(
                replay_spec("two-ble-unknown"),
                TransportSelection::Exact(TransportKind::Wired)
            )
            .is_empty()
        );
        assert_eq!(
            sim_endpoints(
                replay_spec("two-ble-unknown"),
                TransportSelection::Exact(TransportKind::Ble)
            )
            .len(),
            2
        );
    }

    #[test]
    fn sim_garbage_spec_fails_discovery_loudly() {
        let error =
            attack_shark_x3::sim::parse_sim_spec("bogus:thing").expect_err("garbage env must fail");
        let rendered = format!("{error}");
        assert!(
            rendered.contains("X3_SIM"),
            "names the variable: {rendered}"
        );
        assert!(rendered.contains("bogus"), "names the value: {rendered}");
        // The same gap surfaces through manager discovery.
        let missing =
            attack_shark_x3::sim::parse_sim_spec("replay:no-such-scenario").expect("parses");
        let error = super::sim_endpoints_for_spec(missing, TransportSelection::Auto)
            .expect_err("missing scenario must fail discovery");
        assert!(
            format!("{error}").contains("no-such-scenario"),
            "names the scenario: {error}"
        );
    }

    #[test]
    fn hardware_locators_never_read_as_sim() {
        let hardware = DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa61,
            None,
            r"\\?\hid#vid_1d57&pid_fa61&mi_02&col04",
            None,
        )
        .expect("hardware endpoint");
        assert!(!super::is_sim_endpoint(&hardware));
        assert_eq!(super::sim_tag_for_endpoint(&hardware), None);
        assert!(
            super::open_sim_session(&hardware)
                .expect("hardware is not a sim gap")
                .is_none(),
            "hardware locators are never sim sessions"
        );
    }

    #[test]
    fn sim_endpoint_without_env_is_a_loud_gap() {
        let endpoints = sim_endpoints(replay_spec("stock"), TransportSelection::Auto);
        let error = match super::open_sim_session_for_spec(None, &endpoints[0].endpoint) {
            Err(error) => error,
            Ok(_) => panic!("a sim locator without X3_SIM is a gap"),
        };
        assert!(format!("{error}").contains("X3_SIM"));
    }

    #[tokio::test]
    async fn sim_model_session_roundtrips_a_write() {
        let endpoints = sim_endpoints(model_spec("tuned"), TransportSelection::Auto);
        let session = super::open_sim_session_for_spec(model_spec("tuned"), &endpoints[0].endpoint)
            .expect("open sim session")
            .expect("sim locator opens a session");
        assert_eq!(session.transport(), TransportKind::Wired);

        let mut state = session.read_dpi(profile(2)).await.expect("read dpi");
        state.stages[0] = DpiValue::new(1500).expect("valid dpi stage");
        let written = session
            .write_dpi(state.clone(), VerificationMethod::Readback)
            .await
            .expect("sim model accepts the write");
        let SessionWrite::ReadbackVerified(accepted) = written else {
            panic!("wired sim writes read back");
        };
        assert_eq!(accepted, state);
        assert_eq!(
            session.read_dpi(profile(2)).await.expect("read back"),
            state,
            "write persists in the shared model"
        );
    }

    #[tokio::test]
    async fn sim_model_ble_endpoint_refuses_reads_like_hardware() {
        let endpoints = sim_endpoints(model_spec("stock"), TransportSelection::Auto);
        // Same locator, opened as the BLE endpoint the transport says it is.
        let mut ble = endpoints[0].endpoint.clone();
        ble.transport = TransportKind::Ble;
        let session = super::open_sim_session_for_spec(model_spec("stock"), &ble)
            .expect("open sim session")
            .expect("sim locator opens a session");
        assert_eq!(session.transport(), TransportKind::Ble);
        assert!(matches!(
            session.read_dpi(profile(1)).await,
            Err(ManagerError::UnsupportedOperation { .. })
        ));
    }

    #[tokio::test]
    async fn sim_replay_session_gaps_loudly_on_unrecorded_writes() {
        let endpoints = sim_endpoints(replay_spec("stock"), TransportSelection::Auto);
        let session =
            super::open_sim_session_for_spec(replay_spec("stock"), &endpoints[0].endpoint)
                .expect("open sim session")
                .expect("sim locator opens a session");
        let state = session.read_dpi(profile(1)).await.expect("stock serves");
        let error = session
            .write_dpi(state, VerificationMethod::Readback)
            .await
            .expect_err("stock ships no post-write capture");
        let rendered = format!("{error}");
        assert!(
            rendered.contains("post-write") || rendered.contains("no recorded capture"),
            "gap names the missing capture: {rendered}"
        );
    }

    #[tokio::test]
    async fn sim_ble_replay_endpoint_refuses_reads() {
        let endpoints = sim_endpoints(
            replay_spec("two-ble-unknown"),
            TransportSelection::Exact(TransportKind::Ble),
        );
        let session = super::open_sim_session_for_spec(
            replay_spec("two-ble-unknown"),
            &endpoints[0].endpoint,
        )
        .expect("open sim session")
        .expect("sim locator opens a session");
        assert_eq!(session.transport(), TransportKind::Ble);
        assert!(matches!(
            session.read_buttons(profile(1)).await,
            Err(ManagerError::UnsupportedOperation { .. })
        ));
    }
}
