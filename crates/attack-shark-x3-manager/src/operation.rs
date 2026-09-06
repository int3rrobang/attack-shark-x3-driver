use std::collections::BTreeMap;

use attack_shark_x3::driver::ProfileSnapshot;
use attack_shark_x3::{
    BatteryEvent, ButtonsState, ConnectionChangedEvent, DpiButtonEvent, DpiIndexChangedEvent,
    DpiState, InputEvent, LedModeChangedEvent, PhysicalId, PollingRate, PreferencesState,
    ProfileChangedEvent, ProfileId, ProfileMetadata, TransportKind,
};
use serde::{Deserialize, Serialize};

use crate::device::{DeviceEndpoint, DeviceId, DeviceIdentity};
use crate::state::{IdentityMode, ResourceState, Verification};
/// The verification performed after a write operation.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VerificationMethod {
    /// Rely on the transport's own acknowledgement of the write.
    #[default]
    Transport,
    /// Verify the write by reading the value back from the device.
    Readback,
}

/// Where a delta-merge reads the baseline image it merges against.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BaselineSource {
    /// Read the live image from the device immediately before merging.
    #[default]
    Live,
    /// Merge against the durable stored baseline (desired, then observed).
    Stored,
}

/// Safety and verification policy for a typed update.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UpdatePolicy {
    /// Permit explicit captured defaults when a transport cannot read a baseline.
    pub allow_explicit_defaults: bool,
    /// The requested post-write verification strength.
    pub verification: VerificationMethod,
    /// Where a delta-merge reads its baseline image.
    pub baseline: BaselineSource,
}

impl Default for UpdatePolicy {
    fn default() -> Self {
        Self {
            allow_explicit_defaults: false,
            verification: VerificationMethod::Transport,
            baseline: BaselineSource::Live,
        }
    }
}

/// Durable desired and observed evidence for one operation resource.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSnapshot<T> {
    pub resource: ResourceState<T>,
}

/// The requested value and evidence produced by a write.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteOutcome<T> {
    pub desired: T,
    pub observed: Option<T>,
    pub verification: Verification,
}
/// One complete profile image observed during an all-profile refresh.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshedProfile {
    pub dpi: DpiState,
    pub preferences: PreferencesState,
    pub buttons: ButtonsState,
    pub polling_rate: PollingRate,
}

/// Profile resource whose fresh observation differs from durable desired state.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProfileResourceKind {
    Dpi,
    Preferences,
    Buttons,
    PollingRate,
}

/// Fresh USB observations captured across all five hardware profile slots.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FullProfileRefreshOutcome {
    pub original_metadata: ProfileMetadata,
    pub restored_metadata: ProfileMetadata,
    pub temporarily_expanded: bool,
    pub profiles: BTreeMap<ProfileId, RefreshedProfile>,
    pub drift: BTreeMap<ProfileId, Vec<ProfileResourceKind>>,
    pub profile_metadata_drift: bool,
}

/// A raw transport endpoint discovered before logical association.
///
/// This carries an endpoint only, never a logical `DeviceId`. The manager
/// associates raw discoveries to a logical `mouse-N` via [`StateFile::allocate_device_id`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredEndpoint {
    pub endpoint: DeviceEndpoint,
    pub connected: bool,
}

/// A discovered logical mouse and whether it is currently connected.
///
/// After association, the logical identity owns one or more endpoints, so
/// discovery rows are aggregated per logical device and `transports` records
/// which transports produced discovery entries this scan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredDevice {
    pub identity: DeviceIdentity,
    pub connected: bool,
    /// Transports on which the device was discovered this scan, sorted and
    /// without duplicates.
    pub transports: Vec<TransportKind>,
}

