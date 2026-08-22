use std::{env, error::Error, time::Duration};

use attack_shark_x3::{DeviceSelector, MouseHandle, ProfileId, UsbDeviceKind};
use tokio::time::sleep;

// Logical 09 83 -> sliced to 3x 09 40 fragments per X3.exe 0x414340 / 0x413570.
// We build P0/P1/P2 directly per docs/protocols/09-custom-macros.md tables (FA61 live = 09 40 tail)
// and 08 bind 12 00 <slot>. Checksum = sum(P0[8..64])+sum(P1[4..64]) BE at P2[10..11].

fn usage() -> String {
    format!(
        "usage: macro-wired --profile <1..5> --slot <1|2|3|4|7|8> --macro <press-A|press-B> [--transport wired|receiver] [--dry-run]\n\
         slots: 1 Left(01) 2 Right(02) 3 Middle(03) 4 DPI(04) 7 Forward(07) 8 Backward(08) — array slots 1,2,3,4,7,8 (no wheel 5/6)\n\
         macros: press-A = 04 01 0a00 / 04 02 0a00 -> round 01/81 + 04 ; press-B = 05"
    )
}

fn parse_args() -> Result<(ProfileId, u8, String, UsbDeviceKind, bool), String> {
    let mut it = env::args().skip(1);
    let mut profile: Option<u8> = None;
    let mut slot: Option<u8> = None;
    let mut macro_name: Option<String> = None;
    let mut transport = UsbDeviceKind::Wired;
    let mut dry_run = false;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--profile" => profile = Some(it.next().ok_or("--profile requires value")?.parse().map_err(|e| format!("{e}"))?),
            "--slot" => slot = Some(it.next().ok_or("--slot requires value")?.parse().map_err(|e| format!("{e}"))?),
            "--macro" => macro_name = Some(it.next().ok_or("--macro requires value")?),
            "--transport" => {
                let v = it.next().ok_or("--transport requires wired|receiver")?;
                transport = match v.as_str() {
                    "wired" => UsbDeviceKind::Wired,
                    "receiver" => UsbDeviceKind::Receiver,
                    _ => return Err("transport must be wired|receiver".into()),
                }
            }
            "--dry-run" => dry_run = true,
            "--help" | "-h" => return Err(usage()),
            _ => return Err(format!("unknown arg {a}\n{}", usage())),
        }
    }
    let p = ProfileId::try_from(profile.ok_or("missing --profile")?).map_err(|e| format!("{e}"))?;
    let s = slot.ok_or("missing --slot")?;
    if ![1, 2, 3, 4, 7, 8].contains(&s) {
        return Err("slot must be one of 1,2,3,4,7,8 (no wheel 5/6, no slot 5/6)".into());
    }
    let m = macro_name.ok_or("missing --macro press-A|press-B")?;
    if m != "press-A" && m != "press-B" {
        return Err("macro must be press-A|press-B".into());
    }
    Ok((p, s, m, transport, dry_run))
}

