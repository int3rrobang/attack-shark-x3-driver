#![forbid(unsafe_code)]

#[cfg(any(feature = "usb", feature = "ble"))]
pub mod driver;
pub mod error;
pub mod model;
pub mod protocol;

#[cfg(feature = "usb")]
pub use driver::{
    DeviceInfo, DeviceSelector, DriverError, MouseHandle, ProfileSnapshot, ReadFailure, ReadPolicy,
    UsbDeviceKind, list_devices, list_devices_for,
};

#[cfg(feature = "ble")]
pub use driver::ble::{
    BleAck, BleDeviceId, BleDeviceInfo, BleError, BleHandle, BlePolicy, BleReport, BleSelector,
    BleWriteReceipt, decode_ack, fee0_service, fee3_write, fee4_ack,
};
pub use error::ProtocolError;
pub use model::{DpiValue, ProfileId, StageIndex, TransportKind};
pub use protocol::buttons::{
    ButtonActionError, ButtonAssignment, ButtonsReport, ButtonsState, HidKeyboardUsage,
    KeyboardModifiers, X3ButtonAction,
};
pub use protocol::dpi::{
    DecodedDpiReport, DpiFraming, DpiReport, DpiState, LiftOffDistance, PhysicalId, SensorOptions,
    WATERMARK_LENGTH, WatermarkDecode, decode_watermark,
};
pub use protocol::input::{
    BATTERY_REPORT_LENGTH, BATTERY_REPORT_PREFIX, BATTERY_REPORT_PREFIX_X3, BatteryEvent,
    ConnectionChangedEvent, DPI_BUTTON_REPORT_LENGTH, DPI_BUTTON_REPORT_PREFIX,
    DPI_BUTTON_REPORT_TRAILING_BYTE, DpiButtonEvent, DpiIndex, DpiIndexChangedEvent, InputEvent,
    LedModeChangedEvent, ProfileChangedEvent, decode_battery_report, decode_dpi_button_report,
    decode_input_report,
};
pub use protocol::polling_rate::{DecodedPollingRateReport, PollingRate, PollingRateReport};
pub use protocol::preferences::{
    DebounceMs, DecodedPreferencesReport, DeepSleepMinutes, PreferencesFraming, PreferencesReport,
    PreferencesState, SleepTimer,
};
pub use protocol::profile::{
    ProfileControlFraming, ProfileControlReport, ProfileMetadata, ProfileMetadataReport,
    ReadSelector, ReadbackRequest, ReadinessStatus,
};
