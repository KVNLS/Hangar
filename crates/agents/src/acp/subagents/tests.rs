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
