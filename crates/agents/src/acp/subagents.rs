//! Subagents a structured agent launched, derived from its tool calls (overview cards).

use std::collections::HashMap;

use super::protocol::{ToolCall, ToolCallUpdate, ToolContent, ToolStatus};

/// Nesting deeper than this is treated as a cycle.
const MAX_DEPTH: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentStatus {
    Running,
    /// The subagent, or one of its tool calls, waits for the user's permission.
    Approval,
    Done,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subagent {
    /// The id of the tool call that launched it.
    pub id: String,
    pub name: String,
    pub status: SubagentStatus,
    /// One line on what it is doing or how it ended.
    pub activity: Option<String>,
    /// Tool calls it made, nested subagents' included.
    pub tool_calls: usize,
}

/// The permission request the agent waits on.
#[derive(Clone, Copy, Debug)]
pub struct PendingPermission<'a> {
    pub call: &'a ToolCallUpdate,
    pub title: &'a str,
}

/// The top-level subagents among `calls`, in launch order. Nested subagents count toward the
/// top-level one they run under.
pub fn subagents<'a>(
    calls: impl IntoIterator<Item = &'a ToolCall>,
    pending: Option<PendingPermission<'_>>,
) -> Vec<Subagent> {
    let calls: Vec<&ToolCall> = calls.into_iter().collect();
    let by_id: HashMap<&str, &ToolCall> = calls.iter().map(|c| (c.id.as_str(), *c)).collect();
    let root = |parent| root(&by_id, parent);
    // A launch whose parent is unknown (none, or before the calls given) is top-level.
    let top_level = |c: &ToolCall| {
        c.is_subagent() && c.parent.as_deref().is_none_or(|p| !by_id.contains_key(p))
    };
    let pending_root = pending.and_then(|p| match by_id.get(p.call.id.as_str()) {
        Some(c) if top_level(c) => Some(c.id.as_str()),
        Some(c) => root(c.parent.as_deref()),
        None => root(p.call.parent.as_deref()),
    });
    calls
        .iter()
        .filter(|c| top_level(c))
        .map(|launch| {
            let children: Vec<&&ToolCall> = calls
                .iter()
                .filter(|c| root(c.parent.as_deref()) == Some(launch.id.as_str()))
                .collect();
            let status = if pending_root == Some(launch.id.as_str()) {
                SubagentStatus::Approval
            } else {
                match launch.status {
                    ToolStatus::Completed => SubagentStatus::Done,
                    ToolStatus::Failed => SubagentStatus::Failed,
                    ToolStatus::Pending | ToolStatus::InProgress => SubagentStatus::Running,
                }
            };
            let activity = match status {
                SubagentStatus::Approval => pending.map(|p| format!("Wants to run: {}", p.title)),
                SubagentStatus::Running => children.last().map(|c| c.title.clone()),
                SubagentStatus::Done => Some(format!(
                    "Returned · {} tool call{}",
                    children.len(),
                    if children.len() == 1 { "" } else { "s" }
                )),
                SubagentStatus::Failed => first_text_line(launch),
            };
            Subagent {
                id: launch.id.clone(),
                name: [Some(launch.title.as_str()), launch.subagent_type.as_deref()]
                    .into_iter()
                    .flatten()
                    .map(str::trim)
                    .find(|s| !s.is_empty())
                    .unwrap_or("Subagent")
                    .to_owned(),
                status,
                activity,
                tool_calls: children.len(),
            }
        })
        .collect()
}

fn root<'a>(by_id: &HashMap<&'a str, &'a ToolCall>, parent: Option<&'a str>) -> Option<&'a str> {
    owner(parent, |id| by_id.get(id).copied())
}

/// The top-level subagent launch that a message or call with this `parent` belongs to,
/// following `parent` links through `call`. `None` for the agent's own items (and cycles).
pub fn owner<'a>(
    parent: Option<&'a str>,
    call: impl Fn(&str) -> Option<&'a ToolCall>,
) -> Option<&'a str> {
    let mut id = parent?;
    for _ in 0..MAX_DEPTH {
        match call(id).and_then(|launch| launch.parent.as_deref()) {
            Some(up) if call(up).is_some() => id = up,
            _ => return Some(id),
        }
    }
    None
}

