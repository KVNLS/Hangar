use super::*;

fn agents(ids: &[&str]) -> Vec<(String, String)> {
    ids.iter()
        .map(|id| ((*id).to_owned(), id.to_uppercase()))
        .collect()
}

fn request(name: &str, task: &str, ids: &[&str], per_line: bool) -> BatchRequest {
    BatchRequest {
        name: name.into(),
        task: task.into(),
        agents: agents(ids),
        per_line,
        ..BatchRequest::default()
    }
}

#[test]
fn one_agent_one_task_is_a_plain_workspace() {
    let mut r = request(" Login ", "  Fix it ", &["claude"], false);
    r.branch = Some("fix/login".into());
    r.base = Some("develop".into());
    let planned = plan(&r).unwrap();
    assert_eq!(
        planned,
        [Planned {
            workspace: NewWorkspace {
                name: "Login".into(),
                base: Some("develop".into()),
                branch: Some("fix/login".into()),
                task: Some("Fix it".into()),
                group: None,
                parent: None,
            },
            agent: "claude".into(),
        }]
    );
    let blank = plan(&request("Login", "  ", &["claude"], false)).unwrap();
    assert_eq!(blank[0].workspace.task, None);
}

#[test]
fn several_agents_share_one_group_and_get_named_after_the_agent() {
    let mut r = request("Login", "Fix it", &["claude", "codex"], false);
    r.branch = Some("fix/login".into());
    let planned = plan(&r).unwrap();
    let names: Vec<_> = planned.iter().map(|p| p.workspace.name.as_str()).collect();
    assert_eq!(names, ["Login · CLAUDE", "Login · CODEX"]);
    assert!(planned[0].workspace.group.is_some());
    assert_eq!(planned[0].workspace.group, planned[1].workspace.group);
    // One branch name cannot be used twice.
    assert!(planned.iter().all(|p| p.workspace.branch.is_none()));
    assert!(
        planned
            .iter()
            .all(|p| p.workspace.task.as_deref() == Some("Fix it"))
    );
}

#[test]
fn one_workspace_per_line_ignores_the_name_and_blank_lines() {
    let planned = plan(&request(
        "ignored",
        "Fix login\n\n  Add tests  \n",
        &["claude"],
        true,
    ))
    .unwrap();
    let got: Vec<_> = planned
        .iter()
        .map(|p| (p.workspace.name.as_str(), p.workspace.task.as_deref()))
        .collect();
    assert_eq!(
        got,
        [
            ("Fix login", Some("Fix login")),
            ("Add tests", Some("Add tests"))
        ]
    );
    assert!(planned.iter().all(|p| p.workspace.group.is_none()));
}

#[test]
fn lines_times_agents_make_one_group_per_line() {
    let planned = plan(&request("", "A\nB", &["claude", "codex"], true)).unwrap();
    assert_eq!(planned.len(), 4);
    assert_eq!(planned[0].workspace.group, planned[1].workspace.group);
    assert_eq!(planned[2].workspace.group, planned[3].workspace.group);
    assert_ne!(planned[0].workspace.group, planned[2].workspace.group);
}

#[test]
fn refusals() {
    assert_eq!(
        plan(&request("x", "", &[], false)),
        Err(BatchError::NoAgent)
    );
    assert_eq!(
        plan(&request(" ", "task", &["a"], false)),
        Err(BatchError::EmptyName)
    );
    assert_eq!(
        plan(&request("x", " \n ", &["a"], true)),
        Err(BatchError::NoTasks)
    );
    let many = "t\n".repeat(5);
    assert_eq!(
        plan(&request("", &many, &["a", "b", "c"], true)),
        Err(BatchError::TooMany { count: 15 })
    );
}

#[test]
fn long_lines_are_cut_at_a_word() {
    let line = "Rewrite the onboarding flow so new users see the permissions screen first";
    assert_eq!(
        name_from_line(line),
        "Rewrite the onboarding flow so new users…"
    );
    let word = "x".repeat(60);
    assert_eq!(name_from_line(&word), format!("{}…", "x".repeat(40)));
}

fn attempt(group: Option<WorkspaceGroupId>, at: i64, active: bool) -> Workspace {
    Workspace {
        id: WorkspaceId::new(),
        project_id: hangar_core::ProjectId::new(),
        name: format!("ws{at}"),
        slug: format!("ws{at}"),
        branch: format!("hangar/ws{at}"),
        base: "main".into(),
        worktree_path: format!("/wt/ws{at}").into(),
        status: if active {
            hangar_core::WorkspaceStatus::Active
        } else {
            hangar_core::WorkspaceStatus::Archived
        },
        created_at: hangar_core::Timestamp(at),
        task: None,
        group,
        parent: None,
    }
}

#[test]
fn keeping_one_attempt_archives_the_other_active_ones() {
    let group = WorkspaceGroupId::new();
    let all = [
        attempt(Some(group), 3, true),
        attempt(Some(group), 1, true),
        attempt(Some(group), 2, false),
        attempt(Some(WorkspaceGroupId::new()), 4, true),
        attempt(None, 5, true),
    ];
    let listed: Vec<i64> = attempts(&all, group)
        .iter()
        .map(|ws| ws.created_at.0)
        .collect();
    assert_eq!(listed, [1, 3]);
    assert_eq!(others_to_archive(&all, &all[1]), [all[0].id]);
    assert!(others_to_archive(&all, &all[4]).is_empty());
}