fn build_packets(profile: ProfileId, slot: u8, macro_name: &str, kind: UsbDeviceKind) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
    let wire_id = slot; // slot == wire Button ID for tested 01,02,03,04,07,08 (static 01/02/03/07/08 + DPI 04)
    let key: u8 = if macro_name == "press-A" { 0x04 } else { 0x05 }; // HID usage A=04 B=05
    // P0: 64B
    let mut p0 = vec![0u8; 64];
    p0[0] = 0x09;
    p0[1] = 0x40; // wired always 40; receiver P0 also 40 (only P2 tail differs)
    p0[2] = wire_id;
    p0[3] = 0x00; // page 0
    p0[4] = 0x00; // play mode Times (rec+0x0C)
    // p0[8] repeat 1
    p0[8] = 0x01;
    // count 2 events
    p0[29] = 0x02;
    // events: 100ms press + release using round(100/10)=10 -> 0x0A / 0x8A (probe only, 10ms in prod)
    p0[30] = 0x0A;
    p0[31] = key;
    p0[32] = 0x8A;
    p0[33] = key;

    // P1: 64B
    let mut p1 = vec![0u8; 64];
    p1[0] = 0x09;
    p1[1] = 0x40;
    p1[2] = wire_id;
    p1[3] = 0x01;
    // payload 0

    // checksum = sum(p0[8..64]) + sum(p1[4..64])
    let sum: u16 = p0[8..64].iter().map(|b| *b as u16).sum::<u16>()
        + p1[4..64].iter().map(|b| *b as u16).sum::<u16>();

    // P2: 64B
    let mut p2 = vec![0u8; 64];
    p2[0] = 0x09;
    p2[1] = if kind == UsbDeviceKind::Receiver { 0x0C } else { 0x40 }; // 0x413845 transport-gated
    p2[2] = wire_id;
    p2[3] = 0x02;
    p2[10] = (sum >> 8) as u8;
    p2[11] = (sum & 0xFF) as u8;

    // 08 bind: 59B building from current table would be ideal, but probe uses a minimal
    // table derived from live: header 08 3b <profile> + 18*3 slots, with target slot = 12 00 <slot>.
    // To avoid clobbering other slots, we read current table via handle.read_buttons and patch one slot.
    // For dry-run/packet dump we emit a minimal packet with that one slot set; caller should patch via read.
    // Here we emit the 59B packet that the probe will patch after a live read when not dry-run.
    let mut b08 = vec![0u8; 59];
    b08[0] = 0x08;
    b08[1] = 0x3b;
    b08[2] = profile.get();
    // slots initially zeroed; placeholder — Send will be patched after live read in main()
    // encode target: offset 3 + (slot_index*3) where slot_index = slot-1 for 1..4 but 7->6, 8->7
    let idx = match slot { 1 => 0, 2 => 1, 3 => 2, 4 => 3, 7 => 6, 8 => 7, _ => 0 };
    let off = 3 + idx * 3;
    b08[off] = 0x12;
    b08[off + 1] = 0x00;
    b08[off + 2] = wire_id; // reference == slot/wireId per 0x414705

    // patch checksum sum16 slots[3..56] BE at 57..58
    let sum08: u16 = b08[3..57].iter().map(|b| *b as u16).sum::<u16>() & 0xFFFF;
    b08[57] = (sum08 >> 8) as u8;
    b08[58] = (sum08 & 0xFF) as u8;

    (b08, p0, p1, p2)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let (profile, slot, macro_name, kind, dry_run) = parse_args().map_err(|e| format!("{e}\n{}", usage()))?;

    let (b08_template, p0, p1, p2) = build_packets(profile, slot, &macro_name, kind);

    if dry_run {
        println!("dry-run profile={} slot={} wireId={:02x} macro={} transport={:?}", profile.get(), slot, slot, macro_name, kind);
        println!("08 bind packet: {}", hex(&b08_template));
        println!("09 P0: {}", hex(&p0));
        println!("09 P1: {}", hex(&p1));
        println!("09 P2: {} (checksum {:04x} tail {:02x})", hex(&p2), ((p2[10] as u16) << 8) | p2[11] as u16, p2[1]);
        println!("note: 08 slots other than target are zero in dry-run; live run patches via read_buttons for merge");
        return Ok(());
    }

    // Live: open handle, backup 08 table, patch single slot, then send 09 sliced + 08 bind atomically per profile
    let handle = MouseHandle::open_for_kind(DeviceSelector::Unique, kind)?;
    // settle like receiver-smoke
    sleep(Duration::from_millis(300)).await;

    let meta = handle.read_profile_metadata().await?;
    println!("device metadata current={} max={}", meta.current().get(), meta.maximum().get());
    let _snap = handle.read_profile(profile).await?;
    println!("live backup profile {} read", profile.get());
    // Safe read-modify-write for 08: patch single slot to 12 00 <slot> and recompute sum via ButtonsReport
    let live_buttons = handle.read_buttons(profile).await?;
    let mut patched_slots = live_buttons.slots;
    let idx = match slot { 1 => 0, 2 => 1, 3 => 2, 4 => 3, 7 => 6, 8 => 7, _ => 0 };
    patched_slots[idx] = attack_shark_x3::ButtonAssignment::new(0x12, 0x00, slot);
    let patched_state = attack_shark_x3::ButtonsState::new(profile, patched_slots);
    let b08_live_bytes = attack_shark_x3::ButtonsReport::encode(&patched_state).as_bytes().to_vec();
    sleep(Duration::from_millis(200)).await;

    // Send slices: wired expects Sleep 200 after each, receiver 1000 per 0x41369B / 0x413853
    let delay = if kind == UsbDeviceKind::Receiver { Duration::from_millis(1000) } else { Duration::from_millis(200) };
    handle.send_raw_feature_report(p0.clone()).await?;
    sleep(delay).await;
    println!("sent P0 09 40 {} 00 ({} bytes) -> transp ack", slot, p0.len());
    handle.send_raw_feature_report(p1.clone()).await?;
    sleep(delay).await;
    println!("sent P1 09 40 {} 01", slot);
    handle.send_raw_feature_report(p2.clone()).await?;
    sleep(delay).await;
    println!("sent P2 09 {:02x} {} 02 checksum {:02x}{:02x}", p2[1], slot, p2[10], p2[11]);
    handle.send_raw_feature_report(b08_live_bytes.clone()).await?;
    sleep(delay).await;
    println!("sent 08 bind 12 00 {:02x} for profile {} slot {}", slot, profile.get(), slot);

    // If we fetched live_buttons correctly, we would have sent a merged ButtonsState via handle.write_buttons — left as TODO for manager path.


    println!("done — now press the bound button (slot {}) within 5s to see macro {} fire (A=04/B=05 10ms)", slot, macro_name);
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join("")
}
