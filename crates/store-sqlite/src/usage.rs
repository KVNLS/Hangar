use rusqlite::params;

use hangar_core::Timestamp;
use hangar_core::changes::UsageEntry;

use crate::rows::{ensure_changed, readable, sql_count, usage_from_row};
use crate::{Result, Store};

impl Store {
    /// Appends a row to the usage ledger, for a session that exists. The row stays when the
    /// session, its workspace or its project is deleted later.
    pub fn record_usage(&self, entry: &UsageEntry) -> Result<()> {
        let tokens = entry.tokens;
        let changed = self.conn().execute(
            "INSERT INTO usage
             (session_id, at, model, local, cost_amount, cost_currency, input, output,
              cache_read, cache_write)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10
             WHERE EXISTS (SELECT 1 FROM sessions WHERE id = ?1)",
            params![
                entry.session_id.0.to_string(),
                entry.at.0,
                entry.model,
                entry.local,
                entry.cost.as_ref().map(|c| c.amount),
                entry.cost.as_ref().map(|c| c.currency.as_str()),
                tokens.map(|t| sql_count(t.input)),
                tokens.map(|t| sql_count(t.output)),
                tokens.map(|t| sql_count(t.cache_read)),
                tokens.map(|t| sql_count(t.cache_write)),
            ],
        )?;
        ensure_changed(changed, "session", entry.session_id)
    }

    /// Ledger rows at or after `since`, oldest first.
    pub fn list_usage_since(&self, since: Timestamp) -> Result<Vec<UsageEntry>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT session_id, at, model, local, cost_amount, cost_currency, input, output,
                    cache_read, cache_write
             FROM usage WHERE at >= ?1 ORDER BY at, rowid",
        )?;
        let rows = stmt.query_map([since.0], usage_from_row)?;
        Ok(readable(rows, "usage")?)
    }
}
