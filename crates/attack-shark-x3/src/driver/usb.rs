use hidapi::{DeviceInfo as HidDeviceInfo, HidApi, HidDevice};

use super::{DriverError, FeatureTransport, InputTransport};
const ATTACK_SHARK_VENDOR_ID: u16 = 0x1d57;
const FA61_PRODUCT_ID: u16 = 0xfa61;
const FA60_PRODUCT_ID: u16 = 0xfa60;
const CONFIG_INTERFACE_NUMBER: i32 = 2;
const WINDOWS_CONFIG_COLLECTION: &str = "col04";
const AUXILIARY_USAGE_PAGE: u16 = 0x000a;

/// The X3 USB device family selected by the HID transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbDeviceKind {
    /// X3/FA61 wired device.
    Wired,
    /// X3 2.4 GHz receiver with PID `0xfa60`.
    Receiver,
}

impl UsbDeviceKind {
    #[must_use]
    pub const fn product_id(self) -> u16 {
        match self {
            Self::Wired => FA61_PRODUCT_ID,
            Self::Receiver => FA60_PRODUCT_ID,
        }
    }

    #[must_use]
    pub const fn transport_kind(self) -> crate::TransportKind {
        match self {
            Self::Wired => crate::TransportKind::Wired,
            Self::Receiver => crate::TransportKind::Receiver,
        }
    }
}

/// A discovered X3 wired or receiver configuration collection.
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
    /// Open only when exactly one matching configuration collection is present.
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

/// Lists X3 wired configuration collections without opening hardware.
///
/// # Errors
///
/// Returns an error when HID enumeration fails.
pub fn list_devices() -> Result<Vec<DeviceInfo>, DriverError> {
    list_devices_for(UsbDeviceKind::Wired)
}

