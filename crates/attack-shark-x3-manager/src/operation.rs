use attack_shark_x3::{
    BatteryEvent, ButtonsState, DpiButtonEvent, DpiState, PollingRate, PreferencesState, ProfileId,
    ProfileMetadata,
};
use serde::{Deserialize, Serialize};

use crate::{
    device::DeviceIdentity,
    state::{ResourceState, Verification},
};

/// The verification performed after a write operation.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VerificationMethod {
    /// Complete the operation after its immediate transport-level check.
    #[default]
    Immediate,
    /// Verify profile-scoped resources by switching away and back.
    ProfileReload,
}

/// Safety and verification policy for a typed update.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UpdatePolicy {
    /// Permit explicit captured defaults when a transport cannot read a baseline.
    pub allow_explicit_defaults: bool,
    /// The requested post-write verification strength.
    pub verification: VerificationMethod,
}

impl Default for UpdatePolicy {
    fn default() -> Self {
        Self {
            allow_explicit_defaults: false,
            verification: VerificationMethod::Immediate,
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

/// A discovered exact device and whether it is currently connected.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredDevice {
    pub identity: DeviceIdentity,
    pub connected: bool,
}

/// Resource status read from one exact device identity.
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
    Disconnected,
}
