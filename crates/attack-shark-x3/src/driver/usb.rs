use hidapi::{DeviceInfo as HidDeviceInfo, HidApi, HidDevice};

use super::{DriverError, FeatureTransport};

const ATTACK_SHARK_VENDOR_ID: u16 = 0x1d57;
const FA61_PRODUCT_ID: u16 = 0xfa61;
const CONFIG_INTERFACE_NUMBER: i32 = 2;
const WINDOWS_CONFIG_COLLECTION: &str = "col04";

/// A discovered FA61 configuration collection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceInfo {
    pub path: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub interface_number: i32,
    pub product: Option<String>,
    pub serial_number: Option<String>,
}

/// How native USB discovery must choose the configuration collection.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum DeviceSelector {
    /// Open only when exactly one FA61 configuration collection is present.
    #[default]
    Unique,
    /// Open the configuration collection with this exact enumerated path.
    Path(String),
}

impl DeviceSelector {
    #[must_use]
    pub fn path(path: impl Into<String>) -> Self {
        Self::Path(path.into())
    }
}

/// Lists FA61 configuration collections without opening hardware.
///
/// # Errors
///
/// Returns an error when HID enumeration fails.
pub fn list_devices() -> Result<Vec<DeviceInfo>, DriverError> {
    let api = HidApi::new().map_err(|error| DriverError::Transport(error.to_string()))?;
    Ok(configuration_collections(&api)
        .map(to_public_info)
        .collect())
}

pub(super) struct HidFeatureTransport {
    device: HidDevice,
}

impl FeatureTransport for HidFeatureTransport {
    fn send_feature_report(&mut self, report: &[u8]) -> Result<(), String> {
        self.device
            .send_feature_report(report)
            .map_err(|error| error.to_string())
    }

    fn get_feature_report(&mut self, report_id: u8, buffer: &mut [u8]) -> Result<usize, String> {
        if buffer.is_empty() {
            return Ok(0);
        }
        buffer.fill(0);
        buffer[0] = report_id;
        self.device
            .get_feature_report(buffer)
            .map_err(|error| error.to_string())
    }
}

pub(super) fn open_transport(
    selector: &DeviceSelector,
) -> Result<HidFeatureTransport, DriverError> {
    let api = HidApi::new().map_err(|error| DriverError::Transport(error.to_string()))?;
    let candidates: Vec<_> = configuration_collections(&api).collect();
    let paths: Vec<_> = candidates
        .iter()
        .map(|device| device.path().to_string_lossy().into_owned())
        .collect();
    let selected = candidates[select_candidate(selector, &paths)?];
    let device = selected
        .open_device(&api)
        .map_err(|error| DriverError::Transport(error.to_string()))?;
    Ok(HidFeatureTransport { device })
}

fn configuration_collections(api: &HidApi) -> impl Iterator<Item = &HidDeviceInfo> {
    api.device_list().filter(|device| {
        device.vendor_id() == ATTACK_SHARK_VENDOR_ID
            && device.product_id() == FA61_PRODUCT_ID
            && is_configuration_collection(device)
    })
}

fn is_configuration_collection(device: &HidDeviceInfo) -> bool {
    is_configuration_identity(
        &device.path().to_string_lossy(),
        device.interface_number(),
        cfg!(target_os = "windows"),
    )
}

fn is_configuration_identity(path: &str, interface_number: i32, windows: bool) -> bool {
    if interface_number != CONFIG_INTERFACE_NUMBER {
        return false;
    }
    !windows
        || path
            .to_ascii_lowercase()
            .contains(WINDOWS_CONFIG_COLLECTION)
}

fn select_candidate(
    selector: &DeviceSelector,
    candidate_paths: &[String],
) -> Result<usize, DriverError> {
    match selector {
        DeviceSelector::Unique => match candidate_paths.len() {
            0 => Err(DriverError::DeviceNotFound),
            1 => Ok(0),
            count => Err(DriverError::AmbiguousDevice { count }),
        },
        DeviceSelector::Path(expected_path) => candidate_paths
            .iter()
            .position(|path| path == expected_path)
            .ok_or(DriverError::DeviceNotFound),
    }
}

fn to_public_info(device: &HidDeviceInfo) -> DeviceInfo {
    DeviceInfo {
        path: device.path().to_string_lossy().into_owned(),
        vendor_id: device.vendor_id(),
        product_id: device.product_id(),
        interface_number: device.interface_number(),
        product: device.product_string().map(str::to_owned),
        serial_number: device.serial_number().map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::{DeviceSelector, DriverError, is_configuration_identity, select_candidate};

    #[test]
    fn windows_requires_the_confirmed_col04_collection() {
        assert!(is_configuration_identity(
            r"\\?\hid#vid_1d57&pid_fa61&mi_02&col04",
            2,
            true
        ));
        assert!(!is_configuration_identity(
            r"\\?\hid#vid_1d57&pid_fa61&mi_02&col01",
            2,
            true
        ));
        assert!(!is_configuration_identity(
            r"\\?\hid#vid_1d57&pid_fa61&mi_01&col04",
            1,
            true
        ));
    }

    #[test]
    fn non_windows_requires_interface_two() {
        assert!(is_configuration_identity("/dev/hidraw2", 2, false));
        assert!(!is_configuration_identity("device-col04", -1, false));
        assert!(!is_configuration_identity("/dev/hidraw1", 1, false));
    }

    #[test]
    fn unique_selection_refuses_zero_or_multiple_candidates() {
        assert!(matches!(
            select_candidate(&DeviceSelector::Unique, &[]),
            Err(DriverError::DeviceNotFound)
        ));
        assert_eq!(
            select_candidate(&DeviceSelector::Unique, &["only".to_owned()])
                .expect("one candidate is unambiguous"),
            0
        );
        assert!(matches!(
            select_candidate(
                &DeviceSelector::Unique,
                &["first".to_owned(), "second".to_owned()]
            ),
            Err(DriverError::AmbiguousDevice { count: 2 })
        ));
    }

    #[test]
    fn exact_path_selection_never_falls_back_to_the_first_device() {
        let paths = ["first".to_owned(), "second".to_owned()];
        assert_eq!(
            select_candidate(&DeviceSelector::path("second"), &paths)
                .expect("exact path must select its own collection"),
            1
        );
        assert!(matches!(
            select_candidate(&DeviceSelector::path("missing"), &paths),
            Err(DriverError::DeviceNotFound)
        ));
    }
}
