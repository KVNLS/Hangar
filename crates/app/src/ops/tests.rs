use super::*;
use crate::agents;
use hangar_core::{SessionStatus, Setting, Timestamp, WorkspaceStatus, branch_name};
use hangar_store_sqlite::Store;
use hangar_terminal::ExitInfo;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(
        dir.path(),
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    );
    dir
}

#[test]
fn add_project_resolves_root_and_is_idempotent() {
    let repo = repo();
    let sub = repo.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let store = Store::open_in_memory().unwrap();
    let project = add_project(&store, &sub).unwrap();
    assert_eq!(
        project.repo_path.canonicalize().unwrap(),
        repo.path().canonicalize().unwrap()
    );
    assert_eq!(project.default_base, "main");
    assert_eq!(add_project(&store, repo.path()).unwrap().id, project.id);
    assert_eq!(store.list_projects().unwrap().len(), 1);
}

#[test]
fn add_project_rejects_non_repo() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    assert!(add_project(&store, dir.path()).is_err());
}

#[test]
fn create_workspace_from_a_base_on_a_chosen_branch() {
    let repo = repo();
    let root = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let project = add_project(&store, repo.path()).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    git(&["branch", "release"]);
    let release = git(&["rev-parse", "release"]);

    let ws = create_workspace_from(
        &store,
        &project,
        "Hotfix",
        root.path(),
        Some("release"),
        Some("fix/crash"),
    )
    .unwrap();
    assert_eq!(
        (ws.slug.as_str(), ws.branch.as_str()),
        ("hotfix", "fix/crash")
    );
    assert_eq!(ws.base, "release");
    assert_eq!(git(&["rev-parse", "fix/crash"]), release);

    let taken = create_workspace_from(
        &store,
        &project,
        "again",
        root.path(),
        None,
        Some("fix/crash"),
    );
    assert!(taken.unwrap_err().to_string().contains("already exists"));
    let bad = create_workspace_from(&store, &project, "x", root.path(), None, Some("a..b"));
    assert!(
        bad.unwrap_err()
            .to_string()
            .contains("not a valid branch name")
    );
    // Blank fields fall back to the defaults.
    let ws =
        create_workspace_from(&store, &project, "plain", root.path(), Some(" "), Some("")).unwrap();
    assert_eq!(ws.branch, "hangar/plain");
    assert_eq!(ws.base, project.default_base);
}

#[test]
fn create_workspace_makes_branch_worktree_and_row() {
    let repo = repo();
    let root = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let project = add_project(&store, repo.path()).unwrap();

    let ws = create_workspace(&store, &project, "Fix Login", root.path()).unwrap();
    assert_eq!(ws.slug, "fix-login");
    assert_eq!(ws.branch, "hangar/fix-login");
    assert!(ws.worktree_path.is_dir());
    assert!(
        ws.worktree_path
            .ends_with(format!("{}/fix-login", hangar_core::slugify(&project.name)))
    );

    let again = create_workspace(&store, &project, "fix login", root.path()).unwrap();
    assert_eq!(again.slug, "fix-login-2");
    assert_eq!(again.branch, "hangar/fix-login-2");

    let loaded = load(&store).unwrap();
    assert_eq!(loaded.entries.len(), 1);
    assert_eq!(loaded.entries[0].workspaces.len(), 2);

    assert!(create_workspace(&store, &project, "   ", root.path()).is_err());
}

#[test]
fn session_runs_records_and_stale_sessions_are_failed_on_load() {
    let repo = repo();
    let root = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let project = add_project(&store, repo.path()).unwrap();
    let ws = create_workspace(&store, &project, "run", root.path()).unwrap();

    let adapter =
        hangar_agents::PtyAdapter::new("sh", "sh", "/bin/sh").with_args(["-c", "pwd; exit 3"]);
    let started = start_session(&store, &adapter, &ws, None, Arc::new(|| {})).unwrap();
    let exit = started
        .handle
        .session()
        .wait_for_exit(Duration::from_secs(10))
        .unwrap();
    assert_eq!(exit_status(&exit), SessionStatus::Exited { code: 3 });
    let row = store.get_session(started.session.id).unwrap();
    assert_eq!(row.status, SessionStatus::Running);

    load(&store).unwrap();
    let row = store.get_session(started.session.id).unwrap();
    assert!(matches!(row.status, SessionStatus::Failed { .. }));
    assert!(row.ended_at.is_some());
}

