use std::path::PathBuf;

use hangar_core::{
    Project, ProjectId, Timestamp, Workspace, WorkspaceId, WorkspaceStatus, slugify,
};

use crate::Store;

pub(crate) fn store() -> Store {
    Store::open_in_memory().unwrap()
}

pub(crate) fn project(path: &str) -> Project {
    Project::new("Demo", path, "main")
}

pub(crate) fn workspace(project_id: ProjectId, name: &str) -> Workspace {
    let slug = slugify(name);
    Workspace {
        id: WorkspaceId::new(),
        project_id,
        name: name.to_owned(),
        branch: format!("hangar/{slug}"),
        base: "main".to_owned(),
        worktree_path: PathBuf::from(format!("/tmp/worktrees/{slug}")),
        slug,
        status: WorkspaceStatus::Active,
        created_at: Timestamp::now(),
        task: None,
        group: None,
        parent: None,
    }
}
