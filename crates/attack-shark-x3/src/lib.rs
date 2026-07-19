#![forbid(unsafe_code)]

#[cfg(feature = "usb")]
pub mod driver;
pub mod error;
pub mod model;
pub mod protocol;

#[cfg(feature = "usb")]
pub use driver::{
    DeviceInfo, DeviceSelector, DriverError, MouseHandle, ProfileSnapshot, ReadFailure, ReadPolicy,
    list_devices,
};
pub use error::ProtocolError;
pub use model::{DpiValue, ProfileId, StageIndex, TransportKind};
pub use protocol::buttons::{ButtonAssignment, ButtonsReport, ButtonsState, DecodedButtonsReport};
pub use protocol::dpi::{DecodedDpiReport, DpiFraming, DpiReport, DpiState, SensorOptions};
pub use protocol::preferences::{
    DecodedPreferencesReport, PreferencesFraming, PreferencesReport, PreferencesState,
};
pub use protocol::profile::{
    ProfileControlFraming, ProfileControlReport, ProfileMetadata, ProfileMetadataReport,
    ReadSelector, ReadbackRequest, ReadinessStatus,
};