#[test]
fn saved_conversations_load_per_workspace() {
    let repo = repo();
    let root = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let project = add_project(&store, repo.path()).unwrap();
    let ws = create_workspace(&store, &project, "chat", root.path()).unwrap();
    let chat = SavedChat {
        agent: "claude-acp".into(),
        session: "s-1".into(),
    };
    store
        .set_setting(&setting_keys::chat(ws.id), &chat)
        .unwrap();
    let loaded = load(&store).unwrap();
    assert_eq!(loaded.chats.get(&ws.id), Some(&chat));
}

#[test]
fn agent_config_choices_replace_and_keep_order() {
    let store = Store::open_in_memory().unwrap();
    assert!(saved_agent_config(&store, "codex-acp").is_empty());
    remember_agent_config(&store, "codex-acp", "model", "gpt-5.4").unwrap();
    remember_agent_config(&store, "codex-acp", "reasoning_effort", "high").unwrap();
    remember_agent_config(&store, "codex-acp", "model", "gpt-5.5").unwrap();
    remember_agent_config(&store, "claude-acp", "model", "opus").unwrap();
    let pair = |a: &str, b: &str| (a.to_owned(), b.to_owned());
    assert_eq!(
        saved_agent_config(&store, "codex-acp"),
        [pair("reasoning_effort", "high"), pair("model", "gpt-5.5")]
    );
    assert_eq!(
        saved_agent_config(&store, "claude-acp"),
        [pair("model", "opus")]
    );
}

#[test]
fn failed_spawn_is_recorded() {
    let repo = repo();
    let root = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let project = add_project(&store, repo.path()).unwrap();
    let ws = create_workspace(&store, &project, "x", root.path()).unwrap();
    let Some(agents::AgentPreset::Pty(adapter)) = agents::preset("claude") else {
        panic!("claude is a terminal preset");
    };
    let Err(err) = start_session(&store, &adapter, &ws, Some("/nonexistent"), Arc::new(|| {}))
    else {
        panic!("claude is not on /nonexistent");
    };
    let cause = "claude: `claude` was not found on PATH";
    assert_eq!(
        format!("{err:#}"),
        format!("failed to start Claude Code: {cause}")
    );
    let sessions = store.list_sessions(ws.id).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].status,
        SessionStatus::Failed {
            message: cause.to_owned()
        }
    );
}

#[test]
fn session_extras_write_files_and_reach_the_agent() {
    use std::os::unix::fs::PermissionsExt as _;
    let repo = repo();
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let project = add_project(&store, repo.path()).unwrap();
    let ws = create_workspace(&store, &project, "extras", root.path()).unwrap();

    let config = data.path().join("mcp/ws.json");
    let out = data.path().join("out.txt");
    let adapter = hangar_agents::PtyAdapter::new("sh", "sh", "/bin/sh").with_args([
        "-c",
        r#"printf '%s|%s|%s' "$HANGAR_X" "$1" "$(cat "$CONFIG")" > "$OUT""#,
        "sh",
    ]);
    let extra = AgentExtras {
        mcp_servers: Vec::new(),
        args: vec!["--flag".into()],
        env: vec![
            ("HANGAR_X".into(), "x1".into()),
            ("CONFIG".into(), config.display().to_string()),
            ("OUT".into(), out.display().to_string()),
        ],
        files: vec![(config.clone(), "{}".into())],
    };
    let started = start_session_with(&store, &adapter, &ws, None, Arc::new(|| {}), &extra).unwrap();
    let exit = started
        .handle
        .session()
        .wait_for_exit(Duration::from_secs(10))
        .unwrap();
    assert_eq!(exit_status(&exit), SessionStatus::Exited { code: 0 });
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "x1|--flag|{}");
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&config), 0o600);
    assert_eq!(mode(config.parent().unwrap()), 0o700);
    assert_eq!(
        started.session.args.last().map(String::as_str),
        Some("--flag")
    );

    let inside = AgentExtras {
        files: vec![(ws.worktree_path.join(".mcp.json"), "{}".into())],
        ..AgentExtras::default()
    };
    let err = start_session_with(&store, &adapter, &ws, None, Arc::new(|| {}), &inside)
        .err()
        .unwrap();
    assert!(
        format!("{err:#}").contains("outside the worktree"),
        "{err:#}"
    );
    assert!(!ws.worktree_path.join(".mcp.json").exists());
    let sessions = store.list_sessions(ws.id).unwrap();
    assert!(
        sessions
            .iter()
            .any(|s| matches!(s.status, SessionStatus::Failed { .. }))
    );
}

