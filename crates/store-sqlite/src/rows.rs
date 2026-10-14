use std::path::{Path, PathBuf};

use rusqlite::{ErrorCode, Row, ffi};
use uuid::Uuid;

use hangar_core::changes::{Cost, DiffStat, TokenCount, UsageEntry};
use hangar_core::{
    AgentKind, Project, ProjectId, Session, SessionId, Timestamp, Workspace, WorkspaceGroupId,
    WorkspaceId, WorkspaceStatus,
};

use crate::{Result, StoreError};

pub(super) fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| StoreError::NonUtf8Path(path.to_path_buf()))
}

pub(super) fn not_found(entity: &'static str, id: impl ToString) -> StoreError {
    StoreError::NotFound {
        entity,
        id: id.to_string(),
    }
}

pub(super) fn ensure_changed(
    changed: usize,
    entity: &'static str,
    id: impl ToString,
) -> Result<()> {
    if changed == 0 {
        Err(not_found(entity, id))
    } else {
        Ok(())
    }
}

/// Maps unique/primary-key violations to `Conflict` and foreign-key violations to `missing_parent`.
pub(super) fn map_write_error(
    error: rusqlite::Error,
    conflict: impl FnOnce() -> String,
    missing_parent: Option<StoreError>,
) -> StoreError {
    if let rusqlite::Error::SqliteFailure(failure, _) = &error
        && failure.code == ErrorCode::ConstraintViolation
    {
        match failure.extended_code {
            ffi::SQLITE_CONSTRAINT_UNIQUE | ffi::SQLITE_CONSTRAINT_PRIMARYKEY => {
                return StoreError::Conflict(conflict());
            }
            ffi::SQLITE_CONSTRAINT_FOREIGNKEY => {
                if let Some(parent) = missing_parent {
                    return parent;
                }
            }
            _ => {}
        }
    }
    StoreError::Sqlite(error)
}

pub(super) fn conversion_error(
    index: usize,
    error: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(error))
}

pub(super) fn uuid_at(row: &Row<'_>, index: usize) -> rusqlite::Result<Uuid> {
    let text: String = row.get(index)?;
    Uuid::parse_str(&text).map_err(|e| conversion_error(index, e))
}

pub(super) fn json_at<T: serde::de::DeserializeOwned>(
    row: &Row<'_>,
    index: usize,
) -> rusqlite::Result<T> {
    let text: String = row.get(index)?;
    serde_json::from_str(&text).map_err(|e| conversion_error(index, e))
}

#[derive(Debug, thiserror::Error)]
#[error("unknown workspace status `{0}`")]
pub(super) struct UnknownStatus(String);

/// The rows that decode. A row this build cannot read (a value written by a newer Hangar, or
/// edited by hand) is logged and skipped so the rest still loads; database errors still fail.
pub(super) fn readable<T>(
    rows: impl Iterator<Item = rusqlite::Result<T>>,
    what: &str,
) -> rusqlite::Result<Vec<T>> {
    let mut out = Vec::new();
    for row in rows {
        match row {
            Ok(row) => out.push(row),
            Err(
                err @ (rusqlite::Error::FromSqlConversionFailure(..)
                | rusqlite::Error::IntegralValueOutOfRange(..)
                | rusqlite::Error::InvalidColumnType(..)),
            ) => tracing::warn!("skipping an unreadable {what} row: {err}"),
            Err(err) => return Err(err),
        }
    }
    Ok(out)
}

pub(super) fn project_from_row(row: &Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: ProjectId(uuid_at(row, 0)?),
        name: row.get(1)?,
        repo_path: PathBuf::from(row.get::<_, String>(2)?),
        default_base: row.get(3)?,
        created_at: Timestamp(row.get(4)?),
    })
}

pub(super) fn workspace_from_row(row: &Row<'_>) -> rusqlite::Result<Workspace> {
    let status: String = row.get(7)?;
    let status = WorkspaceStatus::parse(&status)
        .ok_or_else(|| conversion_error(7, UnknownStatus(status)))?;
    Ok(Workspace {
        id: WorkspaceId(uuid_at(row, 0)?),
        project_id: ProjectId(uuid_at(row, 1)?),
        name: row.get(2)?,
        slug: row.get(3)?,
        branch: row.get(4)?,
        base: row.get(5)?,
        worktree_path: PathBuf::from(row.get::<_, String>(6)?),
        status,
        created_at: Timestamp(row.get(8)?),
        task: row.get(9)?,
        group: row
            .get::<_, Option<String>>(10)?
            .map(|text| {
                Uuid::parse_str(&text)
                    .map(WorkspaceGroupId)
                    .map_err(|e| conversion_error(10, e))
            })
            .transpose()?,
        parent: row
            .get::<_, Option<String>>(11)?
            .map(|text| {
                Uuid::parse_str(&text)
                    .map(WorkspaceId)
                    .map_err(|e| conversion_error(11, e))
            })
            .transpose()?,
    })
}

pub(super) const SESSION_COLUMNS: &str = "id, workspace_id, agent, command, args, status, created_at, \
     ended_at, prompt, files, insertions, deletions, cost_amount, cost_currency";

pub(super) fn usage_from_row(row: &Row<'_>) -> rusqlite::Result<UsageEntry> {
    let cost_amount: Option<f64> = row.get(4)?;
    let cost_currency: Option<String> = row.get(5)?;
    let input: Option<i64> = row.get(6)?;
    let output: Option<i64> = row.get(7)?;
    let cache_read: Option<i64> = row.get(8)?;
    let cache_write: Option<i64> = row.get(9)?;
    Ok(UsageEntry {
        session_id: SessionId(uuid_at(row, 0)?),
        at: Timestamp(row.get(1)?),
        model: row.get(2)?,
        local: row.get(3)?,
        cost: cost_amount
            .zip(cost_currency)
            .map(|(amount, currency)| Cost { amount, currency }),
        tokens: input.zip(output).map(|(input, output)| TokenCount {
            input: count_from_sql(input),
            output: count_from_sql(output),
            cache_read: cache_read.map_or(0, count_from_sql),
            cache_write: cache_write.map_or(0, count_from_sql),
        }),
    })
}

/// SQLite integers are signed; token counts never come near `i64::MAX`.
pub(super) fn sql_count(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

pub(super) fn count_from_sql(n: i64) -> u64 {
    u64::try_from(n).unwrap_or_default()
}

pub(super) fn session_from_row(row: &Row<'_>) -> rusqlite::Result<Session> {
    let files: Option<u32> = row.get(9)?;
    let insertions: Option<u32> = row.get(10)?;
    let deletions: Option<u32> = row.get(11)?;
    let cost_amount: Option<f64> = row.get(12)?;
    let cost_currency: Option<String> = row.get(13)?;
    Ok(Session {
        id: SessionId(uuid_at(row, 0)?),
        workspace_id: WorkspaceId(uuid_at(row, 1)?),
        agent: json_at::<AgentKind>(row, 2)?,
        command: row.get(3)?,
        args: json_at(row, 4)?,
        status: json_at(row, 5)?,
        created_at: Timestamp(row.get(6)?),
        ended_at: row.get::<_, Option<i64>>(7)?.map(Timestamp),
        prompt: row.get(8)?,
        changes: files.map(|files| DiffStat {
            files,
            insertions: insertions.unwrap_or_default(),
            deletions: deletions.unwrap_or_default(),
        }),
        cost: cost_amount
            .zip(cost_currency)
            .map(|(amount, currency)| Cost { amount, currency }),
    })
}