/// A discovered compatible hardware connection, before any durable
/// association.
///
/// Rows are one per transport endpoint and are never aggregated: a single
/// physical mouse can expose several (wired FA61, receiver FA60, BLE). This
/// is deliberately separate from [`DiscoveredDevice`], which aggregates
/// endpoints under a durable logical `mouse-N`. A connection never carries a
/// logical `DeviceId`; it may carry the opaque physical token when the
/// transport was able to read one.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredConnection {
    pub endpoint: DeviceEndpoint,
    pub connected: bool,
    /// The physical identity token observed on this connection, when the
    /// transport read one. `None` before any identity read or when no
    /// watermark is present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub physical_id: Option<PhysicalId>,
}

/// Why a discovered connection has no associated durable logical mouse.
///
/// These are manager-level conclusions built from the transport-level
/// watermark decode ([`attack_shark_x3::WatermarkDecode`]). Unknown beats
/// incorrect identity: no variant ever guesses an association.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UnassociatedReason {
    /// No watermark was observed: legacy single-mouse mode or an unmarked
    /// device. No identity claim is possible.
    Absent,
    /// A watermark marker was present but malformed (bad magic, length, or
    /// integrity check). It decodes as no valid identity, never best-effort.
    Malformed,
    /// A valid marker for a format version this installation does not
    /// support.
    Unsupported { version: u8 },
    /// A valid token with no registered logical mouse in this installation;
    /// the device is adoptable as a new logical mouse.
    Unknown,
    /// The observed token is already bound to another logical mouse. This is
    /// the duplicate-identity case; automatic association is refused.
    Duplicate,
    /// The token falls in a reserved range (for example all-zero) and is
    /// never assignable to a device.
    Reserved,
    /// The token is reserved by the active identity-setup journal (a pending
    /// enrollment, restore rotation, or adoption). It may not be adopted
    /// independently until the journal completes or is cancelled.
    ReservedByJournal,
}

/// Strict result of resolving one discovered connection against the durable
/// logical mice of this installation.
///
/// Resolution never guesses: a valid known token resolves to the exact
/// logical mouse; anything else is an explicit non-association with a
/// reason. In persistent identity mode an absent token means the physical
/// identity is lost and requires explicit reassociation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IdentityResolution {
    /// A valid known token resolved to the exact durable logical mouse.
    Resolved { identity: DeviceId },
    /// No durable logical mouse is claimed; see [`UnassociatedReason`].
    Unassociated {
        reason: UnassociatedReason,
        /// The valid token observed on the connection, when one exists (for
        /// example the adoptable `Unknown` or conflicting `Duplicate` cases).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        physical_id: Option<PhysicalId>,
    },
}

/// One discovered connection resolved against the durable logical mice of
/// this installation.
///
/// `resolution` is strict: a valid known token resolves to the exact logical
/// mouse; every other case is an explicit non-association with a reason.
/// Unassociated connections are never persisted as logical devices and never
/// receive a temporary `mouse-N`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedConnection {
    pub endpoint: DeviceEndpoint,
    pub connected: bool,
    pub resolution: IdentityResolution,
}

/// Aggregate result of a mode-aware discovery scan.
///
/// In [`IdentityMode::Legacy`] the installation has exactly one fuzzy logical
/// mouse (created on first discovery if absent); every compatible connection
/// resolves to it and no watermark is ever read or written. In
/// [`IdentityMode::Persistent`] connections authenticate by their current DPI
/// watermark once per continuous attachment and resolve to logical mice by
/// token; absent, malformed, unsupported, unknown, duplicate, and reserved
/// markers surface as unassociated connections.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryView {
    /// The installation identity mode at scan time.
    pub mode: IdentityMode,
    /// Logical mice with connections seen this scan, aggregated per device
    /// (`transports` lists the transports observed this scan).
    pub devices: Vec<DiscoveredDevice>,
    /// Every compatible connection observed this scan, resolved individually
    /// and never aggregated.
    pub connections: Vec<ResolvedConnection>,
}
/// A supported USB device appeared or disappeared from the operating
/// system's hotplug event stream.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceTopologyEvent {
    Connected,
    Disconnected,
}

