use super::*;

#[test]
fn archive_restore_and_load_keep_archived_workspaces() {
    let f = fixture();
    let ws = f.workspace("later");
    let session = f.store.list_sessions(ws.id).unwrap()[0].id;
    let stat = hangar_core::changes::DiffStat {
        files: 1,
        insertions: 5,
        deletions: 2,
    };
    archive_workspace(&f.store, ws.id, Some(session), Some(stat)).unwrap();
    assert_eq!(
        f.store.get_workspace(ws.id).unwrap().status,
        WorkspaceStatus::Archived
    );
    let row = f.store.get_session(session).unwrap();
    assert!(matches!(row.status, SessionStatus::Failed { .. }));
    assert_eq!(row.changes, Some(stat));
    let ended = sessions_ended_since(&f.store, hangar_core::Timestamp(0)).unwrap();
    assert_eq!(ended.iter().map(|s| s.id).collect::<Vec<_>>(), [session]);
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

    archive_workspace(&f.store, ws.id, None, None).unwrap();
    remove_project(&f.store, f.project.id).unwrap();
    assert!(f.store.list_projects().unwrap().is_empty());
    // The repository and the archived worktree stay on disk.
    assert!(f.repo.path().join(".git").is_dir());
    assert!(ws.worktree_path.is_dir());
}
