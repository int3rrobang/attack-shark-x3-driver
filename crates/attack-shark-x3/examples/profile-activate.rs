use std::{env, error::Error};

use attack_shark_x3::{DeviceSelector, MouseHandle, ProfileId, UsbDeviceKind};

fn usage() -> String {
    "usage: profile-activate --profile <1..5> [--transport wired|receiver]".into()
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut it = env::args().skip(1);
    let mut profile: Option<u8> = None;
    let mut kind = UsbDeviceKind::Wired;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--profile" => profile = Some(it.next().ok_or("--profile requires value")?.parse()?),
            "--transport" => match it.next().ok_or("--transport requires value")?.as_str() {
                "wired" => kind = UsbDeviceKind::Wired,
                "receiver" => kind = UsbDeviceKind::Receiver,
                v => return Err(format!("transport must be wired|receiver, got {v}").into()),
            },
            "--help" | "-h" => return Err(usage().into()),
            _ => return Err(format!("unknown arg {a}\n{}", usage()).into()),
        }
    }
    let p = ProfileId::try_from(profile.ok_or("missing --profile")?)?;
    let h = MouseHandle::open_for_kind(DeviceSelector::Unique, kind)?;
    let m = h.activate_profile(p).await?;
    println!(
        "activated profile {} (max {})",
        m.current().get(),
        m.maximum().get()
    );
    Ok(())
}
