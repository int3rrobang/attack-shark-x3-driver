use std::collections::BTreeMap;
use std::fmt;

use attack_shark_x3::TransportKind;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::StateError;
use crate::operation::DiscoveredEndpoint;

/// A validated, stable logical key for one physical mouse.
///
/// The value is manager-generated and formatted `mouse-N` where N >=1.
/// No transport path or serial is ever used to compose this key.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct DeviceId(String);

impl DeviceId {
    /// Creates a device key, rejecting values that are not `mouse-N`.
    pub fn new(value: impl Into<String>) -> Result<Self, StateError> {
        let value = value.into();
        validate_logical_id(&value)?;
        Ok(Self(value))
    }

    /// Creates a device key from a numeric allocation.
    pub fn from_number(number: u64) -> Result<Self, StateError> {
        if number == 0 {
            return Err(StateError::invalid_state("device number must be >= 1"));
        }
        // Format without leading zeros.
        Self::new(format!("mouse-{number}"))
    }

    /// Returns the numeric suffix if this is a `mouse-N` id.
    #[must_use]
    pub fn number(&self) -> Option<u64> {
        parse_logical_number(&self.0)
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
    /// paths can change while the stable logical identity remains.
    UsbPath(String),
    /// The stable platform identifier used to reopen a BLE device.
    BlePlatformId(String),
}

/// A transport-specific presence of a logical mouse.
///
/// This is an endpoint locator only - never a physical identity.
/// USB keeps current HID locator plus VID/PID/optional reported serial;
/// BLE keeps platform locator.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceEndpoint {
    pub transport: TransportKind,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    /// The reported serial string as seen on the transport, if any.
    /// Trimmed; blank values are normalized to None. This is metadata only
    /// and never a stable identity.
    pub serial_number: Option<String>,
    pub locator: DeviceLocator,
    pub display_name: Option<String>,
}

impl DeviceEndpoint {
    /// Builds an endpoint for a wired device or a 2.4 GHz receiver.
    ///
    /// The current HID path is retained verbatim in [`DeviceLocator::UsbPath`].
    pub fn usb(
        transport: TransportKind,
        vendor_id: u16,
        product_id: u16,
        serial_number: Option<&str>,
        current_path: &str,
        display_name: Option<&str>,
    ) -> Result<Self, StateError> {
        match transport {
            TransportKind::Wired | TransportKind::Receiver => {}
            TransportKind::Ble => {
                return Err(StateError::invalid_state(
                    "USB endpoint must use wired or receiver transport",
                ));
            }
        }

        if current_path.trim().is_empty() {
            return Err(StateError::invalid_state(
                "USB endpoint requires a nonblank current HID path",
            ));
        }

        let normalized_serial = serial_number
            .filter(|serial| !serial.trim().is_empty())
            .map(|serial| serial.trim().to_owned());

        Ok(Self {
            transport,
            vendor_id: Some(vendor_id),
            product_id: Some(product_id),
            serial_number: normalized_serial,
            locator: DeviceLocator::UsbPath(current_path.to_owned()),
            display_name: display_name
                .filter(|name| !name.trim().is_empty())
                .map(|name| name.trim().to_owned()),
        })
    }

    /// Builds an endpoint for a BLE device using its platform identifier.
    ///
    /// The advertised name is display metadata only.
    pub fn ble(platform_id: &str, display_name: Option<&str>) -> Result<Self, StateError> {
        let trimmed = platform_id.trim();
        if trimmed.is_empty() {
            return Err(StateError::invalid_state(
                "BLE platform device ID must not be blank",
            ));
        }
        Ok(Self {
            transport: TransportKind::Ble,
            vendor_id: None,
            product_id: None,
            serial_number: None,
            locator: DeviceLocator::BlePlatformId(trimmed.to_owned()),
            display_name: display_name
                .filter(|name| !name.trim().is_empty())
                .map(|name| name.trim().to_owned()),
        })
    }

    /// Returns true when transport and locator variants cohere.
    #[must_use]
    pub fn is_coherent(&self) -> bool {
        match (&self.transport, &self.locator) {
            (TransportKind::Wired, DeviceLocator::UsbPath(_))
            | (TransportKind::Receiver, DeviceLocator::UsbPath(_)) => {
                self.vendor_id.is_some() && self.product_id.is_some()
            }
            (TransportKind::Ble, DeviceLocator::BlePlatformId(_)) => true,
            _ => false,
        }
    }
}

