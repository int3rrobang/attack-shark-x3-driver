//! Read-only validator for physical profile-plus/profile-minus events.
//!
//! The tested button must already carry the chosen action on every profile.
//! This probe deliberately performs no configuration writes because interleaved
//! profile activation and temporary per-profile remaps can corrupt working and
//! persisted state on the FA60 receiver.
use std::{env, error::Error, io, io::Write, time::Duration};

use attack_shark_x3::{
    DeviceSelector, InputEvent, MouseHandle, ProfileChangedEvent, ProfileId, UsbDeviceKind,
};
use tokio::{sync::broadcast, time::timeout};

const EVENT_TIMEOUT: Duration = Duration::from_secs(5);

type ProbeResult<T> = Result<T, Box<dyn Error>>;

#[derive(Clone, Copy, Debug)]
enum Direction {
    Plus,
    Minus,
}

impl Direction {
    fn parse() -> ProbeResult<Self> {
        match env::args().nth(1).as_deref() {
            Some("plus") => Ok(Self::Plus),
            Some("minus") => Ok(Self::Minus),
            _ => Err("usage: cargo run -p attack-shark-x3 --example profile-navigation-probe --features usb -- <plus|minus>".into()),
        }
    }

    const fn steps(self) -> [(u8, Option<u8>); 5] {
        match self {
            Self::Plus => [
                (1, Some(2)),
                (2, Some(3)),
                (3, Some(4)),
                (4, Some(5)),
                (5, None),
            ],
            Self::Minus => [
                (5, Some(4)),
                (4, Some(3)),
                (3, Some(2)),
                (2, Some(1)),
                (1, None),
            ],
        }
    }
}

#[tokio::main]
async fn main() -> ProbeResult<()> {
    let direction = Direction::parse()?;
    let handle = MouseHandle::open_for_kind(DeviceSelector::Unique, UsbDeviceKind::Receiver)?;
    let metadata = handle.read_profile_metadata().await?;
    if metadata.maximum().get() != ProfileId::MAX {
        return Err(format!(
            "the full-range probe requires maximum profile 5, found {}",
            metadata.maximum()
        )
        .into());
    }

    let first = ProfileId::try_from(direction.steps()[0].0)?;
    if metadata.current() != first {
        return Err(format!(
            "profile-{direction:?} must start on profile {first}, found {}; switch physically before running the probe",
            metadata.current()
        )
        .into());
    }

    println!("starting metadata: {metadata:?}");
    println!(
        "read-only probe: the chosen physical button must already be bound to profile-{direction:?} on all five profiles"
    );

    for (source_raw, expected_raw) in direction.steps() {
        let source = ProfileId::try_from(source_raw)?;
        let expected = expected_raw.map(ProfileId::try_from).transpose()?;
        observe_one_press(&handle, source, expected).await?;
    }

    println!("PASS: all five profile-{direction:?} steps matched events and metadata");
    Ok(())
}

async fn observe_one_press(
    handle: &MouseHandle,
    source: ProfileId,
    expected: Option<ProfileId>,
) -> ProbeResult<()> {
    let before = handle.read_profile_metadata().await?;
    if before.current() != source {
        return Err(format!(
            "expected source profile {source}, found {} before the press",
            before.current()
        )
        .into());
    }

    let mut events = handle.subscribe_input_events();
    match expected {
        Some(target) => println!(
            "READY {source}→{target}: press the configured profile button exactly once, then press Enter here"
        ),
        None => println!(
            "READY clamp at {source}: press the configured profile button exactly once, then press Enter here"
        ),
    }
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    let event = next_profile_sync(&mut events).await?;
    let metadata = handle.read_profile_metadata().await?;
    println!("event: {event:?}");
    println!("metadata: {metadata:?}");

    match (expected, event) {
        (Some(target), Some(event))
            if event.profile == target && metadata.current() == target =>
        {
            Ok(())
        }
        (None, None) if metadata.current() == source => Ok(()),
        _ => Err(format!(
            "profile step from {source} disagreed: expected {expected:?}, event {event:?}, metadata {metadata:?}"
        )
        .into()),
    }
}

async fn next_profile_sync(
    events: &mut broadcast::Receiver<InputEvent>,
) -> ProbeResult<Option<ProfileChangedEvent>> {
    match timeout(EVENT_TIMEOUT, async {
        loop {
            match events.recv().await {
                Ok(InputEvent::ProfileSync(event)) => break Ok(event),
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => {
                    break Err("input event stream closed".to_owned());
                }
            }
        }
    })
    .await
    {
        Ok(Ok(event)) => Ok(Some(event)),
        Ok(Err(error)) => Err(error.into()),
        Err(_) => Ok(None),
    }
}
