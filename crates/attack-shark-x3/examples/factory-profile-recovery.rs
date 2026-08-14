use std::{env, error::Error, fmt::Write as _, io};

use attack_shark_x3::{
    ButtonsReport, ButtonsState, DeviceSelector, DpiReport, DpiState, MouseHandle, PollingRate,
    PollingRateReport, PreferencesFraming, PreferencesReport, PreferencesState, ProfileId,
    ProfileMetadata, TransportKind, UsbDeviceKind,
};

const APPLY_ARGUMENT: &str = "--apply-all-profiles";
const VERIFY_ARGUMENT: &str = "--verify-after-power-cycle";

#[derive(Clone, Debug, Eq, PartialEq)]
struct FactoryProfile {
    dpi: DpiState,
    preferences: PreferencesState,
    buttons: ButtonsState,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    match env::args().nth(1).as_deref() {
        None | Some("--dry-run") => dry_run()?,
        Some(APPLY_ARGUMENT) => apply().await?,
        Some(VERIFY_ARGUMENT) => verify().await?,
        Some(argument) => return Err(usage_error(argument).into()),
    }
    Ok(())
}

fn usage_error(argument: &str) -> io::Error {
    io::Error::other(format!(
        "unrecognized argument {argument:?}; use --dry-run, {APPLY_ARGUMENT}, or {VERIFY_ARGUMENT}"
    ))
}

fn profiles() -> impl Iterator<Item = ProfileId> {
    (ProfileId::MIN..=ProfileId::MAX).map(|value| {
        ProfileId::try_from(value).expect("the documented profile-ID bounds must be valid")
    })
}

fn factory_profile(profile: ProfileId) -> Result<FactoryProfile, Box<dyn Error>> {
    Ok(FactoryProfile {
        dpi: DpiState::captured_stock_reset(profile)?,
        preferences: PreferencesState::captured_stock_reset(profile),
        buttons: ButtonsState::default_for_profile(profile),
    })
}

fn dry_run() -> Result<(), Box<dyn Error>> {
    println!(
        "DRY RUN: no device will be opened. The apply mode replaces DPI, preferences, and button mappings on all five profiles with the capture-confirmed stock X3 reset image."
    );
    let profile_one = ProfileId::try_from(1)?;
    println!(
        "per-profile polling-rate packet (profile 1): {}",
        hex(PollingRateReport::encode(profile_one, PollingRate::Hz1000).as_bytes())
    );

    for profile in profiles() {
        let state = factory_profile(profile)?;
        let dpi = DpiReport::encode(&state.dpi, TransportKind::Receiver)?;
        let preferences =
            PreferencesReport::encode_framed(&state.preferences, PreferencesFraming::Compact);
        let buttons = ButtonsReport::encode(&state.buttons);
        println!("profile {profile}:");
        println!("  dpi:         {}", hex(dpi.as_bytes()));
        println!("  preferences: {}", hex(preferences.as_bytes()));
        println!("  buttons:     {}", hex(buttons.as_bytes()));
    }

    println!(
        "apply sequence: ensure maximum=5; force a real profile edge into profile 1; for profiles 1..=5 activate, write+wait+readback each complete section; write polling rate; return to profile 1."
    );
    println!(
        "The receiver policy supplies a five-second quiet period after every write before verification. Any mismatch aborts the sequence."
    );
    Ok(())
}

