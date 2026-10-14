//! System notifications (macOS Notification Center, Windows toasts, freedesktop on Linux) when
//! an agent starts waiting on the user or finishes a turn. Sent through `notify-rust`; on macOS
//! that is `NSUserNotificationCenter`, which also works for an unbundled `cargo run` binary
//! (the notification is then attributed to Finder).

use hangar_core::AgentState;

/// Past this many notifications from one observation, a single summary is sent instead.
const MAX_BANNERS: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Banner {
    pub title: String,
    pub body: String,
}

/// What happened to a workspace's agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// Started waiting on approval, input, or after a failure.
    Waiting(AgentState),
    /// Stopped working: ready for review or idle.
    Finished(AgentState),
}

/// The banner for one workspace: its name as the title, what happened and the project below.
pub fn banner_for(workspace: &str, project: &str, event: Event) -> Banner {
    let what = match event {
        Event::Waiting(AgentState::Input) => "Has a question",
        Event::Waiting(AgentState::Failed) => "Agent failed",
        Event::Waiting(state) => state.label(),
        Event::Finished(AgentState::Review) => AgentState::Review.label(),
        Event::Finished(_) => "Finished",
    };
    Banner {
        title: workspace.to_string(),
        body: format!("{what} · {project}"),
    }
}

/// `banners` as sent: unchanged up to [`MAX_BANNERS`], else one summary so a batch of agents
/// changing at once does not flood the notification center.
pub fn coalesce(banners: Vec<Banner>) -> Vec<Banner> {
    if banners.len() <= MAX_BANNERS {
        return banners;
    }
    vec![Banner {
        title: "Hangar".to_string(),
        body: format!("{} agents are waiting or done", banners.len()),
    }]
}

/// Attributes notifications to Hangar's bundle when it runs as an .app; without a bundle
/// identifier the default (Finder) is kept. Call once before the first [`post`].
#[cfg(target_os = "macos")]
pub fn init() {
    use objc2_foundation::NSBundle;

    let Some(id) = NSBundle::mainBundle().bundleIdentifier() else {
        tracing::info!("no bundle identifier: notifications are attributed to Finder");
        return;
    };
    if let Err(err) = notify_rust::set_application(&id.to_string()) {
        tracing::warn!("could not attribute notifications to {id}: {err}");
    }
}

#[cfg(not(target_os = "macos"))]
pub fn init() {}

/// Shows `banner`. Can block on D-Bus or WinRT: call it off the UI thread.
pub fn post(banner: &Banner) -> anyhow::Result<()> {
    notify_rust::Notification::new()
        .appname("Hangar")
        .summary(&banner.title)
        .body(&banner.body)
        .show()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Banner, Event, banner_for, coalesce};
    use hangar_core::AgentState::*;

    fn body(event: Event) -> String {
        banner_for("fix-login", "shop", event).body
    }

    #[test]
    fn banner_names_workspace_and_project() {
        let b = banner_for("fix-login", "shop", Event::Waiting(Approval));
        assert_eq!(b.title, "fix-login");
        assert_eq!(b.body, "Waiting for approval · shop");
    }

    #[test]
    fn each_event_reads_right() {
        assert_eq!(body(Event::Waiting(Input)), "Has a question · shop");
        assert_eq!(body(Event::Waiting(Failed)), "Agent failed · shop");
        assert_eq!(body(Event::Finished(Review)), "Ready for review · shop");
        assert_eq!(body(Event::Finished(Idle)), "Finished · shop");
    }

    #[test]
    fn many_banners_become_one() {
        let b = |n: usize| Banner {
            title: n.to_string(),
            body: String::new(),
        };
        let three: Vec<_> = (0..3).map(b).collect();
        assert_eq!(coalesce(three.clone()), three);
        let four = coalesce((0..4).map(b).collect());
        assert_eq!(four.len(), 1);
        assert_eq!(four[0].body, "4 agents are waiting or done");
    }
}