/// Lists configuration collections for one X3 USB transport.
///
/// # Errors
///
/// Returns an error when HID enumeration fails.
pub fn list_devices_for(kind: UsbDeviceKind) -> Result<Vec<DeviceInfo>, DriverError> {
    let api = HidApi::new().map_err(|error| DriverError::Transport(error.to_string()))?;
    Ok(configuration_collections(&api, kind)
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

pub(super) struct HidInputTransport {
    device: HidDevice,
}

impl InputTransport for HidInputTransport {
    fn read_input_report(&mut self, buffer: &mut [u8], timeout_ms: i32) -> Result<usize, String> {
        self.device
            .read_timeout(buffer, timeout_ms)
            .map_err(|error| error.to_string())
    }
}

pub(super) fn open_transport(
    selector: &DeviceSelector,
    kind: UsbDeviceKind,
) -> Result<HidFeatureTransport, DriverError> {
    let api = HidApi::new().map_err(|error| DriverError::Transport(error.to_string()))?;
    let candidates: Vec<&HidDeviceInfo> = configuration_collections(&api, kind).collect();
    let selected = candidates[select_device_candidate(selector, &candidates)?];
    let device = selected
        .open_device(&api)
        .map_err(|error| DriverError::Transport(error.to_string()))?;
    Ok(HidFeatureTransport { device })
}

pub(super) fn open_input_transport(
    selector: &DeviceSelector,
    kind: UsbDeviceKind,
) -> Result<HidInputTransport, DriverError> {
    let api = HidApi::new().map_err(|error| DriverError::Transport(error.to_string()))?;
    let configurations: Vec<&HidDeviceInfo> = configuration_collections(&api, kind).collect();
    // The fallback is deliberately scoped: only when a single configuration
    // collection exists and exactly one input collection is present does the
    // input lookup fall back to that sole collection. This keeps Linux
    // enumeration usable on FA60/FA61 while avoiding broad ambiguous matches.
    let unambiguous_input_fallback = configurations.len() == 1;
    let configuration = configurations[select_device_candidate(selector, &configurations)?];
    let configuration_path = configuration.path().to_string_lossy();

    let inputs: Vec<_> = api
        .device_list()
        .filter(|device| is_input_collection(device, kind))
        .collect();
    let selected = inputs
        .iter()
        .copied()
        .find(|device| same_device_family(&configuration_path, &device.path().to_string_lossy()))
        .or_else(|| {
            if unambiguous_input_fallback && inputs.len() == 1 {
                inputs.first().copied()
            } else {
                None
            }
        })
        .ok_or(DriverError::DeviceNotFound)?;
    let device = selected
        .open_device(&api)
        .map_err(|error| DriverError::Transport(error.to_string()))?;
    Ok(HidInputTransport { device })
}

fn configuration_collections(
    api: &HidApi,
    kind: UsbDeviceKind,
) -> impl Iterator<Item = &HidDeviceInfo> {
    api.device_list().filter(move |device| {
        device.vendor_id() == ATTACK_SHARK_VENDOR_ID
            && device.product_id() == kind.product_id()
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
    if !windows {
        return true;
    }
    contains_ignore_ascii_case(path, WINDOWS_CONFIG_COLLECTION)
}

fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.len() < needle.len() {
        return false;
    }
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

#[cfg(not(target_os = "linux"))]
fn is_input_collection(device: &HidDeviceInfo, kind: UsbDeviceKind) -> bool {
    device.vendor_id() == ATTACK_SHARK_VENDOR_ID
        && device.product_id() == kind.product_id()
        && !is_configuration_collection(device)
        && device.usage_page() == AUXILIARY_USAGE_PAGE
}

#[cfg(target_os = "linux")]
fn is_input_collection(device: &HidDeviceInfo, kind: UsbDeviceKind) -> bool {
    // Tightened on Linux: require the auxiliary usage page and exclude the
    // configuration collection. The broad interface-only check is no longer
    // used; `open_input_transport` retains a documented single-candidate
    // fallback only when exactly one configuration and one input collection
    // remain and family matching fails. This preserves FA60/FA61 layouts
    // (config on interface 2, input on auxiliary page) without over-matching.
    device.vendor_id() == ATTACK_SHARK_VENDOR_ID
        && device.product_id() == kind.product_id()
        && !is_configuration_collection(device)
        && device.usage_page() == AUXILIARY_USAGE_PAGE
}

fn same_device_family(left: &str, right: &str) -> bool {
    fn family_end(path: &str) -> usize {
        let bytes = path.as_bytes();
        if bytes.len() < 4 {
            return path.len();
        }
        for index in 0..=bytes.len() - 4 {
            if bytes[index] == b'&'
                && bytes[index + 1].eq_ignore_ascii_case(&b'c')
                && bytes[index + 2].eq_ignore_ascii_case(&b'o')
                && bytes[index + 3].eq_ignore_ascii_case(&b'l')
            {
                return index;
            }
        }
        path.len()
    }

    let left_end = family_end(left);
    let right_end = family_end(right);
    left[..left_end].eq_ignore_ascii_case(&right[..right_end])
}

fn select_device_candidate(
    selector: &DeviceSelector,
    candidates: &[&HidDeviceInfo],
) -> Result<usize, DriverError> {
    match selector {
        DeviceSelector::Unique => match candidates.len() {
            0 => Err(DriverError::DeviceNotFound),
            1 => Ok(0),
            count => Err(DriverError::AmbiguousDevice { count }),
        },
        DeviceSelector::Path(expected_path) => candidates
            .iter()
            .position(|device| {
                let path = device.path().to_string_lossy();
                if cfg!(target_os = "windows") {
                    path.eq_ignore_ascii_case(expected_path)
                } else {
                    path == expected_path.as_str()
                }
            })
            .ok_or(DriverError::DeviceNotFound),
    }
}

#[cfg(test)]
fn select_candidate(
    selector: &DeviceSelector,
    candidate_paths: &[&str],
) -> Result<usize, DriverError> {
    match selector {
        DeviceSelector::Unique => match candidate_paths.len() {
            0 => Err(DriverError::DeviceNotFound),
            1 => Ok(0),
            count => Err(DriverError::AmbiguousDevice { count }),
        },
        DeviceSelector::Path(expected_path) => candidate_paths
            .iter()
            .position(|path| {
                if cfg!(target_os = "windows") {
                    path.eq_ignore_ascii_case(expected_path)
                } else {
                    *path == expected_path
                }
            })
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
    use super::{
        DeviceSelector, DriverError, UsbDeviceKind, is_configuration_identity, same_device_family,
        select_candidate,
    };

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
    fn receiver_uses_fa60_and_the_same_col04_configuration_rule() {
        assert_eq!(UsbDeviceKind::Wired.product_id(), 0xfa61);
        assert_eq!(UsbDeviceKind::Receiver.product_id(), 0xfa60);
        assert!(is_configuration_identity(
            r"\\?\hid#vid_1d57&pid_fa60&mi_02&col04",
            2,
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
            select_candidate(&DeviceSelector::Unique, &[] as &[&str]),
            Err(DriverError::DeviceNotFound)
        ));
        assert_eq!(
            select_candidate(&DeviceSelector::Unique, &["only"])
                .expect("one candidate is unambiguous"),
            0
        );
        assert!(matches!(
            select_candidate(&DeviceSelector::Unique, &["first", "second"]),
            Err(DriverError::AmbiguousDevice { count: 2 })
        ));
    }

    #[test]
    fn exact_path_selection_never_falls_back_to_the_first_device() {
        let paths = ["first", "second"];
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

    #[test]
    fn same_device_family_is_case_insensitive_and_ignores_col_suffix() {
        assert!(same_device_family(
            r"\\?\HID#VID_1D57&PID_FA61&MI_02&COL04#8&ABC&0&0003",
            r"\\?\hid#vid_1d57&pid_fa61&mi_02&col01#8&abc&0&0003",
        ));
        assert!(same_device_family(
            r"\\?\hid#vid_1d57&pid_fa61&mi_02&COL04",
            r"\\?\hid#vid_1d57&pid_fa61&mi_02&col01",
        ));
    }

    #[test]
    fn windows_path_selection_respects_case_insensitive_identifiers() {
        let paths = ["first", "SECOND"];
        if cfg!(target_os = "windows") {
            assert_eq!(
                select_candidate(&DeviceSelector::path("second"), &paths)
                    .expect("Windows HID paths are case-insensitive"),
                1
            );
            assert_eq!(
                select_candidate(&DeviceSelector::path("SECOND"), &paths)
                    .expect("Windows HID paths are case-insensitive"),
                1
            );
        } else {
            assert!(matches!(
                select_candidate(&DeviceSelector::path("second"), &paths),
                Err(DriverError::DeviceNotFound)
            ));
        }
    }

    #[test]
    fn contains_ignore_ascii_case_is_allocation_free() {
        assert!(super::contains_ignore_ascii_case(
            r"\\?\hid#vid_1d57&pid_fa61&mi_02&col04",
            "COL04"
        ));
        assert!(!super::contains_ignore_ascii_case(
            r"\\?\hid#vid_1d57&pid_fa61&mi_02&col01",
            "col04"
        ));
    }
}
