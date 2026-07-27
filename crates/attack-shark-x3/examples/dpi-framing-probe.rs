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
    println!("\n--- backup ---");
    let backup_dpi = handle.read_dpi(profile).await?;
    settle("DPI read").await;
    let backup_rate = handle.read_polling_rate().await?;
    settle("polling-rate read").await;
    let backup_prefs = handle.read_preferences(profile).await?;
    settle("preferences read").await;
    println!("DPI stages: {:?}", backup_dpi.stages);
    println!("active stage: {}", backup_dpi.active_stage);
    println!("polling rate: {backup_rate}");
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
    println!("\n--- health checks ---");

    // (a) DPI readback
    let dpi_ok = match handle.read_dpi(profile).await {
        Ok(state) => {
            let matches = state == backup_dpi;
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
        Err(e) => {
            println!("DPI readback: FAILED ({e})");
            false
        }
    };
    settle("DPI readback").await;

    // (b) Polling rate read (unrelated report 0x06)
    let rate_ok = match handle.read_polling_rate().await {
        Ok(rate) => {
            let matches = rate == backup_rate;
            println!(
                "polling-rate read: OK ({rate}, {} backup)",
                if matches { "matches" } else { "DIFFERS from" }
            );
            matches
        }
        Err(e) => {
            println!("polling-rate read: FAILED ({e})");
            false
        }
    };
    settle("polling-rate read").await;

    // (c) Preferences read (unrelated report 0x05)
    let prefs_ok = match handle.read_preferences(profile).await {
        Ok(prefs) => {
            let matches = prefs == backup_prefs;
            println!(
                "preferences read: OK (debounce={}, {} backup)",
                prefs.debounce,
                if matches { "matches" } else { "DIFFERS from" }
            );
            matches
        }
        Err(e) => {
            println!("preferences read: FAILED ({e})");
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

    // Final health check
    match handle.read_polling_rate().await {
        Ok(rate) => println!("final polling-rate check: {rate} (device responsive)"),
        Err(e) => println!("final polling-rate check: FAILED ({e})"),
    }

    Ok(())
}

async fn settle(operation: &str) {
    sleep(WRITE_DELAY).await;
    let _ = operation;
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
