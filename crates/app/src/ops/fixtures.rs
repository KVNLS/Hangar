//! Rows for the `ops` tests. Worktrees are plain directories: `ops` adds no Git behaviour of its
//! own (the lifecycle on a real repository is in `tests/workspaces.rs`).

use hangar_core::{
    Project, Timestamp, Workspace, WorkspaceId, WorkspaceStatus, branch_name, slugify,
};
use hangar_store_sqlite::Store;

pub(super) struct Fixture {
    dir: tempfile::TempDir,
    pub(super) store: Store,
    project: Project,
}

pub(super) fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let project = Project::new("Demo", dir.path().join("repo"), "main");
    store.insert_project(&project).unwrap();
    Fixture {
        dir,
        store,
        project,
    }
}

impl Fixture {
    /// An active workspace row whose worktree is an empty directory.
    pub(super) fn workspace(&self, name: &str) -> Workspace {
        let slug = slugify(name);
        let worktree_path = self.dir.path().join("worktrees").join(&slug);
        std::fs::create_dir_all(&worktree_path).unwrap();
        let ws = Workspace {
            id: WorkspaceId::new(),
            project_id: self.project.id,
            name: name.to_owned(),
            branch: branch_name(&slug),
            slug,
            base: "main".to_owned(),
            worktree_path,
            status: WorkspaceStatus::Active,
            created_at: Timestamp::now(),
            task: None,
            group: None,
            parent: None,
        };
        self.store.insert_workspace(&ws).unwrap();
        ws
    }
}
