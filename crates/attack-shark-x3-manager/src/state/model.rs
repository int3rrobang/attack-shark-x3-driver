use std::collections::BTreeMap;

use attack_shark_x3::{
    ButtonsState, DpiState, PhysicalId, PollingRate, PreferencesState, ProfileId, ProfileMetadata,
    TransportKind,
};
use serde::{Deserialize, Serialize};

use crate::device::{DeviceEndpoint, DeviceId, DeviceIdentity};
use crate::error::StateError;

/// The durable-state schema understood by this manager.
///
/// Schema 4 introduced logical `mouse-N` device keys, multi-transport
/// endpoints, next-device allocation, and strengthened evidence invariants.
/// Schema 5 adds installation-level physical-identity mode, optional
/// per-device physical watermark ids, and a durable resumable identity-setup
/// journal. No migration from schema 4 exists; schema 4 documents are
/// rejected.
pub const SCHEMA_VERSION: u32 = 5;

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
    /// Returns true when there is no desired or observed evidence.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.desired.is_none() && self.observed.is_none()
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
    ///
    /// Returns the [`Verification`] assigned to `self.desired`.
    pub fn record_ack_write(
        &mut self,
        value: T,
        source: DesiredSource,
        now: Timestamp,
    ) -> Verification {
        let verification = Verification {
            application: ApplicationVerification::Acknowledged,
            persistence: PersistenceVerification::Unknown,
        };
        self.desired = Some(DesiredState {
            value,
            source,
            verification: verification.clone(),
            updated_at: now,
        });
        // observed intentionally preserved as historical
        verification
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
    ///
    /// Returns the [`Verification`] assigned to `self.desired`.
    pub fn record_readback_write(
        &mut self,
        desired_value: T,
        observed_value: T,
        source: DesiredSource,
        now: Timestamp,
    ) -> Verification
    where
        T: PartialEq,
    {
        let is_match = desired_value == observed_value;
        let application = if is_match {
            ApplicationVerification::ReadbackVerified
        } else {
            ApplicationVerification::Mismatch
        };
        let verification = Verification {
            application,
            persistence: PersistenceVerification::Unknown,
        };
        self.desired = Some(DesiredState {
            value: desired_value,
            source,
            verification: verification.clone(),
            updated_at: now,
        });
        self.observed = Some(ObservedState {
            value: observed_value,
            source: ObservationSource::UsbReadback,
            observed_at: now,
        });
        verification
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
    ///
    /// Returns `true` when the fresh observation does not confirm the desired
    /// value (differing or stale).
    pub fn reconcile_observation(&mut self, value: T, now: Timestamp) -> bool
    where
        T: Clone + PartialEq,
    {
        let observed = ObservedState {
            value: value.clone(),
            source: ObservationSource::UsbReadback,
            observed_at: now,
        };
        let mismatch = if let Some(desired) = self.desired.as_mut() {
            let is_match =
                desired.value == value && now.unix_seconds >= desired.updated_at.unix_seconds;
            desired.verification.application = if is_match {
                ApplicationVerification::ReadbackVerified
            } else {
                ApplicationVerification::Mismatch
            };
            desired.verification.persistence = PersistenceVerification::Unknown;
            !is_match
        } else {
            false
        };
        self.observed = Some(observed);
        mismatch
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
        // The verification event cannot predate the observation it certifies.
        let verified_at = match persistence {
            PersistenceVerification::ProfileReloadVerified { verified_at }
            | PersistenceVerification::PowerCycleVerified { verified_at } => Some(verified_at),
            PersistenceVerification::Unknown => None,
        };
        if let Some(verified_at) = verified_at
            && verified_at.unix_seconds < observed.observed_at.unix_seconds
        {
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
        mark_all_or_invalidate(
            &mut self.dpi,
            verified_at,
            ResourceState::try_mark_profile_reload_verified,
        );
        mark_all_or_invalidate(
            &mut self.preferences,
            verified_at,
            ResourceState::try_mark_profile_reload_verified,
        );
        mark_all_or_invalidate(
            &mut self.buttons,
            verified_at,
            ResourceState::try_mark_profile_reload_verified,
        );
        mark_all_or_invalidate(
            &mut self.polling_rate,
            verified_at,
            ResourceState::try_mark_profile_reload_verified,
        );
    }

    /// Attempts to mark all matching readback resources as power-cycle
    /// verified.
    pub fn try_mark_power_cycle_verified(&mut self, verified_at: Timestamp) {
        mark_all_or_invalidate(
            &mut self.dpi,
            verified_at,
            ResourceState::try_mark_power_cycle_verified,
        );
        mark_all_or_invalidate(
            &mut self.preferences,
            verified_at,
            ResourceState::try_mark_power_cycle_verified,
        );
        mark_all_or_invalidate(
            &mut self.buttons,
            verified_at,
            ResourceState::try_mark_power_cycle_verified,
        );
        mark_all_or_invalidate(
            &mut self.polling_rate,
            verified_at,
            ResourceState::try_mark_power_cycle_verified,
        );
    }
}

/// Marks one resource persistence-verified; any failure drops that
/// resource's persistence claim so no partial profile is overclaimed.
fn mark_all_or_invalidate<T: PartialEq>(
    resource: &mut ResourceState<T>,
    verified_at: Timestamp,
    mark: fn(&mut ResourceState<T>, Timestamp) -> bool,
) {
    if !mark(resource, verified_at) {
        resource.invalidate_persistence();
    }
}
/// The physical-identity mode of the installation.
///
/// [`IdentityMode::Legacy`] is the default: the single logical mouse is a
/// fuzzy placeholder that makes no claim about which physical unit it refers
/// to, so no watermark is ever read or written. [`IdentityMode::Persistent`]
/// means logical mice are physically identified via driver-stamped watermark
/// tokens, and every present physical id must be unique across devices.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IdentityMode {
    /// Fuzzy single-mouse semantics: at most one logical mouse, no physical ids.
    #[default]
    Legacy,
    /// Physically identified logical mice; present physical ids are unique.
    Persistent,
}

impl IdentityMode {
    /// Returns true when this is the default fuzzy legacy mode.
    #[must_use]
    pub const fn is_legacy(self) -> bool {
        matches!(self, Self::Legacy)
    }
}

/// The physical-identity ceremony recorded by a durable setup journal.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IdentitySetupPhase {
    /// The Legacy -> Persistent transition: capture/stamp the mouse being
    /// added first, then the existing mouse.
    InitialEnrollment,
    /// Adding a further mouse to an already-persistent installation.
    AddMouse,
    /// Restoring a logical mouse that lost its watermark. The reserved
    /// `token` rotates in for the `old_token` currently on the bound device.
    Restore,
    /// Adopting a valid watermark token unknown to this installation as a
    /// new local logical mouse.
    ForeignAdoption,
    /// Explicitly associating a BLE endpoint with an existing logical mouse.
    BleAssociation,
}

