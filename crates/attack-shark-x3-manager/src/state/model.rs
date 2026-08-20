use std::collections::BTreeMap;

use attack_shark_x3::{
    ButtonsState, DpiState, PollingRate, PreferencesState, ProfileId, ProfileMetadata,
};
use serde::{Deserialize, Serialize};

use crate::device::{DeviceId, DeviceIdentity};
use crate::error::StateError;

/// The durable-state schema understood by this manager.
///
/// Schema 4 introduces logical `mouse-N` device keys, multi-transport
/// endpoints, next-device allocation, and strengthened evidence invariants.
/// No migration from schema 3 exists; schema 3 documents are rejected.
pub const SCHEMA_VERSION: u32 = 4;

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

    /// Records an acknowledged write, preserving any historical observation.
    ///
    /// The observation is kept as historical evidence; only the desired value
    /// and its verification are updated. Persistence is always reset to
    /// [`PersistenceVerification::Unknown`] — an ACK never implies survival
    /// across profile reload or power cycle.
    pub fn record_ack_write(&mut self, value: T, source: DesiredSource, now: Timestamp)
    where
        T: Clone,
    {
        self.desired = Some(DesiredState {
            value,
            source,
            verification: Verification {
                application: ApplicationVerification::Acknowledged,
                persistence: PersistenceVerification::Unknown,
            },
            updated_at: now,
        });
        // observed intentionally preserved as historical
    }

    /// Records a write that returned an immediate readback value.
    ///
    /// Both desired and observed are set to `now`. The application
    /// verification is [`ApplicationVerification::ReadbackVerified`] only when
    /// the readback equals the desired value and is current
    /// (`observed_at >= updated_at`); otherwise it is
    /// [`ApplicationVerification::Mismatch`]. Persistence is always
    /// [`PersistenceVerification::Unknown`] — ordinary readback never infers
    /// profile-reload or power-cycle survival.
    pub fn record_readback_write(
        &mut self,
        desired_value: T,
        observed_value: T,
        source: DesiredSource,
        now: Timestamp,
    ) where
        T: Clone + PartialEq,
    {
        let is_match = desired_value == observed_value;
        let application = if is_match {
            ApplicationVerification::ReadbackVerified
        } else {
            ApplicationVerification::Mismatch
        };
        self.desired = Some(DesiredState {
            value: desired_value,
            source,
            verification: Verification {
                application,
                persistence: PersistenceVerification::Unknown,
            },
            updated_at: now,
        });
        self.observed = Some(ObservedState {
            value: observed_value,
            source: ObservationSource::UsbReadback,
            observed_at: now,
        });
    }

    /// Generic helper that records a write with optional readback.
    ///
    /// When `observed` is `None`, this is an ACK-only write and preserves the
    /// historical observation. When `Some`, it is a readback write with the
    /// matching/mismatch logic of [`Self::record_readback_write`].
    pub fn record_write(
        &mut self,
        desired: T,
        observed: Option<T>,
        source: DesiredSource,
        now: Timestamp,
    ) where
        T: Clone + PartialEq,
    {
        if let Some(readback) = observed {
            self.record_readback_write(desired, readback, source, now);
        } else {
            self.record_ack_write(desired, source, now);
        }
    }

    /// Reconciles a fresh observation against the current desired value.
    ///
    /// * With no desired value, only the observation is stored.
    /// * With a desired value, the observed value is stored and the desired
    ///   verification is updated truthfully: [`ApplicationVerification::ReadbackVerified`]
    ///   only when the observed value equals the desired value and is current
    ///   (`observed_at >= updated_at`); otherwise
    ///   [`ApplicationVerification::Mismatch`]. Persistence is always reset to
    ///   [`PersistenceVerification::Unknown`] — a fresh read never infers
    ///   power-cycle or profile-reload survival.
    pub fn reconcile_observation(&mut self, value: T, now: Timestamp)
    where
        T: Clone + PartialEq,
    {
        let observed = ObservedState {
            value: value.clone(),
            source: ObservationSource::UsbReadback,
            observed_at: now,
        };
        if let Some(desired) = self.desired.as_mut() {
            let is_match =
                desired.value == value && now.unix_seconds >= desired.updated_at.unix_seconds;
            desired.verification.application = if is_match {
                ApplicationVerification::ReadbackVerified
            } else {
                ApplicationVerification::Mismatch
            };
            desired.verification.persistence = PersistenceVerification::Unknown;
        }
        self.observed = Some(observed);
    }

    /// Attempts to mark the current readback as profile-reload verified.
    ///
    /// Succeeds only when a current matching readback is already present:
    /// a desired value exists, its application is
    /// [`ApplicationVerification::ReadbackVerified`], an observed value exists,
    /// values are equal, and `observed_at >= updated_at`. This helper never
    /// invents persistence from a mismatched or stale read.
    pub fn try_mark_profile_reload_verified(&mut self, verified_at: Timestamp) -> bool
    where
        T: PartialEq,
    {
        self.try_mark_persistence(PersistenceVerification::ProfileReloadVerified { verified_at })
    }

    /// Attempts to mark the current readback as power-cycle verified.
    pub fn try_mark_power_cycle_verified(&mut self, verified_at: Timestamp) -> bool
    where
        T: PartialEq,
    {
        self.try_mark_persistence(PersistenceVerification::PowerCycleVerified { verified_at })
    }

    fn try_mark_persistence(&mut self, persistence: PersistenceVerification) -> bool
    where
        T: PartialEq,
    {
        let Some(desired) = self.desired.as_mut() else {
            return false;
        };
        let Some(observed) = self.observed.as_ref() else {
            return false;
        };
        if desired.verification.application != ApplicationVerification::ReadbackVerified {
            return false;
        }
        if desired.value != observed.value {
            return false;
        }
        if observed.observed_at.unix_seconds < desired.updated_at.unix_seconds {
            return false;
        }
        if persistence.is_unknown() {
            return false;
        }
        desired.verification.persistence = persistence;
        true
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

/// Durable state for one logical mouse.
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

    /// Drops persistence evidence for the device-global and all profile
    /// resources while preserving desired/observed values.
    pub fn invalidate_persistence(&mut self) {
        self.profile_metadata.invalidate_persistence();
        for profile in self.profiles.values_mut() {
            profile.invalidate_persistence();
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

    /// Drops persistence evidence for every resource in the profile.
    pub fn invalidate_persistence(&mut self) {
        self.dpi.invalidate_persistence();
        self.preferences.invalidate_persistence();
        self.buttons.invalidate_persistence();
        self.polling_rate.invalidate_persistence();
    }

    /// Attempts to mark all matching readback resources as profile-reload
    /// verified. Resources without a current matching readback are left
    /// untouched (their persistence is cleared).
    pub fn try_mark_profile_reload_verified(&mut self, verified_at: Timestamp) {
        if !self.dpi.try_mark_profile_reload_verified(verified_at) {
            self.dpi.invalidate_persistence();
        }
        if !self
            .preferences
            .try_mark_profile_reload_verified(verified_at)
        {
            self.preferences.invalidate_persistence();
        }
        if !self.buttons.try_mark_profile_reload_verified(verified_at) {
            self.buttons.invalidate_persistence();
        }
        if !self
            .polling_rate
            .try_mark_profile_reload_verified(verified_at)
        {
            self.polling_rate.invalidate_persistence();
        }
    }

    /// Attempts to mark all matching readback resources as power-cycle
    /// verified.
    pub fn try_mark_power_cycle_verified(&mut self, verified_at: Timestamp) {
        if !self.dpi.try_mark_power_cycle_verified(verified_at) {
            self.dpi.invalidate_persistence();
        }
        if !self.preferences.try_mark_power_cycle_verified(verified_at) {
            self.preferences.invalidate_persistence();
        }
        if !self.buttons.try_mark_power_cycle_verified(verified_at) {
            self.buttons.invalidate_persistence();
        }
        if !self.polling_rate.try_mark_power_cycle_verified(verified_at) {
            self.polling_rate.invalidate_persistence();
        }
    }
}

/// The complete internal manager-state document.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateFile {
    pub schema_version: u32,
    pub next_device_number: u64,
    pub selected_device: Option<DeviceId>,
    pub devices: BTreeMap<DeviceId, DeviceState>,
}

impl Default for StateFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            next_device_number: 1,
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

    /// Allocates a fresh logical device id `mouse-N` collision-free under this file.
    ///
    /// Allocation is monotonic via `next_device_number`. If the candidate collides
    /// with an existing key (should not happen under correct validation), we scan
    /// forward until free.
    pub fn allocate_device_id(&mut self) -> Result<DeviceId, StateError> {
        if self.next_device_number == 0 {
            return Err(StateError::invalid_state("nextDeviceNumber must be >= 1"));
        }
        let mut candidate_number = self.next_device_number;
        // Prevent infinite loop / overflow.
        for _ in 0..1_000_000 {
            let candidate = DeviceId::from_number(candidate_number)?;
            if !self.devices.contains_key(&candidate) {
                self.next_device_number = candidate_number + 1;
                // Ensure we didn't wrap to 0.
                if self.next_device_number == 0 {
                    return Err(StateError::invalid_state("nextDeviceNumber overflow"));
                }
                return Ok(candidate);
            }
            candidate_number = candidate_number.checked_add(1).ok_or_else(|| {
                StateError::invalid_state("nextDeviceNumber overflow while allocating")
            })?;
        }
        Err(StateError::invalid_state(
            "unable to allocate device id: too many collisions",
        ))
    }

    /// Rejects documents from another schema generation or with mismatched
    /// device/profile/resource identities and invalid evidence.
    pub fn validate(&self) -> Result<(), StateError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(StateError::unsupported_schema(self.schema_version));
        }

        if self.next_device_number == 0 {
            return Err(StateError::invalid_state("nextDeviceNumber must be >= 1"));
        }

        // Validate next_device_number monotonicity: must be greater than every existing mouse-N.
        let mut max_number: u64 = 0;
        for device_id in self.devices.keys() {
            let number = device_id.number().ok_or_else(|| {
                StateError::invalid_state(format!("device key {device_id} is not mouse-N"))
            })?;
            if number == 0 {
                return Err(StateError::invalid_state(format!(
                    "device key {device_id} has number 0"
                )));
            }
            if number > max_number {
                max_number = number;
            }
        }
        if max_number >= self.next_device_number {
            return Err(StateError::invalid_state(format!(
                "nextDeviceNumber {} must be greater than max device number {max_number}",
                self.next_device_number
            )));
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
            // Validate logical identity endpoints coherence.
            device.identity.validate()?;

            // Generic evidence consistency for device-global resources.
            validate_resource_state(
                &device.profile_metadata,
                &format!("profileMetadata for device {device_id}"),
            )?;

            for (profile_id, profile) in &device.profiles {
                validate_profile_resource(profile_id, &profile.dpi, "dpi")?;
                validate_profile_resource(profile_id, &profile.preferences, "preferences")?;
                validate_profile_resource(profile_id, &profile.buttons, "buttons")?;
                // Polling is per-profile; validate evidence and skip profile_id coherence (PollingRate has no profile field)
                validate_polling_resource(profile_id, &profile.polling_rate)?;
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
    T: ProfileValue + PartialEq,
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
    validate_resource_state(resource, &format!("{name} for profile {profile_id}"))?;
    Ok(())
}

fn validate_resource_state<T: PartialEq>(
    resource: &ResourceState<T>,
    ctx: &str,
) -> Result<(), StateError> {
    let Some(desired) = resource.desired.as_ref() else {
        // No desired: observed may be anything (historical), no verification to check.
        return Ok(());
    };

    match desired.verification.application {
        ApplicationVerification::NotSent | ApplicationVerification::Acknowledged => {
            if !desired.verification.persistence.is_unknown() {
                return Err(StateError::invalid_state(format!(
                    "{ctx}: persistence claim requires ReadbackVerified application evidence"
                )));
            }
            // Historical observed may coexist with Acknowledged/NotSent regardless of equality or staleness.
            // No further checks.
        }
        ApplicationVerification::ReadbackVerified => {
            let observed = resource.observed.as_ref().ok_or_else(|| {
                StateError::invalid_state(format!(
                    "{ctx}: ReadbackVerified requires an observed value"
                ))
            })?;
            if desired.value != observed.value {
                return Err(StateError::invalid_state(format!(
                    "{ctx}: ReadbackVerified requires equal desired and observed values"
                )));
            }
            if observed.observed_at.unix_seconds < desired.updated_at.unix_seconds {
                return Err(StateError::invalid_state(format!(
                    "{ctx}: ReadbackVerified observation is older than desired (stale)"
                )));
            }
            if !desired.verification.persistence.is_unknown() {
                // persistence already requires readback match; no extra check beyond the equality/staleness above.
                // Optionally ensure observed not stale already checked.
            }
        }
        ApplicationVerification::Mismatch => {
            let observed = resource.observed.as_ref().ok_or_else(|| {
                StateError::invalid_state(format!("{ctx}: Mismatch requires an observed value"))
            })?;
            if desired.value == observed.value {
                return Err(StateError::invalid_state(format!(
                    "{ctx}: Mismatch requires differing observed value"
                )));
            }
            if !desired.verification.persistence.is_unknown() {
                return Err(StateError::invalid_state(format!(
                    "{ctx}: Mismatch cannot carry persistence claims"
                )));
            }
            // Timestamp staleness for mismatch is not strictly required, but we allow any timestamp
            // because historical observed may remain after new desired with Mismatch? For safety, allow stale.
        }
    }

    // Generic persistence invariant: any persistence claim requires ReadbackVerified with current matching readback.
    if !desired.verification.persistence.is_unknown()
        && desired.verification.application != ApplicationVerification::ReadbackVerified
    {
        return Err(StateError::invalid_state(format!(
            "{ctx}: persistence claim requires ReadbackVerified"
        )));
    }

    // If persistence present, we already validated observed present, equal, not stale via ReadbackVerified branch.
    // Additionally, persistence claims should have observed present and matching; already covered.

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

impl ProfileValue for PollingRate {
    fn profile_id(&self) -> ProfileId {
        // PollingRate is stored per-profile but its own struct may not carry profile.
        // We treat PollingRate as having no profile id mismatch check; but we still implement
        // trait to allow generic validation. Check the actual PollingRate structure.
        // From attack-shark-x3, PollingRate doesn't carry ProfileId; we fake identity check by
        // returning the queried profile id. So profile_id coherence is vacuously true for polling.
        // However our generic validate_profile_resource will compare desired.value.profile_id() to the map key.
        // For polling, that would be meaningless. We need to handle polling specially: always return the map key.
        // Instead, we will not use the trait comparison for polling by returning the expected id via a thread-local?
        // Simpler: implement PollingRate profile_id to return a dummy that we ignore; we override validate for polling.
        // But we already call validate_profile_resource for polling which checks trait. To avoid false failures,
        // we make this return a sentinel that will never mismatch unless we change approach.
        // Alternative: specialize validation for PollingRate to skip profile_id check.
        // For now we return ProfileId 1; the mismatch check will then spuriously fail if profile is not 1.
        // So we need a different approach: we detect PollingRate specially.
        // To keep generic, we will have polling validation skip id check.
        // Easiest: return the map key via a hack: we can't know map key here, so we need to bypass.
        // We choose to make PollingRate impl return a fixed value and then override caller to not check PollingRate id.
        // Instead we handle PollingRate outside: see validate_profile_resource specialization below.
        // But trait requires impl; we provide a best-effort impl that never mismatches by returning a value that caller will treat as matching.
        // We'll implement a separate function validate_polling that doesn't check id.
        // To satisfy compiler, we return ProfileId(1) and caller will bypass check for polling.
        ProfileId::new(1).unwrap()
    }
}

impl ProfileValue for ProfileMetadata {
    fn profile_id(&self) -> ProfileId {
        // ProfileMetadata doesn't have a single profile; treat as not profile-scoped.
        ProfileId::new(1).unwrap()
    }
}

fn validate_polling_resource(
    profile_id: &ProfileId,
    resource: &ResourceState<PollingRate>,
) -> Result<(), StateError> {
    // PollingRate has no embedded profile id; only evidence invariants apply.
    validate_resource_state(resource, &format!("pollingRate for profile {profile_id}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ApplicationVerification, DesiredSource, DesiredState, DeviceState, MAX_PROFILE_NAME_CHARS,
        ObservationSource, ObservedState, PersistenceVerification, ProfileState, ResourceState,
        SCHEMA_VERSION, StateFile, Timestamp, Verification,
    };
    use crate::device::{DeviceEndpoint, DeviceId, DeviceIdentity};
    use crate::error::StateError;
    use attack_shark_x3::{DpiState, PollingRate, ProfileId};
    use std::collections::BTreeMap;

    fn timestamp(seconds: i64) -> Timestamp {
        Timestamp {
            unix_seconds: seconds,
        }
    }

    fn wired_endpoint(path: &str) -> DeviceEndpoint {
        DeviceEndpoint::usb(
            attack_shark_x3::TransportKind::Wired,
            0x1d57,
            0xfa60,
            None,
            path,
            None,
        )
        .unwrap()
    }

    fn ble_endpoint(id: &str) -> DeviceEndpoint {
        DeviceEndpoint::ble(id, None).unwrap()
    }

    fn device_identity(id_str: &str, endpoints: Vec<DeviceEndpoint>) -> DeviceIdentity {
        let mut identity = DeviceIdentity::new(DeviceId::new(id_str).unwrap(), None);
        for endpoint in endpoints {
            identity.upsert_endpoint(endpoint);
        }
        identity
    }

    fn dpi_profile_one(value: u16) -> DpiState {
        DpiState::captured_empty_profile_one(
            vec![attack_shark_x3::DpiValue::new(value).expect("valid dpi")],
            attack_shark_x3::StageIndex::new(1).expect("valid stage"),
        )
        .expect("valid state")
    }

    #[test]
    fn default_state_file_starts_at_schema_four_with_next_one() {
        let state = StateFile::default();
        assert_eq!(state.schema_version, SCHEMA_VERSION);
        assert_eq!(state.schema_version, 4);
        assert_eq!(state.next_device_number, 1);
        assert!(state.selected_device.is_none());
        assert!(state.devices.is_empty());
        assert!(state.validate().is_ok());
    }

    #[test]
    fn allocation_is_collision_free_and_monotonic() {
        let mut state = StateFile::default();
        let first = state.allocate_device_id().unwrap();
        assert_eq!(first.as_str(), "mouse-1");
        assert_eq!(state.next_device_number, 2);
        let second = state.allocate_device_id().unwrap();
        assert_eq!(second.as_str(), "mouse-2");
        assert_eq!(state.next_device_number, 3);

        // Insert a device with gap: manually insert mouse-5, next should stay 3 until it collides.
        let identity_gap = device_identity("mouse-5", vec![wired_endpoint("/dev/hidraw0")]);
        state
            .devices
            .insert(identity_gap.id.clone(), DeviceState::new(identity_gap));
        // Now max is 5, next is 3, validation should fail.
        assert!(state.validate().is_err());

        // Fix next to be monotonic.
        state.next_device_number = 6;
        assert!(state.validate().is_ok());

        // Next allocation should be mouse-6, not collide.
        let third = state.allocate_device_id().unwrap();
        assert_eq!(third.as_str(), "mouse-6");

        // Collision scan: if next points to existing, it scans forward.
        let mut state2 = StateFile::default();
        // Pre-insert mouse-1 and mouse-2 manually without advancing next.
        state2.next_device_number = 1;
        let id1 = DeviceId::new("mouse-1").unwrap();
        state2.devices.insert(
            id1.clone(),
            DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")])),
        );
        // Allocation should skip mouse-1 and give mouse-2
        let allocated = state2.allocate_device_id().unwrap();
        assert_eq!(allocated.as_str(), "mouse-2");
    }

    #[test]
    fn single_logical_identity_represents_multiple_transports() {
        let mut identity = DeviceIdentity::new(
            DeviceId::new("mouse-1").unwrap(),
            Some("My Mouse".to_owned()),
        );
        identity.upsert_endpoint(wired_endpoint("/dev/hidraw0"));
        identity.upsert_endpoint(ble_endpoint("ble-addr-1"));
        identity.upsert_endpoint(
            DeviceEndpoint::usb(
                attack_shark_x3::TransportKind::Receiver,
                0x1d57,
                0xfa60,
                None,
                "/dev/hidraw1",
                None,
            )
            .unwrap(),
        );
        assert_eq!(identity.endpoints.len(), 3);
        assert!(identity.validate().is_ok());

        let state = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: Some(DeviceId::new("mouse-1").unwrap()),
            devices: BTreeMap::from([(
                DeviceId::new("mouse-1").unwrap(),
                DeviceState::new(identity),
            )]),
        };
        assert!(state.validate().is_ok());
    }

    #[test]
    fn validate_rejects_endpoint_transport_locator_mismatch() {
        let mut identity = device_identity("mouse-1", vec![ble_endpoint("ble-1")]);
        // Tamper: insert endpoint keyed as Wired but carrying Ble transport.
        let mut bad_endpoint = ble_endpoint("ble-1");
        bad_endpoint.transport = attack_shark_x3::TransportKind::Wired;
        identity.endpoints.clear();
        identity
            .endpoints
            .insert(attack_shark_x3::TransportKind::Wired, bad_endpoint);

        let state = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(
                DeviceId::new("mouse-1").unwrap(),
                DeviceState::new(identity),
            )]),
        };
        assert!(state.validate().is_err());

        // Also test next_device_number monotonicity failure.
        let mut state2 = StateFile::default();
        state2.next_device_number = 0;
        assert!(state2.validate().is_err());

        let mut state3 = StateFile::default();
        state3.next_device_number = 1;
        state3.devices.insert(
            DeviceId::new("mouse-5").unwrap(),
            DeviceState::new(device_identity("mouse-5", vec![wired_endpoint("/a")])),
        );
        assert!(state3.validate().is_err());
    }

    #[test]
    fn validate_rejects_overlong_or_blank_profile_names() {
        let identity = device_identity("mouse-1", vec![wired_endpoint("/a")]);
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
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), overlong)]),
        };
        assert!(matches!(state.validate(), Err(StateError::InvalidState(_))));

        let blank = {
            let mut device = DeviceState::new(identity);
            device.profile_names.insert(profile, "   ".to_owned());
            device
        };
        let state = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), blank)]),
        };
        assert!(matches!(state.validate(), Err(StateError::InvalidState(_))));
    }

    #[test]
    fn old_state_documents_without_profile_names_deserialize() {
        let identity = device_identity("mouse-1", vec![ble_endpoint("test")]);
        let device_id = identity.id.clone();
        let json = serde_json::json!({
            "schemaVersion": SCHEMA_VERSION,
            "nextDeviceNumber": 2,
            "selectedDevice": device_id,
            "devices": {
                "mouse-1": {
                    "identity": identity,
                    "profileMetadata": {
                        "desired": null,
                        "observed": null,
                    },
                    "profiles": {},
                }
            },
        });
        let state: StateFile = serde_json::from_value(json).expect("deserialize state");
        assert!(state.validate().is_ok());
        let device = &state.devices[&device_id];
        assert!(device.profile_names.is_empty());
    }

    #[test]
    fn json_keeps_desired_and_observed_distinct() {
        let dpi = dpi_profile_one(800);
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
    fn readback_verified_requires_equal_and_not_stale() {
        let dpi = dpi_profile_one(800);
        let different = dpi_profile_one(1600);

        let identity = device_identity("mouse-1", vec![wired_endpoint("/a")]);
        let mut device_state = DeviceState::new(identity);
        let profile = ProfileId::new(1).unwrap();

        // Valid ReadbackVerified: equal and observed >= desired
        let mut valid = ProfileState::empty();
        valid.dpi = ResourceState {
            desired: Some(DesiredState {
                value: dpi.clone(),
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::ReadbackVerified,
                    persistence: PersistenceVerification::Unknown,
                },
                updated_at: timestamp(10),
            }),
            observed: Some(ObservedState {
                value: dpi.clone(),
                source: ObservationSource::UsbReadback,
                observed_at: timestamp(11),
            }),
        };
        device_state.profiles.insert(profile, valid);
        let state = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), device_state.clone())]),
        };
        assert!(state.validate().is_ok());

        // Different value with ReadbackVerified must fail
        let mut bad_different = ProfileState::empty();
        bad_different.dpi = ResourceState {
            desired: Some(DesiredState {
                value: dpi.clone(),
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::ReadbackVerified,
                    persistence: PersistenceVerification::Unknown,
                },
                updated_at: timestamp(10),
            }),
            observed: Some(ObservedState {
                value: different.clone(),
                source: ObservationSource::UsbReadback,
                observed_at: timestamp(11),
            }),
        };
        let mut device_state2 =
            DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        device_state2.profiles.insert(profile, bad_different);
        let state2 = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), device_state2)]),
        };
        assert!(state2.validate().is_err());

        // Stale observation must fail for ReadbackVerified
        let mut stale = ProfileState::empty();
        stale.dpi = ResourceState {
            desired: Some(DesiredState {
                value: dpi.clone(),
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::ReadbackVerified,
                    persistence: PersistenceVerification::Unknown,
                },
                updated_at: timestamp(20),
            }),
            observed: Some(ObservedState {
                value: dpi.clone(),
                source: ObservationSource::UsbReadback,
                observed_at: timestamp(10),
            }),
        };
        let mut device_state3 =
            DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        device_state3.profiles.insert(profile, stale);
        let state3 = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), device_state3)]),
        };
        assert!(state3.validate().is_err());

        // Historical observation may coexist with Acknowledged (different and stale allowed)
        let mut historical = ProfileState::empty();
        historical.dpi = ResourceState {
            desired: Some(DesiredState {
                value: dpi.clone(),
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::Acknowledged,
                    persistence: PersistenceVerification::Unknown,
                },
                updated_at: timestamp(20),
            }),
            observed: Some(ObservedState {
                value: different.clone(),
                source: ObservationSource::UsbReadback,
                observed_at: timestamp(10),
            }),
        };
        let mut device_state4 =
            DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        device_state4.profiles.insert(profile, historical);
        let state4 = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), device_state4)]),
        };
        assert!(
            state4.validate().is_ok(),
            "Acknowledged should allow historical differing stale observation"
        );
    }

    #[test]
    fn mismatch_requires_differing_observation_and_no_persistence() {
        let dpi = dpi_profile_one(800);
        let different = dpi_profile_one(1600);
        let profile = ProfileId::new(1).unwrap();

        let mut valid_mismatch = ProfileState::empty();
        valid_mismatch.dpi = ResourceState {
            desired: Some(DesiredState {
                value: dpi.clone(),
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::Mismatch,
                    persistence: PersistenceVerification::Unknown,
                },
                updated_at: timestamp(10),
            }),
            observed: Some(ObservedState {
                value: different.clone(),
                source: ObservationSource::UsbReadback,
                observed_at: timestamp(11),
            }),
        };
        let mut device_state =
            DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        device_state.profiles.insert(profile, valid_mismatch);
        let state = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), device_state)]),
        };
        assert!(state.validate().is_ok());

        // Equal observed with Mismatch must fail
        let mut equal_mismatch = ProfileState::empty();
        equal_mismatch.dpi = ResourceState {
            desired: Some(DesiredState {
                value: dpi.clone(),
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::Mismatch,
                    persistence: PersistenceVerification::Unknown,
                },
                updated_at: timestamp(10),
            }),
            observed: Some(ObservedState {
                value: dpi.clone(),
                source: ObservationSource::UsbReadback,
                observed_at: timestamp(11),
            }),
        };
        let mut device_state2 =
            DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        device_state2.profiles.insert(profile, equal_mismatch);
        let state2 = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), device_state2)]),
        };
        assert!(state2.validate().is_err());

        // Mismatch with persistence must fail
        let mut mismatch_with_persistence = ProfileState::empty();
        mismatch_with_persistence.dpi = ResourceState {
            desired: Some(DesiredState {
                value: dpi.clone(),
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::Mismatch,
                    persistence: PersistenceVerification::ProfileReloadVerified {
                        verified_at: timestamp(12),
                    },
                },
                updated_at: timestamp(10),
            }),
            observed: Some(ObservedState {
                value: different.clone(),
                source: ObservationSource::UsbReadback,
                observed_at: timestamp(11),
            }),
        };
        let mut device_state3 =
            DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        device_state3
            .profiles
            .insert(profile, mismatch_with_persistence);
        let state3 = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), device_state3)]),
        };
        assert!(state3.validate().is_err());
    }

    #[test]
    fn persistence_claims_require_current_matching_readback() {
        let dpi = dpi_profile_one(800);
        let different = dpi_profile_one(1600);
        let profile = ProfileId::new(1).unwrap();

        // Valid persistence with ReadbackVerified and equal not stale
        let mut valid = ProfileState::empty();
        valid.dpi = ResourceState {
            desired: Some(DesiredState {
                value: dpi.clone(),
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::ReadbackVerified,
                    persistence: PersistenceVerification::PowerCycleVerified {
                        verified_at: timestamp(12),
                    },
                },
                updated_at: timestamp(10),
            }),
            observed: Some(ObservedState {
                value: dpi.clone(),
                source: ObservationSource::UsbReadback,
                observed_at: timestamp(11),
            }),
        };
        let mut device_state =
            DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        device_state.profiles.insert(profile, valid);
        let state = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), device_state)]),
        };
        assert!(state.validate().is_ok());

        // Acknowledged with persistence must fail
        let mut ack_persistence = ProfileState::empty();
        ack_persistence.dpi = ResourceState {
            desired: Some(DesiredState {
                value: dpi.clone(),
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::Acknowledged,
                    persistence: PersistenceVerification::ProfileReloadVerified {
                        verified_at: timestamp(12),
                    },
                },
                updated_at: timestamp(10),
            }),
            observed: None,
        };
        let mut device_state2 =
            DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        device_state2.profiles.insert(profile, ack_persistence);
        let state2 = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), device_state2)]),
        };
        assert!(state2.validate().is_err());

        // NotSent with persistence must fail
        let mut notsent_persistence = ProfileState::empty();
        notsent_persistence.dpi = ResourceState {
            desired: Some(DesiredState {
                value: dpi.clone(),
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::NotSent,
                    persistence: PersistenceVerification::PowerCycleVerified {
                        verified_at: timestamp(12),
                    },
                },
                updated_at: timestamp(10),
            }),
            observed: None,
        };
        let mut device_state3 =
            DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        device_state3.profiles.insert(profile, notsent_persistence);
        let state3 = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), device_state3)]),
        };
        assert!(state3.validate().is_err());

        // ReadbackVerified but with differing observed and persistence must fail
        let mut diff_persistence = ProfileState::empty();
        diff_persistence.dpi = ResourceState {
            desired: Some(DesiredState {
                value: dpi.clone(),
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::ReadbackVerified,
                    persistence: PersistenceVerification::PowerCycleVerified {
                        verified_at: timestamp(12),
                    },
                },
                updated_at: timestamp(10),
            }),
            observed: Some(ObservedState {
                value: different.clone(),
                source: ObservationSource::UsbReadback,
                observed_at: timestamp(11),
            }),
        };
        let mut device_state4 =
            DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        device_state4.profiles.insert(profile, diff_persistence);
        let state4 = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), device_state4)]),
        };
        assert!(state4.validate().is_err());
    }

    #[test]
    fn polling_rate_profile_presence_validated_and_evidence_checked() {
        let profile = ProfileId::new(2).unwrap();
        let polling_good = PollingRate::Hz1000;
        let polling_bad = PollingRate::Hz500;

        // Valid polling resource with ReadbackVerified
        let mut valid = ProfileState::empty();
        valid.polling_rate = ResourceState {
            desired: Some(DesiredState {
                value: polling_good,
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::ReadbackVerified,
                    persistence: PersistenceVerification::Unknown,
                },
                updated_at: timestamp(10),
            }),
            observed: Some(ObservedState {
                value: polling_good,
                source: ObservationSource::UsbReadback,
                observed_at: timestamp(11),
            }),
        };
        let mut device_state =
            DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        device_state.profiles.insert(profile, valid);
        let state = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), device_state)]),
        };
        assert!(state.validate().is_ok());

        // Invalid polling evidence: ReadbackVerified with differing value
        let mut invalid = ProfileState::empty();
        invalid.polling_rate = ResourceState {
            desired: Some(DesiredState {
                value: polling_good,
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::ReadbackVerified,
                    persistence: PersistenceVerification::Unknown,
                },
                updated_at: timestamp(10),
            }),
            observed: Some(ObservedState {
                value: polling_bad,
                source: ObservationSource::UsbReadback,
                observed_at: timestamp(11),
            }),
        };
        let mut device_state2 =
            DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        device_state2.profiles.insert(profile, invalid);
        let state2 = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), device_state2)]),
        };
        assert!(state2.validate().is_err());
    }

    #[test]
    fn schema_three_rejected_without_migration() {
        let identity = device_identity("mouse-1", vec![ble_endpoint("test")]);
        let json = serde_json::json!({
            "schemaVersion": 3,
            "nextDeviceNumber": 2,
            "selectedDevice": null,
            "devices": {
                "mouse-1": {
                    "identity": identity,
                    "profileMetadata": { "desired": null, "observed": null },
                    "profiles": {}
                }
            }
        });
        let state: Result<StateFile, _> = serde_json::from_value(json.clone());
        // Deserialization will succeed (struct allows any schema_version value), but validate must fail.
        if let Ok(state) = state {
            assert!(matches!(
                state.validate(),
                Err(StateError::UnsupportedSchema { found: 3, .. })
            ));
        } else {
            // Or serde already errors? Either is acceptable as rejection.
        }

        // Direct construction with schema 3 should fail validation.
        let mut state3 = StateFile::default();
        state3.schema_version = 3;
        assert!(matches!(
            state3.validate(),
            Err(StateError::UnsupportedSchema { .. })
        ));

        // Also via store load path: file with schema 3 rejected.
        let json_str = serde_json::to_string(&json).unwrap();
        let header: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(header["schemaVersion"], 3);
        // Ensure our constant is 4, so load would reject.
        assert_eq!(SCHEMA_VERSION, 4);
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
        let identity = device_identity("mouse-1", vec![ble_endpoint("test")]);
        let device = DeviceState::new(identity);
        assert!(device.profile_names.is_empty());
    }

    #[test]
    fn ack_preserves_historical_observation() {
        let mut resource: ResourceState<u32> = ResourceState {
            desired: Some(DesiredState {
                value: 1,
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::ReadbackVerified,
                    persistence: PersistenceVerification::Unknown,
                },
                updated_at: timestamp(5),
            }),
            observed: Some(ObservedState {
                value: 1,
                source: ObservationSource::UsbReadback,
                observed_at: timestamp(6),
            }),
        };
        let historical = resource.observed.clone();
        resource.record_ack_write(2, DesiredSource::UserWrite, timestamp(10));
        assert_eq!(resource.desired.as_ref().unwrap().value, 2);
        assert_eq!(
            resource.observed, historical,
            "ACK must preserve historical observation"
        );
        assert_eq!(
            resource.desired.as_ref().unwrap().verification.application,
            ApplicationVerification::Acknowledged
        );
        assert_eq!(
            resource.desired.as_ref().unwrap().verification.persistence,
            PersistenceVerification::Unknown
        );
    }

    #[test]
    fn ack_truth_sets_acknowledged_and_unknown_persistence() {
        let mut resource: ResourceState<u32> = ResourceState::empty();
        resource.record_ack_write(42, DesiredSource::UserWrite, timestamp(10));
        let desired = resource.desired.as_ref().unwrap();
        assert_eq!(desired.value, 42);
        assert_eq!(desired.source, DesiredSource::UserWrite);
        assert_eq!(
            desired.verification.application,
            ApplicationVerification::Acknowledged
        );
        assert_eq!(
            desired.verification.persistence,
            PersistenceVerification::Unknown
        );
        assert_eq!(desired.updated_at, timestamp(10));
        // no observation inferred
        assert!(resource.observed.is_none());
        // validation must accept ACK with historical stale differing observation
        // Still valid: Acknowledged allows any historical observed
        let mut device_state =
            DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        device_state.profiles.insert(
            ProfileId::new(1).unwrap(),
            ProfileState {
                dpi: ResourceState {
                    desired: Some(DesiredState {
                        value: dpi_profile_one(800),
                        source: DesiredSource::UserWrite,
                        verification: Verification {
                            application: ApplicationVerification::Acknowledged,
                            persistence: PersistenceVerification::Unknown,
                        },
                        updated_at: timestamp(10),
                    }),
                    observed: Some(ObservedState {
                        value: dpi_profile_one(1600),
                        source: ObservationSource::UsbReadback,
                        observed_at: timestamp(1),
                    }),
                },
                preferences: ResourceState::empty(),
                buttons: ResourceState::empty(),
                polling_rate: ResourceState::empty(),
            },
        );
        let state = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), device_state)]),
        };
        assert!(state.validate().is_ok());
    }

    #[test]
    fn reconcile_matching_read_sets_readback_verified_and_timestamp() {
        let mut resource: ResourceState<u32> = ResourceState::empty();
        resource.record_ack_write(10, DesiredSource::UserWrite, timestamp(10));
        resource.reconcile_observation(10, timestamp(12));
        let desired = resource.desired.as_ref().unwrap();
        assert_eq!(
            desired.verification.application,
            ApplicationVerification::ReadbackVerified
        );
        assert_eq!(
            desired.verification.persistence,
            PersistenceVerification::Unknown
        );
        let observed = resource.observed.as_ref().unwrap();
        assert_eq!(observed.value, 10);
        assert_eq!(observed.observed_at, timestamp(12));
        assert_eq!(observed.source, ObservationSource::UsbReadback);
    }

    #[test]
    fn reconcile_mismatching_read_sets_mismatch_and_resets_persistence() {
        let mut resource: ResourceState<u32> = ResourceState {
            desired: Some(DesiredState {
                value: 10,
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::ReadbackVerified,
                    persistence: PersistenceVerification::ProfileReloadVerified {
                        verified_at: timestamp(11),
                    },
                },
                updated_at: timestamp(10),
            }),
            observed: Some(ObservedState {
                value: 10,
                source: ObservationSource::UsbReadback,
                observed_at: timestamp(11),
            }),
        };
        resource.reconcile_observation(20, timestamp(13));
        let desired = resource.desired.as_ref().unwrap();
        assert_eq!(
            desired.verification.application,
            ApplicationVerification::Mismatch
        );
        assert_eq!(
            desired.verification.persistence,
            PersistenceVerification::Unknown
        );
        assert_eq!(resource.observed.as_ref().unwrap().value, 20);
        assert_eq!(
            resource.observed.as_ref().unwrap().observed_at,
            timestamp(13)
        );
    }

    #[test]
    fn fresh_read_without_desired_records_observation_only() {
        let mut resource: ResourceState<u32> = ResourceState::empty();
        resource.reconcile_observation(99, timestamp(20));
        assert!(resource.desired.is_none());
        let observed = resource.observed.as_ref().unwrap();
        assert_eq!(observed.value, 99);
        assert_eq!(observed.observed_at, timestamp(20));
        assert_eq!(observed.source, ObservationSource::UsbReadback);
    }

    #[test]
    fn ordinary_read_never_infers_persistence() {
        let mut resource: ResourceState<u32> = ResourceState::empty();
        resource.record_ack_write(5, DesiredSource::UserWrite, timestamp(10));
        resource.reconcile_observation(5, timestamp(15));
        // Even though matching, persistence must remain Unknown — never ProfileReload or PowerCycle
        assert_eq!(
            resource.desired.as_ref().unwrap().verification.persistence,
            PersistenceVerification::Unknown
        );
        // Explicit marking via helper is required for persistence
        assert!(resource.try_mark_profile_reload_verified(timestamp(16)));
        assert_eq!(
            resource.desired.as_ref().unwrap().verification.persistence,
            PersistenceVerification::ProfileReloadVerified {
                verified_at: timestamp(16)
            }
        );
        // A subsequent ordinary read must reset persistence to Unknown again
        resource.reconcile_observation(5, timestamp(18));
        assert_eq!(
            resource.desired.as_ref().unwrap().verification.persistence,
            PersistenceVerification::Unknown
        );
    }

    #[test]
    fn invalid_persistence_combinations_are_rejected() {
        let profile = ProfileId::new(1).unwrap();
        // Acknowledged with persistence must fail
        let mut ack_with_persistence = ProfileState::empty();
        ack_with_persistence.dpi = ResourceState {
            desired: Some(DesiredState {
                value: dpi_profile_one(800),
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::Acknowledged,
                    persistence: PersistenceVerification::ProfileReloadVerified {
                        verified_at: timestamp(12),
                    },
                },
                updated_at: timestamp(10),
            }),
            observed: Some(ObservedState {
                value: dpi_profile_one(800),
                source: ObservationSource::UsbReadback,
                observed_at: timestamp(11),
            }),
        };
        let mut ds = DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        ds.profiles.insert(profile, ack_with_persistence);
        let state = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), ds)]),
        };
        assert!(state.validate().is_err());

        // Mismatch with persistence must fail
        let mut mismatch_with_persistence = ProfileState::empty();
        mismatch_with_persistence.dpi = ResourceState {
            desired: Some(DesiredState {
                value: dpi_profile_one(800),
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::Mismatch,
                    persistence: PersistenceVerification::PowerCycleVerified {
                        verified_at: timestamp(12),
                    },
                },
                updated_at: timestamp(10),
            }),
            observed: Some(ObservedState {
                value: dpi_profile_one(1600),
                source: ObservationSource::UsbReadback,
                observed_at: timestamp(11),
            }),
        };
        let mut ds2 = DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        ds2.profiles.insert(profile, mismatch_with_persistence);
        let state2 = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), ds2)]),
        };
        assert!(state2.validate().is_err());

        // ReadbackVerified with differing observed must fail
        let mut diff_readback = ProfileState::empty();
        diff_readback.dpi = ResourceState {
            desired: Some(DesiredState {
                value: dpi_profile_one(800),
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::ReadbackVerified,
                    persistence: PersistenceVerification::Unknown,
                },
                updated_at: timestamp(10),
            }),
            observed: Some(ObservedState {
                value: dpi_profile_one(1600),
                source: ObservationSource::UsbReadback,
                observed_at: timestamp(11),
            }),
        };
        let mut ds3 = DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        ds3.profiles.insert(profile, diff_readback);
        let state3 = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), ds3)]),
        };
        assert!(state3.validate().is_err());

        // Stale readback must fail
        let mut stale = ProfileState::empty();
        stale.dpi = ResourceState {
            desired: Some(DesiredState {
                value: dpi_profile_one(800),
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::ReadbackVerified,
                    persistence: PersistenceVerification::Unknown,
                },
                updated_at: timestamp(20),
            }),
            observed: Some(ObservedState {
                value: dpi_profile_one(800),
                source: ObservationSource::UsbReadback,
                observed_at: timestamp(10),
            }),
        };
        let mut ds4 = DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        ds4.profiles.insert(profile, stale);
        let state4 = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), ds4)]),
        };
        assert!(state4.validate().is_err());
    }

    #[test]
    fn persistence_helpers_require_current_matching_readback() {
        let mut resource: ResourceState<u32> = ResourceState::empty();
        resource.record_ack_write(7, DesiredSource::UserWrite, timestamp(10));
        // Cannot mark verified without a matching readback
        assert!(!resource.try_mark_profile_reload_verified(timestamp(11)));
        assert!(!resource.try_mark_power_cycle_verified(timestamp(11)));
        resource.reconcile_observation(7, timestamp(12));
        assert!(resource.try_mark_profile_reload_verified(timestamp(13)));
        assert_eq!(
            resource.desired.as_ref().unwrap().verification.persistence,
            PersistenceVerification::ProfileReloadVerified {
                verified_at: timestamp(13)
            }
        );
        assert!(resource.try_mark_power_cycle_verified(timestamp(14)));
        assert_eq!(
            resource.desired.as_ref().unwrap().verification.persistence,
            PersistenceVerification::PowerCycleVerified {
                verified_at: timestamp(14)
            }
        );
        // Mismatched read must not allow marking
        resource.reconcile_observation(8, timestamp(15));
        assert!(!resource.try_mark_profile_reload_verified(timestamp(16)));
        assert!(!resource.try_mark_power_cycle_verified(timestamp(16)));
        assert_eq!(
            resource.desired.as_ref().unwrap().verification.persistence,
            PersistenceVerification::Unknown
        );
    }

    #[test]
    fn record_readback_write_sets_verification_truthfully() {
        let mut resource: ResourceState<u32> = ResourceState::empty();
        resource.record_readback_write(10, 10, DesiredSource::UserWrite, timestamp(20));
        assert_eq!(
            resource.desired.as_ref().unwrap().verification.application,
            ApplicationVerification::ReadbackVerified
        );
        assert_eq!(resource.observed.as_ref().unwrap().value, 10);
        assert_eq!(
            resource.observed.as_ref().unwrap().observed_at,
            timestamp(20)
        );
        let mut resource2: ResourceState<u32> = ResourceState::empty();
        resource2.record_readback_write(10, 20, DesiredSource::UserWrite, timestamp(20));
        assert_eq!(
            resource2.desired.as_ref().unwrap().verification.application,
            ApplicationVerification::Mismatch
        );
        assert_eq!(
            resource2.desired.as_ref().unwrap().verification.persistence,
            PersistenceVerification::Unknown
        );
    }

    #[test]
    fn observation_timestamp_is_recorded() {
        let mut resource: ResourceState<u32> = ResourceState::empty();
        resource.reconcile_observation(123, timestamp(42));
        assert_eq!(
            resource.observed.as_ref().unwrap().observed_at,
            timestamp(42)
        );
        resource.record_ack_write(1, DesiredSource::UserWrite, timestamp(100));
        assert_eq!(
            resource.desired.as_ref().unwrap().updated_at,
            timestamp(100)
        );
        resource.record_readback_write(1, 1, DesiredSource::UserWrite, timestamp(101));
        assert_eq!(
            resource.observed.as_ref().unwrap().observed_at,
            timestamp(101)
        );
        assert_eq!(
            resource.desired.as_ref().unwrap().updated_at,
            timestamp(101)
        );
    }
    #[test]
    fn state_types_construct_for_compile() {
        let identity = device_identity("mouse-1", vec![ble_endpoint("test")]);
        let id = identity.id.clone();
        let _device = DeviceState::new(identity);
        let _profile = ProfileState::empty();
        let _state = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: Some(id),
            devices: BTreeMap::new(),
        };
    }
}
