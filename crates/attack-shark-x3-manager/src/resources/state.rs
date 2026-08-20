use std::collections::BTreeMap;

use attack_shark_x3::{ButtonsState, DpiState, PollingRate, PreferencesState, ProfileId};
use serde::{Deserialize, Serialize};

use crate::device::{DeviceId, DeviceIdentity};
use crate::error::ManagerError;
use crate::manager::DeviceManager;
use crate::state::{
    DesiredSource, DesiredState, DeviceState, MAX_PROFILE_NAME_CHARS, ResourceState, StateReset,
    Timestamp, Verification,
};

/// A profile's portable configuration values.
///
/// The export deliberately contains values only. Resource provenance,
/// verification, observations, timestamps, device IDs, and transport locators
/// remain private to the local state file.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileConfiguration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpi: Option<DpiState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferences: Option<PreferencesState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buttons: Option<ButtonsState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polling_rate: Option<PollingRate>,
}

/// Portable manager configuration.
///
/// This is intentionally not a state-file snapshot. It carries the per-profile
/// DPI/preferences/buttons/polling-rate values and therefore can be imported
/// for a different local device identity.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigurationExport {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<ProfileId, ProfileConfiguration>,
    /// Local display names for profile slots, keyed by profile.
    ///
    /// Names are per-device presentation metadata, never hardware claims, and
    /// travel with the portable configuration so a setup can be restored
    /// without losing its labels.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profile_names: BTreeMap<ProfileId, String>,
}

impl DeviceManager {
    /// Exports desired configuration values, falling back to observed values
    /// only for resources that have no desired value.
    pub fn export_configuration(
        &self,
        device: &DeviceId,
    ) -> Result<ConfigurationExport, ManagerError> {
        let state = self.store().load()?;
        let device_state = state
            .devices
            .get(device)
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;

        let mut profiles = BTreeMap::new();
        for (&profile, profile_state) in &device_state.profiles {
            let configuration = ProfileConfiguration {
                dpi: value_for_export(&profile_state.dpi),
                preferences: value_for_export(&profile_state.preferences),
                buttons: value_for_export(&profile_state.buttons),
                polling_rate: value_for_export(&profile_state.polling_rate),
            };
            if configuration.dpi.is_some()
                || configuration.preferences.is_some()
                || configuration.buttons.is_some()
                || configuration.polling_rate.is_some()
            {
                profiles.insert(profile, configuration);
            }
        }

        Ok(ConfigurationExport {
            profiles,
            profile_names: device_state.profile_names.clone(),
        })
    }

    /// Imports portable desired values for an externally supplied identity.
    ///
    /// Import never opens a session or writes hardware. Existing observations
    /// are retained as evidence of the local device; imported values become
    /// new desired values with `Imported` provenance and `NotSent`/`Unknown`
    /// verification.
    pub fn import_configuration(
        &self,
        identity: &DeviceIdentity,
        configuration: ConfigurationExport,
    ) -> Result<(), ManagerError> {
        validate_configuration(&configuration)?;

        let now = self.now();
        let mut transaction = self.store().transaction()?;
        let state = transaction.state_mut();
        let device_id = identity.id.clone();
        if let Some(num) = device_id.number() {
            if state.next_device_number <= num {
                state.next_device_number = num + 1;
            }
        }
        let device_state = state
            .devices
            .entry(device_id.clone())
            .or_insert_with(|| DeviceState::new(identity.clone()));
        // The caller supplies the current locator separately from the portable
        // document. Refresh it when importing over an existing stable ID.
        device_state.identity = identity.clone();
        if state.selected_device.is_none() {
            state.selected_device = Some(device_id.clone());
        }

        for (&profile, configuration) in &configuration.profiles {
            let profile_state = device_state.profiles.entry(profile).or_default();
            if let Some(dpi) = configuration.dpi.clone() {
                set_imported(&mut profile_state.dpi, dpi, now);
            }
            if let Some(preferences) = configuration.preferences {
                set_imported(&mut profile_state.preferences, preferences, now);
            }
            if let Some(buttons) = configuration.buttons {
                set_imported(&mut profile_state.buttons, buttons, now);
            }
            if let Some(polling_rate) = configuration.polling_rate {
                set_imported(&mut profile_state.polling_rate, polling_rate, now);
            }
        }

        // Names present in the document are applied verbatim: an empty name
        // clears the slot, an absent profile is left untouched. Validation
        // above guarantees every name is within the length bound.
        for (&profile, name) in &configuration.profile_names {
            match normalize_profile_name(name)? {
                Some(name) => {
                    device_state.profile_names.insert(profile, name);
                }
                None => {
                    device_state.profile_names.remove(&profile);
                }
            }
        }

        state.validate()?;
        transaction.commit()?;
        Ok(())
    }

