use super::*;
use hangar_core::{ProjectId, Timestamp};

fn ws(name: &str, parent: Option<&Workspace>, at: i64) -> Workspace {
    Workspace {
        id: WorkspaceId::new(),
        project_id: ProjectId::new(),
        name: name.into(),
        slug: name.to_lowercase(),
        branch: format!("hangar/{}", name.to_lowercase()),
        base: "develop".into(),
        worktree_path: format!("/wt/{name}").into(),
        status: WorkspaceStatus::Active,
        created_at: Timestamp(at),
        task: None,
        group: None,
        parent: parent.map(|p| p.id),
    }
}

#[test]
fn depth_counts_parents_and_survives_cycles_and_deleted_parents() {
    let lead = ws("Lead", None, 1);
    let child = ws("Child", Some(&lead), 2);
    let grandchild = ws("Grandchild", Some(&child), 3);
    let all = [lead.clone(), child.clone(), grandchild.clone()];
    assert_eq!(depth(&all, lead.id), 0);
    assert_eq!(depth(&all, child.id), 1);
    assert_eq!(depth(&all, grandchild.id), 2);
    // The lead was deleted: the child was still created by an agent.
    assert_eq!(depth(&all[1..], child.id), 1);
    assert_eq!(depth(&all[1..], grandchild.id), 2);

    let mut a = ws("A", None, 1);
    let b = ws("B", Some(&a), 2);
    a.parent = Some(b.id);
    let cycle = [a.clone(), b];
    assert!(depth(&cycle, a.id) <= cycle.len());
}

#[test]
fn a_child_may_create_a_grandchild_may_not() {
    let lead = ws("Lead", None, 1);
    let child = ws("Child", Some(&lead), 2);
    let grandchild = ws("Grandchild", Some(&child), 3);
    let all = [lead.clone(), child.clone(), grandchild.clone()];
    assert_eq!(check_create(&all, lead.id, 0), Ok(()));
    assert_eq!(check_create(&all, child.id, 0), Ok(()));
    assert_eq!(
        check_create(&all, grandchild.id, 0),
        Err(Refusal::TooDeep { depth: 2 })
    );
}

#[test]
fn at_most_eight_active_children() {
    let lead = ws("Lead", None, 0);
    let mut all = vec![lead.clone()];
    for i in 1..=MAX_LIVE_CHILDREN as i64 {
        all.push(ws(&format!("C{i}"), Some(&lead), i));
    }
    assert_eq!(
        check_create(&all, lead.id, 0),
        Err(Refusal::TooManyChildren)
    );
    all[1].status = WorkspaceStatus::Archived;
    assert_eq!(check_create(&all, lead.id, 0), Ok(()));
    // Creates still running count too.
    assert_eq!(
        check_create(&all, lead.id, 1),
        Err(Refusal::TooManyChildren)
    );
    assert_eq!(children(&all, lead.id).len(), MAX_LIVE_CHILDREN - 1);
}

#[test]
fn children_are_the_active_ones_oldest_first() {
    let lead = ws("Lead", None, 0);
    let other = ws("Other", None, 0);
    let mut archived = ws("Old", Some(&lead), 1);
    archived.status = WorkspaceStatus::Archived;
    let all = [
        lead.clone(),
        ws("Second", Some(&lead), 3),
        ws("First", Some(&lead), 2),
        archived,
        ws("Theirs", Some(&other), 1),
    ];
    let names: Vec<&str> = children(&all, lead.id)
        .iter()
        .map(|w| w.name.as_str())
        .collect();
    assert_eq!(names, ["First", "Second"]);
}

