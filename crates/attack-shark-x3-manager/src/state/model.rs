use std::collections::BTreeMap;

use attack_shark_x3::{
    ButtonsState, DpiState, PollingRate, PreferencesState, ProfileId, ProfileMetadata,
};
use serde::{Deserialize, Serialize};

use crate::device::{DeviceId, DeviceIdentity};
use crate::error::StateError;

/// The durable-state schema understood by this manager.
///
/// Schema 3 moves polling rate from the device-global resource into each
/// profile's state: firmware evidence shows report `0x06` byte 2 is a
/// one-based target profile and the deferred writer serializes the live
/// profile image into that slot, so polling rate is per-profile and persistent.
pub const SCHEMA_VERSION: u32 = 3;

/// Maximum length of a local profile display name, in Unicode scalar values.
///
/// Names are per-device presentation metadata only: they never describe the
/// hardware, so this bound exists purely to keep user input sane and is not
/// part of any protocol contract.
pub const MAX_PROFILE_NAME_CHARS: usize = 64;

/// A wall-clock timestamp stored in the state document.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Timestamp {
    pub unix_seconds: i64,
}

/// Desired and observed evidence for one resource.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceState<T> {
    pub desired: Option<DesiredState<T>>,
    pub observed: Option<ObservedState<T>>,
}

impl<T> Default for ResourceState<T> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<T> ResourceState<T> {
    /// Creates an empty resource with no desired or observed evidence.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            desired: None,
            observed: None,
        }
    }

    /// Drops persistence evidence without changing the requested value.
    pub fn invalidate_persistence(&mut self) {
        if let Some(desired) = self.desired.as_mut() {
            desired.verification.invalidate_persistence();
        }
    }
}

/// A value the user or an import wants the device to hold.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesiredState<T> {
    pub value: T,
    pub source: DesiredSource,
    pub verification: Verification,
    pub updated_at: Timestamp,
}

impl<T> DesiredState<T> {
    /// Drops persistence evidence while preserving the desired value and its
    /// immediate-application evidence.
    pub fn invalidate_persistence(&mut self) {
        self.verification.invalidate_persistence();
    }
}

/// A value returned by a supported hardware readback.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservedState<T> {
    pub value: T,
    pub source: ObservationSource,
    pub observed_at: Timestamp,
}

/// Provenance for a desired value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DesiredSource {
    UserWrite,
    Imported,
    ExplicitDefaults,
}

/// Provenance for an observed value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ObservationSource {
    UsbReadback,
}

/// Evidence for immediate application and nonvolatile persistence.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Verification {
    pub application: ApplicationVerification,
    pub persistence: PersistenceVerification,
}

impl Verification {
    /// Creates the evidence for a desired value that has not been sent.
    #[must_use]
    pub const fn not_sent() -> Self {
        Self {
            application: ApplicationVerification::NotSent,
            persistence: PersistenceVerification::Unknown,
        }
    }

    /// Invalidates persistence evidence while retaining application evidence.
    pub const fn invalidate_persistence(&mut self) {
        self.persistence = PersistenceVerification::Unknown;
    }
}

/// Evidence that a write took effect immediately.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApplicationVerification {
    #[default]
    NotSent,
    Acknowledged,
    ReadbackVerified,
    Mismatch,
}

/// Evidence that a value survived a stronger persistence check.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PersistenceVerification {
    #[default]
    Unknown,
    ProfileReloadVerified {
        verified_at: Timestamp,
    },
    PowerCycleVerified {
        verified_at: Timestamp,
    },
}

impl PersistenceVerification {
    /// Returns true when no persistence claim is present.
    #[must_use]
    pub const fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }
}

/// Durable state for one exact device identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceState {
    pub identity: DeviceIdentity,
    pub profile_metadata: ResourceState<ProfileMetadata>,
    pub profiles: BTreeMap<ProfileId, ProfileState>,
    /// Local presentation names for the device's profile slots.
    ///
    /// These are per-device display metadata only. They never claim anything
    /// about hardware profile labels and have no protocol meaning;
    /// `profile_metadata` remains the single source of protocol-visible
    /// metadata.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profile_names: BTreeMap<ProfileId, String>,
}

impl DeviceState {
    /// Creates a device entry with no configuration evidence.
    #[must_use]
    pub fn new(identity: DeviceIdentity) -> Self {
        Self {
            identity,
            profile_metadata: ResourceState::empty(),
            profiles: BTreeMap::new(),
            profile_names: BTreeMap::new(),
        }
    }
}

