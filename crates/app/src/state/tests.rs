use super::{
    ChangesLayout, DeviceLayout, PANEL_DEFAULT_WIDTH, PANEL_WIDTH_RANGE, PaneLayout, Panes,
    ProjectList, SIDEBAR_DEFAULT_WIDTH, SIDEBAR_WIDTH_RANGE, pane_shares, panel_width,
    sanitize_shares, sidebar_width, status_label,
};
use crate::device_panel::{DeviceKey, LogsSettings, Platform};
use hangar_core::{Project, SessionStatus, Timestamp, Workspace, WorkspaceId, WorkspaceStatus};
use hangar_devices::LogLevel;

fn workspace(project: &Project, name: &str) -> Workspace {
    Workspace {
        id: WorkspaceId::new(),
        project_id: project.id,
        name: name.into(),
        slug: name.into(),
        branch: format!("hangar/{name}"),
        base: "main".into(),
        worktree_path: format!("/wt/{name}").into(),
        status: WorkspaceStatus::Active,
        created_at: Timestamp::now(),
        task: None,
        group: None,
        parent: None,
    }
}

#[test]
fn project_list_adds_and_updates() {
    let mut list = ProjectList::default();
    let project = Project::new("app", "/repo", "main");
    list.add_project(project.clone());
    list.add_project(project.clone());
    assert_eq!(list.entries().len(), 1);

    let mut ws = workspace(&project, "a");
    assert!(list.upsert_workspace(ws.clone()));
    ws.name = "renamed".into();
    assert!(list.upsert_workspace(ws.clone()));
    assert_eq!(list.entries()[0].workspaces.len(), 1);
    assert_eq!(list.workspace(ws.id).unwrap().name, "renamed");

    let orphan = workspace(&Project::new("other", "/o", "main"), "b");
    assert!(!list.upsert_workspace(orphan.clone()));
    assert!(list.workspace(orphan.id).is_none());
}

#[test]
fn panes_show_in_focused_pane_without_duplicates() {
    let (a, b, c) = (WorkspaceId::new(), WorkspaceId::new(), WorkspaceId::new());
    let mut panes = Panes::default();
    assert_eq!(panes.count(), 1);
    assert_eq!(panes.show(a), 0);
    assert_eq!(panes.split(), Some(1));
    assert_eq!(panes.show(b), 1);
    assert_eq!((panes.slot(0), panes.slot(1)), (Some(a), Some(b)));

    // Already visible in pane 0: focus moves there instead of duplicating.
    assert_eq!(panes.show(a), 0);
    assert_eq!(panes.focused(), 0);
    assert_eq!(panes.slot(1), Some(b));
    assert_eq!(panes.pane_of(b), Some(1));

    assert_eq!(panes.show(c), 0);
    assert!(!panes.is_shown(a));
    assert!(panes.is_shown(c));

    panes.focus(7);
    assert_eq!(panes.focused(), 0);
}

#[test]
fn panes_split_and_close() {
    let (a, b, c) = (WorkspaceId::new(), WorkspaceId::new(), WorkspaceId::new());
    let mut panes = Panes::default();
    assert!(!panes.close(0), "the last pane stays");
    panes.show(a);
    panes.split();
    panes.show(b);
    // Splitting the first pane inserts between a and b.
    panes.focus(0);
    assert_eq!(panes.split(), Some(1));
    panes.show(c);
    assert_eq!(
        (0..3).map(|p| panes.slot(p)).collect::<Vec<_>>(),
        [Some(a), Some(c), Some(b)]
    );
    assert_eq!(panes.split(), Some(2));
    assert_eq!(panes.count(), 4);
    assert_eq!(panes.split(), None);

    // Closing the focused pane focuses its left neighbour.
    assert!(panes.close(2));
    assert_eq!((panes.count(), panes.focused()), (3, 1));
    // Closing a pane left of the focused one keeps the same pane focused.
    assert!(panes.close(0));
    assert_eq!((panes.slot(panes.focused()), panes.count()), (Some(c), 2));
    // Closing the first pane while focused focuses the new first.
    panes.focus(0);
    assert!(panes.close(0));
    assert_eq!((panes.slot(0), panes.focused()), (Some(b), 0));
    assert!(!panes.close(0));
    assert!(!panes.close(5));
}