    /// Returns the local display names for every profile slot on a device.
    ///
    /// Names are per-device presentation metadata only and never imply a
    /// hardware rename or a protocol claim.
    pub fn profile_names(
        &self,
        device: &DeviceId,
    ) -> Result<BTreeMap<ProfileId, String>, ManagerError> {
        let state = self.store().load()?;
        let device_state = state
            .devices
            .get(device)
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
        Ok(device_state.profile_names.clone())
    }

    /// Sets the local display name for one profile slot.
    ///
    /// The name is trimmed before storage; a blank result removes the slot's
    /// name. Names longer than [`MAX_PROFILE_NAME_CHARS`] Unicode scalar
    /// values are rejected with [`ManagerError::InvalidUpdate`].
    pub fn set_profile_name(
        &self,
        device: &DeviceId,
        profile: ProfileId,
        name: &str,
    ) -> Result<(), ManagerError> {
        let Some(name) = normalize_profile_name(name)? else {
            return self.remove_profile_name(device, profile);
        };
        let mut transaction = self.store().transaction()?;
        let device_state = transaction
            .state_mut()
            .devices
            .get_mut(device)
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
        device_state.profile_names.insert(profile, name);
        transaction.state().validate()?;
        transaction.commit()?;
        Ok(())
    }

    /// Removes the local display name for one profile slot.
    pub fn remove_profile_name(
        &self,
        device: &DeviceId,
        profile: ProfileId,
    ) -> Result<(), ManagerError> {
        let mut transaction = self.store().transaction()?;
        let device_state = transaction
            .state_mut()
            .devices
            .get_mut(device)
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
        device_state.profile_names.remove(&profile);
        transaction.state().validate()?;
        transaction.commit()?;
        Ok(())
    }

    /// Invalidates persistence evidence for every desired resource on a
    /// device while preserving all desired and observed values.
    pub fn invalidate_state(&self, device: &DeviceId) -> Result<(), ManagerError> {
        let mut transaction = self.store().transaction()?;
        let device_state = transaction
            .state_mut()
            .devices
            .get_mut(device)
            .ok_or_else(|| ManagerError::DeviceNotFound(device.clone()))?;
        device_state.invalidate_persistence();
        transaction.state().validate()?;
        transaction.commit()?;
        Ok(())
    }
    /// Replaces an unreadable state file with a fresh empty state, preserving
    /// the previous file at a sibling backup path.
    ///
    /// Use when [`StateStore::load`](crate::state::StateStore::load) fails,
    /// for example after a schema bump. Refuses when the current file loads
    /// successfully, so a valid state can never be destroyed by accident.
    pub fn discard_unreadable_state(&self) -> Result<StateReset, ManagerError> {
        self.store()
            .discard_unreadable()
            .map_err(ManagerError::from)
    }
}

fn value_for_export<T: Clone>(resource: &ResourceState<T>) -> Option<T> {
    resource
        .desired
        .as_ref()
        .map(|desired| desired.value.clone())
        .or_else(|| {
            resource
                .observed
                .as_ref()
                .map(|observed| observed.value.clone())
        })
}

/// Normalizes a user-supplied profile display name.
///
/// Surrounding whitespace is trimmed; a blank result means "no name". Names
/// longer than [`MAX_PROFILE_NAME_CHARS`] Unicode scalar values are rejected
/// with [`ManagerError::InvalidUpdate`].
fn normalize_profile_name(name: &str) -> Result<Option<String>, ManagerError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > MAX_PROFILE_NAME_CHARS {
        return Err(ManagerError::InvalidUpdate(format!(
            "profile name exceeds {MAX_PROFILE_NAME_CHARS} Unicode scalar values"
        )));
    }
    Ok(Some(trimmed.to_owned()))
}

