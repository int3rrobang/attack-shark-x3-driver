use std::error::Error;
use std::time::Duration;

use attack_shark_x3::{
    DeviceSelector, DpiFraming, DpiReport, MouseHandle, ProfileId, UsbDeviceKind,
};
use tokio::time::sleep;

const WRITE_DELAY: Duration = Duration::from_millis(500);

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let use_receiver = std::env::args().any(|a| a == "--receiver");
    let device_label = if use_receiver {
        "FA60 receiver"
    } else {
        "FA61 wired"
    };

    println!("=== DPI Full-Framing Firmware Probe ===");
    println!("opening {device_label} device");
    let handle = MouseHandle::open_for_kind(
        DeviceSelector::Unique,
        if use_receiver {
            UsbDeviceKind::Receiver
        } else {
            UsbDeviceKind::Wired
        },
    )?;
    settle("device open").await;

    let profile = ProfileId::try_from(1)?;

    // --- Backup ---
    // Profile-scoped polling rate requires anchoring the live image: the
    // complete target profile must be loaded/read on the same serialized
    // handle immediately before the live polling-rate read. Report 0x06
    // skips the profile loader, so the alias byte is a wire side effect
    // only and does not by itself prove profile content.
    println!("\n--- backup ---");
    let backup_snapshot = handle.read_profile(profile).await?;
    settle("profile read").await;
    let backup_dpi = backup_snapshot.dpi.clone();
    let backup_prefs = backup_snapshot.preferences;
    let backup_rate = handle.read_live_polling_rate(profile).await?;
    settle("polling-rate read (live, alias is wire side effect)").await;
    println!("DPI stages: {:?}", backup_dpi.stages);
    println!("active stage: {}", backup_dpi.active_stage);
    println!("polling rate (live after loading profile {profile}): {backup_rate}");
    println!("preferences debounce: {}", backup_prefs.debounce);

    // --- Encode full-framed packet ---
    let full_report = DpiReport::encode_framed(&backup_dpi, DpiFraming::Full)?;
    let full_bytes = full_report.as_bytes().to_vec();
    println!("\n--- probe ---");
    println!(
        "sending {}-byte full-framed DPI packet (compact would be {} bytes)",
        full_bytes.len(),
        DpiReport::encode_framed(&backup_dpi, DpiFraming::Compact)?
            .as_bytes()
            .len()
    );
    println!("packet: {}", hex(&full_bytes));

    // --- Send raw ---
    match handle.send_raw_feature_report(full_bytes).await {
        Ok(()) => println!("transport: send succeeded (HID layer accepted the report)"),
        Err(e) => {
            println!("transport: send FAILED: {e}");
            println!("\nRESULT: HID layer rejected the oversized report. Firmware never saw it.");
            return Ok(());
        }
    }
    settle("full-framed write").await;

    // --- Health checks ---
    // To claim a polling rate for profile P, the same serialized handle
    // must have just loaded/read the complete P profile before the live
    // read. Report 0x06 is live-only; alias does not identify content.
    println!("\n--- health checks ---");

    // Reload the complete profile on the same handle so the following
    // live polling-rate read is anchored to profile P's live image.
    let health_snapshot = match handle.read_profile(profile).await {
        Ok(snapshot) => Some(snapshot),
        Err(e) => {
            println!("profile read: FAILED ({e})");
            None
        }
    };
    settle("profile readback").await;

    // (a) DPI readback - derived from the anchored complete profile
    let dpi_ok = match &health_snapshot {
        Some(snapshot) => {
            let state = &snapshot.dpi;
            let matches = *state == backup_dpi;
            println!(
                "DPI readback: OK (state {} backup)",
                if matches { "matches" } else { "DIFFERS from" }
            );
            if !matches {
                println!("  expected stages: {:?}", backup_dpi.stages);
                println!("  actual stages:   {:?}", state.stages);
                println!(
                    "  expected active: {}, actual active: {}",
                    backup_dpi.active_stage, state.active_stage
                );
            }
            matches
        }
        None => {
            println!("DPI readback: FAILED (profile load failed)");
            false
        }
    };

    // (b) Polling rate read - live image after anchoring profile P
    let rate_ok = match handle.read_live_polling_rate(profile).await {
        Ok(rate) => {
            let matches = rate == backup_rate;
            println!(
                "polling-rate read (live, alias {profile} is wire alias only): OK ({rate}, {} backup live rate)",
                if matches { "matches" } else { "DIFFERS from" }
            );
            matches
        }
        Err(e) => {
            println!("polling-rate read (live): FAILED ({e})");
            false
        }
    };
    settle("polling-rate read (live)").await;

    // (c) Preferences read - derived from the same anchored profile
    let prefs_ok = match &health_snapshot {
        Some(snapshot) => {
            let prefs = &snapshot.preferences;
            let matches = *prefs == backup_prefs;
            println!(
                "preferences read: OK (debounce={}, {} backup)",
                prefs.debounce,
                if matches { "matches" } else { "DIFFERS from" }
            );
            matches
        }
        None => {
            println!("preferences read: FAILED (profile load failed)");
            false
        }
    };

    // --- Summary ---
    println!("\n--- summary ---");
    if dpi_ok && rate_ok && prefs_ok {
        println!("RESULT: firmware ACCEPTED the 56-byte full-framed packet.");
        println!("The extra 4 trailing zero bytes did not corrupt state.");
    } else if !dpi_ok && rate_ok && prefs_ok {
        println!("RESULT: firmware accepted the packet but DPI state was CORRUPTED.");
        println!("The device remains responsive to other reports.");
    } else if !rate_ok || !prefs_ok {
        println!("RESULT: device entered an INVALID STATE after the full-framed packet.");
        println!("Unrelated reports failed or returned wrong data.");
    }

    // --- Restore ---
    println!("\n--- restore ---");
    match handle.write_dpi(backup_dpi.clone()).await {
        Ok(restored) => {
            if restored == backup_dpi {
                println!("DPI restored and verified OK");
            } else {
                println!("WARNING: DPI restore readback differs from backup!");
            }
        }
        Err(e) => println!("WARNING: DPI restore failed: {e}"),
    }
    settle("DPI restore").await;

    // Final health check - pure responsiveness, not profile proof.
    // Alias is wire side effect only; result is labeled live.
    match handle.read_live_polling_rate(profile).await {
        Ok(rate) => println!(
            "final live polling-rate check: {rate} (device responsive, live value, alias is wire alias only)"
        ),
        Err(e) => println!("final live polling-rate check: FAILED ({e})"),
    }

    Ok(())
}

async fn settle(operation: &str) {
    sleep(WRITE_DELAY).await;
    let _ = operation;
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join("")
}