#[test]
fn archive_restore_and_remove() {
    let mut list = ProjectList::default();
    let project = Project::new("app", "/repo", "main");
    list.add_project(project.clone());
    let a = workspace(&project, "a");
    let b = workspace(&project, "b");
    list.upsert_workspace(a.clone());
    list.upsert_workspace(b.clone());

    assert!(list.set_status(a.id, WorkspaceStatus::Archived));
    assert!(!list.is_active(a.id));
    let entry = list.entry(project.id).unwrap();
    assert_eq!(entry.active().map(|w| w.id).collect::<Vec<_>>(), [b.id]);
    assert_eq!(entry.archived().map(|w| w.id).collect::<Vec<_>>(), [a.id]);

    assert!(list.set_status(a.id, WorkspaceStatus::Active));
    assert!(list.is_active(a.id));
    assert!(!list.set_status(WorkspaceId::new(), WorkspaceStatus::Active));

    assert_eq!(list.remove_workspace(a.id).map(|w| w.id), Some(a.id));
    assert!(list.remove_workspace(a.id).is_none());
    assert!(list.workspace(a.id).is_none());

    let removed = list.remove_project(project.id).unwrap();
    assert_eq!(removed.workspaces.len(), 1);
    assert!(list.is_empty());
    assert!(list.remove_project(project.id).is_none());
}

#[test]
fn panes_clear_and_layout_round_trip() {
    let (a, b, gone) = (WorkspaceId::new(), WorkspaceId::new(), WorkspaceId::new());
    let mut panes = Panes::default();
    panes.show(a);
    panes.split();
    panes.show(b);
    assert!(panes.set_column_shares(vec![0.3, 0.7]));
    assert!(!panes.set_column_shares(vec![1.0]));

    let layout = panes.layout();
    assert_eq!(layout.panes, vec![Some(a), Some(b)]);
    assert_eq!(layout.focused, 1);
    assert_eq!(layout.columns, [1, 1]);
    assert_eq!(layout.sizes, [0.3, 0.7]);
    assert_eq!(layout.restore(|_| true), panes);

    // Unknown workspaces, duplicates and empty panes are dropped; focus follows its pane.
    let saved = PaneLayout {
        panes: vec![Some(gone), None, Some(a), Some(a), Some(b)],
        focused: 4,
        sizes: vec![0.2, 0.2, 0.2, 0.2, 0.2],
        ..Default::default()
    };
    let restored = saved.restore(|id| id != gone);
    assert_eq!((restored.slot(0), restored.slot(1)), (Some(a), Some(b)));
    assert_eq!((restored.count(), restored.focused()), (2, 1));
    assert_eq!(restored.shares().columns, [0.5, 0.5]);

    // Nothing left: one empty pane.
    assert_eq!(saved.restore(|_| false), Panes::default());

    assert!(panes.clear(a));
    assert!(!panes.clear(a));
    assert_eq!((panes.slot(0), panes.slot(1)), (None, Some(b)));
}

