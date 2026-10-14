use tokio::sync::broadcast;

use crate::model::{
    Project, ProjectId, Session, SessionId, SessionStatus, Workspace, WorkspaceId, WorkspaceStatus,
};

pub const DEFAULT_BUS_CAPACITY: usize = 256;

/// Domain events broadcast to every subscriber (UI, agents, file watchers).
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    ProjectAdded(Project),
    ProjectRemoved(ProjectId),
    WorkspaceCreated(Workspace),
    WorkspaceStatusChanged {
        id: WorkspaceId,
        status: WorkspaceStatus,
    },
    WorkspaceRemoved(WorkspaceId),
    SessionStarted(Session),
    SessionStatusChanged {
        id: SessionId,
        status: SessionStatus,
    },
    Error {
        message: String,
    },
}

/// Cheaply cloneable broadcast bus. Slow subscribers that fall more than `capacity` events behind
/// receive `RecvError::Lagged` and skip ahead.
#[derive(Clone, Debug)]
pub struct EventBus {
    sender: broadcast::Sender<Event>,
}

impl EventBus {
    /// `capacity` is clamped to at least 1.
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self { sender }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.sender.subscribe()
    }

    /// Returns how many subscribers received the event; publishing with none is not an error.
    pub fn publish(&self, event: Event) -> usize {
        self.sender.send(event).unwrap_or(0)
    }

    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(DEFAULT_BUS_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast::error::{RecvError, TryRecvError};

    #[test]
    fn delivers_to_every_subscriber() {
        let bus = EventBus::default();
        let mut a = bus.subscribe();
        let mut b = bus.clone().subscribe();
        let id = ProjectId::new();

        assert_eq!(bus.publish(Event::ProjectRemoved(id)), 2);
        assert_eq!(a.try_recv().unwrap(), Event::ProjectRemoved(id));
        assert_eq!(b.try_recv().unwrap(), Event::ProjectRemoved(id));
        assert_eq!(a.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn publishing_without_subscribers_is_ignored() {
        let bus = EventBus::new(0);
        assert_eq!(bus.subscriber_count(), 0);
        assert_eq!(
            bus.publish(Event::Error {
                message: "nobody listens".into()
            }),
            0
        );
    }

    #[tokio::test]
    async fn async_receive_and_lag() {
        let bus = EventBus::new(2);
        let mut rx = bus.subscribe();
        for code in 0..3 {
            bus.publish(Event::SessionStatusChanged {
                id: SessionId::new(),
                status: SessionStatus::Exited { code },
            });
        }
        assert_eq!(rx.recv().await, Err(RecvError::Lagged(1)));
        assert!(matches!(
            rx.recv().await,
            Ok(Event::SessionStatusChanged {
                status: SessionStatus::Exited { code: 1 },
                ..
            })
        ));
    }
}