/// Which physical-identity ceremony a progress report describes.
///
/// Frontend-facing counterpart of the durable journal phase
/// [`crate::state::IdentitySetupPhase`], which the manager persists for
/// resumability; the variant sets align one-to-one.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IdentityCeremonyKind {
    /// First transition from fuzzy single-mouse mode into persistent
    /// physical identity: two physical mice are captured and stamped.
    InitialEnrollment,
    /// Adding a further mouse to an already-identified installation.
    AddMouse,
    /// Reassociating a logical mouse whose watermark was lost or erased.
    Restore,
    /// Adopting a valid token unknown to this installation as a new logical
    /// mouse (a foreign mouse already tagged elsewhere).
    ForeignAdoption,
    /// Associating a BLE platform endpoint with a physical/logical mouse.
    BleAssociation,
}

/// Status of a running physical-identity ceremony.
///
/// The stage is the ceremony's current status; the frontend renders its own
/// copy, so no presentation text lives here. The durable, resumable journal
/// records only coarse phases ([`crate::state::IdentitySetupStage`]); this
/// status covers the full transient lifecycle including user gestures,
/// verification, and failure. `error` in [`IdentityCeremonyStage::Failed`]
/// is a machine-readable diagnostic, not user-facing copy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IdentityCeremonyStage {
    /// Ceremony created; no physical action has started yet.
    Ready,
    /// Waiting for the user to reconnect or present the physical mouse.
    AwaitingReconnect,
    /// Reading the watermark and capturing the presented device's state.
    Capturing,
    /// Assigning the physical token and writing the watermark.
    Stamping,
    /// The stamped watermark was read back and verified.
    Verified,
    /// The ceremony completed successfully.
    Complete,
    /// The ceremony was aborted by the user.
    Cancelled,
    /// The ceremony failed; `error` is a machine-readable diagnostic, not
    /// user-facing text.
    Failed { error: String },
}

/// Progress snapshot of a running physical-identity ceremony.
///
/// Frontends render this directly; all presentation copy lives in the
/// frontend, never here.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityCeremonyProgress {
    pub kind: IdentityCeremonyKind,
    pub stage: IdentityCeremonyStage,
    /// 1-based index of the physical device currently being processed, when
    /// the ceremony spans several devices (for example initial setup).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u8>,
    /// Total number of physical devices the ceremony processes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_steps: Option<u8>,
    /// The logical mouse being processed or produced, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<DeviceId>,
    /// The physical token assigned or observed, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub physical_id: Option<PhysicalId>,
    /// The endpoint currently driving the ceremony, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<DeviceEndpoint>,
}

/// A typed action a frontend can request against a physical-identity
/// ceremony.
///
/// Which actions are valid depends on the ceremony kind and stage; the
/// manager validates them against the running ceremony.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IdentityCeremonyAction {
    /// Start a ceremony of the given kind.
    Begin(IdentityCeremonyKind),
    /// The user reconnected or presented the physical mouse; proceed.
    Reconnected,
    /// Capture the presented device's current state.
    Capture,
    /// Assign the generated token and stamp the watermark.
    Stamp,
    /// Adopt the observed valid foreign token as a new logical mouse.
    Adopt,
    /// Associate the presented endpoint with the target logical mouse.
    Associate,
    /// Apply legacy name/state migration when the match is unique (initial
    /// setup).
    AcceptMigration,
    /// Skip legacy name/state migration; use fresh defaults (initial setup).
    SkipMigration,
    /// Abort the running ceremony.
    Cancel,
}

/// Resource status read from one logical device identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStatus {
    pub identity: DeviceIdentity,
    pub battery: Option<u8>,
    pub profile_metadata: Option<ResourceSnapshot<ProfileMetadata>>,
    pub polling_rate: Option<ResourceSnapshot<PollingRate>>,
}
/// One complete USB working-profile read performed through a single session.
///
/// Unlike separately calling status and profile reads, this loads the target
/// profile only once before reading its live polling rate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveProfileSnapshot {
    pub identity: DeviceIdentity,
    pub battery: Option<u8>,
    pub profile_metadata: ProfileMetadata,
    pub profile: ProfileSnapshot,
    pub polling_rate: PollingRate,
}