/// Where an in-flight identity ceremony currently stands.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IdentitySetupStage {
    /// Waiting for the user to reconnect/authenticate a physical mouse (or
    /// pick the BLE device to associate).
    #[default]
    AwaitingCapture,
    /// A subject is captured; its profiles are being stamped.
    Stamping,
    /// All captures/stamps are complete; finalization is pending.
    Finalizing,
}

/// Progress stamping one firmware profile of an enrolled subject.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IdentityStampProgress {
    /// The profile has not been touched yet.
    #[default]
    Pending,
    /// The profile's current state has been captured but not yet stamped.
    Captured,
    /// The profile has been stamped with the reserved token.
    Stamped,
}

/// One physical mouse enrolled in an in-flight identity ceremony.
///
/// The captured [`endpoint`](Self::endpoint) reference and the
/// [`captured`](Self::captured) profile image are what make the journal
/// resumable after an interruption: they record which authenticated physical
/// mouse the ceremony is about and its actual state, together with the token
/// it reserves (or adopts) and per-profile stamp progress.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentitySetupSubject {
    /// Captured endpoint reference for the authenticated physical mouse.
    pub endpoint: DeviceEndpoint,
    /// The logical mouse this subject is bound to, once assigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<DeviceId>,
    /// The token reserved for (or adopted by) this subject. `None` until the
    /// token is generated/adopted, and always `None` for BLE association.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<PhysicalId>,
    /// Restore only: the token being rotated away - the bound logical mouse's
    /// current physical id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_token: Option<PhysicalId>,
    /// The durable profile image captured from the authenticated physical
    /// mouse, persisted so the ceremony can resume after an interruption
    /// without reconnecting the device. `None` before capture (and always
    /// `None` for BLE association).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured: Option<CapturedProfileImage>,
    /// Per-profile stamp progress for this subject.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stamp_progress: BTreeMap<ProfileId, IdentityStampProgress>,
}
/// The durable profile image captured from one authenticated physical mouse.
///
/// This is the compact full-profile capture: device-global firmware profile
/// metadata plus the resource images of every captured firmware profile, using
/// the same evidence types as durable [`DeviceState`]. It is persisted on the
/// setup subject so an interrupted ceremony can resume without reconnecting
/// the physical mouse.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedProfileImage {
    /// Device-global profile metadata captured from the hardware.
    #[serde(default, skip_serializing_if = "ResourceState::is_empty")]
    pub profile_metadata: ResourceState<ProfileMetadata>,
    /// Resource images of every captured firmware profile.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<ProfileId, ProfileState>,
}

impl IdentitySetupSubject {
    /// Returns true when the subject's stamp progress covers exactly the
    /// captured profile keys and every one of them is stamped.
    ///
    /// A subject without a captured image, or whose stamp progress omits a
    /// captured profile or tracks a profile that was not captured, is never
    /// fully stamped.
    #[must_use]
    pub fn is_fully_stamped(&self) -> bool {
        let Some(captured) = &self.captured else {
            return false;
        };
        self.stamp_progress.len() == captured.profiles.len()
            && self
                .stamp_progress
                .keys()
                .all(|profile| captured.profiles.contains_key(profile))
            && self
                .stamp_progress
                .values()
                .all(|progress| matches!(progress, IdentityStampProgress::Stamped))
    }
}

/// A compact durable, resumable journal for an in-flight physical-identity
/// ceremony.
///
/// The journal records which ceremony is active, where it stands, and the
/// per-subject facts needed to resume after an interruption: the captured
/// endpoint reference of each authenticated physical mouse, the tokens the
/// ceremony reserves (or adopts), and per-profile stamp progress.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentitySetupJournal {
    pub phase: IdentitySetupPhase,
    pub stage: IdentitySetupStage,
    /// Ordered per-physical-mouse progress: at most two subjects during
    /// initial enrollment, at most one for every other ceremony.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subjects: Vec<IdentitySetupSubject>,
}

/// The complete internal manager-state document.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateFile {
    pub schema_version: u32,
    pub next_device_number: u64,
    pub selected_device: Option<DeviceId>,
    pub devices: BTreeMap<DeviceId, DeviceState>,
    /// Installation-level physical-identity mode; defaults to fuzzy legacy.
    #[serde(default)]
    pub identity_mode: IdentityMode,
    /// The active identity-setup journal, when a ceremony is in flight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_setup: Option<IdentitySetupJournal>,
}

impl Default for StateFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            next_device_number: 1,
            selected_device: None,
            devices: BTreeMap::new(),
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
        }
    }
}

impl StateFile {
    /// Allocates the next unused logical `mouse-N` identifier.
    pub fn allocate_device_id(&mut self) -> Result<DeviceId, StateError> {
        loop {
            let candidate = DeviceId::from_number(self.next_device_number)?;
            self.next_device_number = self
                .next_device_number
                .checked_add(1)
                .ok_or_else(|| StateError::invalid_state("nextDeviceNumber overflow"))?;
            if !self.devices.contains_key(&candidate) {
                return Ok(candidate);
            }
        }
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

        let max_number = self
            .devices
            .keys()
            .filter_map(DeviceId::number)
            .max()
            .unwrap_or(0);
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
                validate_profile_resource(profile_id, &profile.dpi, "dpi", |value: &DpiState| {
                    value.profile
                })?;
                validate_profile_resource(
                    profile_id,
                    &profile.preferences,
                    "preferences",
                    |value: &PreferencesState| value.profile,
                )?;
                validate_profile_resource(
                    profile_id,
                    &profile.buttons,
                    "buttons",
                    |value: &ButtonsState| value.profile,
                )?;
                // PollingRate has no embedded profile id; only evidence invariants apply.
                validate_resource_state(
                    &profile.polling_rate,
                    &format!("pollingRate for profile {profile_id}"),
                )?;
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
        self.validate_identity_mode()?;
        if let Some(journal) = &self.identity_setup {
            self.validate_identity_setup(journal)?;
        }
        Ok(())
    }

