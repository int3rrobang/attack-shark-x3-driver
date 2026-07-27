#![cfg_attr(not(feature = "usb"), allow(dead_code, unused_imports))]

#[cfg(feature = "usb")]
mod usb;

#[cfg(feature = "ble")]
pub mod ble;

mod handle;
mod worker;

#[cfg(feature = "usb")]
pub use usb::{DeviceInfo, DeviceSelector, UsbDeviceKind, list_devices, list_devices_for};

pub use handle::{DriverError, MouseHandle, ProfileSnapshot, ReadFailure, ReadPolicy};

pub(crate) use worker::{FeatureTransport, InputTransport};