/// A durable logical mouse identity that can own multiple transport endpoints.
///
/// Legacy serial/path derived keys are gone; the logical `mouse-N` is the
/// only stable key, and each discovered transport is added as an endpoint.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceIdentity {
    pub id: DeviceId,
    pub display_name: Option<String>,
    pub endpoints: BTreeMap<TransportKind, DeviceEndpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_transport: Option<TransportKind>,
}

impl DeviceIdentity {
    /// Creates a logical identity with no endpoints.
    #[must_use]
    pub fn new(id: DeviceId, display_name: Option<String>) -> Self {
        Self {
            id,
            display_name: display_name
                .filter(|name| !name.trim().is_empty())
                .map(|name| name.trim().to_owned()),
            endpoints: BTreeMap::new(),
            preferred_transport: None,
        }
    }

    /// Inserts or replaces the endpoint for its transport.
    pub fn upsert_endpoint(&mut self, endpoint: DeviceEndpoint) {
        self.endpoints.insert(endpoint.transport, endpoint);
    }

    /// Returns the endpoint for the given transport, if present.
    #[must_use]
    pub fn endpoint(&self, transport: TransportKind) -> Option<&DeviceEndpoint> {
        self.endpoints.get(&transport)
    }

    /// Returns true when an endpoint for the transport exists.
    #[must_use]
    pub fn has_endpoint(&self, transport: TransportKind) -> bool {
        self.endpoints.contains_key(&transport)
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_endpoint(mut self, endpoint: DeviceEndpoint) -> Self {
        self.upsert_endpoint(endpoint);
        self
    }

    #[cfg(all(test, any(feature = "usb", feature = "ble")))]
    pub(crate) fn test_usb(
        transport: TransportKind,
        vendor_id: u16,
        product_id: u16,
        serial_number: Option<&str>,
        current_path: &str,
        display_name: Option<&str>,
    ) -> Result<Self, StateError> {
        Self::test_identity(DeviceEndpoint::usb(
            transport,
            vendor_id,
            product_id,
            serial_number,
            current_path,
            display_name,
        )?)
    }

    #[cfg(test)]
    pub(crate) fn test_ble(
        platform_id: &str,
        display_name: Option<&str>,
    ) -> Result<Self, StateError> {
        Self::test_identity(DeviceEndpoint::ble(platform_id, display_name)?)
    }

    #[cfg(test)]
    fn test_identity(endpoint: DeviceEndpoint) -> Result<Self, StateError> {
        let display_name = endpoint.display_name.clone();
        Ok(Self::new(DeviceId::from_number(999)?, display_name).with_endpoint(endpoint))
    }

    /// Selects the endpoint that should be opened for a hardware operation.
    ///
    /// If `preferred_transport` is set and that endpoint exists, it is returned.
    /// Otherwise the deterministic priority Wired -> Receiver -> BLE among stored endpoints is used.
    #[must_use]
    pub fn selected_endpoint(&self) -> Option<&DeviceEndpoint> {
        if let Some(pref) = self.preferred_transport
            && let Some(endpoint) = self.endpoints.get(&pref)
        {
            return Some(endpoint);
        }
        // Deterministic priority
        for kind in [
            TransportKind::Wired,
            TransportKind::Receiver,
            TransportKind::Ble,
        ] {
            if let Some(endpoint) = self.endpoints.get(&kind) {
                return Some(endpoint);
            }
        }
        None
    }

    /// Returns the transport of the selected endpoint, if any.
    #[must_use]
    pub fn selected_transport(&self) -> Option<TransportKind> {
        self.selected_endpoint().map(|endpoint| endpoint.transport)
    }

    /// Validates transport/locator coherence for all endpoints.
    pub fn validate(&self) -> Result<(), StateError> {
        for (kind, endpoint) in &self.endpoints {
            if *kind != endpoint.transport {
                return Err(StateError::invalid_state(format!(
                    "endpoint key {kind:?} does not match endpoint transport {:?} for device {}",
                    endpoint.transport, self.id
                )));
            }
            if !endpoint.is_coherent() {
                return Err(StateError::invalid_state(format!(
                    "endpoint transport/locator mismatch for device {} transport {kind:?}",
                    self.id
                )));
            }
            match &endpoint.locator {
                DeviceLocator::UsbPath(path) => {
                    if path.trim().is_empty() {
                        return Err(StateError::invalid_state(format!(
                            "USB locator for device {} transport {kind:?} is blank",
                            self.id
                        )));
                    }
                }
                DeviceLocator::BlePlatformId(id) => {
                    if id.trim().is_empty() {
                        return Err(StateError::invalid_state(format!(
                            "BLE locator for device {} is blank",
                            self.id
                        )));
                    }
                }
            }
            if let Some(serial) = &endpoint.serial_number
                && serial.trim().is_empty()
            {
                return Err(StateError::invalid_state(format!(
                    "serial for device {} transport {kind:?} is blank",
                    self.id
                )));
            }
        }
        Ok(())
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

fn validate_logical_id(value: &str) -> Result<(), StateError> {
    if value.trim().is_empty() {
        return Err(StateError::invalid_state(
            "device identity must not be blank",
        ));
    }
    // Reject surrounding whitespace: canonical form has no trim.
    if value != value.trim() {
        return Err(StateError::invalid_state(format!(
            "device identity '{value}' must not have surrounding whitespace"
        )));
    }
    if let Some(number) = parse_logical_number(value) {
        // ensure canonical form: no leading zeros, exact "mouse-{number}"
        if format!("mouse-{number}") != value {
            return Err(StateError::invalid_state(format!(
                "device identity '{value}' must be canonical mouse-N without leading zeros"
            )));
        }
        if number == 0 {
            return Err(StateError::invalid_state(
                "device identity number must be >= 1",
            ));
        }
        return Ok(());
    }
    Err(StateError::invalid_state(format!(
        "device identity '{value}' must be mouse-N (N >= 1)"
    )))
}
fn parse_logical_number(value: &str) -> Option<u64> {
    if !value.starts_with("mouse-") {
        return None;
    }
    let suffix = &value[6..];
    if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    // Reject leading zeros: "mouse-001" is not canonical.
    if suffix.len() > 1 && suffix.starts_with('0') {
        return None;
    }
    suffix.parse::<u64>().ok().filter(|&n| n != 0)
}

/// Filters connected discoveries down to the rebind candidates for one stored
/// endpoint: same transport and same VID/PID, sorted by locator and
/// deduplicated. VID/PID is the only stable model signal on this hardware
/// (empty serial, locator changes on replug); BLE endpoints always carry
/// `None` VID/PID, so the filter is a no-op there.
pub(crate) fn rebind_candidates(
    discovered: Vec<DiscoveredEndpoint>,
    transport: TransportKind,
    stored: &DeviceEndpoint,
) -> Vec<DeviceEndpoint> {
    let mut candidates: Vec<DeviceEndpoint> = discovered
        .into_iter()
        .filter(|candidate| {
            candidate.connected
                && candidate.endpoint.transport == transport
                && candidate.endpoint.vendor_id == stored.vendor_id
                && candidate.endpoint.product_id == stored.product_id
        })
        .map(|candidate| candidate.endpoint)
        .collect();
    candidates.sort_by(|a, b| a.locator.cmp(&b.locator));
    candidates.dedup_by(|a, b| a.locator == b.locator);
    candidates
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{DeviceEndpoint, DeviceId, DeviceIdentity, DeviceLocator};
    use attack_shark_x3::TransportKind;

    #[test]
    fn device_id_requires_mouse_n_format() {
        assert!(DeviceId::new("mouse-1").is_ok());
        assert!(DeviceId::new("mouse-42").is_ok());
        assert_eq!(DeviceId::from_number(7).unwrap().as_str(), "mouse-7");
        assert_eq!(DeviceId::new("mouse-7").unwrap().number(), Some(7));
        // Rejects legacy forms
        assert!(DeviceId::new("usb:1d57:fa60:path:foo").is_err());
        assert!(DeviceId::new("ble:AA").is_err());
        assert!(DeviceId::new("mouse-0").is_err());
        assert!(DeviceId::new("mouse-01").is_err());
        assert!(DeviceId::new(" mouse-1 ").is_err());
        assert!(DeviceId::new("").is_err());
        assert!(DeviceId::new("  ").is_err());
        assert!(DeviceId::new("mouse-").is_err());
        assert!(DeviceId::new("mouse-1a").is_err());
    }

    #[test]
    fn device_ids_round_trip_as_json_map_keys() {
        let id = DeviceId::new("mouse-42").unwrap();
        let identity = DeviceIdentity::new(id.clone(), Some("Mouse".to_owned()));
        let mut devices = BTreeMap::new();
        devices.insert(id.clone(), identity.clone());

        let json = serde_json::to_string(&devices).expect("serialize device map");
        let decoded: BTreeMap<DeviceId, DeviceIdentity> =
            serde_json::from_str(&json).expect("deserialize device map");

        assert_eq!(decoded.get(&id), Some(&identity));
    }

    #[test]
    fn usb_endpoint_keeps_hid_locator_and_metadata() {
        let endpoint = DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa61,
            Some("  SN\\AbC  "),
            r"C:\\hid-path",
            Some("My Mouse"),
        )
        .expect("usb endpoint");
        assert_eq!(endpoint.transport, TransportKind::Wired);
        assert_eq!(endpoint.vendor_id, Some(0x1d57));
        assert_eq!(endpoint.product_id, Some(0xfa61));
        assert_eq!(endpoint.serial_number.as_deref(), Some("SN\\AbC"));
        assert_eq!(
            endpoint.locator,
            DeviceLocator::UsbPath(r"C:\\hid-path".to_owned())
        );
        assert_eq!(endpoint.display_name.as_deref(), Some("My Mouse"));
        assert!(endpoint.is_coherent());
    }

    #[test]
    fn ble_endpoint_keeps_platform_locator_only() {
        let endpoint =
            DeviceEndpoint::ble("  AA\\BB  ", Some("Office Mouse")).expect("BLE endpoint");
        assert_eq!(endpoint.transport, TransportKind::Ble);
        assert_eq!(endpoint.vendor_id, None);
        assert_eq!(endpoint.product_id, None);
        assert_eq!(endpoint.serial_number, None);
        assert_eq!(
            endpoint.locator,
            DeviceLocator::BlePlatformId("AA\\BB".to_owned())
        );
        assert_eq!(endpoint.display_name.as_deref(), Some("Office Mouse"));
        assert!(endpoint.is_coherent());
    }

    #[test]
    fn endpoint_transport_locator_coherence_rejected() {
        // BLE transport with USB locator is incoherent - constructed via validation.
        let mut endpoint = DeviceEndpoint::ble("platform-1", None).unwrap();
        endpoint.transport = TransportKind::Wired; // tamper
        assert!(!endpoint.is_coherent());

        let identity =
            DeviceIdentity::new(DeviceId::new("mouse-1").unwrap(), None).with_endpoint(endpoint);
        assert!(identity.validate().is_err());
    }

    #[test]
    fn blank_ids_and_selectors_are_rejected() {
        assert!(DeviceId::new("  ").is_err());
        assert!(DeviceId::new("not-mouse").is_err());
        assert!(DeviceEndpoint::ble("\t", None).is_err());
        assert!(
            DeviceEndpoint::usb(TransportKind::Wired, 0x1d57, 0xfa61, None, "  ", None).is_err()
        );
        assert!(
            DeviceEndpoint::usb(
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
    fn blank_usb_serial_normalizes_to_none() {
        let endpoint = DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa61,
            Some("  "),
            r"\\?\hid#path",
            None,
        )
        .expect("blank serial should be treated as absent");
        assert_eq!(endpoint.serial_number, None);
    }

    #[test]
    fn single_logical_identity_can_hold_multiple_transports() {
        let mut identity =
            DeviceIdentity::new(DeviceId::new("mouse-1").unwrap(), Some("Mouse".to_owned()));
        let wired = DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa60,
            None,
            r"\\?\hid#wired-path",
            None,
        )
        .unwrap();
        let receiver = DeviceEndpoint::usb(
            TransportKind::Receiver,
            0x1d57,
            0xfa60,
            None,
            r"\\?\hid#receiver-path",
            None,
        )
        .unwrap();
        let ble = DeviceEndpoint::ble("platform-ble-42", None).unwrap();

        identity.upsert_endpoint(wired);
        identity.upsert_endpoint(receiver);
        identity.upsert_endpoint(ble);

        assert_eq!(identity.endpoints.len(), 3);
        assert!(identity.has_endpoint(TransportKind::Wired));
        assert!(identity.has_endpoint(TransportKind::Receiver));
        assert!(identity.has_endpoint(TransportKind::Ble));
        assert!(identity.validate().is_ok());

        // Updating one transport replaces it, not duplicating.
        let wired_v2 = DeviceEndpoint::usb(
            TransportKind::Wired,
            0x1d57,
            0xfa60,
            Some("NEWSN"),
            r"\\?\hid#wired-path-v2",
            None,
        )
        .unwrap();
        identity.upsert_endpoint(wired_v2);
        assert_eq!(identity.endpoints.len(), 3);
        assert_eq!(
            identity
                .endpoint(TransportKind::Wired)
                .unwrap()
                .serial_number
                .as_deref(),
            Some("NEWSN")
        );
    }

    #[test]
    fn allocation_numbers_are_strictly_formatted() {
        assert!(DeviceId::from_number(0).is_err());
        assert_eq!(DeviceId::from_number(1).unwrap().as_str(), "mouse-1");
        assert_eq!(DeviceId::from_number(100).unwrap().as_str(), "mouse-100");
    }
}