fn set_imported<T>(resource: &mut ResourceState<T>, value: T, now: Timestamp) {
    resource.desired = Some(DesiredState {
        value,
        source: DesiredSource::Imported,
        verification: Verification::not_sent(),
        updated_at: now,
    });
}

/// Records an ACK-only write while preserving historical observation.
///
/// Generic over any resource value; persistence is always reset to Unknown
/// and the old observation is kept as history. Used uniformly by DPI,
/// preferences, buttons and polling-rate.
pub(crate) fn record_ack<T: Clone>(resource: &mut ResourceState<T>, value: T, now: Timestamp) {
    resource.record_ack_write(value, DesiredSource::UserWrite, now);
}

/// Records a write with immediate readback, setting verification truthfully.
pub(crate) fn record_readback<T: Clone + PartialEq>(
    resource: &mut ResourceState<T>,
    desired: T,
    observed: T,
    now: Timestamp,
) {
    resource.record_readback_write(desired, observed, DesiredSource::UserWrite, now);
}

/// Reconciles a fresh observation for any resource.
///
/// With no desired value, only the observation is stored. With a desired
/// value, verification is set to ReadbackVerified when equal/current,
/// otherwise Mismatch, and persistence is always Unknown. Never infers
/// profile-reload or power-cycle persistence.
pub(crate) fn reconcile_observed<T: Clone + PartialEq>(
    resource: &mut ResourceState<T>,
    value: T,
    now: Timestamp,
) {
    resource.reconcile_observation(value, now);
}

/// Attempts to mark a resource as profile-reload verified.
pub(crate) fn try_mark_profile_reload<T: PartialEq>(
    resource: &mut ResourceState<T>,
    verified_at: Timestamp,
) -> bool {
    resource.try_mark_profile_reload_verified(verified_at)
}

/// Attempts to mark a resource as power-cycle verified.
pub(crate) fn try_mark_power_cycle<T: PartialEq>(
    resource: &mut ResourceState<T>,
    verified_at: Timestamp,
) -> bool {
    resource.try_mark_power_cycle_verified(verified_at)
}

fn validate_configuration(configuration: &ConfigurationExport) -> Result<(), ManagerError> {
    for (&profile, values) in &configuration.profiles {
        if values
            .dpi
            .as_ref()
            .is_some_and(|value| value.profile != profile)
        {
            return Err(invalid_profile_value("DPI", profile));
        }
        if values
            .preferences
            .as_ref()
            .is_some_and(|value| value.profile != profile)
        {
            return Err(invalid_profile_value("preferences", profile));
        }
        if values
            .buttons
            .as_ref()
            .is_some_and(|value| value.profile != profile)
        {
            return Err(invalid_profile_value("buttons", profile));
        }
    }
    for name in configuration.profile_names.values() {
        normalize_profile_name(name)?;
    }
    Ok(())
}

fn invalid_profile_value(resource: &'static str, profile: ProfileId) -> ManagerError {
    ManagerError::InvalidUpdate(format!(
        "{resource} configuration targets a different profile than {profile}"
    ))
}

#[cfg(test)]
mod tests {
    use super::{ConfigurationExport, ProfileConfiguration};
    use crate::device::{DeviceId, DeviceIdentity};
    use crate::error::ManagerError;
    use crate::manager::DeviceManager;
    use crate::state::{
        ApplicationVerification, DesiredSource, DesiredState, MAX_PROFILE_NAME_CHARS,
        ObservationSource, ObservedState, PersistenceVerification, ResourceState, StatePaths,
        StateStore, Timestamp, Verification,
    };
    use attack_shark_x3::{
        ButtonAssignment, ButtonsState, DpiState, DpiValue, PollingRate, PreferencesState,
        ProfileId, ProfileMetadata, StageIndex, TransportKind,
    };
    use std::collections::BTreeMap;

    fn store(dir: &tempfile::TempDir) -> StateStore {
        StateStore::open(StatePaths::new(dir.path().join("state.json")))
    }