#[test]
fn exit_status_mapping() {
    let ok = |code, signal: Option<&str>| {
        Ok(ExitInfo {
            code,
            signal: signal.map(str::to_owned),
        })
    };
    assert_eq!(exit_status(&ok(0, None)), SessionStatus::Exited { code: 0 });
    assert!(matches!(
        exit_status(&ok(1, Some("SIGKILL"))),
        SessionStatus::Failed { .. }
    ));
    assert!(matches!(
        exit_status(&Err("boom".into())),
        SessionStatus::Failed { .. }
    ));
}

fn commit_all(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    hangar_worktree::branch_exists(repo, branch).unwrap()
}

struct Fixture {
    repo: tempfile::TempDir,
    root: tempfile::TempDir,
    store: Store,
    project: Project,
}

fn fixture() -> Fixture {
    let repo = repo();
    let root = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let project = add_project(&store, repo.path()).unwrap();
    Fixture {
        repo,
        root,
        store,
        project,
    }
}

impl Fixture {
    fn workspace(&self, name: &str) -> Workspace {
        let ws = create_workspace(&self.store, &self.project, name, self.root.path()).unwrap();
        self.store
            .insert_session(&Session::new(ws.id, agents::kind("shell")))
            .unwrap();
        ws
    }

    fn assert_rows_gone(&self, ws: &Workspace) {
        assert!(self.store.get_workspace(ws.id).is_err());
        assert!(self.store.list_sessions(ws.id).unwrap().is_empty());
    }
}

#[test]
fn delete_clean_workspace_keeps_branch_by_default() {
    let f = fixture();
    let ws = f.workspace("clean");
    let summary = delete_summary(&ws).unwrap();
    assert_eq!(
        summary,
        DeleteSummary {
            exists: true,
            uncommitted: 0,
            untracked: 0,
            ahead: Some(0),
        }
    );

    let outcome = delete_workspace(&f.store, ws.id, DeleteOptions::default()).unwrap();
    assert!(outcome.branch_error.is_none());
    assert!(!ws.worktree_path.exists());
    assert!(branch_exists(f.repo.path(), &ws.branch));
    f.assert_rows_gone(&ws);
    // The repository and the worktree root are untouched.
    assert!(f.repo.path().join(".git").is_dir());
    assert!(f.root.path().is_dir());
    assert!(
        hangar_worktree::list(f.repo.path())
            .unwrap()
            .iter()
            .all(|wt| wt.is_main)
    );
}

#[test]
fn delete_can_delete_a_merged_branch() {
    let f = fixture();
    let ws = f.workspace("merged");
    let options = DeleteOptions {
        force: false,
        delete_branch: true,
    };
    let outcome = delete_workspace(&f.store, ws.id, options).unwrap();
    assert!(outcome.branch_error.is_none());
    assert!(!ws.worktree_path.exists());
    assert!(!branch_exists(f.repo.path(), &ws.branch));
    f.assert_rows_gone(&ws);
}

#[test]
fn delete_keeps_an_unmerged_branch_and_reports_why() {
    let f = fixture();
    let ws = f.workspace("work");
    std::fs::write(ws.worktree_path.join("new.txt"), "x").unwrap();
    commit_all(&ws.worktree_path, "work");
    assert_eq!(delete_summary(&ws).unwrap().ahead, Some(1));

    let options = DeleteOptions {
        force: false,
        delete_branch: true,
    };
    let outcome = delete_workspace(&f.store, ws.id, options).unwrap();
    let error = outcome.branch_error.unwrap();
    assert!(error.contains("hangar/work"), "{error}");
    assert!(branch_exists(f.repo.path(), &ws.branch));
    assert!(!ws.worktree_path.exists());
    f.assert_rows_gone(&ws);
}

#[test]
fn dirty_workspace_needs_force() {
    let f = fixture();
    let ws = f.workspace("dirty");
    std::fs::write(ws.worktree_path.join("scratch.txt"), "x").unwrap();
    let summary = delete_summary(&ws).unwrap();
    assert_eq!((summary.uncommitted, summary.untracked), (0, 1));
    assert!(summary.is_dirty());

    assert!(delete_workspace(&f.store, ws.id, DeleteOptions::default()).is_err());
    assert!(ws.worktree_path.join("scratch.txt").exists());
    assert!(f.store.get_workspace(ws.id).is_ok());

    let force = DeleteOptions {
        force: true,
        delete_branch: false,
    };
    delete_workspace(&f.store, ws.id, force).unwrap();
    assert!(!ws.worktree_path.exists());
    f.assert_rows_gone(&ws);
}

