//! A workspace's turn on its device as its agent sees it.

use hangar_core::WorkspaceId;
use hangar_mcp::ToolError;
use hangar_usecases::device_pool::{RESERVATION_IDLE, Sharing};

use super::{Acquisition, Released};

/// Whether `ws` holds the device (a call or a reservation).
pub fn holds(s: &Sharing, ws: WorkspaceId) -> bool {
    s.holder.as_ref().is_some_and(|h| h.id == ws)
}

/// What `device_acquire` answers at one look at the pool: `None` while the workspace still
/// waits in line within its `wait`; the answer once it holds the device or `expired`.
pub fn acquisition(s: &Sharing, ws: WorkspaceId, expired: bool) -> Option<Acquisition> {
    let granted = holds(s, ws);
    (granted || expired).then(|| Acquisition {
        granted,
        sharing: agent_sharing(s, ws),
    })
}

/// What `device_release` answers: `before` is the workspace's turn read just before the pool
/// released it, `had_waiters` what the release returned.
pub fn released(before: Option<&Sharing>, ws: WorkspaceId, had_waiters: bool) -> Released {
    let next = before
        .filter(|s| holds(s, ws))
        .and_then(|s| s.queue.iter().find(|m| m.id != ws))
        .map(|m| m.name.clone());
    Released {
        had_waiters,
        next: next.filter(|_| had_waiters),
        left_line: before.is_some_and(|s| s.position.is_some()),
    }
}

/// The pool's view of `ws`'s turn on its device, as agents see it.
pub fn agent_sharing(s: &Sharing, ws: WorkspaceId) -> hangar_mcp::Sharing {
    hangar_mcp::Sharing {
        device_name: s.device.clone(),
        device_id: s.device_id.clone(),
        holder: s.holder.as_ref().map(|h| h.name.clone()),
        you_hold: s.holder.as_ref().is_some_and(|h| h.id == ws),
        position: s.position,
        queue: s.queue.iter().map(|m| m.name.clone()).collect(),
        shared_with: s.shared_with.clone(),
        reservation_left_s: s.reservation_left.map(|d| d.as_secs()),
        estimated_wait_s: s.estimated_wait.map(|d| d.as_secs()),
        idle_release_s: RESERVATION_IDLE.as_secs(),
        stale_app_from: s.stale_app_from.clone(),
        notice: None,
    }
}

/// The error of a device tool that waited in line past its budget: where it stands.
pub fn still_queued(s: Option<&Sharing>, ws: WorkspaceId) -> ToolError {
    ToolError::Failed(match s {
        Some(s) => hangar_mcp::queued_message(&agent_sharing(s, ws)),
        None => "The device is not available yet; your place is kept: call device_acquire with \
                 wait_s to wait, or device_info to check."
            .to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hangar_core::WorkspaceId;
    use hangar_core::device::Platform;
    use hangar_mcp::ToolError;

    use hangar_usecases::device_pool::{Member, PoolKey, Sharing};

    use super::{agent_sharing, still_queued};

    fn member(id: WorkspaceId, name: &str) -> Member {
        Member {
            id,
            name: name.into(),
        }
    }

    #[test]
    fn sharing_maps_to_what_agents_see() {
        let (a, b, c) = (WorkspaceId::new(), WorkspaceId::new(), WorkspaceId::new());
        let pool = Sharing {
            key: PoolKey::Own(a, Platform::Ios),
            device: "Hangar ws-a".into(),
            device_id: Some("UDID-A".into()),
            holder: Some(member(a, "ws-a")),
            queue: vec![member(c, "ws-c"), member(b, "ws-b")],
            position: Some(1),
            shared_with: vec!["ws-a".into(), "ws-c".into()],
            reservation_left: Some(Duration::from_millis(42_900)),
            estimated_wait: Some(Duration::from_secs(342)),
            stale_app_from: Some("ws-a".into()),
        };
        let seen = agent_sharing(&pool, b);
        assert_eq!(
            seen,
            hangar_mcp::Sharing {
                device_name: "Hangar ws-a".into(),
                device_id: Some("UDID-A".into()),
                holder: Some("ws-a".into()),
                you_hold: false,
                position: Some(1),
                queue: vec!["ws-c".into(), "ws-b".into()],
                shared_with: vec!["ws-a".into(), "ws-c".into()],
                reservation_left_s: Some(42),
                estimated_wait_s: Some(342),
                idle_release_s: 90,
                stale_app_from: Some("ws-a".into()),
                notice: None,
            }
        );
        assert!(agent_sharing(&pool, a).you_hold);

        let ToolError::Failed(message) = still_queued(Some(&pool), b) else {
            panic!("a queued call fails");
        };
        assert!(message.starts_with("Hangar ws-a (shared with ws-a, ws-c) is in use by ws-a"));
        assert!(message.contains("2nd in line"), "{message}");
        assert!(
            matches!(still_queued(None, b), ToolError::Failed(m) if m.contains("place is kept"))
        );
    }
}