    fn identity() -> DeviceIdentity {
        DeviceIdentity::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa61,
            Some("TEST001"),
            r"\\?\hid#test",
            Some("Test"),
        )
        .expect("valid identity")
    }

    fn dpi(_profile: ProfileId) -> DpiState {
        DpiState::captured_empty_profile_one(
            vec![DpiValue::new(800).expect("dpi")],
            StageIndex::new(1).expect("stage"),
        )
        .expect("state")
    }

    fn buttons(profile: ProfileId) -> ButtonsState {
        let mut slots = [ButtonAssignment::default(); 18];
        slots[17] = ButtonAssignment::new(0x10, 0x20, 0x30);
        ButtonsState::new(profile, slots)
    }

    #[test]
    fn import_export_is_portable_and_excludes_machine_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manager = DeviceManager::new(store(&dir)).expect("manager");
        let identity = identity();
        let profile = ProfileId::new(1).expect("profile");
        let configuration = ConfigurationExport {
            profiles: [(
                profile,
                ProfileConfiguration {
                    dpi: Some(dpi(profile)),
                    preferences: Some(PreferencesState::new(profile, 1, 2, 3, [4, 5, 6], 7, 8)),
                    buttons: Some(buttons(profile)),
                    polling_rate: Some(PollingRate::Hz1000),
                },
            )]
            .into_iter()
            .collect(),
            profile_names: BTreeMap::new(),
        };

        manager
            .import_configuration(&identity, configuration.clone())
            .expect("import");
        let exported = manager.export_configuration(&identity.id).expect("export");
        assert_eq!(exported, configuration);
        let json = serde_json::to_value(exported).expect("serialize export");
        let text = json.to_string();
        assert!(!text.contains(identity.id.as_str()));
        assert!(!text.contains("hid#test"));
        assert!(!text.contains("imported"));
        assert!(!text.contains("notSent"));
        assert!(!text.contains("updatedAt"));

        let state = manager.store().load().expect("state");
        let desired = state.devices[&identity.id].profiles[&profile]
            .buttons
            .desired
            .as_ref()
            .expect("imported desired");
        assert_eq!(desired.source, DesiredSource::Imported);
        assert_eq!(desired.verification, Verification::not_sent());
    }

    #[test]
    fn invalidation_preserves_values_and_clears_persistence_evidence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store(&dir);
        let manager = DeviceManager::new(store.clone()).expect("manager");
        let identity = identity();
        manager.register_device(identity.clone()).expect("register");
        let profile = ProfileId::new(1).expect("profile");
        let dpi = dpi(profile);
        let preferences = PreferencesState::new(profile, 1, 2, 3, [4, 5, 6], 7, 8);
        let buttons = buttons(profile);
        let timestamp = Timestamp { unix_seconds: 10 };
        let persistent = PersistenceVerification::PowerCycleVerified {
            verified_at: timestamp,
        };
        let desired_dpi = DesiredState {
            value: dpi.clone(),
            source: DesiredSource::UserWrite,
            verification: Verification {
                application: ApplicationVerification::ReadbackVerified,
                persistence: persistent.clone(),
            },
            updated_at: timestamp,
        };
        let observed_dpi = ObservedState {
            value: dpi.clone(),
            source: ObservationSource::UsbReadback,
            observed_at: timestamp,
        };
        let desired_preferences = DesiredState {
            value: preferences,
            source: DesiredSource::UserWrite,
            verification: Verification {
                application: ApplicationVerification::ReadbackVerified,
                persistence: persistent.clone(),
            },
            updated_at: timestamp,
        };
        let observed_preferences = ObservedState {
            value: preferences,
            source: ObservationSource::UsbReadback,
            observed_at: timestamp,
        };
        let desired_buttons = DesiredState {
            value: buttons,
            source: DesiredSource::UserWrite,
            verification: Verification {
                application: ApplicationVerification::ReadbackVerified,
                persistence: persistent.clone(),
            },
            updated_at: timestamp,
        };
        let observed_buttons = ObservedState {
            value: buttons,
            source: ObservationSource::UsbReadback,
            observed_at: timestamp,
        };
        let observed_metadata = ObservedState {
            value: ProfileMetadata::new(profile, profile).expect("metadata"),
            source: ObservationSource::UsbReadback,
            observed_at: timestamp,
        };
        let mut transaction = store.transaction().expect("transaction");
        let device = transaction
            .state_mut()
            .devices
            .get_mut(&identity.id)
            .expect("device");
        device.profile_metadata = ResourceState {
            desired: Some(DesiredState {
                value: ProfileMetadata::new(profile, profile).expect("metadata"),
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::ReadbackVerified,
                    persistence: persistent.clone(),
                },
                updated_at: timestamp,
            }),
            observed: Some(observed_metadata),
        };
        device.profiles.insert(
            profile,
            crate::state::ProfileState {
                dpi: ResourceState {
                    desired: Some(desired_dpi),
                    observed: Some(observed_dpi),
                },
                preferences: ResourceState {
                    desired: Some(desired_preferences),
                    observed: Some(observed_preferences),
                },
                buttons: ResourceState {
                    desired: Some(desired_buttons),
                    observed: Some(observed_buttons),
                },
                polling_rate: ResourceState {
                    desired: Some(DesiredState {
                        value: PollingRate::Hz1000,
                        source: DesiredSource::UserWrite,
                        verification: Verification {
                            application: ApplicationVerification::ReadbackVerified,
                            persistence: persistent.clone(),
                        },
                        updated_at: timestamp,
                    }),
                    observed: Some(ObservedState {
                        value: PollingRate::Hz1000,
                        source: ObservationSource::UsbReadback,
                        observed_at: timestamp,
                    }),
                },
            },
        );
        transaction.commit().expect("commit");
        manager.invalidate_state(&identity.id).expect("invalidate");
        let state = manager.store().load().expect("state");
        let device = &state.devices[&identity.id];
        let profile_state = &device.profiles[&profile];
        assert_eq!(
            profile_state
                .polling_rate
                .desired
                .as_ref()
                .expect("rate")
                .value,
            PollingRate::Hz1000
        );
        assert!(
            profile_state
                .polling_rate
                .desired
                .as_ref()
                .expect("rate")
                .verification
                .persistence
                .is_unknown()
        );
        assert_eq!(
            device
                .profile_metadata
                .desired
                .as_ref()
                .expect("metadata")
                .value
                .current(),
            profile
        );
        assert_eq!(profile_state.dpi.desired.as_ref().expect("dpi").value, dpi);
        assert!(
            profile_state
                .dpi
                .desired
                .as_ref()
                .expect("dpi")
                .verification
                .persistence
                .is_unknown()
        );
        assert_eq!(
            profile_state
                .buttons
                .observed
                .as_ref()
                .expect("observed")
                .value,
            buttons
        );
        assert!(
            profile_state
                .buttons
                .desired
                .as_ref()
                .expect("buttons")
                .verification
                .persistence
                .is_unknown()
        );
    }

    #[test]
    fn profile_names_support_all_slots_and_normalize_on_set() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manager = DeviceManager::new(store(&dir)).expect("manager");
        let identity = identity();
        manager.register_device(identity.clone()).expect("register");
        let profile = ProfileId::new(1).expect("profile");

        // Establish a clean baseline even if setup has already populated names.
        let existing_profiles = manager
            .profile_names(&identity.id)
            .expect("names")
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for existing_profile in existing_profiles {
            manager
                .remove_profile_name(&identity.id, existing_profile)
                .expect("clear existing name");
        }
        assert!(
            manager
                .profile_names(&identity.id)
                .expect("names")
                .is_empty()
        );

        for slot in 1..=5 {
            let slot_profile = ProfileId::new(slot).expect("profile");
            manager
                .set_profile_name(&identity.id, slot_profile, &format!("  Profile {slot}  "))
                .expect("set");
        }
        let names = manager.profile_names(&identity.id).expect("names");
        assert_eq!(names.len(), 5);
        assert_eq!(names[&profile], "Profile 1");

        // Blank input removes the name.
        manager
            .set_profile_name(&identity.id, profile, "   ")
            .expect("set blank");
        let names = manager.profile_names(&identity.id).expect("names");
        assert_eq!(names.len(), 4);
        assert!(!names.contains_key(&profile));

        manager
            .set_profile_name(&identity.id, profile, "Office")
            .expect("set");
        manager
            .remove_profile_name(&identity.id, profile)
            .expect("remove");
        let names = manager.profile_names(&identity.id).expect("names");
        assert_eq!(names.len(), 4);
        assert!(!names.contains_key(&profile));

        for slot in 2..=5 {
            manager
                .remove_profile_name(&identity.id, ProfileId::new(slot).expect("profile"))
                .expect("remove");
        }
        assert!(
            manager
                .profile_names(&identity.id)
                .expect("names")
                .is_empty()
        );

        match manager.set_profile_name(&DeviceId::new("mouse-2").expect("id"), profile, "X") {
            Err(ManagerError::DeviceNotFound(_)) => {}
            other => panic!("expected DeviceNotFound, got {other:?}"),
        }
        match manager.profile_names(&DeviceId::new("mouse-2").expect("id")) {
            Err(ManagerError::DeviceNotFound(_)) => {}
            other => panic!("expected DeviceNotFound, got {other:?}"),
        }
    }

    #[test]
    fn profile_name_limit_is_enforced_in_unicode_scalar_values() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manager = DeviceManager::new(store(&dir)).expect("manager");
        let identity = identity();
        manager.register_device(identity.clone()).expect("register");
        let profile = ProfileId::new(1).expect("profile");

        // 64 ASCII scalars are accepted; 65 are rejected.
        let max = "x".repeat(MAX_PROFILE_NAME_CHARS);
        manager
            .set_profile_name(&identity.id, profile, &max)
            .expect("max length accepted");
        match manager.set_profile_name(
            &identity.id,
            profile,
            &"x".repeat(MAX_PROFILE_NAME_CHARS + 1),
        ) {
            Err(ManagerError::InvalidUpdate(_)) => {}
            other => panic!("expected InvalidUpdate, got {other:?}"),
        }
        assert_eq!(
            manager.profile_names(&identity.id).expect("names")[&profile],
            max
        );

        // 64 multi-byte emoji (256 UTF-8 bytes) still count as 64 scalar
        // values, so they are accepted.
        let emoji = "😀".repeat(MAX_PROFILE_NAME_CHARS);
        manager
            .set_profile_name(&identity.id, profile, &emoji)
            .expect("emoji at scalar limit accepted");
        assert_eq!(
            manager.profile_names(&identity.id).expect("names")[&profile],
            emoji
        );
    }

    #[test]
    fn profile_names_survive_state_invalidation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store(&dir);
        let manager = DeviceManager::new(store.clone()).expect("manager");
        let identity = identity();
        manager.register_device(identity.clone()).expect("register");
        let profile = ProfileId::new(1).expect("profile");
        manager
            .set_profile_name(&identity.id, profile, "Gaming")
            .expect("set");

        manager.invalidate_state(&identity.id).expect("invalidate");

        assert_eq!(
            manager.profile_names(&identity.id).expect("names")[&profile],
            "Gaming"
        );
    }

    #[test]
    fn import_export_round_trips_profile_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manager = DeviceManager::new(store(&dir)).expect("manager");
        let identity = identity();
        let profile = ProfileId::new(1).expect("profile");
        let configuration = ConfigurationExport {
            profiles: [(
                profile,
                ProfileConfiguration {
                    dpi: Some(dpi(profile)),
                    preferences: Some(PreferencesState::new(profile, 1, 2, 3, [4, 5, 6], 7, 8)),
                    buttons: Some(buttons(profile)),
                    polling_rate: Some(PollingRate::Hz1000),
                },
            )]
            .into_iter()
            .collect(),
            profile_names: BTreeMap::from([
                (profile, "Gaming".to_owned()),
                (ProfileId::new(2).expect("profile"), "Office".to_owned()),
            ]),
        };

        manager
            .import_configuration(&identity, configuration.clone())
            .expect("import");
        let exported = manager.export_configuration(&identity.id).expect("export");
        assert_eq!(exported, configuration);

        // A name-only entry round-trips even when its profile carries no
        // hardware configuration values.
        let named_profile = ProfileId::new(3).expect("profile");
        let names_only = ConfigurationExport {
            profiles: BTreeMap::new(),
            profile_names: BTreeMap::from([(named_profile, "Stream".to_owned())]),
        };
        manager
            .import_configuration(&identity, names_only)
            .expect("import names");
        assert_eq!(
            manager
                .export_configuration(&identity.id)
                .expect("export")
                .profile_names[&named_profile],
            "Stream"
        );

        // Blank names in an imported document clear the slot.
        let blank = ConfigurationExport {
            profiles: BTreeMap::new(),
            profile_names: BTreeMap::from([(named_profile, "  ".to_owned())]),
        };
        manager
            .import_configuration(&identity, blank)
            .expect("import blank");
        assert!(
            !manager
                .profile_names(&identity.id)
                .expect("names")
                .contains_key(&named_profile)
        );
    }

    #[test]
    fn import_rejects_overlong_profile_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manager = DeviceManager::new(store(&dir)).expect("manager");
        let identity = identity();
        let profile = ProfileId::new(1).expect("profile");
        let overlong = ConfigurationExport {
            profiles: BTreeMap::new(),
            profile_names: BTreeMap::from([(profile, "x".repeat(MAX_PROFILE_NAME_CHARS + 1))]),
        };
        match manager.import_configuration(&identity, overlong) {
            Err(ManagerError::InvalidUpdate(_)) => {}
            other => panic!("expected InvalidUpdate, got {other:?}"),
        }
    }

    #[test]
    fn central_reconciliation_is_consistent_across_all_four_resources() {
        use crate::resources::state::{
            reconcile_observed, record_ack, record_readback, try_mark_power_cycle,
            try_mark_profile_reload,
        };
        use crate::state::{ResourceState, Timestamp};

        fn ts(s: i64) -> Timestamp {
            Timestamp { unix_seconds: s }
        }

        fn assert_ack_preserves_observed<T: Clone + PartialEq + std::fmt::Debug>(
            mut resource: ResourceState<T>,
            observed_value: T,
            ack_value: T,
        ) {
            reconcile_observed(&mut resource, observed_value.clone(), ts(10));
            let historical = resource.observed.clone().expect("observed seeded");
            assert!(resource.desired.is_none());
            record_ack(&mut resource, ack_value.clone(), ts(20));
            assert_eq!(resource.desired.as_ref().unwrap().value, ack_value);
            assert_eq!(
                resource.desired.as_ref().unwrap().verification.application,
                ApplicationVerification::Acknowledged
            );
            assert!(
                resource
                    .desired
                    .as_ref()
                    .unwrap()
                    .verification
                    .persistence
                    .is_unknown()
            );
            assert_eq!(resource.observed, Some(historical));
            reconcile_observed(&mut resource, ack_value.clone(), ts(22));
            assert_eq!(
                resource.desired.as_ref().unwrap().verification.application,
                ApplicationVerification::ReadbackVerified
            );
            assert!(
                resource
                    .desired
                    .as_ref()
                    .unwrap()
                    .verification
                    .persistence
                    .is_unknown()
            );
        }

        fn assert_readback_and_mismatch<T: Clone + PartialEq + std::fmt::Debug>(
            ack_value: T,
            other_value: T,
        ) {
            let mut matched: ResourceState<T> = ResourceState::empty();
            record_readback(&mut matched, ack_value.clone(), ack_value.clone(), ts(30));
            assert_eq!(
                matched.desired.as_ref().unwrap().verification.application,
                ApplicationVerification::ReadbackVerified
            );
            assert_eq!(matched.observed.as_ref().unwrap().value, ack_value);
            assert!(
                matched
                    .desired
                    .as_ref()
                    .unwrap()
                    .verification
                    .persistence
                    .is_unknown()
            );
            assert!(try_mark_profile_reload(&mut matched, ts(31)));
            assert!(
                !matched
                    .desired
                    .as_ref()
                    .unwrap()
                    .verification
                    .persistence
                    .is_unknown()
            );
            reconcile_observed(&mut matched, ack_value.clone(), ts(32));
            assert!(
                matched
                    .desired
                    .as_ref()
                    .unwrap()
                    .verification
                    .persistence
                    .is_unknown()
            );
            assert!(try_mark_power_cycle(&mut matched, ts(33)));
            assert_eq!(
                matched.desired.as_ref().unwrap().verification.persistence,
                PersistenceVerification::PowerCycleVerified {
                    verified_at: ts(33)
                }
            );
            let mut mismatched: ResourceState<T> = ResourceState::empty();
            record_readback(
                &mut mismatched,
                ack_value.clone(),
                other_value.clone(),
                ts(30),
            );
            assert_eq!(
                mismatched
                    .desired
                    .as_ref()
                    .unwrap()
                    .verification
                    .application,
                ApplicationVerification::Mismatch
            );
            assert_eq!(mismatched.observed.as_ref().unwrap().value, other_value);
            assert!(!try_mark_profile_reload(&mut mismatched, ts(31)));
            assert!(!try_mark_power_cycle(&mut mismatched, ts(31)));
            let mut via_reconcile: ResourceState<T> = ResourceState::empty();
            record_ack(&mut via_reconcile, ack_value.clone(), ts(40));
            reconcile_observed(&mut via_reconcile, ack_value.clone(), ts(39));
            assert_eq!(
                via_reconcile
                    .desired
                    .as_ref()
                    .unwrap()
                    .verification
                    .application,
                ApplicationVerification::Mismatch
            );
            reconcile_observed(&mut via_reconcile, other_value.clone(), ts(41));
            assert_eq!(
                via_reconcile
                    .desired
                    .as_ref()
                    .unwrap()
                    .verification
                    .application,
                ApplicationVerification::Mismatch
            );
            assert!(!try_mark_profile_reload(&mut via_reconcile, ts(42)));
            let mut empty: ResourceState<T> = ResourceState::empty();
            reconcile_observed(&mut empty, other_value.clone(), ts(50));
            assert!(empty.desired.is_none());
            assert_eq!(empty.observed.as_ref().unwrap().value, other_value);
        }

        let profile = ProfileId::new(1).unwrap();
        let dpi_a = DpiState::captured_empty_profile_one(
            vec![DpiValue::new(800).unwrap()],
            StageIndex::new(1).unwrap(),
        )
        .unwrap();
        let dpi_b = DpiState::captured_empty_profile_one(
            vec![DpiValue::new(1600).unwrap()],
            StageIndex::new(1).unwrap(),
        )
        .unwrap();
        assert_ack_preserves_observed(
            ResourceState::<DpiState>::empty(),
            dpi_a.clone(),
            dpi_b.clone(),
        );
        assert_readback_and_mismatch(dpi_a.clone(), dpi_b.clone());
        let pref_a = PreferencesState::new(profile, 1, 2, 3, [4, 5, 6], 7, 8);
        let pref_b = PreferencesState::new(profile, 9, 8, 7, [6, 5, 4], 3, 2);
        assert_ack_preserves_observed(ResourceState::<PreferencesState>::empty(), pref_a, pref_b);
        assert_readback_and_mismatch(pref_a, pref_b);
        let mut slots_a = [ButtonAssignment::default(); 18];
        slots_a[0] = ButtonAssignment::new(0x01, 0x02, 0x03);
        let btn_a = ButtonsState::new(profile, slots_a);
        let mut slots_b = [ButtonAssignment::default(); 18];
        slots_b[0] = ButtonAssignment::new(0x04, 0x05, 0x06);
        let btn_b = ButtonsState::new(profile, slots_b);
        assert_ack_preserves_observed(ResourceState::<ButtonsState>::empty(), btn_a, btn_b);
        assert_readback_and_mismatch(btn_a, btn_b);
        assert_ack_preserves_observed(
            ResourceState::<PollingRate>::empty(),
            PollingRate::Hz500,
            PollingRate::Hz1000,
        );
        assert_readback_and_mismatch(PollingRate::Hz500, PollingRate::Hz1000);
    }
    #[test]
    fn old_import_documents_without_profile_names_deserialize() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manager = DeviceManager::new(store(&dir)).expect("manager");
        let identity = identity();
        let profile = ProfileId::new(1).expect("profile");
        let json = serde_json::json!({
            "profiles": {
                profile.get().to_string(): {
                    "dpi": dpi(profile),
                },
            },
        });
        let configuration: ConfigurationExport =
            serde_json::from_value(json).expect("deserialize old import");
        assert!(configuration.profile_names.is_empty());

        manager
            .import_configuration(&identity, configuration)
            .expect("import");
        assert!(
            manager
                .profile_names(&identity.id)
                .expect("names")
                .is_empty()
        );
    }
}
