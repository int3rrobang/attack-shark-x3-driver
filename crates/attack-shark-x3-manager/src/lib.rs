#![forbid(unsafe_code)]

//! Attack Shark X3 manager: logical `mouse-N` device identities, multi-transport
//! endpoints, schema 4 durable state with `nextDeviceNumber`, strengthened
//! evidence invariants, and per-device operation locks separate from `state.lock`.

#[cfg(any(feature = "usb", feature = "ble"))]
mod backend;
pub mod device;
pub mod error;
#[cfg(any(feature = "usb", feature = "ble"))]
pub(crate) mod events;
#[cfg(any(feature = "usb", feature = "ble"))]
pub mod manager;
pub mod offline_debug;
pub mod operation;
#[cfg(any(feature = "usb", feature = "ble"))]
pub(crate) mod refresh;
#[cfg(any(feature = "usb", feature = "ble"))]
pub(crate) mod resources;
pub mod state;
#[cfg(any(feature = "usb", feature = "ble"))]
pub(crate) mod verification;

pub use attack_shark_x3::{
    BatteryEvent, ButtonActionError, ButtonAssignment, ButtonsState, ConnectionChangedEvent,
    DpiButtonEvent, DpiFraming, DpiIndex, DpiIndexChangedEvent, DpiState, DpiValue,
    HidKeyboardUsage, InputEvent, KeyboardModifiers, LedModeChangedEvent, LiftOffDistance,
    PollingRate, PreferencesFraming, PreferencesState, ProfileChangedEvent, ProfileControlFraming,
    ProfileId, ProfileMetadata, ReadbackRequest, SensorOptions, StageIndex, TransportKind,
    X3ButtonAction,
};
pub use device::{DeviceEndpoint, DeviceId, DeviceIdentity, DeviceLocator, TransportSelection};
pub use error::{ManagerError, StateError};
#[cfg(any(feature = "usb", feature = "ble"))]
pub use events::EventSubscriptions;
#[cfg(any(feature = "usb", feature = "ble"))]
pub use manager::DeviceManager;
pub use offline_debug::{
    OfflinePacket, OfflinePacketFraming, OfflinePacketKind, debug_buttons, debug_dpi, debug_prefs,
    encode_debug_buttons, encode_debug_dpi, encode_debug_prefs,
};
#[cfg(any(feature = "usb", feature = "ble"))]
pub use operation::ProfileUpdate;
pub use operation::{
    BaselineSource, DeviceEvent, DeviceStatus, DiscoveredDevice, DiscoveredEndpoint,
    FullProfileRefreshOutcome, PowerCycleVerificationOutcome, ProfileResourceKind,
    ProfileUpdateOutcome, ProfileVerificationOutcome, RefreshedProfile, ResourceSnapshot,
    UpdatePolicy, VerificationMethod, WriteOutcome,
};
#[cfg(any(feature = "usb", feature = "ble"))]
pub use resources::buttons::{ButtonSlotDelta, SafeButtonAction, SafeButtonSlot};
#[cfg(any(feature = "usb", feature = "ble"))]
pub use resources::dpi::{DpiDelta, SensorOptionsDelta};
#[cfg(any(feature = "usb", feature = "ble"))]
pub use resources::settings::PreferencesDelta;
#[cfg(any(feature = "usb", feature = "ble"))]
pub use resources::state::{ConfigurationExport, ProfileConfiguration};
pub use state::{
    ApplicationVerification, DesiredSource, DesiredState, DeviceOperationGuard, DeviceState,
    MAX_PROFILE_NAME_CHARS, ObservationSource, ObservedState, PersistenceVerification,
    ProfileState, ResourceState, SCHEMA_VERSION, StateFile, StatePaths, StateReset, StateStore,
    StateTransaction, Timestamp, Verification,
};