/// Complete profile verification evidence after a profile-reload workflow.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileVerificationOutcome {
    pub profile: ProfileId,
    pub dpi: WriteOutcome<DpiState>,
    pub preferences: WriteOutcome<PreferencesState>,
    pub buttons: WriteOutcome<ButtonsState>,
    pub polling_rate: WriteOutcome<PollingRate>,
}

/// Complete profile verification evidence after a physical power cycle.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PowerCycleVerificationOutcome {
    pub profile: ProfileId,
    pub dpi: WriteOutcome<DpiState>,
    pub preferences: WriteOutcome<PreferencesState>,
    pub buttons: WriteOutcome<ButtonsState>,
    pub polling_rate: WriteOutcome<PollingRate>,
}
#[cfg(any(feature = "usb", feature = "ble"))]
/// Complete profile images already read through this manager and held by a
/// frontend while it edits a draft. Supplying this baseline avoids a second
/// pre-write hardware read; post-write verification remains unchanged.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileUpdateBaseline {
    pub dpi: DpiState,
    pub preferences: PreferencesState,
    pub buttons: ButtonsState,
}

#[cfg(any(feature = "usb", feature = "ble"))]
/// Composite profile delta applied through one manager-owned session.
/// Every field is optional. Frontends express their full draft in one value:
/// DPI and preferences as sparse deltas, buttons as a list of slot deltas,
/// and polling rate as the desired rate. Empty updates are rejected before
/// any transport access; polling rate may not be combined with other fields
/// because report `0x06` must remain isolated.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpi: Option<crate::resources::dpi::DpiDelta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferences: Option<crate::resources::settings::PreferencesDelta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buttons: Vec<crate::resources::buttons::ButtonSlotDelta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polling_rate: Option<PollingRate>,
}

#[cfg(any(feature = "usb", feature = "ble"))]
impl ProfileUpdate {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.dpi.is_none()
            && self.preferences.is_none()
            && self.buttons.is_empty()
            && self.polling_rate.is_none()
    }

    #[must_use]
    pub fn has_non_rate(&self) -> bool {
        self.dpi.is_some() || self.preferences.is_some() || !self.buttons.is_empty()
    }

    #[must_use]
    pub fn has_polling(&self) -> bool {
        self.polling_rate.is_some()
    }
}

/// Outcome of a composite profile update.
///
/// Each field is the final `WriteOutcome` for that resource when it was part
/// of the update. Fields not included in the update remain `None`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileUpdateOutcome {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpi: Option<WriteOutcome<DpiState>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferences: Option<WriteOutcome<PreferencesState>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buttons: Option<WriteOutcome<ButtonsState>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polling_rate: Option<WriteOutcome<PollingRate>>,
}

/// Input events exposed by the manager without transport-specific handles.
///
/// This stream is lossy and bounded: the underlying transport delivers raw
/// HID reports into a fixed-capacity broadcast channel (16). When subscribers
/// fall behind, older reports are dropped and the next successful delivery
/// surfaces as [`DeviceEvent::Lagged`] carrying the number of skipped messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceEvent {
    BatteryChanged(BatteryEvent),
    ActiveDpiStageChanged(DpiButtonEvent),
    ProfileChanged(ProfileChangedEvent),
    SecondaryProfileChanged(ProfileChangedEvent),
    ConnectionChanged(ConnectionChangedEvent),
    DpiIndexChanged(DpiIndexChangedEvent),
    LedModeChanged(LedModeChangedEvent),
    ProfileSync(ProfileChangedEvent),
    Disconnected,
    /// Bounded channel overflow: `skipped` reports were dropped before this
    /// notification. Treat as a lossy gap, not a delivered event.
    Lagged {
        skipped: u64,
    },
}

