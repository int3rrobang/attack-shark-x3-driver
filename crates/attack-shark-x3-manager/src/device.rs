use std::fmt;

use attack_shark_x3::TransportKind;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::StateError;

/// A validated, stable key for one exact device entry.
///
/// The value is intentionally opaque: transport-specific constructors are the
/// only place where manager identities are composed, and a key never joins
/// wired, receiver, and BLE transports.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct DeviceId(String);

impl DeviceId {
    /// Creates a device key, rejecting empty and whitespace-only values.
    pub fn new(value: impl Into<String>) -> Result<Self, StateError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(StateError::invalid_state(
                "device identity must not be blank",
            ));
        }
        Ok(Self(value))
    }

    /// Returns the key's string representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for DeviceId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for DeviceId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DeviceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// The current platform-specific selector for an exact device.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceLocator {
    /// The current openable HID path. This is deliberately kept verbatim;
    /// paths can change while the stable identity remains serial-based.
    UsbPath(String),
    /// The stable platform identifier used to reopen a BLE device.
    BlePlatformId(String),
}

/// A durable identity plus the metadata needed to rediscover one exact device.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceIdentity {
    pub id: DeviceId,
    pub transport: TransportKind,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub serial_number: Option<String>,
    pub locator: DeviceLocator,
    pub display_name: Option<String>,
}

impl DeviceIdentity {
    /// Builds an identity for a wired device or a 2.4 GHz receiver.
    ///
    /// Serial numbers are preferred over paths for the durable key. The
    /// current HID path is retained verbatim in [`DeviceLocator::UsbPath`],
    /// while its normalized form is used only when a serial is unavailable.
    pub fn usb(
        transport: TransportKind,
        vendor_id: u16,
        product_id: u16,
        serial_number: Option<&str>,
        current_path: &str,
        display_name: Option<&str>,
    ) -> Result<Self, StateError> {
        let prefix = match transport {
            TransportKind::Wired => "usb",
            TransportKind::Receiver => "receiver",
            TransportKind::Ble => {
                return Err(StateError::invalid_state(
                    "USB identity must use wired or receiver transport",
                ));
            }
        };

        if current_path.trim().is_empty() {
            return Err(StateError::invalid_state(
                "USB identity requires a nonblank current HID path",
            ));
        }

        let normalized_serial = serial_number
            .map(|serial| normalize_key(serial, "USB serial number"))
            .transpose()?;
        let key_suffix = match normalized_serial.as_deref() {
            Some(serial) => format!("serial:{serial}"),
            None => format!("path:{}", normalize_key(current_path, "USB HID path")?),
        };
        let id = DeviceId::new(format!(
            "{prefix}:{vendor_id:04x}:{product_id:04x}:{key_suffix}"
        ))?;

        Ok(Self {
            id,
            transport,
            vendor_id: Some(vendor_id),
            product_id: Some(product_id),
            serial_number: normalized_serial,
            locator: DeviceLocator::UsbPath(current_path.to_owned()),
            display_name: display_name.map(str::to_owned),
        })
    }

    /// Builds an identity for a BLE device using its stable platform ID.
    ///
    /// The advertised name is display metadata only and never contributes to
    /// the identity key.
    pub fn ble(stable_platform_id: &str, display_name: Option<&str>) -> Result<Self, StateError> {
        let normalized_id = normalize_key(stable_platform_id, "BLE platform device ID")?;
        let id = DeviceId::new(format!("ble:{normalized_id}"))?;

        Ok(Self {
            id,
            transport: TransportKind::Ble,
            vendor_id: None,
            product_id: None,
            serial_number: None,
            locator: DeviceLocator::BlePlatformId(normalized_id),
            display_name: display_name.map(str::to_owned),
        })
    }
}

/// How discovery should select transports.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TransportSelection {
    #[default]
    Auto,
    Exact(TransportKind),
}

fn normalize_key(value: &str, field: &str) -> Result<String, StateError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(StateError::invalid_state(format!(
            "{field} must not be blank"
        )));
    }

    Ok(trimmed.replace('\\', "/").to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{DeviceId, DeviceIdentity, DeviceLocator};
    use attack_shark_x3::TransportKind;

    #[test]
    fn wired_and_receiver_keys_are_distinct_without_interface_numbers() {
        let wired = DeviceIdentity::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa60,
            None,
            r"\\?\hid#VID_1D57&PID_FA60#path",
            Some("Mouse"),
        )
        .expect("wired identity");
        let receiver = DeviceIdentity::usb(
            TransportKind::Receiver,
            0x1d57,
            0xfa60,
            None,
            r"\\?\hid#VID_1D57&PID_FA60#path",
            Some("Mouse"),
        )
        .expect("receiver identity");

        assert_ne!(wired.id, receiver.id);
        assert_eq!(
            wired.id.as_str(),
            "usb:1d57:fa60:path://?/hid#vid_1d57&pid_fa60#path"
        );
        assert_eq!(
            receiver.id.as_str(),
            "receiver:1d57:fa60:path://?/hid#vid_1d57&pid_fa60#path"
        );
    }

    #[test]
    fn serial_is_preferred_over_path_and_is_normalized() {
        let identity = DeviceIdentity::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa61,
            Some("  SN\\AbC  "),
            r"C:\\never-used-path",
            None,
        )
        .expect("serial identity");

        assert_eq!(identity.id.as_str(), "usb:1d57:fa61:serial:sn/abc");
        assert_eq!(identity.serial_number.as_deref(), Some("sn/abc"));
        assert_eq!(
            identity.locator,
            DeviceLocator::UsbPath(r"C:\\never-used-path".to_owned())
        );
    }

    #[test]
    fn ble_name_does_not_change_identity() {
        let named = DeviceIdentity::ble("  AA\\BB  ", Some("Office Mouse")).expect("BLE identity");
        let renamed = DeviceIdentity::ble("aa/bb", Some("Travel Mouse")).expect("BLE identity");

        assert_eq!(named.id, renamed.id);
        assert_eq!(named.id.as_str(), "ble:aa/bb");
        assert_eq!(named.transport, TransportKind::Ble);
        assert_eq!(renamed.transport, TransportKind::Ble);
    }

    #[test]
    fn blank_ids_and_selectors_are_rejected() {
        assert!(DeviceId::new("  ").is_err());
        assert!(DeviceIdentity::ble("\t", None).is_err());
        assert!(
            DeviceIdentity::usb(
                TransportKind::Wired,
                0x1d57,
                0xfa61,
                Some("  "),
                "/dev/hidraw0",
                None,
            )
            .is_err()
        );
        assert!(
            DeviceIdentity::usb(TransportKind::Wired, 0x1d57, 0xfa61, None, "  ", None,).is_err()
        );
        assert!(
            DeviceIdentity::usb(
                TransportKind::Ble,
                0x1d57,
                0xfa61,
                None,
                "/dev/hidraw0",
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn device_ids_round_trip_as_json_map_keys() {
        let identity = DeviceIdentity::ble("platform-42", Some("Mouse")).expect("BLE identity");
        let mut devices = BTreeMap::new();
        devices.insert(identity.id.clone(), identity.clone());

        let json = serde_json::to_string(&devices).expect("serialize device map");
        let decoded: BTreeMap<DeviceId, DeviceIdentity> =
            serde_json::from_str(&json).expect("deserialize device map");

        assert_eq!(decoded.get(&identity.id), Some(&identity));
    }
}
