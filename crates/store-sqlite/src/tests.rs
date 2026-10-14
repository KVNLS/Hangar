use std::path::PathBuf;
use std::sync::Arc;

use super::*;
use crate::migrations::MIGRATIONS;
use hangar_core::Setting;
use hangar_core::changes::{Cost, DiffStat, TokenCount, UsageEntry};
use hangar_core::slugify;
use hangar_core::{
    AgentKind, Project, ProjectId, Session, SessionId, SessionStatus, Timestamp, Workspace,
    WorkspaceGroupId, WorkspaceId, WorkspaceStatus,
};
use rusqlite::params;

pub(super) fn store() -> Store {
    Store::open_in_memory().unwrap()
}

pub(super) fn project(path: &str) -> Project {
    Project::new("Demo", path, "main")
}

pub(super) fn workspace(project_id: ProjectId, name: &str) -> Workspace {
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

mod crud;
mod history;
mod open;
mod settings;
mod usage;
