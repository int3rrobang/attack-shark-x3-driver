use crate::error::ProtocolError;

/// The transport used to communicate with the mouse.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "camelCase"))]
pub enum TransportKind {
    Wired,
    Receiver,
    /// Bluetooth Low Energy GATT.
    Ble,
}

/// A one-based X3 profile identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct ProfileId(u8);

impl ProfileId {
    pub const MIN: u8 = 1;
    pub const MAX: u8 = 5;

    /// Creates a profile identifier when `value` is in the supported range.
    #[must_use]
    pub const fn new(value: u8) -> Option<Self> {
        if value >= Self::MIN && value <= Self::MAX {
            Some(Self(value))
        } else {
            None
        }
    }

    /// Returns the wire value of this profile identifier.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for ProfileId {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value).ok_or(ProtocolError::InvalidProfile { value })
    }
}

impl std::fmt::Display for ProfileId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A one-based DPI stage index.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct StageIndex(u8);

impl StageIndex {
    pub const MIN: u8 = 1;
    pub const MAX: u8 = 8;

    /// Creates a stage index when `value` is in the supported range.
    #[must_use]
    pub const fn new(value: u8) -> Option<Self> {
        if value >= Self::MIN && value <= Self::MAX {
            Some(Self(value))
        } else {
            None
        }
    }

    /// Returns the one-based stage number.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for StageIndex {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value).ok_or(ProtocolError::InvalidStage { value })
    }
}

impl std::fmt::Display for StageIndex {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A DPI value supported by the X3 sensor, in DPI.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct DpiValue(u16);

impl DpiValue {
    pub const MIN: u16 = 50;
    pub const MAX: u16 = 26_000;
    pub const STEP: u16 = 50;

    /// Creates a DPI value when it is in range and aligned to the wire step.
    #[must_use]
    pub const fn new(value: u16) -> Option<Self> {
        if value >= Self::MIN && value <= Self::MAX && value / Self::STEP * Self::STEP == value {
            Some(Self(value))
        } else {
            None
        }
    }

    /// Returns the DPI value.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl TryFrom<u16> for DpiValue {
    type Error = ProtocolError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::new(value).ok_or(ProtocolError::InvalidDpi { value })
    }
}

impl std::fmt::Display for DpiValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::{DpiValue, ProfileId, StageIndex};

    #[test]
    fn profile_boundaries_and_neighbors() {
        assert!(ProfileId::try_from(ProfileId::MIN).is_ok());
        assert!(ProfileId::try_from(ProfileId::MAX).is_ok());
        assert!(ProfileId::try_from(ProfileId::MIN - 1).is_err());
        assert!(ProfileId::try_from(ProfileId::MAX + 1).is_err());
        assert!(ProfileId::try_from(3).is_ok_and(|value| value.get() == 3));
        assert!(ProfileId::try_from(3).is_ok_and(|value| value.to_string() == "3"));
    }

    #[test]
    fn stage_boundaries_and_neighbors() {
        assert!(StageIndex::try_from(StageIndex::MIN).is_ok());
        assert!(StageIndex::try_from(StageIndex::MAX).is_ok());
        assert!(StageIndex::try_from(StageIndex::MIN - 1).is_err());
        assert!(StageIndex::try_from(StageIndex::MAX + 1).is_err());
        assert!(StageIndex::try_from(4).is_ok_and(|value| value.get() == 4));
        assert!(StageIndex::try_from(4).is_ok_and(|value| value.to_string() == "4"));
    }

    #[test]
    fn dpi_boundaries_neighbors_and_step() {
        assert!(DpiValue::try_from(DpiValue::MIN).is_ok());
        assert!(DpiValue::try_from(DpiValue::MAX).is_ok());
        assert!(DpiValue::try_from(DpiValue::MIN - 1).is_err());
        assert!(DpiValue::try_from(DpiValue::MAX + 1).is_err());
        assert!(DpiValue::try_from(51).is_err());
        assert!(DpiValue::try_from(12_800).is_ok_and(|value| value.get() == 12_800));
        assert!(DpiValue::try_from(12_800).is_ok_and(|value| value.to_string() == "12800"));
    }
}
