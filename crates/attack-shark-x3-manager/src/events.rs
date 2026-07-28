use tokio::sync::broadcast;

use crate::backend::DeviceSession;
use crate::device::DeviceId;
use crate::error::ManagerError;
use crate::manager::DeviceManager;
use crate::operation::DeviceEvent;

/// Receivers for decoded device events from the opened hardware session.
///
/// The session is held alive for the lifetime of the subscriptions; dropping
/// this struct closes the session and stops the input worker.
pub struct EventSubscriptions {
    pub events: Option<broadcast::Receiver<DeviceEvent>>,
    _session: Box<dyn DeviceSession>,
}

impl DeviceManager {
    /// Opens the exact device and returns its decoded hardware event stream.
    /// No manager-side disconnect event is synthesized.
    pub async fn subscribe_events(
        &self,
        device: &DeviceId,
    ) -> Result<EventSubscriptions, ManagerError> {
        let (_, session) = self.open_session(device).await?;
        let events = session.subscribe_events();
        let mapped = events.input.map(|mut input| {
            let (sender, receiver) = broadcast::channel(16);
            tokio::spawn(async move {
                loop {
                    match input.recv().await {
                        Ok(event) => {
                            if sender.send(DeviceEvent::from(event)).is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
            receiver
        });
        Ok(EventSubscriptions {
            events: mapped,
            _session: session,
        })
    }
}
