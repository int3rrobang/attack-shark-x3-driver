//! `watermark_probe` — diagnostic probe for candidate firmware watermark surfaces.
//!
//! Research tool for deciding whether the DPI opaque tail (report `0x04`,
//! bytes 25..=49) or the unused button slots (report `0x08`, slots 9..=15)
//! are viable driver-owned watermark surfaces.
//!
//! It deliberately lives at the protocol/transport layer (not in the manager
//! or `x3ctl`) and is explicitly diagnostic-only. It lets a human mark both
//! candidate regions with conspicuous test bytes, back them up, then run the
//! stock Attack Shark / Kysona software between `dump` invocations and diff
//! the before/after state. The result tells us whether stock profile
//! switching / settings edits reconstruct and overwrite these regions.
//!
//! WARNING: this is a research probe, not a production tool. It performs
//! temporary hardware writes. Keep the backup file (`mark` prints its path)
//! and use `restore` to return the device to its exact prior state.
//!
//! Commands:
//!   watermark_probe list
//!   watermark_probe dump --path <hid-path> [--out <file>]
//!   watermark_probe mark --path <hid-path> [--backup <file>]
//!                        [--dpi-marker <hex3>] [--button-slot <9..=15>] [--button-marker <hex3>]
//!   watermark_probe restore --path <hid-path> --backup <file>
//!   watermark_probe diff <before.json> <after.json>
//!
//! Run:
//!   cargo run -p attack-shark-x3 --example watermark_probe --features usb -- list

use attack_shark_x3::{
    ButtonAssignment, ButtonsReport, ButtonsState, DeviceSelector, DpiReport, DpiState,
    MouseHandle, ProfileId, ReadbackRequest, UsbDeviceKind, list_devices, list_devices_for,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Candidate watermark surfaces and the default test markers.
//
// Only the hidden button slots 9..=15 may be touched; visible physical button
// slots (0..8, 16, 17) are never modified by this probe.
// ---------------------------------------------------------------------------

/// DPI opaque-tail offset within `DpiState::preserved_tail` / packet bytes 25..=27.
const DPI_TAIL_MARKER_OFFSET: usize = 0;
/// The packet byte where the DPI tail marker lives (report `0x04` byte 25).
const DPI_TAIL_PACKET_OFFSET: usize = 25;
/// Default DPI tail marker: a conspicuous, recognizable byte triple.
const DPI_TAIL_MARKER: [u8; 3] = [0xba, 0xbe, 0xfa];

/// Default hidden button slot used for the marker (within 9..=15).
const BUTTON_SLOT_INDEX: usize = 9;
/// Default button marker: keeps `action = 0x01` (Disable) so the slot stays
/// disabled and the marker rides in the ignored modifier/key bytes.
const BUTTON_SLOT_MARKER: [u8; 3] = [0x01, 0xa5, 0x5a];

const BUTTON_SLOTS_START: usize = 3;
const BUTTON_SLOT_COUNT: usize = 18;
const BUTTON_MIN_HIDDEN_SLOT: usize = 9;
const BUTTON_MAX_HIDDEN_SLOT: usize = 15;

// ---------------------------------------------------------------------------
// JSON payloads
// ---------------------------------------------------------------------------

/// One `dump` snapshot; machine-readable and easy to diff.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct DumpReport {
    timestamp: String,
    device_path: String,
    product_id: u16,
    transport: String,
    profile: u8,
    maximum_profile: u8,
    /// Contiguous lowercase hex of the raw `0x04` readback bytes.
    dpi_raw: String,
    /// Contiguous lowercase hex of the raw `0x08` readback bytes.
    buttons_raw: String,
    /// Hex of the DPI opaque tail (bytes 25..=49).
    dpi_tail: String,
    /// Per-slot hex triplets of the button table (slot index -> hex).
    button_slots: BTreeMap<String, String>,
}

/// The pre-`mark` snapshot used by `restore`.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct BackupFile {
    timestamp: String,
    device_path: String,
    product_id: u16,
    transport: String,
    profile: u8,
    dpi_raw: String,
    buttons_raw: String,
}

// ---------------------------------------------------------------------------
// Marker application helpers (pure, unit-tested)
// ---------------------------------------------------------------------------