#[test]
fn an_agent_finds_only_its_own_active_children() {
    let lead = ws("Lead", None, 0);
    let rival = ws("Rival", None, 0);
    let mine = ws("Fix login", Some(&lead), 1);
    let theirs = ws("Add tests", Some(&rival), 2);
    let mut old = ws("Old", Some(&lead), 3);
    old.status = WorkspaceStatus::Archived;
    let all = [
        lead.clone(),
        rival.clone(),
        mine.clone(),
        theirs.clone(),
        old.clone(),
    ];
    assert_eq!(
        find_child(&all, lead.id, &mine.id.to_string()).map(|w| w.id),
        Ok(mine.id)
    );
    assert_eq!(
        find_child(&all, lead.id, " fix LOGIN ").map(|w| w.id),
        Ok(mine.id)
    );
    assert_eq!(
        find_child(&all, lead.id, &theirs.id.to_string()),
        Err(Refusal::NotAChild("Add tests".into()))
    );
    assert_eq!(
        find_child(&all, lead.id, "Add tests"),
        Err(Refusal::NotAChild("Add tests".into()))
    );
    // The caller itself is not its own child.
    assert_eq!(
        find_child(&all, lead.id, "Lead"),
        Err(Refusal::NotAChild("Lead".into()))
    );
    assert_eq!(
        find_child(&all, lead.id, "Old"),
        Err(Refusal::Archived("Old".into()))
    );
    assert_eq!(
        find_child(&all, lead.id, "nope"),
        Err(Refusal::UnknownChild("nope".into()))
    );
}

#[test]
fn names_shared_with_other_workspaces_still_resolve_or_ask_for_the_id() {
    let lead = ws("Lead", None, 0);
    let rival = ws("Rival", None, 0);
    let mine = ws("Docs", Some(&lead), 1);
    let theirs = ws("Docs", Some(&rival), 2);
    let all = [lead.clone(), rival, mine.clone(), theirs];
    assert_eq!(find_child(&all, lead.id, "Docs").map(|w| w.id), Ok(mine.id));
    let twin = ws("Docs", Some(&lead), 3);
    let all = [all.to_vec(), vec![twin]].concat();
    assert_eq!(
        find_child(&all, lead.id, "docs"),
        Err(Refusal::Ambiguous("docs".into()))
    );
}

#[test]
fn a_child_starts_from_the_parents_base_named_after_its_task() {
    let lead = ws("Lead", None, 0);
    let spec = child_workspace(&lead, "\n  Fix the login crash \nwith a test\n", None).unwrap();
    assert_eq!(
        spec,
        NewWorkspace {
            name: "Fix the login crash".into(),
            base: Some("develop".into()),
            branch: None,
            task: Some("Fix the login crash \nwith a test".into()),
            group: None,
            parent: Some(lead.id),
        }
    );
    let named = child_workspace(&lead, "Do it", Some("  Login ")).unwrap();
    assert_eq!(named.name, "Login");
    let blank_name = child_workspace(&lead, "Do it", Some(" ")).unwrap();
    assert_eq!(blank_name.name, "Do it");
    assert_eq!(
        child_workspace(&lead, " \n ", None),
        Err(Refusal::EmptyTask)
    );
    let long = child_workspace(
        &lead,
        "Rewrite the onboarding flow so new users see the permissions screen first",
        None,
    )
    .unwrap();
    assert_eq!(long.name, "Rewrite the onboarding flow so new users…");
}

#[test]
fn a_child_waiting_for_the_user_cannot_be_prompted() {
    assert_eq!(
        check_prompt("Child", AgentState::Approval),
        Err(Refusal::WaitingForUser {
            name: "Child".into(),
            what: "a permission request",
        })
    );
    assert!(matches!(
        check_prompt("Child", AgentState::Input),
        Err(Refusal::WaitingForUser { .. })
    ));
    for state in [
        AgentState::Idle,
        AgentState::Working,
        AgentState::Review,
        AgentState::Failed,
    ] {
        assert_eq!(check_prompt("Child", state), Ok(()));
    }
}

#[test]
fn prompt_text_keeps_only_printable_text_and_line_breaks() {
    assert_eq!(prompt_text("fix it\n\tnow"), "fix it\n\tnow");
    assert_eq!(prompt_text("1\r"), "1");
    assert_eq!(prompt_text("a\x03b\x1b[Ac\u{9b}d"), "ab[Acd");
}
