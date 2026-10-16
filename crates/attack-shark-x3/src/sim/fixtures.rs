//! TOML scenario loader for the replay backend.
//!
//! A scenario names an endpoint set plus the recorded per-profile images
//! each endpoint serves. The loader validates and builds the typed
//! [`ProfileImage`]s; byte-exact serving and transport behavior live in
//! [`super::replay::SimReplay`].

use std::collections::BTreeMap;
use std::path::Path;

use super::{ProfileImage, SimGap};
use crate::protocol::buttons::BUTTON_SLOT_COUNT;
use crate::{
    ButtonAssignment, ButtonsState, DpiState, DpiValue, PreferencesState, ProfileId, StageIndex,
    TransportKind, WATERMARK_LENGTH,
};

/// One discovery endpoint declared by a scenario file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScenarioEndpoint {
    /// Transport this endpoint presents as.
    pub transport: TransportKind,
}

/// A fully loaded scenario: endpoint set, per-profile recorded images
/// (when the scenario captures any), and the recorded battery level
/// (`None` = the device reports no battery).
#[derive(Clone, Debug, PartialEq)]
pub struct Scenario {
    /// Source file, for gap messages.
    pub name: String,
    /// Discovery endpoints, in file order.
    pub endpoints: Vec<ScenarioEndpoint>,
    /// Recorded profile images by one-based profile number.
    pub profiles: BTreeMap<u8, ProfileImage>,
    /// Recorded post-write images by one-based profile number. A write is
    /// verified against the entry here when present; otherwise the backend
    /// reports a gap naming the missing capture.
    pub post_write: BTreeMap<u8, ProfileImage>,
    /// Recorded battery level (`None` when the scenario omits it).
    pub battery: Option<u8>,
}

/// Loads the scenario file at `path`.
///
/// # Errors
///
/// Returns a [`SimGap`] naming the file and line for TOML syntax errors,
/// and naming the file plus the offending value for schema or
/// protocol-validation failures.
pub fn load_scenario(path: &Path) -> Result<Scenario, SimGap> {
    let name = path.display().to_string();
    let text = std::fs::read_to_string(path)
        .map_err(|error| SimGap(format!("sim-replay: cannot read scenario {name}: {error}")))?;
    parse_scenario(&text, &name)
}