fn apply_dpi_marker(state: &mut DpiState, marker: [u8; 3]) {
    let start = DPI_TAIL_MARKER_OFFSET;
    state.preserved_tail[start..start + 3].copy_from_slice(&marker);
}

fn apply_button_marker(state: &mut ButtonsState, slot: usize, marker: [u8; 3]) {
    debug_assert!(
        (BUTTON_MIN_HIDDEN_SLOT..=BUTTON_MAX_HIDDEN_SLOT).contains(&slot),
        "only hidden slots 9..=15 may be marked"
    );
    state.slots[slot] = ButtonAssignment::new(marker[0], marker[1], marker[2]);
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        print_usage();
        return Ok(());
    }
    match args[1].as_str() {
        "list" => cmd_list(),
        "dump" => cmd_dump(&args).await,
        "mark" => cmd_mark(&args).await,
        "restore" => cmd_restore(&args).await,
        "diff" => cmd_diff(&args),
        "-h" | "--help" => {
            print_usage();
            Ok(())
        }
        other => {
            eprintln!("unknown command: {other}\n");
            print_usage();
            Ok(())
        }
    }
}

fn print_usage() {
    println!(
        "watermark_probe — X3/M600 watermark-surface probe (research only)\n\
         \n\
         USAGE\n\
         \x20 watermark_probe list\n\
         \x20 watermark_probe dump --path <hid-path> [--out <file>]\n\
         \x20 watermark_probe mark --path <hid-path> [--backup <file>]\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20[--dpi-marker <hex3>] [--button-slot <9..=15>] [--button-marker <hex3>]\n\
         \x20 watermark_probe restore --path <hid-path> --backup <file>\n\
         \x20 watermark_probe diff <before.json> <after.json>\n\
         \n\
         list     enumerate wired/receiver X3/M600 configuration interfaces\n\
         dump     open an exact path, read 0x0c metadata + raw 0x04/0x08, print JSON\n\
         mark     back up, write conspicuous markers to the DPI tail and one hidden\n\
                  button slot, verify, print the backup path\n\
         restore  restore the exact original 0x04/0x08 from a backup and verify\n\
         diff     report changed bytes / slots and marker state between two dumps\n\
         \n\
         Default markers: DPI tail bytes {}-{} = {}; button slot {} = {}.\n\
         A deliberate ~500 ms receiver pause is normal for each armed read.",
        DPI_TAIL_PACKET_OFFSET,
        DPI_TAIL_PACKET_OFFSET + 2,
        hex(&DPI_TAIL_MARKER),
        BUTTON_SLOT_INDEX,
        hex(&BUTTON_SLOT_MARKER),
    );
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

fn cmd_list() -> Result<(), Box<dyn Error>> {
    let mut found = 0;
    println!("transport\tpath\tvid\tpid\tinterface\tproduct\tserial");
    for kind in [UsbDeviceKind::Wired, UsbDeviceKind::Receiver] {
        for dev in list_devices_for(kind)? {
            found += 1;
            println!(
                "{}\t{}\t0x{:04x}\t0x{:04x}\t{}\t{}\t{}",
                transport_label(kind),
                dev.path,
                dev.vendor_id,
                dev.product_id,
                dev.interface_number,
                dev.product.as_deref().unwrap_or(""),
                dev.serial_number.as_deref().unwrap_or(""),
            );
        }
    }
    println!("\n{} configuration collection(s) found", found);
    if found == 0 {
        eprintln!("hint: plug the mouse (or receiver dongle) in and retry.");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// dump
// ---------------------------------------------------------------------------

async fn cmd_dump(args: &[String]) -> Result<(), Box<dyn Error>> {
    let path = require_flag(args, "--path")?;
    let out = optional_flag(args, "--out");

    let (handle, kind) = open_by_path(&path).await?;
    let metadata = handle.read_profile_metadata().await?;
    let current = metadata.current();
    let dpi_raw = handle
        .read_raw_readback(ReadbackRequest::Dpi(current))
        .await?;
    let buttons_raw = handle
        .read_raw_readback(ReadbackRequest::Buttons(current))
        .await?;
    let report = build_dump(
        &path,
        kind,
        current.get(),
        metadata.maximum().get(),
        &dpi_raw,
        &buttons_raw,
    )?;
    let json = serde_json::to_string_pretty(&report)?;
    match out {
        Some(file) => fs::write(&file, json)?,
        None => println!("{json}"),
    }
    Ok(())
}

fn build_dump(
    path: &str,
    kind: UsbDeviceKind,
    profile: u8,
    maximum_profile: u8,
    dpi_raw: &[u8],
    buttons_raw: &[u8],
) -> Result<DumpReport, Box<dyn Error>> {
    if dpi_raw.len() < 50 {
        return Err(format!(
            "0x04 readback too short ({}) to expose the tail",
            dpi_raw.len()
        )
        .into());
    }
    if buttons_raw.len() < BUTTON_SLOTS_START + BUTTON_SLOT_COUNT * 3 {
        return Err("0x08 readback too short to expose the slot table".into());
    }
    let mut slots = BTreeMap::new();
    for index in 0..BUTTON_SLOT_COUNT {
        let offset = BUTTON_SLOTS_START + index * 3;
        slots.insert(index.to_string(), hex(&buttons_raw[offset..offset + 3]));
    }
    Ok(DumpReport {
        timestamp: now_iso(),
        device_path: path.to_owned(),
        product_id: kind.product_id(),
        transport: transport_label(kind).to_owned(),
        profile,
        maximum_profile,
        dpi_raw: hex(dpi_raw),
        buttons_raw: hex(buttons_raw),
        dpi_tail: hex(&dpi_raw[DPI_TAIL_PACKET_OFFSET..50]),
        button_slots: slots,
    })
}

// ---------------------------------------------------------------------------
// mark
// ---------------------------------------------------------------------------

async fn cmd_mark(args: &[String]) -> Result<(), Box<dyn Error>> {
    let path = require_flag(args, "--path")?;
    let backup = optional_flag(args, "--backup").unwrap_or_else(|| default_backup_name(&path));
    let dpi_marker = parse_marker(optional_flag(args, "--dpi-marker"), &DPI_TAIL_MARKER)?;
    let button_slot = optional_flag(args, "--button-slot")
        .map(|s| s.parse::<usize>())
        .transpose()?
        .unwrap_or(BUTTON_SLOT_INDEX);
    let button_marker = parse_marker(optional_flag(args, "--button-marker"), &BUTTON_SLOT_MARKER)?;
    if !(BUTTON_MIN_HIDDEN_SLOT..=BUTTON_MAX_HIDDEN_SLOT).contains(&button_slot) {
        return Err(format!(
            "button slot must be within {}..={}, got {button_slot}",
            BUTTON_MIN_HIDDEN_SLOT, BUTTON_MAX_HIDDEN_SLOT
        )
        .into());
    }
    if button_marker[0] != 0x01 {
        eprintln!(
            "note: button marker action byte is 0x{:02x}, not 0x01 (Disable); \
             the marked slot may become a live binding",
            button_marker[0]
        );
    }

    let (handle, kind) = open_by_path(&path).await?;
    let metadata = handle.read_profile_metadata().await?;
    let current = metadata.current();

    // 1. Back up the exact original 0x04 and 0x08 readbacks.
    let dpi_raw = handle
        .read_raw_readback(ReadbackRequest::Dpi(current))
        .await?;
    let buttons_raw = handle
        .read_raw_readback(ReadbackRequest::Buttons(current))
        .await?;
    let backup_file = BackupFile {
        timestamp: now_iso(),
        device_path: path.clone(),
        product_id: kind.product_id(),
        transport: transport_label(kind).to_owned(),
        profile: current.get(),
        dpi_raw: hex(&dpi_raw),
        buttons_raw: hex(&buttons_raw),
    };
    fs::write(&backup, serde_json::to_string_pretty(&backup_file)?)?;

    // 2. Apply markers through the typed encoders (checksums recomputed).
    let mut dpi_state = DpiReport::decode(&dpi_raw, kind.transport_kind(), current)?.state;
    apply_dpi_marker(&mut dpi_state, dpi_marker);
    let _ = handle.write_dpi(dpi_state).await?;

    let mut buttons_state = ButtonsReport::decode(&buttons_raw, current)?.state;
    apply_button_marker(&mut buttons_state, button_slot, button_marker);
    let _ = handle.write_buttons(buttons_state).await?;

    // 3. Re-read and verify both markers are present.
    let dpi_after = handle
        .read_raw_readback(ReadbackRequest::Dpi(current))
        .await?;
    let buttons_after = handle
        .read_raw_readback(ReadbackRequest::Buttons(current))
        .await?;
    if dpi_after.get(DPI_TAIL_PACKET_OFFSET..DPI_TAIL_PACKET_OFFSET + 3)
        != Some(dpi_marker.as_slice())
    {
        return Err("DPI tail marker was not preserved in the readback".into());
    }
    let slot_offset = BUTTON_SLOTS_START + button_slot * 3;
    if buttons_after.get(slot_offset..slot_offset + 3) != Some(button_marker.as_slice()) {
        return Err(format!("button marker was not preserved in slot {button_slot}").into());
    }

    println!("markers applied and verified on {path}");
    println!(
        "  dpi tail bytes {}-{}: {}",
        DPI_TAIL_PACKET_OFFSET,
        DPI_TAIL_PACKET_OFFSET + 2,
        hex(&dpi_marker)
    );
    println!("  button slot {button_slot}: {}", hex(&button_marker));
    println!("backup saved to: {backup}");
    Ok(())
}

// ---------------------------------------------------------------------------
// restore
// ---------------------------------------------------------------------------

async fn cmd_restore(args: &[String]) -> Result<(), Box<dyn Error>> {
    let path = require_flag(args, "--path")?;
    let backup = require_flag(args, "--backup")?;

    let backup_file: BackupFile = serde_json::from_str(&fs::read_to_string(&backup)?)?;
    let dpi_orig = unhex(&backup_file.dpi_raw)?;
    let buttons_orig = unhex(&backup_file.buttons_raw)?;

    let (handle, kind) = open_by_path(&path).await?;
    let current = ProfileId::try_from(backup_file.profile)?;

    // Decode the backed-up readbacks into typed state and write them back
    // losslessly through the typed encoders (checksums recomputed), matching
    // the transport framing the normal read path accepts.
    let dpi_state = DpiReport::decode(&dpi_orig, kind.transport_kind(), current)?.state;
    let _ = handle.write_dpi(dpi_state).await?;
    let buttons_state = ButtonsReport::decode(&buttons_orig, current)?.state;
    let _ = handle.write_buttons(buttons_state).await?;

    // Verify the final readback equals the saved original, byte for byte.
    let dpi_after = handle
        .read_raw_readback(ReadbackRequest::Dpi(current))
        .await?;
    let buttons_after = handle
        .read_raw_readback(ReadbackRequest::Buttons(current))
        .await?;
    if dpi_after != dpi_orig {
        return Err(format!(
            "0x04 did not restore exactly: {} byte(s) differ from backup",
            changed_indices(&dpi_after, &dpi_orig).len()
        )
        .into());
    }
    if buttons_after != buttons_orig {
        return Err(format!(
            "0x08 did not restore exactly: {} byte(s) differ from backup",
            changed_indices(&buttons_after, &buttons_orig).len()
        )
        .into());
    }
    println!("restored 0x04 and 0x08 exactly; readback matches backup {backup}");
    Ok(())
}

// ---------------------------------------------------------------------------
// diff
// ---------------------------------------------------------------------------

fn cmd_diff(args: &[String]) -> Result<(), Box<dyn Error>> {
    let before_path = args.get(2).ok_or("diff needs two dump JSON files")?;
    let after_path = args.get(3).ok_or("diff needs two dump JSON files")?;
    let before: DumpReport = serde_json::from_str(&fs::read_to_string(before_path)?)?;
    let after: DumpReport = serde_json::from_str(&fs::read_to_string(after_path)?)?;

    let before_dpi = unhex(&before.dpi_raw)?;
    let after_dpi = unhex(&after.dpi_raw)?;
    let before_btn = unhex(&before.buttons_raw)?;
    let after_btn = unhex(&after.buttons_raw)?;

    let dpi_changed = changed_indices(&before_dpi, &after_dpi);
    println!("0x04 bytes changed: {}", display_indices(&dpi_changed));
    println!(
        "DPI test marker (bytes {}-{} {}): {}",
        DPI_TAIL_PACKET_OFFSET,
        DPI_TAIL_PACKET_OFFSET + 2,
        hex(&DPI_TAIL_MARKER),
        marker_state(
            &before_dpi,
            &after_dpi,
            DPI_TAIL_PACKET_OFFSET,
            &DPI_TAIL_MARKER
        )
    );

    let slot_changed = changed_slots(&before_btn, &after_btn);
    println!("0x08 slots changed: {}", display_indices(&slot_changed));
    let slot_offset = BUTTON_SLOTS_START + BUTTON_SLOT_INDEX * 3;
    println!(
        "button test marker (slot {} {}): {}",
        BUTTON_SLOT_INDEX,
        hex(&BUTTON_SLOT_MARKER),
        marker_state(&before_btn, &after_btn, slot_offset, &BUTTON_SLOT_MARKER)
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

async fn open_by_path(path: &str) -> Result<(MouseHandle, UsbDeviceKind), Box<dyn Error>> {
    let kind = resolve_kind(path)?;
    let handle = MouseHandle::open_for_kind(DeviceSelector::path(path), kind)?;
    Ok((handle, kind))
}

fn resolve_kind(path: &str) -> Result<UsbDeviceKind, Box<dyn Error>> {
    if list_devices()?.iter().any(|d| d.path == path) {
        return Ok(UsbDeviceKind::Wired);
    }
    if list_devices_for(UsbDeviceKind::Receiver)?
        .iter()
        .any(|d| d.path == path)
    {
        return Ok(UsbDeviceKind::Receiver);
    }
    Err(format!("no X3 configuration collection with path {path:?}").into())
}

const fn transport_label(kind: UsbDeviceKind) -> &'static str {
    match kind {
        UsbDeviceKind::Wired => "wired",
        UsbDeviceKind::Receiver => "receiver",
    }
}

fn default_backup_name(path: &str) -> String {
    let stamp = now_iso().replace(':', "");
    let safe = path.replace(['\\', '/', ':'], "_");
    format!("watermark-backup-{safe}-{stamp}.json")
}

fn require_flag(args: &[String], name: &str) -> Result<String, Box<dyn Error>> {
    optional_flag(args, name).ok_or_else(|| format!("missing required flag {name}").into())
}

fn optional_flag(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

fn parse_marker(arg: Option<String>, default: &[u8; 3]) -> Result<[u8; 3], Box<dyn Error>> {
    match arg {
        Some(value) => {
            let bytes = unhex(&value)?;
            if bytes.len() != 3 {
                return Err(
                    format!("marker must be exactly 3 bytes (6 hex chars), got {bytes:?}").into(),
                );
            }
            let mut marker = [0_u8; 3];
            marker.copy_from_slice(&bytes);
            Ok(marker)
        }
        None => Ok(*default),
    }
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn unhex(value: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    if !value.len().is_multiple_of(2) {
        return Err(format!("odd-length hex string: {value}").into());
    }
    (0..value.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&value[i..i + 2], 16)
                .map_err(|e| format!("bad hex byte: {e}").into())
        })
        .collect()
}

fn changed_indices(before: &[u8], after: &[u8]) -> Vec<usize> {
    let len = before.len().max(after.len());
    (0..len)
        .filter(|&i| before.get(i) != after.get(i))
        .collect()
}

fn changed_slots(before: &[u8], after: &[u8]) -> Vec<usize> {
    (0..BUTTON_SLOT_COUNT)
        .filter(|&slot| {
            let offset = BUTTON_SLOTS_START + slot * 3;
            let end = offset + 3;
            before.get(offset..end) != after.get(offset..end)
        })
        .collect()
}

fn display_indices(indices: &[usize]) -> String {
    if indices.is_empty() {
        "none".to_owned()
    } else {
        format!("{indices:?}")
    }
}

fn marker_state(before: &[u8], after: &[u8], offset: usize, marker: &[u8; 3]) -> &'static str {
    let before_had = before.get(offset..offset + 3) == Some(marker.as_slice());
    let after_has = after.get(offset..offset + 3) == Some(marker.as_slice());
    match (before_had, after_has) {
        (true, true) => "preserved",
        (true, false) => "absent",
        (false, true) => "added",
        (false, false) => "absent (not present in baseline)",
    }
}

fn now_iso() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let secs_of_day = secs % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    let (hour, minute, second) = (
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60,
        secs_of_day % 60,
    );
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Convert days since the Unix epoch to a (year, month, day) civil date.
/// Uses Howard Hinnant's `civil_from_days` algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let shifted = z + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month as u32, day)
}

// ---------------------------------------------------------------------------
// Offline unit tests (no hardware)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use attack_shark_x3::{DpiValue, StageIndex, TransportKind};

    fn profile(value: u8) -> ProfileId {
        ProfileId::try_from(value).expect("test profile must be valid")
    }

    fn sample_dpi() -> DpiState {
        let stages = vec![
            DpiValue::try_from(800).unwrap(),
            DpiValue::try_from(1600).unwrap(),
            DpiValue::try_from(2400).unwrap(),
        ];
        let tail = [
            0xff, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0x00, 0x00, 0xff,
            0xff, 0xff, 0x00, 0xff, 0xff, 0x40, 0x00, 0xff, 0xff, 0xff, 0x01,
        ];
        DpiState::new(profile(1), stages, StageIndex::try_from(1).unwrap(), tail).unwrap()
    }

    #[test]
    fn dpi_marker_changes_only_tail_bytes() {
        let mut marked = sample_dpi();
        apply_dpi_marker(&mut marked, DPI_TAIL_MARKER);
        let original = DpiReport::encode(&sample_dpi(), TransportKind::Wired)
            .unwrap()
            .as_full_bytes()
            .to_vec();
        let changed = DpiReport::encode(&marked, TransportKind::Wired)
            .unwrap()
            .as_full_bytes()
            .to_vec();
        for index in 0..50 {
            let in_marker = (DPI_TAIL_PACKET_OFFSET..DPI_TAIL_PACKET_OFFSET + 3).contains(&index);
            if in_marker {
                assert_ne!(
                    original[index], changed[index],
                    "byte {index} is the marker and must change"
                );
            } else {
                assert_eq!(
                    original[index], changed[index],
                    "byte {index} must be unchanged by the DPI marker"
                );
            }
        }
    }

    #[test]
    fn button_marker_changes_only_selected_hidden_slot() {
        let mut marked = ButtonsState::default_for_profile(profile(1));
        apply_button_marker(&mut marked, BUTTON_SLOT_INDEX, BUTTON_SLOT_MARKER);
        let original = ButtonsReport::encode(&ButtonsState::default_for_profile(profile(1)))
            .as_bytes()
            .to_vec();
        let changed = ButtonsReport::encode(&marked).as_bytes().to_vec();
        for slot in 0..BUTTON_SLOT_COUNT {
            let offset = BUTTON_SLOTS_START + slot * 3;
            if slot == BUTTON_SLOT_INDEX {
                assert_ne!(
                    &original[offset..offset + 3],
                    &changed[offset..offset + 3],
                    "slot {slot} carries the marker and must change"
                );
            } else {
                assert_eq!(
                    &original[offset..offset + 3],
                    &changed[offset..offset + 3],
                    "slot {slot} must be unchanged by the button marker"
                );
            }
        }
    }

    #[test]
    fn restore_reconstructs_the_original_report_exactly() {
        let original = DpiReport::encode(&sample_dpi(), TransportKind::Wired)
            .unwrap()
            .as_full_bytes()
            .to_vec();
        let decoded = DpiReport::decode(&original, TransportKind::Wired, profile(1))
            .unwrap()
            .state;
        let reencoded = DpiReport::encode(&decoded, TransportKind::Wired)
            .unwrap()
            .as_full_bytes()
            .to_vec();
        assert_eq!(original, reencoded);

        let original_buttons =
            ButtonsReport::encode(&ButtonsState::default_for_profile(profile(1)))
                .as_bytes()
                .to_vec();
        let decoded_buttons = ButtonsReport::decode(&original_buttons, profile(1))
            .unwrap()
            .state;
        let reencoded_buttons = ButtonsReport::encode(&decoded_buttons).as_bytes().to_vec();
        assert_eq!(original_buttons, reencoded_buttons);
    }

    #[test]
    fn hex_round_trips() {
        let bytes = [0x00, 0x0f, 0x10, 0xba, 0xbe, 0xfa, 0xff];
        assert_eq!(unhex(&hex(&bytes)).unwrap(), bytes);
    }
}