impl From<InputEvent> for DeviceEvent {
    fn from(event: InputEvent) -> Self {
        match event {
            InputEvent::ActiveDpiStageChanged(event) => Self::ActiveDpiStageChanged(event),
            InputEvent::ProfileChanged(event) => Self::ProfileChanged(event),
            InputEvent::SecondaryProfileChanged(event) => Self::SecondaryProfileChanged(event),
            InputEvent::BatteryChanged(event) => Self::BatteryChanged(event),
            InputEvent::ConnectionChanged(event) => Self::ConnectionChanged(event),
            InputEvent::DpiIndexChanged(event) => Self::DpiIndexChanged(event),
            InputEvent::LedModeChanged(event) => Self::LedModeChanged(event),
            InputEvent::ProfileSync(event) => Self::ProfileSync(event),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DeviceEvent, DiscoveredEndpoint, PowerCycleVerificationOutcome, ProfileVerificationOutcome,
        UpdatePolicy, VerificationMethod, WriteOutcome,
    };
    use crate::device::DeviceEndpoint;
    use crate::state::{ApplicationVerification, PersistenceVerification, Timestamp, Verification};
    use attack_shark_x3::{
        ButtonsState, ConnectionChangedEvent, DpiState, InputEvent, PollingRate, PreferencesState,
        ProfileId, TransportKind,
    };

    #[test]
    fn maps_low_level_input_events_without_losing_raw_bytes() {
        let raw_report = [0x03, 0x10, 0x50, 0x01, 0x00];
        let event = InputEvent::ConnectionChanged(ConnectionChangedEvent {
            raw_report,
            connected: false,
        });
        assert_eq!(
            DeviceEvent::from(event),
            DeviceEvent::ConnectionChanged(ConnectionChangedEvent {
                raw_report,
                connected: false,
            })
        );
    }

    #[test]
    fn default_update_policy_uses_transport_verification() {
        assert_eq!(
            UpdatePolicy::default().verification,
            VerificationMethod::Transport
        );
    }

    #[test]
    fn verification_method_serializes_to_transport_and_readback() {
        assert_eq!(
            serde_json::to_string(&VerificationMethod::Transport).unwrap(),
            "\"transport\""
        );
        assert_eq!(
            serde_json::to_string(&VerificationMethod::Readback).unwrap(),
            "\"readback\""
        );
    }

    #[test]
    fn verification_method_deserializes_transport_and_readback() {
        assert_eq!(
            serde_json::from_str::<VerificationMethod>("\"transport\"").unwrap(),
            VerificationMethod::Transport
        );
        assert_eq!(
            serde_json::from_str::<VerificationMethod>("\"readback\"").unwrap(),
            VerificationMethod::Readback
        );
    }

    #[test]
    fn raw_discovery_carries_endpoint_without_logical_id() {
        let endpoint = DeviceEndpoint::ble("platform-123", Some("Mouse")).unwrap();
        let discovered = DiscoveredEndpoint {
            endpoint: endpoint.clone(),
            connected: true,
        };
        let json = serde_json::to_string(&discovered).unwrap();
        assert!(
            !json.contains("mouse-"),
            "raw discovery should not embed logical id"
        );
        assert!(json.contains("blePlatformId") || json.contains("platform-123"));
        assert_eq!(discovered.endpoint, endpoint);
    }

    #[test]
    fn wired_endpoint_not_used_as_stable_identity_and_ble_uses_platform_id() {
        let wired = DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa60,
            None,
            "/dev/hidraw0",
            None,
        )
        .unwrap();
        let wired2 = DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa60,
            None,
            "/dev/hidraw1",
            None,
        )
        .unwrap();
        assert_ne!(wired.locator, wired2.locator);
        assert_eq!(wired.transport, wired2.transport);
        let ble = DeviceEndpoint::ble("ble-platform-id-XYZ", None).unwrap();
        assert!(matches!(
            ble.locator,
            crate::device::DeviceLocator::BlePlatformId(_)
        ));
    }

    #[test]
    fn lagged_variant_carries_skipped_count_and_is_observable() {
        let event = DeviceEvent::Lagged { skipped: 7 };
        assert_eq!(event, DeviceEvent::Lagged { skipped: 7 });
        assert_ne!(event, DeviceEvent::Lagged { skipped: 3 });
        assert_ne!(event, DeviceEvent::Disconnected);
        if let DeviceEvent::Lagged { skipped } = event {
            assert_eq!(skipped, 7);
        } else {
            panic!("expected Lagged");
        }
    }

    #[test]
    fn lagged_serializes_with_skipped_count() {
        let event = DeviceEvent::Lagged { skipped: 42 };
        let json = serde_json::to_string(&event).unwrap();
        assert!(
            json.contains("lagged"),
            "lagged variant name must appear: {json}"
        );
        assert!(json.contains("42"), "skipped count must appear: {json}");
        let decoded: DeviceEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, event);
    }

    #[test]
    fn lagged_does_not_collide_with_input_event_mapping() {
        let input = InputEvent::BatteryChanged(attack_shark_x3::BatteryEvent {
            raw_report: [0x03, 0x10, 0x40, 0x01, 0x02],
            level: 2,
        });
        let mapped = DeviceEvent::from(input);
        assert!(!matches!(mapped, DeviceEvent::Lagged { .. }));
    }

    fn sample_outcome(profile: ProfileId) -> ProfileVerificationOutcome {
        let verification = Verification {
            application: ApplicationVerification::ReadbackVerified,
            persistence: PersistenceVerification::ProfileReloadVerified {
                verified_at: Timestamp { unix_seconds: 10 },
            },
        };
        let dpi = DpiState::captured_stock_reset(profile).unwrap();
        let preferences = PreferencesState::captured_stock_reset(profile);
        let buttons = ButtonsState::default_for_profile(profile);
        ProfileVerificationOutcome {
            profile,
            dpi: WriteOutcome {
                desired: dpi.clone(),
                observed: Some(dpi),
                verification: verification.clone(),
            },
            preferences: WriteOutcome {
                desired: preferences,
                observed: Some(preferences),
                verification: verification.clone(),
            },
            buttons: WriteOutcome {
                desired: buttons,
                observed: Some(buttons),
                verification: verification.clone(),
            },
            polling_rate: WriteOutcome {
                desired: PollingRate::Hz1000,
                observed: Some(PollingRate::Hz1000),
                verification,
            },
        }
    }

    #[test]
    fn profile_verification_outcome_includes_polling_rate_and_round_trips() {
        let profile = ProfileId::new(2).unwrap();
        let outcome = sample_outcome(profile);
        let json = serde_json::to_string(&outcome).unwrap();
        assert!(
            json.contains("pollingRate"),
            "pollingRate must be serialized: {json}"
        );
        let decoded: ProfileVerificationOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, outcome);
        assert_eq!(decoded.polling_rate.desired, PollingRate::Hz1000);
    }

    #[test]
    fn power_cycle_verification_outcome_includes_polling_rate_and_round_trips() {
        let profile = ProfileId::new(3).unwrap();
        let verification = Verification {
            application: ApplicationVerification::ReadbackVerified,
            persistence: PersistenceVerification::PowerCycleVerified {
                verified_at: Timestamp { unix_seconds: 20 },
            },
        };
        let dpi = DpiState::captured_stock_reset(profile).unwrap();
        let preferences = PreferencesState::captured_stock_reset(profile);
        let buttons = ButtonsState::default_for_profile(profile);
        let outcome = PowerCycleVerificationOutcome {
            profile,
            dpi: WriteOutcome {
                desired: dpi.clone(),
                observed: Some(dpi),
                verification: verification.clone(),
            },
            preferences: WriteOutcome {
                desired: preferences,
                observed: Some(preferences),
                verification: verification.clone(),
            },
            buttons: WriteOutcome {
                desired: buttons,
                observed: Some(buttons),
                verification: verification.clone(),
            },
            polling_rate: WriteOutcome {
                desired: PollingRate::Hz500,
                observed: Some(PollingRate::Hz500),
                verification,
            },
        };
        let json = serde_json::to_string(&outcome).unwrap();
        assert!(
            json.contains("pollingRate"),
            "pollingRate must be present: {json}"
        );
        let decoded: PowerCycleVerificationOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, outcome);
    }
}