/// Parses scenario TOML with `name` identifying the source in errors.
///
/// `name` is `"scenario {scenario}"` for registry names or the file path
/// for direct loads; every error message starts with it.
pub fn parse_scenario(text: &str, name: &str) -> Result<Scenario, SimGap> {
    let value: toml::Value =
        toml::from_str(text).map_err(|error| SimGap(format!("{name}: parse error at {error}")))?;
    let table = value
        .as_table()
        .ok_or_else(|| SimGap(format!("{name}: expected a TOML table")))?;

    let mut endpoints = Vec::new();
    if let Some(list) = table.get("endpoints") {
        let list = list
            .as_array()
            .ok_or_else(|| SimGap(format!("{name}: `endpoints` must be an array of tables")))?;
        for entry in list {
            let entry_table = entry
                .as_table()
                .ok_or_else(|| SimGap(format!("{name}: `endpoints` entries must be tables")))?;
            let transport = entry_table
                .get("transport")
                .and_then(toml::Value::as_str)
                .ok_or_else(|| {
                    SimGap(format!(
                        "{name}: endpoint is missing `transport` (expected \"wired\" or \"ble\")"
                    ))
                })?;
            endpoints.push(ScenarioEndpoint {
                transport: parse_transport(name, transport)?,
            });
        }
    }
    if endpoints.is_empty() {
        endpoints.push(ScenarioEndpoint {
            transport: TransportKind::Wired,
        });
    }

    let battery = match table.get("battery") {
        None => None,
        Some(value) => Some(
            value
                .as_integer()
                .filter(|level| (0..=100).contains(level))
                .ok_or_else(|| SimGap(format!("{name}: `battery` must be an integer in 0..=100")))
                .map(|level| level as u8)?,
        ),
    };

    let mut profiles = BTreeMap::new();
    if let Some(section) = table.get("profiles") {
        let section_table = section
            .as_table()
            .ok_or_else(|| SimGap(format!("{name}: `[profiles.<n>]` must be a table")))?;
        for (key, value) in section_table {
            let number: u8 = key.parse().map_err(|_| {
                SimGap(format!(
                    "{name}: [profiles.{key}]: profile number must be 1..={max}",
                    max = ProfileId::MAX
                ))
            })?;
            if ProfileId::new(number).is_none() {
                return Err(SimGap(format!(
                    "{name}: [profiles.{key}]: profile number must be 1..={max}",
                    max = ProfileId::MAX
                )));
            }
            let profile_table = value
                .as_table()
                .ok_or_else(|| SimGap(format!("{name}: [profiles.{key}]: must be a table")))?;
            profiles.insert(number, parse_profile_image(name, number, profile_table)?);
        }
    }

    let mut post_write = BTreeMap::new();
    if let Some(section) = table.get("post_write") {
        let section_table = section
            .as_table()
            .ok_or_else(|| SimGap(format!("{name}: `[post_write.<n>]` must be a table")))?;
        for (key, value) in section_table {
            let number: u8 = key.parse().map_err(|_| {
                SimGap(format!(
                    "{name}: [post_write.{key}]: profile number must be 1..={max}",
                    max = ProfileId::MAX
                ))
            })?;
            if ProfileId::new(number).is_none() {
                return Err(SimGap(format!(
                    "{name}: [post_write.{key}]: profile number must be 1..={max}",
                    max = ProfileId::MAX
                )));
            }
            let profile_table = value
                .as_table()
                .ok_or_else(|| SimGap(format!("{name}: [post_write.{key}]: must be a table")))?;
            post_write.insert(number, parse_profile_image(name, number, profile_table)?);
        }
    }

    Ok(Scenario {
        name: name.to_owned(),
        endpoints,
        profiles,
        post_write,
        battery,
    })
}

fn parse_transport(name: &str, transport: &str) -> Result<TransportKind, SimGap> {
    match transport {
        "wired" => Ok(TransportKind::Wired),
        "ble" => Ok(TransportKind::Ble),
        other => Err(SimGap(format!(
            "{name}: unknown transport {other:?}; expected \"wired\" or \"ble\""
        ))),
    }
}