/// Durable state for the complete image of one profile.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileState {
    pub dpi: ResourceState<DpiState>,
    pub preferences: ResourceState<PreferencesState>,
    pub buttons: ResourceState<ButtonsState>,
    pub polling_rate: ResourceState<PollingRate>,
}

impl Default for ProfileState {
    fn default() -> Self {
        Self::empty()
    }
}

impl ProfileState {
    /// Creates a profile entry with no configuration evidence.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            dpi: ResourceState::empty(),
            preferences: ResourceState::empty(),
            buttons: ResourceState::empty(),
            polling_rate: ResourceState::empty(),
        }
    }
}

/// The complete internal manager-state document.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateFile {
    pub schema_version: u32,
    pub selected_device: Option<DeviceId>,
    pub devices: BTreeMap<DeviceId, DeviceState>,
}

impl Default for StateFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            selected_device: None,
            devices: BTreeMap::new(),
        }
    }
}

impl StateFile {
    /// Creates an empty state document for the current schema.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Rejects documents from another schema generation or with mismatched
    /// device/profile/resource identities.
    pub fn validate(&self) -> Result<(), StateError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(StateError::unsupported_schema(self.schema_version));
        }

        if let Some(selected_device) = self.selected_device.as_ref()
            && !self.devices.contains_key(selected_device)
        {
            return Err(StateError::invalid_state(format!(
                "selected device {selected_device} is not present in devices"
            )));
        }

        for (device_id, device) in &self.devices {
            if device.identity.id != *device_id {
                return Err(StateError::invalid_state(format!(
                    "device identity {} is stored under key {device_id}",
                    device.identity.id
                )));
            }
            for (profile_id, profile) in &device.profiles {
                validate_profile_resource(profile_id, &profile.dpi, "dpi")?;
                validate_profile_resource(profile_id, &profile.preferences, "preferences")?;
                validate_profile_resource(profile_id, &profile.buttons, "buttons")?;
            }
            for (profile_id, name) in &device.profile_names {
                if name.trim().is_empty() {
                    return Err(StateError::invalid_state(format!(
                        "profile name for {profile_id} on device {device_id} is blank"
                    )));
                }
                if name.chars().count() > MAX_PROFILE_NAME_CHARS {
                    return Err(StateError::invalid_state(format!(
                        "profile name for {profile_id} on device {device_id} exceeds {MAX_PROFILE_NAME_CHARS} Unicode scalar values"
                    )));
                }
            }
        }
        Ok(())
    }
}

fn validate_profile_resource<T>(
    profile_id: &ProfileId,
    resource: &ResourceState<T>,
    name: &str,
) -> Result<(), StateError>
where
    T: ProfileValue,
{
    if resource
        .desired
        .as_ref()
        .is_some_and(|desired| desired.value.profile_id() != *profile_id)
        || resource
            .observed
            .as_ref()
            .is_some_and(|observed| observed.value.profile_id() != *profile_id)
    {
        return Err(StateError::invalid_state(format!(
            "{name} resource targets profile {profile_id} but is stored under another profile"
        )));
    }
    Ok(())
}

trait ProfileValue {
    fn profile_id(&self) -> ProfileId;
}

impl ProfileValue for DpiState {
    fn profile_id(&self) -> ProfileId {
        self.profile
    }
}

impl ProfileValue for PreferencesState {
    fn profile_id(&self) -> ProfileId {
        self.profile
    }
}

impl ProfileValue for ButtonsState {
    fn profile_id(&self) -> ProfileId {
        self.profile
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ApplicationVerification, DesiredSource, DesiredState, DeviceState, MAX_PROFILE_NAME_CHARS,
        ObservationSource, ObservedState, PersistenceVerification, ProfileState, ResourceState,
        SCHEMA_VERSION, StateFile, Timestamp, Verification,
    };
    use crate::device::DeviceIdentity;
    use crate::error::StateError;
    use attack_shark_x3::{DpiState, ProfileId};
    use std::collections::BTreeMap;

    fn timestamp(seconds: i64) -> Timestamp {
        Timestamp {
            unix_seconds: seconds,
        }
    }

    #[test]
    fn default_state_file_starts_at_schema_three() {
        let state = StateFile::default();
        assert_eq!(state.schema_version, SCHEMA_VERSION);
        assert!(state.selected_device.is_none());
        assert!(state.devices.is_empty());
    }

