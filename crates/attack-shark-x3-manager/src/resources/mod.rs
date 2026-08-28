pub(crate) mod buttons;
pub(crate) mod dpi;
pub(crate) mod settings;
pub(crate) mod state;

/// Resource-layer watermark stamping for production DPI writes. Manager-level
/// callers (composite profile updates) must stamp before every `write_dpi`.
pub(crate) use dpi::with_physical_watermark;