fn parse_profile_image(
    name: &str,
    profile: u8,
    table: &toml::Table,
) -> Result<ProfileImage, SimGap> {
    let ctx = format!("{name}: [profiles.{profile}]");
    let id = ProfileId::try_from(profile)
        .map_err(|_| SimGap(format!("{ctx}: profile number must be 1..=5")))?;

    let stages = table
        .get("dpi_stages")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| SimGap(format!("{ctx}: missing `dpi_stages` array")))?;
    let mut dpi_stages = Vec::with_capacity(stages.len());
    for stage in stages {
        let raw = stage
            .as_integer()
            .ok_or_else(|| SimGap(format!("{ctx}: `dpi_stages` entries must be integers")))?;
        let value = u16::try_from(raw)
            .ok()
            .and_then(DpiValue::new)
            .ok_or_else(|| {
                SimGap(format!(
                    "{ctx}: invalid DPI stage {raw}; expected 50..=26000 in steps of 50"
                ))
            })?;
        dpi_stages.push(value);
    }
    let active_raw = table
        .get("active_stage")
        .and_then(toml::Value::as_integer)
        .ok_or_else(|| SimGap(format!("{ctx}: missing `active_stage` integer")))?;
    let active = u8::try_from(active_raw)
        .ok()
        .and_then(StageIndex::new)
        .ok_or_else(|| {
            SimGap(format!(
                "{ctx}: invalid `active_stage` {active_raw}; expected 1..=8"
            ))
        })?;
    let tail_hex = table
        .get("dpi_tail")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| SimGap(format!("{ctx}: missing `dpi_tail` hex string")))?;
    let tail = parse_hex_bytes(name, profile, "dpi_tail", tail_hex, WATERMARK_LENGTH)?;
    let dpi = DpiState::new(id, dpi_stages, active, tail)
        .map_err(|error| SimGap(format!("{ctx}: invalid DPI image: {error}")))?;

    let get_byte = |key: &str| -> Result<u8, SimGap> {
        table
            .get(key)
            .and_then(toml::Value::as_integer)
            .and_then(|value| u8::try_from(value).ok())
            .ok_or_else(|| SimGap(format!("{ctx}: missing or invalid `{key}` byte (0..=255)")))
    };
    let host_color = table
        .get("host_color")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| SimGap(format!("{ctx}: missing `host_color` array of 3 bytes")))?;
    if host_color.len() != 3 {
        return Err(SimGap(format!(
            "{ctx}: `host_color` must hold exactly 3 bytes, found {}",
            host_color.len()
        )));
    }
    let mut color = [0u8; 3];
    for (index, byte) in host_color.iter().enumerate() {
        color[index] = byte
            .as_integer()
            .and_then(|value| u8::try_from(value).ok())
            .ok_or_else(|| SimGap(format!("{ctx}: `host_color[{index}]` must be 0..=255")))?;
    }
    let preferences = PreferencesState::new(
        id,
        get_byte("light_mode")?,
        get_byte("configuration")?,
        get_byte("deep_sleep")?,
        color,
        get_byte("sleep_timer")?,
        get_byte("debounce")?,
    );

    let slots = match table.get("buttons") {
        Some(value) if value.as_str() == Some("default") => {
            ButtonsState::default_for_profile(id).slots
        }
        Some(value) => parse_button_slots(&ctx, value)?,
        None => {
            return Err(SimGap(format!(
                "{ctx}: missing `buttons` (\"default\" or an array of 18 [action, modifier, key] triples)"
            )));
        }
    };
    let buttons = ButtonsState::new(id, slots);

    Ok((dpi, preferences, buttons))
}

fn parse_button_slots(
    ctx: &str,
    value: &toml::Value,
) -> Result<[ButtonAssignment; BUTTON_SLOT_COUNT], SimGap> {
    let list = value.as_array().ok_or_else(|| {
        SimGap(format!(
            "{ctx}: `buttons` must be \"default\" or an array of {BUTTON_SLOT_COUNT} triples"
        ))
    })?;
    if list.len() != BUTTON_SLOT_COUNT {
        return Err(SimGap(format!(
            "{ctx}: `buttons` must hold exactly {BUTTON_SLOT_COUNT} triples, found {}",
            list.len()
        )));
    }
    let mut slots = [ButtonAssignment::default(); BUTTON_SLOT_COUNT];
    for (index, entry) in list.iter().enumerate() {
        let triple = entry.as_array().ok_or_else(|| {
            SimGap(format!(
                "{ctx}: `buttons[{index}]` must be [action, modifier, key]"
            ))
        })?;
        if triple.len() != 3 {
            return Err(SimGap(format!(
                "{ctx}: `buttons[{index}]` must hold exactly 3 bytes, found {}",
                triple.len()
            )));
        }
        let mut raw = [0u8; 3];
        for (offset, byte) in triple.iter().enumerate() {
            raw[offset] = byte
                .as_integer()
                .and_then(|value| u8::try_from(value).ok())
                .ok_or_else(|| {
                    SimGap(format!(
                        "{ctx}: `buttons[{index}][{offset}]` must be 0..=255"
                    ))
                })?;
        }
        slots[index] = ButtonAssignment::new(raw[0], raw[1], raw[2]);
    }
    Ok(slots)
}

