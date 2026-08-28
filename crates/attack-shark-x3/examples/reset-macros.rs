use attack_shark_x3::{ButtonsState, DeviceSelector, MouseHandle, ProfileId, UsbDeviceKind};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let kind = UsbDeviceKind::Wired;
    let h = MouseHandle::open_for_kind(DeviceSelector::Unique, kind)?;
    for p in [1u8, 2, 3, 4, 5] {
        let pid = ProfileId::try_from(p).unwrap();
        // read live then reset slots 1,2,3,4,7,8 to defaults (Left 02, Right 03, Middle 04, DPI 0d, Forward 06, Backward 05)
        let live = h.read_buttons(pid).await?;
        let mut slots = live.slots;
        // defaults from DEFAULT_BUTTON_SLOTS: 0:02,1:03,2:04,3:0d,4:3c,5:0f,6:06,7:05 etc — but we just restore safe defaults
        slots[0] = attack_shark_x3::ButtonAssignment::new(0x02, 0x00, 0x00); // Left
        slots[1] = attack_shark_x3::ButtonAssignment::new(0x03, 0x00, 0x00); // Right
        slots[2] = attack_shark_x3::ButtonAssignment::new(0x04, 0x00, 0x00); // Middle
        slots[3] = attack_shark_x3::ButtonAssignment::new(0x0d, 0x00, 0x00); // DPI cycle
        slots[6] = attack_shark_x3::ButtonAssignment::new(0x06, 0x00, 0x00); // Forward
        slots[7] = attack_shark_x3::ButtonAssignment::new(0x05, 0x00, 0x00); // Backward
        // keep other slots as live to avoid clobbering scroll etc, but ensure DPI slot 4 is reset
        let patched = ButtonsState::new(pid, slots);
        h.write_buttons(patched).await?;
        println!("reset profile {} buttons 01/02/03/04/07/08 to defaults", p);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    println!(
        "done — macros unbound (12 00 XX cleared), DPI/Forward/Backward back to normal. Flash 09 content still there but not firing until re-bound."
    );
    Ok(())
}
