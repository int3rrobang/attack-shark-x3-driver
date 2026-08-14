use std::collections::BTreeMap;

use attack_shark_x3::{ButtonsState, DpiState, PollingRate, PreferencesState, ProfileId};
use serde::{Deserialize, Serialize};

use crate::device::{DeviceId, DeviceIdentity};
use crate::error::ManagerError;
use crate::manager::DeviceManager;
use crate::state::{
    DesiredSource, DesiredState, DeviceState, ProfileState, ResourceState, Timestamp, Verification,
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

        Ok(ConfigurationExport { profiles })
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
        let device_state = state
            .devices
            .entry(device_id.clone())
            .or_insert_with(|| DeviceState::new(identity.clone()));
        // The caller supplies the current locator separately from the portable
        // document. Refresh it when importing over an existing stable ID.
        device_state.identity = identity.clone();
        if state.selected_device.is_none() {
            state.selected_device = Some(device_id);
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

        state.validate()?;
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
        invalidate_device_state(device_state);
        transaction.state().validate()?;
        transaction.commit()?;
        Ok(())
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

fn set_imported<T>(resource: &mut ResourceState<T>, value: T, now: Timestamp) {
    resource.desired = Some(DesiredState {
        value,
        source: DesiredSource::Imported,
        verification: Verification::not_sent(),
        updated_at: now,
    });
}

fn invalidate_device_state(device: &mut DeviceState) {
    device.profile_metadata.invalidate_persistence();
    for profile in device.profiles.values_mut() {
        invalidate_profile_state(profile);
    }
}

fn invalidate_profile_state(profile: &mut ProfileState) {
    profile.dpi.invalidate_persistence();
    profile.preferences.invalidate_persistence();
    profile.buttons.invalidate_persistence();
    profile.polling_rate.invalidate_persistence();
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
    use crate::device::DeviceIdentity;
    use crate::manager::DeviceManager;
    use crate::state::{
        ApplicationVerification, DesiredSource, DesiredState, ObservationSource, ObservedState,
        PersistenceVerification, ResourceState, StatePaths, StateStore, Timestamp, Verification,
    };
    use attack_shark_x3::{
        ButtonAssignment, ButtonsState, DpiState, DpiValue, PollingRate, PreferencesState,
        ProfileId, ProfileMetadata, StageIndex, TransportKind,
    };

    fn store(dir: &tempfile::TempDir) -> StateStore {
        StateStore::open(StatePaths {
            state_file: dir.path().join("state.json"),
            lock_file: dir.path().join("state.lock"),
        })
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
        let desired_preferences = DesiredState {
            value: preferences,
            source: DesiredSource::UserWrite,
            verification: Verification {
                application: ApplicationVerification::Acknowledged,
                persistence: persistent.clone(),
            },
            updated_at: timestamp,
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
                    application: ApplicationVerification::Acknowledged,
                    persistence: persistent.clone(),
                },
                updated_at: timestamp,
            }),
            observed: None,
        };
        device.profiles.insert(
            profile,
            crate::state::ProfileState {
                dpi: ResourceState {
                    desired: Some(desired_dpi),
                    observed: None,
                },
                preferences: ResourceState {
                    desired: Some(desired_preferences),
                    observed: None,
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
                            application: ApplicationVerification::Acknowledged,
                            persistence: persistent.clone(),
                        },
                        updated_at: timestamp,
                    }),
                    observed: None,
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
}
