use attack_shark_x3::{BatteryEvent, DpiButtonEvent};
use tokio::sync::broadcast;

use crate::device::DeviceId;
use crate::error::ManagerError;
use crate::manager::DeviceManager;

/// Receivers for the input streams that the opened hardware session actually
/// exposes. A missing receiver means that transport has no such stream.
pub struct EventSubscriptions {
    pub dpi_button: Option<broadcast::Receiver<DpiButtonEvent>>,
    pub battery: Option<broadcast::Receiver<BatteryEvent>>,
}

impl DeviceManager {
    /// Opens the exact device and returns its native hardware event streams.
    /// No manager-side event bus or synthetic disconnect event is introduced.
    pub async fn subscribe_events(
        &self,
        device: &DeviceId,
    ) -> Result<EventSubscriptions, ManagerError> {
        let (_, session) = self.open_session(device).await?;
        let events = session.subscribe_events();
        Ok(EventSubscriptions {
            dpi_button: events.dpi_button,
            battery: events.battery,
        })
    }
}