#[test]
fn panes_split_down_into_columns() {
    let (a, b, c, d) = (
        WorkspaceId::new(),
        WorkspaceId::new(),
        WorkspaceId::new(),
        WorkspaceId::new(),
    );
    let mut panes = Panes::default();
    panes.show(a);
    assert_eq!(panes.split_down(), Some(1));
    panes.show(b);
    assert_eq!(panes.layout().columns, [2]);
    assert_eq!(panes.row_shares(0), [0.5, 0.5]);
    // Split right of the bottom pane: a new column after the first.
    assert_eq!(panes.split(), Some(2));
    panes.show(c);
    assert_eq!(panes.columns(), [0..2, 2..3]);
    assert_eq!(panes.column_of(1), 0);
    assert_eq!(panes.column_shares(), [0.5, 0.5]);
    // Split the top pane down: it shares its half of the height.
    panes.focus(0);
    assert_eq!(panes.split_down(), Some(1));
    panes.show(d);
    assert_eq!(panes.columns(), [0..3, 3..4]);
    assert_eq!(panes.row_shares(0), [0.25, 0.25, 0.5]);
    assert_eq!(
        (0..4).map(|p| panes.slot(p)).collect::<Vec<_>>(),
        [Some(a), Some(d), Some(b), Some(c)]
    );
    assert_eq!(panes.split_down(), None);
    assert_eq!(panes.split(), None);

    // The round trip keeps the columns and their row shares.
    let layout = panes.layout();
    assert_eq!(layout.columns, [3, 1]);
    assert_eq!(layout.restore(|_| true), panes);

    // Closing a pane gives its height to its column; the last pane of a column takes the
    // column with it.
    assert!(panes.close(1));
    assert_eq!(panes.columns(), [0..2, 2..3]);
    assert_eq!(panes.row_shares(0), [1. / 3., 2. / 3.]);
    assert_eq!(panes.focused(), 0);
    assert!(panes.close(2));
    assert_eq!(panes.layout().columns, [2]);
    assert!(panes.shares().columns.is_empty());
    assert!(panes.set_row_shares(0, vec![0.4, 0.6]));
    assert!(!panes.set_row_shares(0, vec![1.0]));
    assert!(!panes.set_row_shares(3, vec![0.4, 0.6]));
    assert!(panes.close(0));
    assert_eq!(panes.layout().columns, [1]);
    assert_eq!(panes.slot(0), Some(b));
}

#[test]
fn column_layouts_restore_without_dropped_panes() {
    let (a, b, c) = (WorkspaceId::new(), WorkspaceId::new(), WorkspaceId::new());
    let saved = PaneLayout {
        panes: vec![Some(a), Some(b), None, Some(c)],
        focused: 3,
        columns: vec![2, 1, 1],
        sizes: vec![0.5, 0.2, 0.3],
        rows: vec![vec![0.7, 0.3]],
        ..Default::default()
    };
    let restored = saved.restore(|_| true);
    // The empty column is dropped with its share.
    assert_eq!(restored.columns(), [0..2, 2..3]);
    assert_eq!(restored.focused(), 2);
    assert_eq!(restored.row_shares(0), [0.7, 0.3]);
    let columns = restored.column_shares();
    assert!((columns[0] - 0.5 / 0.8).abs() < 1e-5);

    // A column that loses a pane keeps one pane, no row shares.
    let restored = saved.restore(|id| id != b);
    assert_eq!(restored.columns(), [0..1, 1..2]);
    assert_eq!(restored.shares().rows, [Vec::<f32>::new(), Vec::new()]);

    // Columns that do not add up are read as one pane per column.
    let broken = PaneLayout {
        columns: vec![3, 3],
        ..saved.clone()
    };
    assert_eq!(broken.restore(|_| true).column_count(), 3);
}