    #[test]
    fn json_keeps_desired_and_observed_distinct() {
        let dpi = DpiState::captured_empty_profile_one(
            vec![attack_shark_x3::DpiValue::new(800).expect("valid dpi")],
            attack_shark_x3::StageIndex::new(1).expect("valid stage"),
        )
        .expect("valid state");
        let resource = ResourceState {
            desired: Some(DesiredState {
                value: dpi.clone(),
                source: DesiredSource::Imported,
                verification: Verification::not_sent(),
                updated_at: timestamp(10),
            }),
            observed: Some(ObservedState {
                value: dpi,
                source: ObservationSource::UsbReadback,
                observed_at: timestamp(11),
            }),
        };
        let json = serde_json::to_value(&resource).expect("serialize resource");
        assert!(json.get("desired").is_some());
        assert!(json.get("observed").is_some());
        assert_eq!(json["desired"]["source"], "imported");
        assert_eq!(json["observed"]["source"], "usbReadback");
        assert_eq!(json["desired"]["verification"]["application"], "notSent");
    }

    #[test]
    fn acknowledged_ble_desired_state_has_no_observation() {
        let state = ResourceState::<DpiState> {
            desired: Some(DesiredState {
                value: DpiState::captured_empty_profile_one(
                    vec![attack_shark_x3::DpiValue::new(800).expect("valid dpi")],
                    attack_shark_x3::StageIndex::new(1).expect("valid stage"),
                )
                .expect("valid state"),
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::Acknowledged,
                    persistence: PersistenceVerification::Unknown,
                },
                updated_at: timestamp(20),
            }),
            observed: None,
        };
        let json = serde_json::to_value(&state).expect("serialize BLE state");
        assert_eq!(
            json["desired"]["verification"]["application"],
            "acknowledged"
        );
        assert!(json["observed"].is_null());
    }

    #[test]
    fn invalidating_persistence_preserves_application_evidence() {
        let mut verification = Verification {
            application: ApplicationVerification::ReadbackVerified,
            persistence: PersistenceVerification::PowerCycleVerified {
                verified_at: timestamp(30),
            },
        };
        verification.invalidate_persistence();
        assert_eq!(
            verification.application,
            ApplicationVerification::ReadbackVerified
        );
        assert_eq!(verification.persistence, PersistenceVerification::Unknown);
    }

    #[test]
    fn new_device_state_starts_without_profile_names() {
        let identity = DeviceIdentity::ble("test", None).expect("valid BLE identity");
        let device = DeviceState::new(identity);
        assert!(device.profile_names.is_empty());
    }

    #[test]
    fn validate_rejects_overlong_or_blank_profile_names() {
        let identity = DeviceIdentity::ble("test", None).expect("valid BLE identity");
        let profile = ProfileId::new(1).expect("profile");

        let overlong = {
            let mut device = DeviceState::new(identity.clone());
            device
                .profile_names
                .insert(profile, "x".repeat(MAX_PROFILE_NAME_CHARS + 1));
            device
        };
        let state = StateFile {
            schema_version: SCHEMA_VERSION,
            selected_device: None,
            devices: BTreeMap::from([(identity.id.clone(), overlong)]),
        };
        assert!(matches!(state.validate(), Err(StateError::InvalidState(_))));

        let blank_id = identity.id.clone();
        let blank = {
            let mut device = DeviceState::new(identity);
            device.profile_names.insert(profile, "   ".to_owned());
            device
        };
        let state = StateFile {
            schema_version: SCHEMA_VERSION,
            selected_device: None,
            devices: BTreeMap::from([(blank_id, blank)]),
        };
        assert!(matches!(state.validate(), Err(StateError::InvalidState(_))));
    }

    #[test]
    fn old_state_documents_without_profile_names_deserialize() {
        let identity = DeviceIdentity::ble("test", None).expect("valid BLE identity");
        let device_id = identity.id.clone();
        let json = serde_json::json!({
            "schemaVersion": SCHEMA_VERSION,
            "selectedDevice": device_id,
            "devices": {
                "ble:test": {
                    "identity": identity,
                    "profileMetadata": {
                        "desired": null,
                        "observed": null,
                    },
                    "profiles": {},
                }
            },
        });
        let state: StateFile = serde_json::from_value(json).expect("deserialize old state");
        assert!(state.validate().is_ok());
        let device = &state.devices[&device_id];
        assert!(device.profile_names.is_empty());
    }

    #[allow(dead_code)]
    fn _construct_state_types_for_compile() {
        let identity = DeviceIdentity::ble("test", None).expect("valid BLE identity");
        let id = identity.id.clone();
        let _device = DeviceState::new(identity);
        let _profile = ProfileState::empty();
        let _state = StateFile {
            schema_version: SCHEMA_VERSION,
            selected_device: Some(id),
            devices: BTreeMap::new(),
        };
    }
}
