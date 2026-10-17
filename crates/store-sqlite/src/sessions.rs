use rusqlite::{OptionalExtension, params};

use hangar_core::changes::{Cost, DiffStat};
use hangar_core::{Session, SessionId, SessionStatus, Timestamp, WorkspaceId};

use crate::rows::{
    SESSION_COLUMNS, ensure_changed, map_write_error, not_found, readable, session_from_row,
};
use crate::{Result, Store};

impl Store {
    pub fn insert_session(&self, session: &Session) -> Result<()> {
        let agent = serde_json::to_string(&session.agent)?;
        let args = serde_json::to_string(&session.args)?;
        let status = serde_json::to_string(&session.status)?;
        self.conn()
            .execute(
                "INSERT INTO sessions
                 (id, workspace_id, agent, command, args, status, created_at, ended_at, prompt,
                  files, insertions, deletions, cost_amount, cost_currency)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    session.id.0.to_string(),
                    session.workspace_id.0.to_string(),
                    agent,
                    session.command,
                    args,
                    status,
                    session.created_at.0,
                    session.ended_at.map(|t| t.0),
                    session.prompt,
                    session.changes.map(|c| c.files),
                    session.changes.map(|c| c.insertions),
                    session.changes.map(|c| c.deletions),
                    session.cost.as_ref().map(|c| c.amount),
                    session.cost.as_ref().map(|c| c.currency.as_str()),
                ],
            )
            .map_err(|e| {
                map_write_error(
                    e,
                    || format!("session {} already exists", session.id),
                    Some(not_found("workspace", session.workspace_id)),
                )
            })?;
        Ok(())
    }

    pub fn list_sessions(&self, workspace_id: WorkspaceId) -> Result<Vec<Session>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {SESSION_COLUMNS} FROM sessions WHERE workspace_id = ?1
             ORDER BY created_at, rowid"
        ))?;
        let rows = stmt.query_map([workspace_id.0.to_string()], session_from_row)?;
        Ok(readable(rows, "session")?)
    }

    /// Sessions of every workspace that ended at or after `since`, the latest ended first.
    pub fn list_sessions_since(&self, since: Timestamp) -> Result<Vec<Session>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {SESSION_COLUMNS} FROM sessions WHERE ended_at >= ?1
             ORDER BY ended_at DESC, rowid DESC"
        ))?;
        let rows = stmt.query_map([since.0], session_from_row)?;
        Ok(readable(rows, "session")?)
    }

    pub fn get_session(&self, id: SessionId) -> Result<Session> {
        self.conn()
            .query_row(
                &format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE id = ?1"),
                [id.0.to_string()],
                session_from_row,
            )
            .optional()?
            .ok_or_else(|| not_found("session", id))
    }

    /// Updates the status; a terminal status (`Exited`/`Failed`) also stamps `ended_at` once.
    /// Returns the updated session.
    pub fn update_session_status(&self, id: SessionId, status: &SessionStatus) -> Result<Session> {
        self.finish_session(id, status, None)
    }

    /// [`Store::update_session_status`] that also records the workspace's diff when it is known;
    /// `None` keeps the diff already recorded.
    pub fn finish_session(
        &self,
        id: SessionId,
        status: &SessionStatus,
        changes: Option<DiffStat>,
    ) -> Result<Session> {
        let encoded = serde_json::to_string(status)?;
        let ended_at = status.is_terminal().then(|| Timestamp::now().0);
        let changed = self.conn().execute(
            "UPDATE sessions SET status = ?2, ended_at = COALESCE(ended_at, ?3),
                 files = COALESCE(?4, files), insertions = COALESCE(?5, insertions),
                 deletions = COALESCE(?6, deletions)
             WHERE id = ?1",
            params![
                id.0.to_string(),
                encoded,
                ended_at,
                changes.map(|c| c.files),
                changes.map(|c| c.insertions),
                changes.map(|c| c.deletions),
            ],
        )?;
        ensure_changed(changed, "session", id)?;
        self.get_session(id)
    }

    /// Records what the session has cost so far (the agent's latest running total).
    pub fn record_session_cost(&self, id: SessionId, cost: &Cost) -> Result<()> {
        let changed = self.conn().execute(
            "UPDATE sessions SET cost_amount = ?2, cost_currency = ?3 WHERE id = ?1",
            params![id.0.to_string(), cost.amount, cost.currency],
        )?;
        ensure_changed(changed, "session", id)
    }
}

#[cfg(test)]
mod tests;