#[test]
fn legacy_two_pane_layouts_migrate() {
    let (a, b) = (WorkspaceId::new(), WorkspaceId::new());
    let json = format!(r#"{{"panes":["{a}",null],"focused":1,"split":0.3}}"#);
    let saved: PaneLayout = serde_json::from_str(&json).unwrap();
    let panes = saved.restore(|_| true);
    assert_eq!(
        (panes.count(), panes.slot(0), panes.focused()),
        (1, Some(a), 0)
    );
    assert!(panes.shares().columns.is_empty());

    let json = format!(r#"{{"panes":["{a}","{b}"],"focused":1,"split":0.25}}"#);
    let saved: PaneLayout = serde_json::from_str(&json).unwrap();
    let panes = saved.restore(|_| true);
    assert_eq!((panes.count(), panes.focused()), (2, 1));
    assert_eq!(panes.columns(), [0..1, 1..2]);
    assert_eq!(panes.shares().columns, [0.25, 0.75]);
    assert!(
        !serde_json::to_string(&panes.layout())
            .unwrap()
            .contains("split")
    );

    // Saved before columns: one pane per column, sizes are the columns' widths.
    let json = format!(r#"{{"panes":["{a}","{b}"],"focused":0,"sizes":[0.4,0.6]}}"#);
    let saved: PaneLayout = serde_json::from_str(&json).unwrap();
    let panes = saved.restore(|_| true);
    assert_eq!(panes.column_count(), 2);
    assert_eq!(panes.shares().columns, [0.4, 0.6]);
}

#[test]
fn pane_shares_sanitised() {
    assert_eq!(pane_shares(&[300.0, 700.0], 1000.0), [0.3, 0.7]);
    assert_eq!(pane_shares(&[300.0, 700.0], 0.0), Vec::<f32>::new());
    assert_eq!(pane_shares(&[f32::NAN, 10.0], 10.0), Vec::<f32>::new());
    assert_eq!(pane_shares(&[500.0], 500.0), Vec::<f32>::new());
    // Normalised, and no pane under the minimum share.
    assert_eq!(sanitize_shares(&[1.0, 1.0], 2), [0.5, 0.5]);
    let tiny = sanitize_shares(&[0.01, 0.99], 2);
    assert!(tiny[0] > 0.09 && (tiny.iter().sum::<f32>() - 1.0).abs() < 1e-5);
    assert_eq!(sanitize_shares(&[0.5, 0.5], 3), Vec::<f32>::new());
    assert_eq!(sanitize_shares(&[0.5, -0.5], 2), Vec::<f32>::new());
    assert_eq!(sanitize_shares(&[f32::INFINITY, 0.5], 2), Vec::<f32>::new());
}

#[test]
fn panel_width_shared_and_migrated() {
    let saved = |json: &str| serde_json::from_str::<PaneLayout>(json).unwrap();
    // Saved before the tabs shared a width: the default.
    assert_eq!(
        saved(r#"{"panes":[null],"focused":0}"#).panel_width(),
        PANEL_DEFAULT_WIDTH
    );
    assert_eq!(
        saved(r#"{"panes":[null],"focused":0,"panel_width":500.0}"#).panel_width(),
        500.0
    );
    assert_eq!(
        saved(r#"{"panes":[null],"focused":0,"panel_width":10.0}"#).panel_width(),
        PANEL_WIDTH_RANGE.0
    );
    assert_eq!(panel_width(f32::NAN), None);
    assert_eq!(panel_width(1e9), Some(PANEL_WIDTH_RANGE.1));
    // Old per-tab widths are ignored.
    let changes: ChangesLayout = serde_json::from_str(r#"{"open":true,"width":520.0}"#).unwrap();
    assert_eq!(changes, ChangesLayout { open: true });
    let device: DeviceLayout = serde_json::from_str(r#"{"open":true,"width":420.0}"#).unwrap();
    assert!(device.open);
    assert_eq!(
        crate::ui::RIGHT_PANEL_WIDTH,
        gpui_kit::px(PANEL_DEFAULT_WIDTH)
    );
}

#[test]
fn sidebar_width_saved_and_migrated() {
    let saved = |json: &str| serde_json::from_str::<PaneLayout>(json).unwrap();
    assert_eq!(
        saved(r#"{"panes":[null],"focused":0}"#).sidebar_width(),
        SIDEBAR_DEFAULT_WIDTH
    );
    assert_eq!(
        saved(r#"{"panes":[null],"focused":0,"sidebar_width":300.0}"#).sidebar_width(),
        300.0
    );
    assert_eq!(
        saved(r#"{"panes":[null],"focused":0,"sidebar_width":50.0}"#).sidebar_width(),
        SIDEBAR_WIDTH_RANGE.0
    );
    assert_eq!(sidebar_width(f32::NAN), None);
}

#[test]
fn device_layout_logs() {
    let ok = DeviceLayout {
        open: true,
        selected: Some(DeviceKey {
            platform: Platform::Ios,
            id: "U".into(),
        }),
        stats: true,
        logs: true,
        log_level: Some("warn".into()),
    };
    assert_eq!(
        ok.logs_settings(),
        LogsSettings {
            open: true,
            min_level: LogLevel::Warn
        }
    );
    assert_eq!(
        DeviceLayout::default().logs_settings(),
        LogsSettings::default()
    );
    let junk = DeviceLayout {
        log_level: Some("loud".into()),
        ..ok.clone()
    };
    assert_eq!(junk.logs_settings().min_level, LogLevel::Verbose);
}

#[test]
fn status_labels() {
    assert_eq!(status_label(&SessionStatus::Running), "running");
    assert_eq!(status_label(&SessionStatus::Exited { code: 2 }), "exited 2");
    assert_eq!(
        status_label(&SessionStatus::Failed {
            message: "x".into()
        }),
        "failed: x"
    );
}