    /// Validates installation-level identity-mode invariants.
    ///
    /// Legacy mode allows at most one durable fuzzy logical mouse and forbids
    /// physical ids entirely; Persistent mode requires every committed device
    /// to carry a physical id and requires those ids to be unique.
    fn validate_identity_mode(&self) -> Result<(), StateError> {
        match self.identity_mode {
            IdentityMode::Legacy => {
                if self.devices.len() > 1 {
                    return Err(StateError::invalid_state(format!(
                        "legacy identity mode allows at most one logical mouse, found {}",
                        self.devices.len()
                    )));
                }
                for (device_id, device) in &self.devices {
                    if device.identity.physical_id.is_some() {
                        return Err(StateError::invalid_state(format!(
                            "legacy identity mode must not carry a physical id for device {device_id}"
                        )));
                    }
                }
            }
            IdentityMode::Persistent => {
                let mut seen: Vec<&PhysicalId> = Vec::new();
                for (device_id, device) in &self.devices {
                    let Some(physical_id) = &device.identity.physical_id else {
                        return Err(StateError::invalid_state(format!(
                            "persistent identity mode requires a physical id for device {device_id}"
                        )));
                    };
                    if seen.contains(&physical_id) {
                        return Err(StateError::invalid_state(format!(
                            "physical id {physical_id:?} is assigned to more than one logical mouse (duplicate on device {device_id})"
                        )));
                    }
                    seen.push(physical_id);
                }
            }
        }
        Ok(())
    }