async fn apply() -> Result<(), Box<dyn Error>> {
    println!(
        "DESTRUCTIVE RECOVERY: replacing every user profile with the capture-confirmed stock profile-1 reset image, retargeted and rechecksummed for profiles 1..=5."
    );
    let handle = MouseHandle::open_for_kind(DeviceSelector::Unique, UsbDeviceKind::Receiver)?;
    let maximum = ProfileId::try_from(ProfileId::MAX)?;
    let mut metadata = handle.read_profile_metadata().await?;
    if metadata.maximum() != maximum {
        metadata = handle.set_maximum_profile(maximum).await?;
    }

    let profile_one = ProfileId::try_from(1)?;
    if metadata.current() == profile_one {
        let profile_two = ProfileId::try_from(2)?;
        metadata = handle.activate_profile(profile_two).await?;
    }
    metadata = activate_if_needed(&handle, metadata, profile_one).await?;

    for profile in profiles() {
        metadata = activate_if_needed(&handle, metadata, profile).await?;
        let expected = factory_profile(profile)?;
        println!("writing profile {profile} DPI");
        handle.write_dpi(expected.dpi).await?;
        println!("writing profile {profile} preferences");
        handle.write_preferences(expected.preferences).await?;
        println!("writing profile {profile} buttons");
        handle.write_buttons(expected.buttons).await?;
        // Polling rate is per-profile: report `0x06` skips the profile loader
        // and its deferred writer serializes the live image into the slot
        // named by byte 2, so the rate is targeted at the profile that is
        // live at submission and confirmed by a fresh 0x06 readback.
        let verified = handle
            .write_polling_rate_unchecked(profile, PollingRate::Hz1000)
            .await?;
        if verified != PollingRate::Hz1000 {
            return Err(io::Error::other(format!(
                "profile {profile} polling-rate write did not verify: expected 1000 Hz, got {verified}"
            ))
            .into());
        }
        println!("profile {profile} readback verified");
    }

    let final_metadata = activate_if_needed(&handle, metadata, profile_one).await?;
    if final_metadata != ProfileMetadata::new(profile_one, maximum)? {
        return Err(io::Error::other("final profile metadata mismatch").into());
    }

    println!(
        "APPLY PASS: all writes and immediate readbacks matched; profile 1 is active. Power-cycle the mouse before running {VERIFY_ARGUMENT}."
    );
    Ok(())
}

async fn verify() -> Result<(), Box<dyn Error>> {
    println!(
        "persistence verification after power cycle (profile activation and section reads only)"
    );
    let handle = MouseHandle::open_for_kind(DeviceSelector::Unique, UsbDeviceKind::Receiver)?;
    let maximum = ProfileId::try_from(ProfileId::MAX)?;
    let profile_one = ProfileId::try_from(1)?;
    let mut metadata = handle.read_profile_metadata().await?;
    if metadata.maximum() != maximum {
        return Err(io::Error::other(format!(
            "maximum-profile mismatch: expected {maximum}, got {}",
            metadata.maximum()
        ))
        .into());
    }

    for profile in profiles() {
        metadata = activate_if_needed(&handle, metadata, profile).await?;
        let actual = handle.read_profile(profile).await?;
        let expected = factory_profile(profile)?;
        if actual.dpi != expected.dpi
            || actual.preferences != expected.preferences
            || actual.buttons != expected.buttons
        {
            return Err(io::Error::other(format!(
                "profile {profile} persistence mismatch\nexpected: {expected:#?}\nactual: {actual:#?}"
            ))
            .into());
        }
        let rate = handle.read_polling_rate(profile).await?;
        if rate != PollingRate::Hz1000 {
            return Err(io::Error::other(format!(
                "profile {profile} polling-rate persistence mismatch: expected 1000 Hz, got {rate}"
            ))
            .into());
        }
        println!("profile {profile} persisted state verified");
    }

    activate_if_needed(&handle, metadata, profile_one).await?;
    println!("VERIFY PASS: all five profiles and polling rate persisted; profile 1 is active");
    Ok(())
}

async fn activate_if_needed(
    handle: &MouseHandle,
    metadata: ProfileMetadata,
    target: ProfileId,
) -> Result<ProfileMetadata, Box<dyn Error>> {
    if metadata.current() == target {
        Ok(metadata)
    } else {
        println!("activating profile {target}");
        Ok(handle.activate_profile(target).await?)
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 3 - 1);
    for (index, byte) in bytes.iter().enumerate() {
        if index != 0 {
            output.push(':');
        }
        write!(output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}
