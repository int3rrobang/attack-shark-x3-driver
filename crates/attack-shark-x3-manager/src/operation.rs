use std::collections::BTreeMap;

use attack_shark_x3::{
    BatteryEvent, ButtonsState, ConnectionChangedEvent, DpiButtonEvent, DpiIndexChangedEvent,
    DpiState, InputEvent, LedModeChangedEvent, PollingRate, PreferencesState, ProfileChangedEvent,
    ProfileId, ProfileMetadata, TransportKind,
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