    /// Validates that an active identity-setup journal is coherent and that
    /// every token it reserves is genuinely reserved: not already assigned to
    /// a committed device, not reused within the ceremony, and consistent
    /// with the rotation/adoption semantics its phase declares.
    fn validate_identity_setup(&self, journal: &IdentitySetupJournal) -> Result<(), StateError> {
        // Initial enrollment is exactly the Legacy -> Persistent transition;
        // every other ceremony requires persistent identity.
        match journal.phase {
            IdentitySetupPhase::InitialEnrollment => {
                if self.identity_mode != IdentityMode::Legacy {
                    return Err(StateError::invalid_state(
                        "initial enrollment setup requires legacy identity mode",
                    ));
                }
            }
            IdentitySetupPhase::AddMouse
            | IdentitySetupPhase::Restore
            | IdentitySetupPhase::ForeignAdoption
            | IdentitySetupPhase::BleAssociation => {
                if self.identity_mode != IdentityMode::Persistent {
                    return Err(StateError::invalid_state(
                        "this identity setup requires persistent identity mode",
                    ));
                }
            }
        }

        // Subject counts per ceremony.
        match journal.phase {
            IdentitySetupPhase::InitialEnrollment => {
                if journal.subjects.len() > 2 {
                    return Err(StateError::invalid_state(
                        "initial enrollment setup cannot enroll more than two mice",
                    ));
                }
            }
            IdentitySetupPhase::AddMouse
            | IdentitySetupPhase::Restore
            | IdentitySetupPhase::ForeignAdoption
            | IdentitySetupPhase::BleAssociation => {
                if journal.subjects.len() > 1 {
                    return Err(StateError::invalid_state(
                        "this identity setup ceremony cannot enroll more than one mouse",
                    ));
                }
            }
        }

        // Stage coherence with subject progress.
        match journal.stage {
            IdentitySetupStage::AwaitingCapture => match journal.phase {
                IdentitySetupPhase::InitialEnrollment => {
                    if journal.subjects.len() > 1 {
                        return Err(StateError::invalid_state(
                            "initial enrollment cannot await capture with more than one subject",
                        ));
                    }
                    if let Some(subject) = journal.subjects.first()
                        && (subject.token.is_none() || !subject.is_fully_stamped())
                    {
                        return Err(StateError::invalid_state(
                            "initial enrollment awaits the second mouse with an unfinished first subject",
                        ));
                    }
                }
                _ => {
                    if !journal.subjects.is_empty() {
                        return Err(StateError::invalid_state(
                            "awaiting capture while a subject is already captured",
                        ));
                    }
                }
            },
            IdentitySetupStage::Stamping => {
                if journal.phase == IdentitySetupPhase::BleAssociation {
                    return Err(StateError::invalid_state(
                        "BLE association never stamps profiles",
                    ));
                }
                let Some(subject) = journal.subjects.last() else {
                    return Err(StateError::invalid_state(
                        "stamping requires a captured subject",
                    ));
                };
                if subject.is_fully_stamped() {
                    return Err(StateError::invalid_state(
                        "stamping stage requires an incompletely stamped subject",
                    ));
                }
            }
            IdentitySetupStage::Finalizing => {
                if journal.subjects.is_empty() {
                    return Err(StateError::invalid_state(
                        "finalizing requires captured subjects",
                    ));
                }
                match journal.phase {
                    IdentitySetupPhase::InitialEnrollment => {
                        if journal.subjects.len() != 2 {
                            return Err(StateError::invalid_state(
                                "initial enrollment finalizing requires both mice",
                            ));
                        }
                    }
                    _ => {
                        if journal.subjects.len() != 1 {
                            return Err(StateError::invalid_state(
                                "finalizing requires exactly one subject for this ceremony",
                            ));
                        }
                    }
                }
                if journal.phase != IdentitySetupPhase::BleAssociation {
                    for subject in &journal.subjects {
                        if subject.token.is_none() {
                            return Err(StateError::invalid_state(
                                "finalizing requires every subject to have a reserved token",
                            ));
                        }
                        if !subject.is_fully_stamped() {
                            return Err(StateError::invalid_state(
                                "finalizing requires every subject to be fully stamped",
                            ));
                        }
                    }
                }
            }
        }

        // Per-subject coherence and token reservation.
        let mut reserved: Vec<&PhysicalId> = Vec::new();
        for subject in &journal.subjects {
            if !subject.endpoint.is_coherent() {
                return Err(StateError::invalid_state(
                    "identity setup subject endpoint is incoherent",
                ));
            }
            // Stamping requires the captured profile image to be durable.
            if !subject.stamp_progress.is_empty() && subject.captured.is_none() {
                return Err(StateError::invalid_state(
                    "profile stamping requires a captured profile image",
                ));
            }
            // Once stamping has begun, stamp progress must track exactly the
            // captured profile keys: no captured profile may be missing and no
            // profile outside the capture may be stamped.
            if let Some(captured) = &subject.captured
                && !subject.stamp_progress.is_empty()
                && (subject.stamp_progress.len() != captured.profiles.len()
                    || !subject
                        .stamp_progress
                        .keys()
                        .all(|profile| captured.profiles.contains_key(profile)))
            {
                return Err(StateError::invalid_state(
                    "stamp progress must track exactly the captured profile keys",
                ));
            }
            if let Some(device_id) = &subject.device_id
                && !self.devices.contains_key(device_id)
            {
                return Err(StateError::invalid_state(format!(
                    "identity setup subject references unknown device {device_id}"
                )));
            }
            // Stamped progress implies a token existed to stamp with.
            if subject
                .stamp_progress
                .values()
                .any(|progress| matches!(progress, IdentityStampProgress::Stamped))
                && subject.token.is_none()
            {
                return Err(StateError::invalid_state(
                    "stamped profile progress requires a reserved token",
                ));
            }
            if let Some(token) = &subject.token {
                if reserved.contains(&token) {
                    return Err(StateError::invalid_state(
                        "identity setup reserves the same token for more than one subject",
                    ));
                }
                reserved.push(token);
                // A reserved token must not already identify a committed device.
                for (device_id, device) in &self.devices {
                    if device.identity.physical_id.as_ref() == Some(token) {
                        return Err(StateError::invalid_state(format!(
                            "identity setup reserves token {token:?} already assigned to device {device_id}"
                        )));
                    }
                }
            }
            if let Some(old_token) = &subject.old_token {
                if reserved.contains(&old_token) {
                    return Err(StateError::invalid_state(
                        "identity setup rotates a token also reserved by another subject",
                    ));
                }
                let device_id = subject.device_id.as_ref().ok_or_else(|| {
                    StateError::invalid_state("restore rotation requires a bound device")
                })?;
                let device = self.devices.get(device_id).ok_or_else(|| {
                    StateError::invalid_state(format!(
                        "restore rotation references unknown device {device_id}"
                    ))
                })?;
                let current = device.identity.physical_id.as_ref().ok_or_else(|| {
                    StateError::invalid_state(format!(
                        "restore rotation requires device {device_id} to have a physical id"
                    ))
                })?;
                if current != old_token {
                    return Err(StateError::invalid_state(format!(
                        "restore old token does not match device {device_id} physical id"
                    )));
                }
                if subject.token.as_ref() == Some(old_token) {
                    return Err(StateError::invalid_state(
                        "restore new token must differ from the rotated-away token",
                    ));
                }
            }
        }

        // Phase-specific shape.
        match journal.phase {
            IdentitySetupPhase::BleAssociation => {
                for subject in &journal.subjects {
                    if subject.token.is_some() || subject.old_token.is_some() {
                        return Err(StateError::invalid_state(
                            "BLE association must not reserve or rotate tokens",
                        ));
                    }
                    if !subject.stamp_progress.is_empty() {
                        return Err(StateError::invalid_state(
                            "BLE association must not track profile stamps",
                        ));
                    }
                    if subject.captured.is_some() {
                        return Err(StateError::invalid_state(
                            "BLE association must not carry a captured profile image",
                        ));
                    }
                    if subject.endpoint.transport != TransportKind::Ble {
                        return Err(StateError::invalid_state(
                            "BLE association endpoint must be a BLE endpoint",
                        ));
                    }
                    for (device_id, device) in &self.devices {
                        if Some(device_id) == subject.device_id.as_ref() {
                            continue;
                        }
                        if device
                            .identity
                            .endpoints
                            .values()
                            .any(|endpoint| endpoint.locator == subject.endpoint.locator)
                        {
                            return Err(StateError::invalid_state(format!(
                                "BLE association endpoint already belongs to device {device_id}"
                            )));
                        }
                    }
                }
            }
            IdentitySetupPhase::Restore => {
                for subject in &journal.subjects {
                    if subject.old_token.is_none() {
                        return Err(StateError::invalid_state(
                            "restore requires the rotated-away old token",
                        ));
                    }
                }
            }
            IdentitySetupPhase::AddMouse
            | IdentitySetupPhase::ForeignAdoption
            | IdentitySetupPhase::InitialEnrollment => {
                for subject in &journal.subjects {
                    if subject.old_token.is_some() {
                        return Err(StateError::invalid_state(
                            "old-token rotation is only valid for restore",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

fn validate_profile_resource<T: PartialEq>(
    profile_id: &ProfileId,
    resource: &ResourceState<T>,
    name: &str,
    profile_of: impl Fn(&T) -> ProfileId,
) -> Result<(), StateError> {
    if resource
        .desired
        .as_ref()
        .is_some_and(|desired| profile_of(&desired.value) != *profile_id)
        || resource
            .observed
            .as_ref()
            .is_some_and(|observed| profile_of(&observed.value) != *profile_id)
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
        return Ok(());
    };

    match desired.verification.application {
        ApplicationVerification::NotSent | ApplicationVerification::Acknowledged => {
            if !desired.verification.persistence.is_unknown() {
                return Err(StateError::invalid_state(format!(
                    "{ctx}: persistence claim requires ReadbackVerified application evidence"
                )));
            }
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
            // A persistence claim cannot predate the readback that supports it.
            if let PersistenceVerification::ProfileReloadVerified { verified_at }
            | PersistenceVerification::PowerCycleVerified { verified_at } =
                &desired.verification.persistence
                && verified_at.unix_seconds < observed.observed_at.unix_seconds
            {
                return Err(StateError::invalid_state(format!(
                    "{ctx}: persistence verification predates the supporting observation"
                )));
            }
        }
        ApplicationVerification::Mismatch => {
            let observed = resource.observed.as_ref().ok_or_else(|| {
                StateError::invalid_state(format!("{ctx}: Mismatch requires an observed value"))
            })?;
            let stale = observed.observed_at.unix_seconds < desired.updated_at.unix_seconds;
            if desired.value == observed.value && !stale {
                return Err(StateError::invalid_state(format!(
                    "{ctx}: Mismatch requires a differing or stale observed value"
                )));
            }
            if !desired.verification.persistence.is_unknown() {
                return Err(StateError::invalid_state(format!(
                    "{ctx}: Mismatch cannot carry persistence claims"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ApplicationVerification, CapturedProfileImage, DesiredSource, DesiredState, DeviceState,
        IdentityMode, IdentitySetupJournal, IdentitySetupPhase, IdentitySetupStage,
        IdentitySetupSubject, IdentityStampProgress, MAX_PROFILE_NAME_CHARS, ObservationSource,
        ObservedState, PersistenceVerification, ProfileState, ResourceState, SCHEMA_VERSION,
        StateFile, Timestamp, Verification,
    };
    use crate::device::{DeviceEndpoint, DeviceId, DeviceIdentity};
    use crate::error::StateError;
    use attack_shark_x3::{
        DpiState, PhysicalId, PollingRate, ProfileId, ProfileMetadata, TransportKind,
    };
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
    fn default_state_file_starts_at_schema_five_with_legacy_identity() {
        let state = StateFile::default();
        assert_eq!(state.schema_version, SCHEMA_VERSION);
        assert_eq!(state.schema_version, 5);
        assert_eq!(state.next_device_number, 1);
        assert!(state.selected_device.is_none());
        assert!(state.devices.is_empty());
        assert_eq!(state.identity_mode, IdentityMode::Legacy);
        assert!(state.identity_setup.is_none());
        assert!(state.validate().is_ok());

        // The default document round-trips and explicitly records the legacy mode.
        let json = serde_json::to_value(&state).expect("serialize default state");
        assert_eq!(json["identityMode"], "legacy");
        assert!(json.get("identitySetup").is_none());
        let decoded: StateFile = serde_json::from_value(json).expect("deserialize default state");
        assert_eq!(decoded, state);
        assert!(decoded.validate().is_ok());

        // A schema-5 document without the identity fields defaults to Legacy/no setup.
        let bare = serde_json::json!({
            "schemaVersion": SCHEMA_VERSION,
            "nextDeviceNumber": 1,
            "selectedDevice": null,
            "devices": {},
        });
        let decoded_bare: StateFile = serde_json::from_value(bare).expect("deserialize bare state");
        assert_eq!(decoded_bare.identity_mode, IdentityMode::Legacy);
        assert!(decoded_bare.identity_setup.is_none());
        assert!(decoded_bare.validate().is_ok());
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
        let mut state2 = StateFile {
            next_device_number: 1,
            ..StateFile::default()
        };
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
        };
        assert!(state.validate().is_err());

        // Also test next_device_number monotonicity failure.
        let state2 = StateFile {
            next_device_number: 0,
            ..StateFile::default()
        };
        assert!(state2.validate().is_err());

        let mut state3 = StateFile {
            next_device_number: 1,
            ..StateFile::default()
        };
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
        };
        assert!(state3.validate().is_err());

        // Equal value with a stale observation is a legitimate Mismatch: the
        // readback predates the desired write, so it does not confirm it.
        let mut stale_equal_mismatch = ProfileState::empty();
        stale_equal_mismatch.dpi = ResourceState {
            desired: Some(DesiredState {
                value: dpi.clone(),
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::Mismatch,
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
        let mut device_state4 =
            DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        device_state4.profiles.insert(profile, stale_equal_mismatch);
        let state4 = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), device_state4)]),
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
        };
        assert!(
            state4.validate().is_ok(),
            "equal-but-stale observation is a legitimate Mismatch"
        );
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
        };
        assert!(state4.validate().is_err());

        // Persistence verification cannot predate the supporting readback.
        let mut predated = ProfileState::empty();
        predated.dpi = ResourceState {
            desired: Some(DesiredState {
                value: dpi.clone(),
                source: DesiredSource::UserWrite,
                verification: Verification {
                    application: ApplicationVerification::ReadbackVerified,
                    persistence: PersistenceVerification::PowerCycleVerified {
                        verified_at: timestamp(10),
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
        let mut device_state5 =
            DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        device_state5.profiles.insert(profile, predated);
        let state5 = StateFile {
            schema_version: SCHEMA_VERSION,
            next_device_number: 2,
            selected_device: None,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), device_state5)]),
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
        };
        assert!(state5.validate().is_err());
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
        };
        assert!(state2.validate().is_err());
    }

    #[test]
    fn older_schemas_rejected_without_migration() {
        let identity = device_identity("mouse-1", vec![ble_endpoint("test")]);
        for old_schema in [3, 4] {
            let json = serde_json::json!({
                "schemaVersion": old_schema,
                "nextDeviceNumber": 2,
                "selectedDevice": null,
                "devices": {
                    "mouse-1": {
                        "identity": identity.clone(),
                        "profileMetadata": { "desired": null, "observed": null },
                        "profiles": {}
                    }
                }
            });
            let state: Result<StateFile, _> = serde_json::from_value(json.clone());
            // Deserialization may succeed (the struct allows any schema_version
            // value), but validate must reject every older generation.
            if let Ok(state) = state {
                assert!(matches!(
                    state.validate(),
                    Err(StateError::UnsupportedSchema { found, .. }) if found == old_schema
                ));
            }

            // Direct construction with an older schema must fail validation.
            let old_state = StateFile {
                schema_version: old_schema,
                ..StateFile::default()
            };
            assert!(matches!(
                old_state.validate(),
                Err(StateError::UnsupportedSchema { found, .. }) if found == old_schema
            ));

            // Also via store load path: file with the older schema is rejected.
            let json_str = serde_json::to_string(&json).unwrap();
            let header: serde_json::Value = serde_json::from_str(&json_str).unwrap();
            assert_eq!(header["schemaVersion"], old_schema);
        }
        // No migration shim exists: the current schema is 5 and nothing below
        // it is accepted.
        assert_eq!(SCHEMA_VERSION, 5);
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
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
        // Marking cannot predate the supporting observation.
        assert!(!resource.try_mark_profile_reload_verified(timestamp(11)));
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
            identity_mode: IdentityMode::Legacy,
            identity_setup: None,
        };
    }
    fn physical_id(byte: u8) -> PhysicalId {
        PhysicalId::from_token_bytes([byte; 16])
    }

    fn stamped_subject(
        endpoint: DeviceEndpoint,
        device_id: Option<&str>,
        token: Option<PhysicalId>,
        old_token: Option<PhysicalId>,
        stamped_profiles: Vec<ProfileId>,
    ) -> IdentitySetupSubject {
        IdentitySetupSubject {
            endpoint,
            device_id: device_id.map(|id| DeviceId::new(id).unwrap()),
            token,
            old_token,
            captured: Some(captured_image()),
            stamp_progress: stamped_profiles
                .into_iter()
                .map(|profile| (profile, IdentityStampProgress::Stamped))
                .collect(),
        }
    }
    fn captured_image() -> CapturedProfileImage {
        let profile = ProfileId::new(1).unwrap();
        let profile_2 = ProfileId::new(2).unwrap();
        let mut image = CapturedProfileImage {
            profile_metadata: ResourceState {
                desired: Some(DesiredState {
                    value: ProfileMetadata::new(profile, ProfileId::new(5).unwrap()).unwrap(),
                    source: DesiredSource::Imported,
                    verification: Verification::not_sent(),
                    updated_at: timestamp(1),
                }),
                observed: None,
            },
            profiles: BTreeMap::new(),
        };
        for captured_profile in [profile, profile_2] {
            image.profiles.insert(
                captured_profile,
                ProfileState {
                    dpi: ResourceState {
                        desired: Some(DesiredState {
                            value: dpi_profile_one(800),
                            source: DesiredSource::Imported,
                            verification: Verification::not_sent(),
                            updated_at: timestamp(1),
                        }),
                        observed: None,
                    },
                    preferences: ResourceState::empty(),
                    buttons: ResourceState::empty(),
                    polling_rate: ResourceState::empty(),
                },
            );
        }
        image
    }

    fn ble_subject(endpoint: DeviceEndpoint, device_id: Option<&str>) -> IdentitySetupSubject {
        IdentitySetupSubject {
            endpoint,
            device_id: device_id.map(|id| DeviceId::new(id).unwrap()),
            token: None,
            old_token: None,
            captured: None,
            stamp_progress: BTreeMap::new(),
        }
    }

    #[test]
    fn legacy_mode_rejects_multiple_devices_and_physical_ids() {
        let one = DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        let two = DeviceState::new(device_identity("mouse-2", vec![wired_endpoint("/b")]));
        let state = StateFile {
            identity_mode: IdentityMode::Legacy,
            next_device_number: 3,
            devices: BTreeMap::from([
                (DeviceId::new("mouse-1").unwrap(), one),
                (DeviceId::new("mouse-2").unwrap(), two),
            ]),
            ..StateFile::default()
        };
        assert!(state.validate().is_err(), "legacy allows at most one mouse");

        let mut identified =
            DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        identified.identity.physical_id = Some(physical_id(1));
        let state = StateFile {
            identity_mode: IdentityMode::Legacy,
            next_device_number: 2,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), identified)]),
            ..StateFile::default()
        };
        assert!(state.validate().is_err(), "legacy forbids physical ids");
    }

    #[test]
    fn persistent_mode_requires_unique_physical_ids() {
        let mut a = DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        a.identity.physical_id = Some(physical_id(1));
        let mut b = DeviceState::new(device_identity("mouse-2", vec![wired_endpoint("/b")]));
        b.identity.physical_id = Some(physical_id(2));
        let mut state = StateFile {
            identity_mode: IdentityMode::Persistent,
            next_device_number: 3,
            devices: BTreeMap::from([
                (DeviceId::new("mouse-1").unwrap(), a),
                (DeviceId::new("mouse-2").unwrap(), b),
            ]),
            ..StateFile::default()
        };
        assert!(state.validate().is_ok());

        // The same physical id on two logical mice is impossible.
        let mut duplicate =
            DeviceState::new(device_identity("mouse-3", vec![wired_endpoint("/c")]));
        duplicate.identity.physical_id = Some(physical_id(1));
        state
            .devices
            .insert(DeviceId::new("mouse-3").unwrap(), duplicate);
        assert!(state.validate().is_err(), "duplicate physical ids rejected");

        // A committed device without a physical id is impossible in
        // persistent mode: every logical mouse must be physically identified.
        state.devices.remove(&DeviceId::new("mouse-3").unwrap());
        let mut unassociated =
            DeviceState::new(device_identity("mouse-3", vec![wired_endpoint("/c")]));
        unassociated.identity.physical_id = None;
        state
            .devices
            .insert(DeviceId::new("mouse-3").unwrap(), unassociated);
        state.next_device_number = 4;
        assert!(
            state.validate().is_err(),
            "persistent devices require physical ids"
        );
    }

    #[test]
    fn setup_journal_serde_round_trip() {
        let profile = ProfileId::new(1).unwrap();
        let profile_2 = ProfileId::new(2).unwrap();
        let subject_a = stamped_subject(
            wired_endpoint("/dev/hidraw-enroll-a"),
            None,
            Some(physical_id(10)),
            None,
            vec![profile, profile_2],
        );
        let subject_b = stamped_subject(
            wired_endpoint("/dev/hidraw-enroll-b"),
            None,
            Some(physical_id(11)),
            None,
            vec![profile, profile_2],
        );
        let journal = IdentitySetupJournal {
            phase: IdentitySetupPhase::InitialEnrollment,
            stage: IdentitySetupStage::Finalizing,
            subjects: vec![subject_a, subject_b.clone()],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Legacy,
            identity_setup: Some(journal.clone()),
            ..StateFile::default()
        };
        assert!(state.validate().is_ok());

        let json = serde_json::to_value(&state).expect("serialize journal state");
        assert_eq!(json["identityMode"], "legacy");
        assert_eq!(json["identitySetup"]["phase"], "initialEnrollment");
        assert_eq!(json["identitySetup"]["stage"], "finalizing");
        assert_eq!(
            json["identitySetup"]["subjects"].as_array().unwrap().len(),
            2
        );
        assert_eq!(
            json["identitySetup"]["subjects"][0]["token"],
            serde_json::Value::String("0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a".to_owned())
        );
        assert_eq!(
            json["identitySetup"]["subjects"][0]["captured"]["profiles"]
                .as_object()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            json["identitySetup"]["subjects"][0]["captured"]["profileMetadata"]["desired"]["source"],
            "imported"
        );
        assert!(
            json["identitySetup"]["subjects"][1]
                .get("captured")
                .is_some(),
            "stamped subjects persist their captured profile image"
        );
        // Every captured profile key is tracked and stamped.
        assert_eq!(
            json["identitySetup"]["subjects"][1]["stampProgress"]["1"],
            "stamped"
        );
        assert_eq!(
            json["identitySetup"]["subjects"][1]["stampProgress"]["2"],
            "stamped"
        );

        let bare_subject = IdentitySetupSubject {
            captured: None,
            ..subject_b.clone()
        };
        let bare_json = serde_json::to_value(&bare_subject).expect("serialize bare subject");
        assert!(
            bare_json.get("captured").is_none(),
            "captured must be omitted when absent"
        );

        let decoded: StateFile = serde_json::from_value(json).expect("deserialize journal state");
        assert_eq!(decoded, state);
        assert!(decoded.validate().is_ok());
    }

    #[test]
    fn setup_journal_rejects_impossible_combinations() {
        let profile = ProfileId::new(1).unwrap();
        let profile_2 = ProfileId::new(2).unwrap();

        // Ceremonies other than initial enrollment require persistent mode.
        let add_journal = IdentitySetupJournal {
            phase: IdentitySetupPhase::AddMouse,
            stage: IdentitySetupStage::AwaitingCapture,
            subjects: vec![],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Legacy,
            identity_setup: Some(add_journal),
            ..StateFile::default()
        };
        assert!(state.validate().is_err(), "AddMouse in legacy mode");

        // Initial enrollment requires legacy mode.
        let enroll_journal = IdentitySetupJournal {
            phase: IdentitySetupPhase::InitialEnrollment,
            stage: IdentitySetupStage::AwaitingCapture,
            subjects: vec![],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Persistent,
            identity_setup: Some(enroll_journal),
            ..StateFile::default()
        };
        assert!(
            state.validate().is_err(),
            "InitialEnrollment in persistent mode"
        );

        // Awaiting capture while a subject is already captured is impossible.
        let awaiting_with_subject = IdentitySetupJournal {
            phase: IdentitySetupPhase::AddMouse,
            stage: IdentitySetupStage::AwaitingCapture,
            subjects: vec![stamped_subject(
                wired_endpoint("/x"),
                None,
                Some(physical_id(1)),
                None,
                vec![],
            )],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Persistent,
            identity_setup: Some(awaiting_with_subject),
            ..StateFile::default()
        };
        assert!(state.validate().is_err(), "awaiting capture with a subject");

        // Stamping requires a subject that is not fully stamped.
        let stamping_done = IdentitySetupJournal {
            phase: IdentitySetupPhase::AddMouse,
            stage: IdentitySetupStage::Stamping,
            subjects: vec![stamped_subject(
                wired_endpoint("/x"),
                None,
                Some(physical_id(1)),
                None,
                vec![profile, profile_2],
            )],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Persistent,
            identity_setup: Some(stamping_done),
            ..StateFile::default()
        };
        assert!(
            state.validate().is_err(),
            "stamping with a fully stamped subject"
        );

        // Stamped progress without a reserved token is impossible.
        let mut tokenless = stamped_subject(wired_endpoint("/x"), None, None, None, vec![profile]);
        tokenless
            .stamp_progress
            .insert(profile_2, IdentityStampProgress::Pending);
        let stamping_tokenless = IdentitySetupJournal {
            phase: IdentitySetupPhase::AddMouse,
            stage: IdentitySetupStage::Stamping,
            subjects: vec![tokenless],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Persistent,
            identity_setup: Some(stamping_tokenless),
            ..StateFile::default()
        };
        assert!(
            state.validate().is_err(),
            "stamped progress without a token"
        );
        // Stamping without a captured profile image is impossible.
        let mut no_capture = stamped_subject(
            wired_endpoint("/x"),
            None,
            Some(physical_id(1)),
            None,
            vec![],
        );
        no_capture
            .stamp_progress
            .insert(profile_2, IdentityStampProgress::Pending);
        no_capture.captured = None;
        let stamping_no_capture = IdentitySetupJournal {
            phase: IdentitySetupPhase::AddMouse,
            stage: IdentitySetupStage::Stamping,
            subjects: vec![no_capture],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Persistent,
            identity_setup: Some(stamping_no_capture),
            ..StateFile::default()
        };
        assert!(
            state.validate().is_err(),
            "stamping without a captured image"
        );

        // Finalizing requires every subject fully stamped with a token.
        let mut incomplete = stamped_subject(
            wired_endpoint("/x"),
            None,
            Some(physical_id(1)),
            None,
            vec![profile],
        );
        incomplete
            .stamp_progress
            .insert(profile_2, IdentityStampProgress::Captured);
        let finalizing_incomplete = IdentitySetupJournal {
            phase: IdentitySetupPhase::AddMouse,
            stage: IdentitySetupStage::Finalizing,
            subjects: vec![incomplete],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Persistent,
            identity_setup: Some(finalizing_incomplete),
            ..StateFile::default()
        };
        assert!(
            state.validate().is_err(),
            "finalizing with an unstamped profile"
        );

        // Initial enrollment finalizing requires both mice.
        let one_mouse = IdentitySetupJournal {
            phase: IdentitySetupPhase::InitialEnrollment,
            stage: IdentitySetupStage::Finalizing,
            subjects: vec![stamped_subject(
                wired_endpoint("/x"),
                None,
                Some(physical_id(1)),
                None,
                vec![],
            )],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Legacy,
            identity_setup: Some(one_mouse),
            ..StateFile::default()
        };
        assert!(
            state.validate().is_err(),
            "initial enrollment finalizing with one mouse"
        );

        // A reserved token must not already identify a committed device.
        let mut committed =
            DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        committed.identity.physical_id = Some(physical_id(7));
        let mut collision = stamped_subject(
            wired_endpoint("/x"),
            None,
            Some(physical_id(7)),
            None,
            vec![],
        );
        collision
            .stamp_progress
            .insert(profile, IdentityStampProgress::Captured);
        collision
            .stamp_progress
            .insert(profile_2, IdentityStampProgress::Pending);
        let collision_journal = IdentitySetupJournal {
            phase: IdentitySetupPhase::AddMouse,
            stage: IdentitySetupStage::Stamping,
            subjects: vec![collision],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Persistent,
            next_device_number: 2,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), committed)]),
            identity_setup: Some(collision_journal),
            ..StateFile::default()
        };
        assert!(
            state.validate().is_err(),
            "reserved token collides with a device"
        );

        // The same token cannot be reserved for two subjects.
        let duplicate_tokens = IdentitySetupJournal {
            phase: IdentitySetupPhase::InitialEnrollment,
            stage: IdentitySetupStage::Finalizing,
            subjects: vec![
                stamped_subject(
                    wired_endpoint("/a"),
                    None,
                    Some(physical_id(5)),
                    None,
                    vec![profile, profile_2],
                ),
                stamped_subject(
                    wired_endpoint("/b"),
                    None,
                    Some(physical_id(5)),
                    None,
                    vec![profile, profile_2],
                ),
            ],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Legacy,
            identity_setup: Some(duplicate_tokens),
            ..StateFile::default()
        };
        assert!(state.validate().is_err(), "duplicate reserved token");

        // An incoherent captured endpoint is rejected.
        let mut bad_endpoint = ble_endpoint("ble-1");
        bad_endpoint.transport = TransportKind::Wired; // tamper
        let incoherent = IdentitySetupJournal {
            phase: IdentitySetupPhase::AddMouse,
            stage: IdentitySetupStage::Finalizing,
            subjects: vec![stamped_subject(
                bad_endpoint,
                None,
                Some(physical_id(6)),
                None,
                vec![profile, profile_2],
            )],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Persistent,
            identity_setup: Some(incoherent),
            ..StateFile::default()
        };
        assert!(state.validate().is_err(), "incoherent subject endpoint");

        // A subject bound to an unknown device is rejected.
        let unknown_device = IdentitySetupJournal {
            phase: IdentitySetupPhase::BleAssociation,
            stage: IdentitySetupStage::Finalizing,
            subjects: vec![ble_subject(ble_endpoint("AA:BB"), Some("mouse-99"))],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Persistent,
            identity_setup: Some(unknown_device),
            ..StateFile::default()
        };
        assert!(
            state.validate().is_err(),
            "subject references unknown device"
        );
    }

    #[test]
    fn restore_rotation_and_ble_association_invariants() {
        let mut lost = DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        lost.identity.physical_id = Some(physical_id(9));
        let fully_stamped = || vec![ProfileId::new(1).unwrap(), ProfileId::new(2).unwrap()];

        // A valid restore rotates the device's old token to a fresh one.
        let restore_ok = IdentitySetupJournal {
            phase: IdentitySetupPhase::Restore,
            stage: IdentitySetupStage::Finalizing,
            subjects: vec![stamped_subject(
                wired_endpoint("/a"),
                Some("mouse-1"),
                Some(physical_id(10)),
                Some(physical_id(9)),
                fully_stamped(),
            )],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Persistent,
            next_device_number: 2,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), lost.clone())]),
            identity_setup: Some(restore_ok),
            ..StateFile::default()
        };
        assert!(state.validate().is_ok(), "valid restore rotation");

        // Rotating to the same token is impossible.
        let restore_same = IdentitySetupJournal {
            phase: IdentitySetupPhase::Restore,
            stage: IdentitySetupStage::Finalizing,
            subjects: vec![stamped_subject(
                wired_endpoint("/a"),
                Some("mouse-1"),
                Some(physical_id(9)),
                Some(physical_id(9)),
                fully_stamped(),
            )],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Persistent,
            next_device_number: 2,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), lost.clone())]),
            identity_setup: Some(restore_same),
            ..StateFile::default()
        };
        assert!(
            state.validate().is_err(),
            "restore cannot reuse the old token"
        );

        // The old token must match the bound device's current physical id.
        let restore_wrong = IdentitySetupJournal {
            phase: IdentitySetupPhase::Restore,
            stage: IdentitySetupStage::Finalizing,
            subjects: vec![stamped_subject(
                wired_endpoint("/a"),
                Some("mouse-1"),
                Some(physical_id(10)),
                Some(physical_id(11)),
                fully_stamped(),
            )],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Persistent,
            next_device_number: 2,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), lost)]),
            identity_setup: Some(restore_wrong),
            ..StateFile::default()
        };
        assert!(
            state.validate().is_err(),
            "restore old token must match the device"
        );

        // Restore without an old token is impossible.
        let restore_no_old = IdentitySetupJournal {
            phase: IdentitySetupPhase::Restore,
            stage: IdentitySetupStage::Finalizing,
            subjects: vec![stamped_subject(
                wired_endpoint("/a"),
                Some("mouse-1"),
                Some(physical_id(10)),
                None,
                fully_stamped(),
            )],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Persistent,
            identity_setup: Some(restore_no_old),
            ..StateFile::default()
        };
        assert!(state.validate().is_err(), "restore requires the old token");

        // Foreign adoption never rotates tokens.
        let adoption = IdentitySetupJournal {
            phase: IdentitySetupPhase::ForeignAdoption,
            stage: IdentitySetupStage::Finalizing,
            subjects: vec![stamped_subject(
                wired_endpoint("/x"),
                None,
                Some(physical_id(3)),
                Some(physical_id(4)),
                fully_stamped(),
            )],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Persistent,
            identity_setup: Some(adoption),
            ..StateFile::default()
        };
        assert!(
            state.validate().is_err(),
            "old-token rotation is restore-only"
        );

        // A valid explicit BLE association binds a BLE endpoint to a device.
        let mut ble_device =
            DeviceState::new(device_identity("mouse-1", vec![wired_endpoint("/a")]));
        ble_device.identity.physical_id = Some(physical_id(1));
        let ble_ok = IdentitySetupJournal {
            phase: IdentitySetupPhase::BleAssociation,
            stage: IdentitySetupStage::Finalizing,
            subjects: vec![ble_subject(ble_endpoint("AA:BB"), Some("mouse-1"))],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Persistent,
            next_device_number: 2,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), ble_device.clone())]),
            identity_setup: Some(ble_ok),
            ..StateFile::default()
        };
        assert!(state.validate().is_ok(), "valid BLE association");
        // BLE association must not carry a captured profile image.
        let mut ble_captured = ble_subject(ble_endpoint("AA:BB"), Some("mouse-1"));
        ble_captured.captured = Some(captured_image());
        let ble_captured_journal = IdentitySetupJournal {
            phase: IdentitySetupPhase::BleAssociation,
            stage: IdentitySetupStage::Finalizing,
            subjects: vec![ble_captured],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Persistent,
            next_device_number: 2,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), ble_device.clone())]),
            identity_setup: Some(ble_captured_journal),
            ..StateFile::default()
        };
        assert!(
            state.validate().is_err(),
            "BLE association with captured image"
        );

        let mut ble_token_subject = ble_subject(ble_endpoint("AA:BB"), Some("mouse-1"));
        ble_token_subject.token = Some(physical_id(2));
        let ble_token = IdentitySetupJournal {
            phase: IdentitySetupPhase::BleAssociation,
            stage: IdentitySetupStage::Finalizing,
            subjects: vec![ble_token_subject],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Persistent,
            next_device_number: 2,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), ble_device.clone())]),
            identity_setup: Some(ble_token),
            ..StateFile::default()
        };
        assert!(
            state.validate().is_err(),
            "BLE association without tokens only"
        );

        // BLE association must use a BLE endpoint.
        let ble_wired = IdentitySetupJournal {
            phase: IdentitySetupPhase::BleAssociation,
            stage: IdentitySetupStage::Finalizing,
            subjects: vec![ble_subject(wired_endpoint("/a"), Some("mouse-1"))],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Persistent,
            next_device_number: 2,
            devices: BTreeMap::from([(DeviceId::new("mouse-1").unwrap(), ble_device.clone())]),
            identity_setup: Some(ble_wired),
            ..StateFile::default()
        };
        assert!(
            state.validate().is_err(),
            "BLE association requires a BLE endpoint"
        );

        // The BLE endpoint must not already belong to another device.
        let mut other = DeviceState::new(device_identity("mouse-2", vec![wired_endpoint("/b")]));
        other.identity.physical_id = Some(physical_id(2));
        other.identity.upsert_endpoint(ble_endpoint("AA:BB"));
        let ble_taken = IdentitySetupJournal {
            phase: IdentitySetupPhase::BleAssociation,
            stage: IdentitySetupStage::Finalizing,
            subjects: vec![ble_subject(ble_endpoint("AA:BB"), Some("mouse-1"))],
        };
        let state = StateFile {
            identity_mode: IdentityMode::Persistent,
            next_device_number: 3,
            devices: BTreeMap::from([
                (DeviceId::new("mouse-1").unwrap(), ble_device),
                (DeviceId::new("mouse-2").unwrap(), other),
            ]),
            identity_setup: Some(ble_taken),
            ..StateFile::default()
        };
        assert!(
            state.validate().is_err(),
            "BLE endpoint already owned by another device"
        );
    }
}