fn parse_hex_bytes(
    name: &str,
    profile: u8,
    key: &str,
    hex: &str,
    expected: usize,
) -> Result<[u8; WATERMARK_LENGTH], SimGap> {
    if hex.len() != expected * 2 {
        return Err(SimGap(format!(
            "{name}: [profiles.{profile}]: `{key}` must be exactly {} hex chars, found {}",
            expected * 2,
            hex.len()
        )));
    }
    let mut out = [0u8; WATERMARK_LENGTH];
    for (index, byte) in out.iter_mut().enumerate() {
        let pair = &hex[index * 2..index * 2 + 2];
        *byte = u8::from_str_radix(pair, 16).map_err(|_| {
            SimGap(format!(
                "{name}: [profiles.{profile}]: `{key}` is not lowercase hex at byte {index} ({pair:?})"
            ))
        })?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_fixtures_parse() {
        for name in ["stock", "tuned", "foreign-token", "two-ble-unknown"] {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/sim")
                .join(format!("{name}.toml"));
            let scenario = load_scenario(&path).expect("shipped fixture loads");
            assert!(!scenario.endpoints.is_empty(), "{name}: needs endpoints");
        }
    }

    #[test]
    fn shipped_fixture_shapes() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/sim");
        let stock = load_scenario(&dir.join("stock.toml")).expect("stock loads");
        assert_eq!(stock.battery, Some(87));
        assert_eq!(stock.profiles.len(), 5);
        let tuned = load_scenario(&dir.join("tuned.toml")).expect("tuned loads");
        assert_eq!(tuned.battery, Some(64));
        assert_eq!(tuned.profiles.len(), 5);
        let foreign = load_scenario(&dir.join("foreign-token.toml")).expect("foreign loads");
        assert_eq!(foreign.profiles.len(), 5);
        let ble = load_scenario(&dir.join("two-ble-unknown.toml")).expect("ble loads");
        assert_eq!(ble.endpoints.len(), 2);
        assert!(
            ble.endpoints
                .iter()
                .all(|endpoint| endpoint.transport == TransportKind::Ble)
        );
        assert!(ble.profiles.is_empty());
        assert_eq!(ble.battery, None);
    }

    #[test]
    fn toml_syntax_error_names_source() {
        let error =
            parse_scenario("[[endpoints\ntransport = ", "scenario bad").expect_err("bad TOML");
        assert!(
            format!("{error:?}").contains("scenario bad"),
            "names source: {error:?}"
        );
    }

    #[test]
    fn bad_transport_names_value() {
        let text = "[[endpoints]]\ntransport = \"receiver\"\n";
        let error = parse_scenario(text, "scenario bad-transport").expect_err("bad transport");
        let rendered = format!("{error:?}");
        assert!(
            rendered.contains("scenario bad-transport"),
            "names source: {rendered}"
        );
        assert!(rendered.contains("receiver"), "names value: {rendered}");
    }

    #[test]
    fn bad_profile_number_names_section() {
        let text = "[[endpoints]]\ntransport = \"wired\"\n[profiles.9]\n";
        let error = parse_scenario(text, "scenario bad-profile").expect_err("bad profile");
        let rendered = format!("{error:?}");
        assert!(
            rendered.contains("scenario bad-profile"),
            "names source: {rendered}"
        );
        assert!(rendered.contains("profiles.9"), "names section: {rendered}");
    }

    #[test]
    fn bad_dpi_stage_names_profile() {
        let text = "[[endpoints]]\ntransport = \"wired\"\n\
            [profiles.1]\ndpi_stages = [7]\nactive_stage = 1\n\
            dpi_tail = \"00000000000000000000000000000000000000000000000000\"\n\
            light_mode = 0\nconfiguration = 3\ndeep_sleep = 168\n\
            host_color = [0, 0, 255]\nsleep_timer = 1\ndebounce = 4\nbuttons = \"default\"\n";
        let error = parse_scenario(text, "scenario bad-stage").expect_err("bad stage");
        let rendered = format!("{error:?}");
        assert!(
            rendered.contains("scenario bad-stage"),
            "names source: {rendered}"
        );
        assert!(rendered.contains("profiles.1"), "names profile: {rendered}");
    }
}
