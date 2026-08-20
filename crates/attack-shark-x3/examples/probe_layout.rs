//! Interactive button-layout probe for FA61 (wired) and FA60 (receiver).
//! Maps each physical button to its firmware slot index by temporarily
//! setting candidate slots to left-click and asking the user to test.
//!
//! Run in an interactive terminal:
//!   cargo run --example probe_layout --features usb
//!
//! WARNING: This example performs temporary hardware writes and can leave a
//! mapping active until the device is restored or power-cycled.
//! Keep a known-good mapping and unplug recovery path ready before running it.

use attack_shark_x3::{
    ButtonAssignment, ButtonsState, DeviceSelector, MouseHandle, ProfileId, UsbDeviceKind,
    list_devices, list_devices_for,
};
use std::error::Error;
use std::io::{self, Write};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Discover both wired and receiver devices
    let mut all_devices = Vec::new();
    if let Ok(devs) = list_devices() {
        for d in devs {
            all_devices.push(("FA61 (wired)", d.path.clone(), UsbDeviceKind::Wired));
        }
    }
    if let Ok(devs) = list_devices_for(UsbDeviceKind::Receiver) {
        for d in devs {
            all_devices.push(("FA60 (receiver)", d.path.clone(), UsbDeviceKind::Receiver));
        }
    }

    if all_devices.is_empty() {
        eprintln!("No devices found. Connect via USB and/or receiver dongle.");
        return Ok(());
    }

    println!("=== Attack Shark Button Layout Probe ===\n");
    eprintln!("WARNING: experimental probe; keep a known-good restore path ready.");
    println!("Found {} device(s):", all_devices.len());
    for (label, path, _) in &all_devices {
        println!("  {label}: {path}");
    }

    // --- Explicit interactive confirmation gate before any hardware write ---
    println!("\nWARNING: This probe will temporarily overwrite button mappings on the device(s).");
    println!("It can leave a modified mapping active until restored or power-cycled.");
    println!("Keep a known-good mapping and an unplug recovery path ready before proceeding.");
    print!("Type YES to confirm and continue: ");
    io::stdout().flush()?;
    let mut confirm = String::new();
    io::stdin().read_line(&mut confirm)?;
    if confirm.trim() != "YES" {
        println!("Aborted: confirmation not received. No hardware writes were performed.");
        return Ok(());
    }

    let mut overall_error: Option<Box<dyn Error>> = None;

    for (device_label, path, kind) in &all_devices {
        println!("\n╔══════════════════════════════════════════╗");
        println!("║  DEVICE: {device_label}  ║");
        println!("╚══════════════════════════════════════════╝");

        let handle = match MouseHandle::open_for_kind(DeviceSelector::Path(path.clone()), *kind) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("  Failed to open: {e}");
                continue;
            }
        };

        let profile = ProfileId::try_from(1u8)?;
        let original = match handle.read_buttons(profile).await {
            Ok(b) => b,
            Err(e) => {
                eprintln!("  Failed to read buttons: {e}");
                continue;
            }
        };

        let physical_buttons = [
            "LEFT CLICK",
            "RIGHT CLICK",
            "MIDDLE CLICK (wheel press)",
            "DPI BUTTON (top, behind wheel)",
            "FORWARD (side, upper)",
            "BACKWARD (side, lower)",
        ];

        let mut found_slots: Vec<(usize, &str)> = Vec::new();
        let stdin = io::stdin();
        let mut line = String::new();
        let mut did_write = false;
        let mut probe_error: Option<Box<dyn Error>> = None;

        'outer: for &btn_label in &physical_buttons {
            println!("\n  --- Find the {btn_label} button ---");
            println!("  (each test: only ONE slot = left-click, all others disabled)");
            println!(
                "  Press the {btn_label} button. Type the slot # that left-clicks, or 'n' to skip:"
            );

            for slot in 0..10usize {
                // Skip already-found slots
                if found_slots.iter().any(|(s, _)| *s == slot) {
                    continue;
                }

                let mut slots = [ButtonAssignment::default(); 18];
                slots[slot] = ButtonAssignment::new(0x02, 0x00, 0x00);
                let state = ButtonsState::new(profile, slots);
                if let Err(e) = handle.write_buttons(state).await {
                    probe_error = Some(Box::new(io::Error::other(format!(
                        "write for slot {slot} ({btn_label}) failed: {e}"
                    ))) as Box<dyn Error>);
                    break 'outer;
                }
                did_write = true;

                print!("    [slot {slot}] Press {btn_label} -> left-click? (slot#/n/q): ");
                if let Err(e) = io::stdout().flush() {
                    probe_error = Some(Box::new(e) as Box<dyn Error>);
                    break 'outer;
                }
                line.clear();
                if let Err(e) = stdin.read_line(&mut line) {
                    probe_error = Some(Box::new(e) as Box<dyn Error>);
                    break 'outer;
                }

                let answer = line.trim();
                if answer == "q" {
                    println!("  Quitting probe for this device.");
                    break 'outer;
                }
                if answer == "n" {
                    continue;
                }
                if let Ok(num) = answer.parse::<usize>() {
                    if num == slot {
                        found_slots.push((slot, btn_label));
                        println!("    ✓ {btn_label} = slot {slot}");
                        break;
                    } else {
                        println!(
                            "    Mismatch: you typed {num} but test was slot {slot}. Retrying..."
                        );
                    }
                }
            }
        }

        // --- Restoration is attempted on every post-write exit ---
        if did_write {
            println!("\n  Restoring original button mapping...");
            match handle.write_buttons(original).await {
                Ok(restored) => {
                    if restored != original {
                        eprintln!(
                            "  Restoration write mismatch: device did not return to original mapping"
                        );
                        eprintln!("  Expected: {original:?}");
                        eprintln!("  Actual:   {restored:?}");
                        let mismatch_msg = format!(
                            "restoration failed: mapping mismatch (expected {original:?}, actual {restored:?})"
                        );
                        if let Some(orig) = probe_error.take() {
                            eprintln!("  Original probe error was: {orig}");
                            probe_error = Some(Box::new(io::Error::other(format!(
                                "{orig}; {mismatch_msg}"
                            ))) as Box<dyn Error>);
                        } else {
                            probe_error =
                                Some(Box::new(io::Error::other(mismatch_msg)) as Box<dyn Error>);
                        }
                    }
                }
                Err(restore_err) => {
                    eprintln!("  Restoration write FAILED: {restore_err}");
                    if let Some(orig) = probe_error.take() {
                        eprintln!("  Original probe error was: {orig}");
                        probe_error = Some(Box::new(io::Error::other(format!(
                            "{orig}; restoration also failed: {restore_err}"
                        ))) as Box<dyn Error>);
                    } else {
                        probe_error = Some(Box::new(io::Error::other(format!(
                            "restoration failed: {restore_err}"
                        ))) as Box<dyn Error>);
                    }
                }
            }

            // --- Verification readback after restoration ---
            match handle.read_buttons(profile).await {
                Ok(current) => {
                    if current != original {
                        eprintln!(
                            "  Verification FAILED: device did not return to original mapping"
                        );
                        eprintln!("  Expected: {original:?}");
                        eprintln!("  Actual:   {current:?}");
                        if let Some(existing) = probe_error.take() {
                            probe_error = Some(Box::new(io::Error::other(format!(
                                "{existing}; verification failed: mapping mismatch"
                            ))) as Box<dyn Error>);
                        } else {
                            probe_error = Some(Box::new(io::Error::other(
                                "restoration verification failed: device did not return to original mapping",
                            )) as Box<dyn Error>);
                        }
                    } else {
                        println!("  Restore verified: mapping matches original");
                    }
                }
                Err(e) => {
                    eprintln!("  Verification read FAILED: {e}");
                    if let Some(existing) = probe_error.take() {
                        probe_error = Some(Box::new(io::Error::other(format!(
                            "{existing}; verification read also failed: {e}"
                        ))) as Box<dyn Error>);
                    } else {
                        probe_error = Some(Box::new(io::Error::other(format!(
                            "verification read failed: {e}"
                        ))) as Box<dyn Error>);
                    }
                }
            }

            if let Some(err) = probe_error {
                eprintln!("  Device {device_label} ended with error: {err}");
                if overall_error.is_none() {
                    overall_error = Some(err);
                }
            }
        } else if let Some(err) = probe_error {
            eprintln!("  Device {device_label} probe error (no writes performed): {err}");
            if overall_error.is_none() {
                overall_error = Some(err);
            }
        }

        println!("\n  === Results for {device_label} ===");
        for (slot, label) in &found_slots {
            println!("    slot {slot}: {label}");
        }
    }

    println!("\n=== Probe complete ===");
    if let Some(err) = overall_error {
        return Err(err);
    }
    Ok(())
}