fn first_text_line(call: &ToolCall) -> Option<String> {
    call.content.iter().find_map(|c| match c {
        ToolContent::Text(text) => text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(str::to_owned),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(id: &str, title: &str, status: ToolStatus) -> ToolCall {
        ToolCall {
            id: id.into(),
            title: title.into(),
            status,
            tool_name: Some("Agent".into()),
            ..ToolCall::default()
        }
    }

    fn child(id: &str, parent: &str, title: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            title: title.into(),
            tool_name: Some("Read".into()),
            parent: Some(parent.into()),
            ..ToolCall::default()
        }
    }

    #[test]
    fn statuses_and_activity() {
        let mut failed = launch("a3", "Verify", ToolStatus::Failed);
        failed.content = vec![ToolContent::Text("\nerror: boom\nmore".into())];
        let calls = [
            ToolCall {
                id: "top".into(),
                title: "Read README".into(),
                ..ToolCall::default()
            },
            launch("a1", "Migrate CartStore", ToolStatus::Completed),
            child("c1", "a1", "Edit Cart.swift"),
            launch("a2", "Migrate OrdersStore", ToolStatus::InProgress),
            child("c2", "a2", "Read Orders.swift"),
            child("c3", "a2", "Edit Orders.swift"),
            failed,
        ];
        let list = subagents(&calls, None);
        let summary: Vec<(&str, SubagentStatus, Option<&str>, usize)> = list
            .iter()
            .map(|s| {
                (
                    s.name.as_str(),
                    s.status,
                    s.activity.as_deref(),
                    s.tool_calls,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (
                    "Migrate CartStore",
                    SubagentStatus::Done,
                    Some("Returned · 1 tool call"),
                    1
                ),
                (
                    "Migrate OrdersStore",
                    SubagentStatus::Running,
                    Some("Edit Orders.swift"),
                    2
                ),
                ("Verify", SubagentStatus::Failed, Some("error: boom"), 0),
            ]
        );
    }

    #[test]
    fn permission_wins_over_running() {
        let calls = [
            launch("a1", "Verify", ToolStatus::InProgress),
            child("c1", "a1", "simctl launch"),
            launch("a2", "Other", ToolStatus::InProgress),
        ];
        let request = ToolCallUpdate {
            id: "c1".into(),
            ..ToolCallUpdate::default()
        };
        let pending = PendingPermission {
            call: &request,
            title: "simctl launch com.app",
        };
        let list = subagents(&calls, Some(pending));
        assert_eq!(list[0].status, SubagentStatus::Approval);
        assert_eq!(
            list[0].activity.as_deref(),
            Some("Wants to run: simctl launch com.app")
        );
        assert_eq!(list[1].status, SubagentStatus::Running);

        // A request for a call not reported yet still names its parent.
        let request = ToolCallUpdate {
            id: "new".into(),
            parent: Some("a2".into()),
            ..ToolCallUpdate::default()
        };
        let pending = PendingPermission {
            call: &request,
            title: "rm",
        };
        let list = subagents(&calls, Some(pending));
        assert_eq!(list[0].status, SubagentStatus::Running);
        assert_eq!(list[1].status, SubagentStatus::Approval);

        // The parent agent's own request touches no subagent.
        let request = ToolCallUpdate {
            id: "mine".into(),
            ..ToolCallUpdate::default()
        };
        let pending = PendingPermission {
            call: &request,
            title: "rm",
        };
        assert!(
            subagents(&calls, Some(pending))
                .iter()
                .all(|s| s.status == SubagentStatus::Running)
        );
    }

    #[test]
    fn nested_subagents_roll_up() {
        let mut nested = launch("n1", "Inner", ToolStatus::InProgress);
        nested.parent = Some("a1".into());
        let calls = [
            launch("a1", "Outer", ToolStatus::InProgress),
            nested,
            child("c1", "n1", "Grep foo"),
        ];
        let list = subagents(&calls, None);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].tool_calls, 2);
        assert_eq!(list[0].activity.as_deref(), Some("Grep foo"));
    }

    #[test]
    fn owner_follows_nesting() {
        let mut nested = launch("n1", "Inner", ToolStatus::InProgress);
        nested.parent = Some("a1".into());
        let calls = [launch("a1", "Outer", ToolStatus::InProgress), nested];
        let find = |id: &str| calls.iter().find(|c| c.id == id);
        assert_eq!(owner(None, find), None);
        assert_eq!(owner(Some("a1"), find), Some("a1"));
        assert_eq!(owner(Some("n1"), find), Some("a1"));
        // A launch not seen (an earlier turn's, or lost) still groups its own items.
        assert_eq!(owner(Some("gone"), find), Some("gone"));

        let mut a = launch("a", "A", ToolStatus::InProgress);
        a.parent = Some("b".into());
        let mut b = launch("b", "B", ToolStatus::InProgress);
        b.parent = Some("a".into());
        let cycle = [a, b];
        assert_eq!(
            owner(Some("a"), |id: &str| cycle.iter().find(|c| c.id == id)),
            None
        );
    }

    #[test]
    fn name_falls_back_to_type() {
        let mut call = launch("a1", " ", ToolStatus::Pending);
        call.subagent_type = Some("Explore".into());
        assert_eq!(subagents(&[call], None)[0].name, "Explore");
        let call = launch("a1", "", ToolStatus::Pending);
        assert_eq!(subagents(&[call], None)[0].name, "Subagent");
    }
}