#[test]
fn delete_of_missing_directory_only_prunes_and_forgets() {
    let f = fixture();
    let ws = f.workspace("gone");
    std::fs::remove_dir_all(&ws.worktree_path).unwrap();
    assert_eq!(delete_summary(&ws).unwrap(), DeleteSummary::default());
    delete_workspace(&f.store, ws.id, DeleteOptions::default()).unwrap();
    f.assert_rows_gone(&ws);
    assert_eq!(hangar_worktree::list(f.repo.path()).unwrap().len(), 1);
}

#[test]
fn delete_refuses_paths_that_are_not_linked_worktrees() {
    let f = fixture();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("keep.txt"), "x").unwrap();
    for (slug, path) in [
        ("main", f.repo.path().to_path_buf()),
        ("outside", outside.path().to_path_buf()),
    ] {
        let ws = Workspace {
            id: WorkspaceId::new(),
            project_id: f.project.id,
            name: slug.into(),
            slug: slug.into(),
            branch: branch_name(slug),
            base: "main".into(),
            worktree_path: path.clone(),
            status: WorkspaceStatus::Active,
            created_at: Timestamp::now(),
        };
        f.store.insert_workspace(&ws).unwrap();
        let force = DeleteOptions {
            force: true,
            delete_branch: true,
        };
        let err = delete_workspace(&f.store, ws.id, force).unwrap_err();
        assert!(format!("{err:#}").contains("refusing"), "{err:#}");
        assert!(f.store.get_workspace(ws.id).is_ok());
    }
    assert!(outside.path().join("keep.txt").exists());
    assert!(f.repo.path().join(".git").is_dir());
}

#[test]
fn archive_restore_and_load_keep_archived_workspaces() {
    let f = fixture();
    let ws = f.workspace("later");
    let session = f.store.list_sessions(ws.id).unwrap()[0].id;
    archive_workspace(&f.store, ws.id, Some(session)).unwrap();
    assert_eq!(
        f.store.get_workspace(ws.id).unwrap().status,
        WorkspaceStatus::Archived
    );
    let row = f.store.get_session(session).unwrap();
    assert!(matches!(row.status, SessionStatus::Failed { .. }));
    assert!(ws.worktree_path.is_dir());

    let loaded = load(&f.store).unwrap();
    assert_eq!(loaded.entries[0].archived().count(), 1);
    assert_eq!(loaded.entries[0].active().count(), 0);

    restore_workspace(&f.store, ws.id).unwrap();
    let loaded = load(&f.store).unwrap();
    assert_eq!(loaded.entries[0].active().count(), 1);
}

#[test]
fn remove_project_requires_no_active_workspaces() {
    let f = fixture();
    let ws = f.workspace("busy");
    let err = remove_project(&f.store, f.project.id).unwrap_err();
    assert!(err.to_string().contains("busy"), "{err}");

    archive_workspace(&f.store, ws.id, None).unwrap();
    remove_project(&f.store, f.project.id).unwrap();
    assert!(f.store.list_projects().unwrap().is_empty());
    // The repository and the archived worktree stay on disk.
    assert!(f.repo.path().join(".git").is_dir());
    assert!(ws.worktree_path.is_dir());
}

#[test]
fn settings_are_loaded_and_bad_ones_reported() {
    let f = fixture();
    let loaded = load(&f.store).unwrap();
    assert!(loaded.layout.is_none() && loaded.agent.is_none());
    assert!(loaded.settings_error.is_none());

    let layout = PaneLayout {
        panes: vec![Some(WorkspaceId::new()), None],
        focused: 1,
        columns: vec![1, 1],
        sizes: vec![0.4, 0.6],
        rows: vec![Vec::new(), Vec::new()],
        split: None,
        panel_width: Some(480.0),
        sidebar_width: Some(300.0),
    };
    save_layout(&f.store, &layout).unwrap();
    save_agent(&f.store, "codex").unwrap();
    let loaded = load(&f.store).unwrap();
    assert_eq!(loaded.layout, Some(layout));
    assert_eq!(loaded.agent.as_deref(), Some("codex"));
    assert!(loaded.changes.is_none());

    let changes = ChangesLayout { open: true };
    save_changes(&f.store, &changes).unwrap();
    assert_eq!(load(&f.store).unwrap().changes, Some(changes));

    f.store.set_setting(&Setting::new("layout"), &42).unwrap();
    let loaded = load(&f.store).unwrap();
    assert!(loaded.layout.is_none());
    assert!(loaded.settings_error.is_some());
    assert_eq!(loaded.entries.len(), 1);
}

