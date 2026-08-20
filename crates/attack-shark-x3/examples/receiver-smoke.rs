use std::{error::Error, io, time::Duration};

use attack_shark_x3::{
    DeviceSelector, DpiValue, MouseHandle, PollingRate, ProfileId, ProfileSnapshot, UsbDeviceKind,
};
use tokio::time::sleep;

const SETTLE_DELAY: Duration = Duration::from_secs(1);

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    println!("opening the unique FA60 receiver");
    let handle = MouseHandle::open_for_kind(DeviceSelector::Unique, UsbDeviceKind::Receiver)?;
    settle("receiver open").await;

    println!("capturing the current configuration before any write");
    let original_metadata = handle.read_profile_metadata().await?;
    settle("metadata read").await;
    let original_profile = handle.read_profile(original_metadata.current()).await?;
    settle("profile read").await;
    // Live polling rate: alias is wire side effect only; the returned rate is from the
    // current live image. Preceding read_profile(current) establishes the live image
    // for profile-scoped comparison in this same MouseHandle sequence.
    let original_rate = handle
        .read_live_polling_rate(original_metadata.current())
        .await?;
    settle("polling-rate read").await;
    print_snapshot("backup", original_rate, &original_profile);

    probe_polling_rate(&handle, original_metadata.current(), original_rate).await?;
    probe_dpi(&handle, &original_profile).await?;
    probe_preferences(&handle, &original_profile).await?;

    println!("performing final readback and comparing it with the backup");
    let final_metadata = handle.read_profile_metadata().await?;
    let final_profile = handle.read_profile(original_metadata.current()).await?;
    settle("final profile read").await;
    // Same safe association: reload complete target profile before live-rate read.
    let final_rate = handle
        .read_live_polling_rate(original_metadata.current())
        .await?;
    settle("final polling-rate read").await;

    if final_metadata != original_metadata
        || final_rate != original_rate
        || final_profile != original_profile
    {
        return Err(io::Error::other(format!(
			"final state does not match backup\nbackup metadata: {original_metadata:?}\nfinal metadata: {final_metadata:?}\nbackup rate: {original_rate:?}\nfinal rate: {final_rate:?}\nbackup profile: {original_profile:#?}\nfinal profile: {final_profile:#?}"
		))
		.into());
    }

    print_snapshot("restored", final_rate, &final_profile);
    println!("PASS: receiver reads, reversible writes, verification, and restoration succeeded");
    Ok(())
}

async fn probe_polling_rate(
    handle: &MouseHandle,
    profile: ProfileId,
    original: PollingRate,
) -> Result<(), Box<dyn Error>> {
    let candidate = match original {
        PollingRate::Hz125 => PollingRate::Hz250,
        PollingRate::Hz250 | PollingRate::Hz500 | PollingRate::Hz1000 => PollingRate::Hz125,
    };
    println!("probing polling rate for profile {profile}: {original} -> {candidate} -> {original}");

    let probe_result = handle
        .write_polling_rate_unchecked(profile, candidate)
        .await;
    settle("polling-rate probe write").await;
    let restore_result = handle.write_polling_rate_unchecked(profile, original).await;
    settle("polling-rate restore").await;

    probe_result
        .map_err(|error| io::Error::other(format!("polling-rate probe failed: {error}")))?;
    let restored = restore_result
        .map_err(|error| io::Error::other(format!("polling-rate restore failed: {error}")))?;
    if restored != original {
        return Err(io::Error::other("polling-rate restoration mismatch").into());
    }
    Ok(())
}

async fn probe_dpi(handle: &MouseHandle, original: &ProfileSnapshot) -> Result<(), Box<dyn Error>> {
    let mut candidate = original.dpi.clone();
    let active_index = usize::from(candidate.active_stage.get() - 1);
    let original_dpi = candidate.stages[active_index].get();
    let candidate_dpi = if original_dpi < DpiValue::MAX {
        original_dpi + DpiValue::STEP
    } else {
        original_dpi - DpiValue::STEP
    };
    candidate.stages[active_index] = DpiValue::try_from(candidate_dpi)?;
    println!(
        "probing active DPI stage {}: {} -> {} -> {}",
        candidate.active_stage, original_dpi, candidate_dpi, original_dpi
    );

    let probe_result = handle.write_dpi(candidate).await;
    settle("DPI probe write").await;
    let restore_result = handle.write_dpi(original.dpi.clone()).await;
    settle("DPI restore").await;

    probe_result.map_err(|error| io::Error::other(format!("DPI probe failed: {error}")))?;
    let restored =
        restore_result.map_err(|error| io::Error::other(format!("DPI restore failed: {error}")))?;
    if restored != original.dpi {
        return Err(io::Error::other("DPI restoration mismatch").into());
    }
    Ok(())
}

async fn probe_preferences(
    handle: &MouseHandle,
    original: &ProfileSnapshot,
) -> Result<(), Box<dyn Error>> {
    println!("rewriting the captured preferences unchanged to validate FA60 full framing");
    let restored = handle.write_preferences(original.preferences).await?;
    settle("preferences rewrite").await;
    if restored != original.preferences {
        return Err(io::Error::other("preferences rewrite mismatch").into());
    }
    Ok(())
}

async fn settle(operation: &str) {
    println!("waiting {} ms after {operation}", SETTLE_DELAY.as_millis());
    sleep(SETTLE_DELAY).await;
}

fn print_snapshot(label: &str, rate: PollingRate, profile: &ProfileSnapshot) {
    println!("{label} metadata: {:?}", profile.persistent_metadata);
    println!("{label} polling rate: {rate}");
    println!("{label} profile {}: {profile:#?}", profile.target_profile);
}
