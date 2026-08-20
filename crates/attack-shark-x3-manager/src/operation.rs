use std::collections::BTreeMap;

use attack_shark_x3::{
    BatteryEvent, ButtonsState, ConnectionChangedEvent, DpiButtonEvent, DpiIndexChangedEvent,
    DpiState, InputEvent, LedModeChangedEvent, PollingRate, PreferencesState, ProfileChangedEvent,
    ProfileId, ProfileMetadata,
};
use serde::{Deserialize, Serialize};

use crate::{
    device::{DeviceEndpoint, DeviceIdentity},
    state::{ResourceState, Verification},
};

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
/// After association, the logical identity owns one or more endpoints, so the
/// transport path/platform id is an endpoint locator, never the physical identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredDevice {
    pub identity: DeviceIdentity,
    pub connected: bool,
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

/// Complete profile verification evidence after a profile-reload workflow.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileVerificationOutcome {
    pub profile: ProfileId,
    pub dpi: WriteOutcome<DpiState>,
    pub preferences: WriteOutcome<PreferencesState>,
    pub buttons: WriteOutcome<ButtonsState>,
}

/// Complete profile verification evidence after a physical power cycle.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PowerCycleVerificationOutcome {
    pub profile: ProfileId,
    pub dpi: WriteOutcome<DpiState>,
    pub preferences: WriteOutcome<PreferencesState>,
    pub buttons: WriteOutcome<ButtonsState>,
}

/// Input events exposed by the manager without transport-specific handles.
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
    use super::{DeviceEvent, DiscoveredEndpoint, UpdatePolicy, VerificationMethod};
    use crate::device::DeviceEndpoint;
    use attack_shark_x3::{ConnectionChangedEvent, InputEvent, TransportKind};

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
        // Serializing raw discovery must not contain a mouse-N id.
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
        // USB endpoint locator is verbatim HID path, not stable identity.
        let wired = DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa60,
            None,
            "/dev/hidraw0",
            None,
        )
        .unwrap();
        // Changing path yields a different endpoint but same logical device can be reused.
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

        // BLE endpoint uses platform id.
        let ble = DeviceEndpoint::ble("ble-platform-id-XYZ", None).unwrap();
        assert!(matches!(
            ble.locator,
            crate::device::DeviceLocator::BlePlatformId(_)
        ));
    }
}
