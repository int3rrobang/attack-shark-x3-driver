//! Interactive button-layout probe for FA61 (wired) and FA60 (receiver).
//! Maps each physical button to its firmware slot index by temporarily
//! setting candidate slots to left-click and asking the user to test.
//!
//! Run in an interactive terminal:
//!   cargo run --example probe_layout --features usb
//!
//! WARNING: This example performs temporary hardware writes and can leave a
//! mapping active until the device is restored or power-cycled.
//! TODO: Add an explicit confirmation gate and verify the post-restore readback.
//! Keep a known-good mapping and unplug recovery path ready before running it.

use attack_shark_x3::{
    ButtonAssignment, ButtonsState, DeviceSelector, MouseHandle, ProfileId, UsbDeviceKind,
    list_devices, list_devices_for,
};
use std::io::{self, Write};

#[tokio::main]
async fn main() {
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
        return;
    }

    println!("=== Attack Shark Button Layout Probe ===\n");
    eprintln!("WARNING: experimental probe; keep a known-good restore path ready.");
    println!("Found {} device(s):", all_devices.len());
    for (label, path, _) in &all_devices {
        println!("  {label}: {path}");
    }

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

        let profile = ProfileId::try_from(1u8).unwrap();
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

        for &btn_label in &physical_buttons {
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
                handle.write_buttons(state).await.expect("write");

                print!("    [slot {slot}] Press {btn_label} -> left-click? (slot#/n/q): ");
                io::stdout().flush().ok();
                line.clear();
                stdin.read_line(&mut line).ok();

                let answer = line.trim();
                if answer == "q" {
                    println!("  Quitting probe for this device.");
                    break;
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

        // Restore original
        println!("\n  Restoring original button mapping...");
        handle.write_buttons(original).await.expect("restore");

        println!("\n  === Results for {device_label} ===");
        for (slot, label) in &found_slots {
            println!("    slot {slot}: {label}");
        }
    }

    println!("\n=== Probe complete ===");
}
