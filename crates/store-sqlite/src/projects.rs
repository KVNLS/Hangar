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

#[cfg(test)]
mod tests {
    use hangar_core::{AgentKind, Session};

    use crate::StoreError;
    use crate::fixtures::{project, store, workspace};

    #[test]
    fn project_crud_round_trip() {
        let store = store();
        let a = project("/repos/a");
        let b = project("/repos/b");
        store.insert_project(&a).unwrap();
        store.insert_project(&b).unwrap();

        assert_eq!(store.get_project(a.id).unwrap(), a);
        assert_eq!(store.list_projects().unwrap(), vec![a.clone(), b.clone()]);

        store.delete_project(a.id).unwrap();
        assert_eq!(store.list_projects().unwrap(), vec![b]);
        assert!(matches!(
            store.get_project(a.id),
            Err(StoreError::NotFound {
                entity: "project",
                ..
            })
        ));
        assert!(matches!(
            store.delete_project(a.id),
            Err(StoreError::NotFound { .. })
        ));
    }

    #[test]
    fn duplicate_repo_path_is_a_conflict() {
        let store = store();
        store.insert_project(&project("/repos/a")).unwrap();
        assert!(matches!(
            store.insert_project(&project("/repos/a")),
            Err(StoreError::Conflict(_))
        ));
    }

    #[test]
    fn deleting_a_project_cascades() {
        let store = store();
        let p = project("/repos/a");
        store.insert_project(&p).unwrap();
        let w = workspace(p.id, "task");
        store.insert_workspace(&w).unwrap();
        let s = Session::new(w.id, AgentKind::Codex);
        store.insert_session(&s).unwrap();

        store.delete_project(p.id).unwrap();
        assert!(matches!(
            store.get_workspace(w.id),
            Err(StoreError::NotFound { .. })
        ));
        assert!(matches!(
            store.get_session(s.id),
            Err(StoreError::NotFound { .. })
        ));
    }
}