#[test]
fn notify_attention_round_trips() {
    let f = fixture();
    assert_eq!(load(&f.store).unwrap().notify_attention, None);
    save_notify_attention(&f.store, false).unwrap();
    assert_eq!(load(&f.store).unwrap().notify_attention, Some(false));
    save_notify_attention(&f.store, true).unwrap();
    assert_eq!(load(&f.store).unwrap().notify_attention, Some(true));
}

#[test]
fn device_settings_round_trip() {
    use crate::device_panel::{DeviceKey, Platform};

    let f = fixture();
    assert!(load(&f.store).unwrap().device.is_none());
    let device = DeviceLayout {
        open: true,
        selected: Some(DeviceKey {
            platform: Platform::Ios,
            id: "E33FE595-67AD-470C-AD94-A38EAC9D0F38".into(),
        }),
        stats: true,
        logs: true,
        log_level: Some("error".into()),
    };
    save_device(&f.store, &device).unwrap();
    let loaded = load(&f.store).unwrap();
    assert_eq!(loaded.device, Some(device.clone()));
    assert!(loaded.settings_error.is_none());

    let android = DeviceLayout {
        selected: Some(DeviceKey {
            platform: Platform::Android,
            id: "emulator-5554".into(),
        }),
        ..device
    };
    save_device(&f.store, &android).unwrap();
    assert_eq!(load(&f.store).unwrap().device, Some(android));

    // A value saved before Android existed (platform written as a plain string).
    #[derive(serde::Serialize)]
    struct Saved {
        open: bool,
        width: f32,
        selected: SavedKey,
        stats: bool,
    }
    #[derive(serde::Serialize)]
    struct SavedKey {
        platform: &'static str,
        id: &'static str,
    }
    f.store
        .set_setting(
            &Setting::new("device_panel"),
            &Saved {
                open: true,
                width: 400.0,
                selected: SavedKey {
                    platform: "ios",
                    id: "U",
                },
                stats: false,
            },
        )
        .unwrap();
    let old = load(&f.store).unwrap().device.unwrap();
    assert_eq!(
        old.selected,
        Some(DeviceKey {
            platform: Platform::Ios,
            id: "U".into()
        })
    );

    // Fields added later default when an older value is read.
    #[derive(serde::Serialize)]
    struct Old {
        open: bool,
        width: f32,
    }
    f.store
        .set_setting(
            &Setting::new("device_panel"),
            &Old {
                open: false,
                width: 300.0,
            },
        )
        .unwrap();
    let old = load(&f.store).unwrap().device.unwrap();
    assert_eq!((old.open, old.selected, old.stats), (false, None, false));
    assert!(!old.logs && old.log_level.is_none());

    // A broken device setting is reported but leaves the other settings alone.
    save_agent(&f.store, "codex").unwrap();
    f.store
        .set_setting(&Setting::new("device_panel"), &"nope")
        .unwrap();
    let loaded = load(&f.store).unwrap();
    assert!(loaded.device.is_none());
    assert!(loaded.settings_error.is_some());
    assert_eq!(loaded.agent.as_deref(), Some("codex"));
}

#[test]
fn delete_summary_description() {
    let clean = DeleteSummary {
        exists: true,
        ahead: Some(0),
        ..Default::default()
    };
    assert_eq!(
        clean.describe("main"),
        [
            "No uncommitted or untracked files.",
            "No commits ahead of main."
        ]
    );
    let dirty = DeleteSummary {
        exists: true,
        uncommitted: 2,
        untracked: 1,
        ahead: Some(3),
    };
    assert_eq!(
        dirty.describe("main"),
        [
            "2 uncommitted and 1 untracked file(s) will be lost.",
            "3 commit(s) ahead of main."
        ]
    );
    assert_eq!(DeleteSummary::default().describe("main").len(), 1);
}
