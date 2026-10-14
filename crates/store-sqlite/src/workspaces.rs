use rusqlite::{OptionalExtension, params};

use hangar_core::{ProjectId, Workspace, WorkspaceId, WorkspaceStatus};

use crate::rows::{
    ensure_changed, map_write_error, not_found, path_str, readable, workspace_from_row,
};
use crate::{Result, Store};

impl Store {
    pub fn insert_workspace(&self, workspace: &Workspace) -> Result<()> {
        let worktree_path = path_str(&workspace.worktree_path)?;
        self.conn()
            .execute(
                "INSERT INTO workspaces
                 (id, project_id, name, slug, branch, base, worktree_path, status, created_at, task,
                  group_id, parent_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    workspace.id.0.to_string(),
                    workspace.project_id.0.to_string(),
                    workspace.name,
                    workspace.slug,
                    workspace.branch,
                    workspace.base,
                    worktree_path,
                    workspace.status.as_str(),
                    workspace.created_at.0,
                    workspace.task,
                    workspace.group.map(|g| g.0.to_string()),
                    workspace.parent.map(|p| p.0.to_string()),
                ],
            )
            .map_err(|e| {
                map_write_error(
                    e,
                    || {
                        format!(
                            "workspace slug `{}` is already used in this project",
                            workspace.slug
                        )
                    },
                    Some(not_found("project", workspace.project_id)),
                )
            })?;
        Ok(())
    }

    pub fn list_workspaces(&self, project_id: ProjectId) -> Result<Vec<Workspace>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, project_id, name, slug, branch, base, worktree_path, status, created_at,
                    task, group_id, parent_id
             FROM workspaces WHERE project_id = ?1 ORDER BY created_at, rowid",
        )?;
        let rows = stmt.query_map([project_id.0.to_string()], workspace_from_row)?;
        Ok(readable(rows, "workspace")?)
    }

    pub fn get_workspace(&self, id: WorkspaceId) -> Result<Workspace> {
        self.conn()
            .query_row(
                "SELECT id, project_id, name, slug, branch, base, worktree_path, status, created_at,
                    task, group_id, parent_id
                 FROM workspaces WHERE id = ?1",
                [id.0.to_string()],
                workspace_from_row,
            )
            .optional()?
            .ok_or_else(|| not_found("workspace", id))
    }

    pub fn update_workspace_status(&self, id: WorkspaceId, status: WorkspaceStatus) -> Result<()> {
        let changed = self.conn().execute(
            "UPDATE workspaces SET status = ?2 WHERE id = ?1",
            params![id.0.to_string(), status.as_str()],
        )?;
        ensure_changed(changed, "workspace", id)
    }

    /// Deletes the workspace and, by cascade, its sessions.
    pub fn delete_workspace(&self, id: WorkspaceId) -> Result<()> {
        let changed = self
            .conn()
            .execute("DELETE FROM workspaces WHERE id = ?1", [id.0.to_string()])?;
        ensure_changed(changed, "workspace", id)
    }
}
