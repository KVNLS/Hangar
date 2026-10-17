use hangar_core::{AgentKind, ProjectId, Session, Workspace, WorkspaceGroupId, WorkspaceStatus};

use crate::StoreError;
use crate::fixtures::{project, store, workspace};

#[test]
fn workspace_crud_round_trip() {
    let store = store();
    let p = project("/repos/a");
    store.insert_project(&p).unwrap();
    let w1 = workspace(p.id, "Fix login");
    let w2 = workspace(p.id, "Add OAuth");
    store.insert_workspace(&w1).unwrap();
    store.insert_workspace(&w2).unwrap();

    assert_eq!(store.get_workspace(w1.id).unwrap(), w1);
    assert_eq!(
        store.list_workspaces(p.id).unwrap(),
        vec![w1.clone(), w2.clone()]
    );
    assert!(store.list_workspaces(ProjectId::new()).unwrap().is_empty());

    store
        .update_workspace_status(w1.id, WorkspaceStatus::Archived)
        .unwrap();
    assert_eq!(
        store.get_workspace(w1.id).unwrap().status,
        WorkspaceStatus::Archived
    );

    store.delete_workspace(w1.id).unwrap();
    assert_eq!(store.list_workspaces(p.id).unwrap(), vec![w2]);
    assert!(matches!(
        store.update_workspace_status(w1.id, WorkspaceStatus::Active),
        Err(StoreError::NotFound {
            entity: "workspace",
            ..
        })
    ));
}

#[test]
fn duplicate_slug_conflicts_only_within_a_project() {
    let store = store();
    let a = project("/repos/a");
    let b = project("/repos/b");
    store.insert_project(&a).unwrap();
    store.insert_project(&b).unwrap();
    store.insert_workspace(&workspace(a.id, "task")).unwrap();
    store.insert_workspace(&workspace(b.id, "task")).unwrap();
    assert!(matches!(
        store.insert_workspace(&workspace(a.id, "task")),
        Err(StoreError::Conflict(_))
    ));
}

#[test]
fn workspace_for_missing_project_is_not_found() {
    let store = store();
    assert!(matches!(
        store.insert_workspace(&workspace(ProjectId::new(), "orphan")),
        Err(StoreError::NotFound {
            entity: "project",
            ..
        })
    ));
}

#[test]
fn deleting_a_workspace_cascades_to_sessions() {
    let store = store();
    let p = project("/repos/a");
    store.insert_project(&p).unwrap();
    let w = workspace(p.id, "task");
    store.insert_workspace(&w).unwrap();
    store
        .insert_session(&Session::new(w.id, AgentKind::Gemini))
        .unwrap();

    store.delete_workspace(w.id).unwrap();
    assert!(store.list_sessions(w.id).unwrap().is_empty());
}

#[test]
fn a_workspace_task_and_group_round_trip() {
    let store = store();
    let p = project("/repos/task");
    store.insert_project(&p).unwrap();
    let ws = Workspace {
        task: Some("Fix the login screen\nand its tests".to_owned()),
        group: Some(WorkspaceGroupId::new()),
        ..workspace(p.id, "Login")
    };
    store.insert_workspace(&ws).unwrap();
    assert_eq!(store.get_workspace(ws.id).unwrap(), ws);
    assert_eq!(store.list_workspaces(p.id).unwrap(), vec![ws]);
}

#[test]
fn a_workspace_parent_round_trips() {
    let store = store();
    let p = project("/repos/parent");
    store.insert_project(&p).unwrap();
    let lead = workspace(p.id, "Lead");
    let child = Workspace {
        parent: Some(lead.id),
        ..workspace(p.id, "Child")
    };
    store.insert_workspace(&lead).unwrap();
    store.insert_workspace(&child).unwrap();
    assert_eq!(store.get_workspace(child.id).unwrap(), child);
    assert_eq!(store.list_workspaces(p.id).unwrap(), vec![lead, child]);
}
