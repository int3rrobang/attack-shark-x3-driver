use hidapi::HidApi;
use std::time::Instant;

fn main() {
    let api = HidApi::new().unwrap();
    println!("=== Attack Shark HID collections (vid:1d57, pid:fa60/fa61) ===");
    let collections: Vec<_> = api
        .device_list()
        .filter(|d| {
            d.vendor_id() == 0x1d57 && (d.product_id() == 0xfa60 || d.product_id() == 0xfa61)
        })
        .collect();
    for dev in &collections {
        println!(
            "  pid=0x{:04x} usage_page=0x{:04x} usage=0x{:04x} interface={} path={}",
            dev.product_id(),
            dev.usage_page(),
            dev.usage(),
            dev.interface_number(),
            dev.path().to_string_lossy(),
        );
    }

    println!("\n=== Reading input reports (15s) — press DPI button now! ===\n");
    let start = Instant::now();
    let mut devices: Vec<_> = collections
        .iter()
        .filter(|d| d.usage_page() != 0xff00)
        .filter_map(|d| {
            d.open_device(&api)
                .ok()
                .map(|h| (d.usage_page(), d.usage(), h))
        })
        .collect();

    let mut buf = [0u8; 64];
    while start.elapsed().as_secs() < 15 {
        for (up, u, dev) in &mut devices {
            dev.set_blocking_mode(false).ok();
            match dev.read_timeout(&mut buf, 50) {
                Ok(n) if n > 0 => {
                    println!(
                        "  [up=0x{up:04x} u=0x{u:04x}] {} bytes: {:02x?}",
                        n,
                        &buf[..n]
                    );
                }
                _ => {}
            }
        }
    }
    println!("\nDone.");
}
