use rusqlite::{OptionalExtension, params};

use hangar_core::{Project, ProjectId};

use crate::rows::{
    ensure_changed, map_write_error, not_found, path_str, project_from_row, readable,
};
use crate::{Result, Store};

impl Store {
    pub fn insert_project(&self, project: &Project) -> Result<()> {
        let repo_path = path_str(&project.repo_path)?;
        self.conn()
            .execute(
                "INSERT INTO projects (id, name, repo_path, default_base, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    project.id.0.to_string(),
                    project.name,
                    repo_path,
                    project.default_base,
                    project.created_at.0,
                ],
            )
            .map_err(|e| {
                map_write_error(
                    e,
                    || format!("a project already exists at {repo_path}"),
                    None,
                )
            })?;
        Ok(())
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, repo_path, default_base, created_at
             FROM projects ORDER BY created_at, rowid",
        )?;
        let rows = stmt.query_map([], project_from_row)?;
        Ok(readable(rows, "project")?)
    }

    pub fn get_project(&self, id: ProjectId) -> Result<Project> {
        self.conn()
            .query_row(
                "SELECT id, name, repo_path, default_base, created_at FROM projects WHERE id = ?1",
                [id.0.to_string()],
                project_from_row,
            )
            .optional()?
            .ok_or_else(|| not_found("project", id))
    }

    /// Deletes the project and, by cascade, its workspaces and sessions.
    pub fn delete_project(&self, id: ProjectId) -> Result<()> {
        let changed = self
            .conn()
            .execute("DELETE FROM projects WHERE id = ?1", [id.0.to_string()])?;
        ensure_changed(changed, "project", id)
    }
}
