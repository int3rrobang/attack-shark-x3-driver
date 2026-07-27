#![forbid(unsafe_code)]

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
pub(crate) mod resources;
pub mod state;
#[cfg(any(feature = "usb", feature = "ble"))]
pub(crate) mod verification;

pub use attack_shark_x3::{
    BatteryEvent, ButtonAssignment, ButtonsState, DpiButtonEvent, DpiFraming, DpiState, DpiValue,
    LiftOffDistance, PollingRate, PreferencesFraming, PreferencesState, ProfileControlFraming,
    ProfileId, ProfileMetadata, ReadbackRequest, SensorOptions, StageIndex, TransportKind,
};
pub use device::*;
pub use error::{ManagerError, StateError};
#[cfg(any(feature = "usb", feature = "ble"))]
pub use manager::DeviceManager;
pub use offline_debug::*;
pub use operation::*;
#[cfg(any(feature = "usb", feature = "ble"))]
pub use resources::buttons::{ButtonSlotDelta, SafeButtonAction, SafeButtonSlot};
#[cfg(any(feature = "usb", feature = "ble"))]
pub use resources::dpi::{DpiDelta, SensorOptionsDelta};
#[cfg(any(feature = "usb", feature = "ble"))]
pub use resources::settings::PreferencesDelta;
#[cfg(any(feature = "usb", feature = "ble"))]
pub use resources::state::{ConfigurationExport, ProfileConfiguration};
pub use state::{
    ApplicationVerification, DesiredSource, DesiredState, DeviceState, ObservationSource,
    ObservedState, PersistenceVerification, ProfileState, ResourceState, SCHEMA_VERSION, StateFile,
    StatePaths, StateStore, StateTransaction, Timestamp, Verification,
};
