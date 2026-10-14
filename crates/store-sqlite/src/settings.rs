use rusqlite::{OptionalExtension, params};

use hangar_core::Setting;

use crate::{Result, Store, StoreError};

impl Store {
    /// The raw JSON stored under `key`; `None` when it was never set.
    pub fn read_setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    /// Stores raw JSON under `key`, replacing any previous value.
    pub fn write_setting(&self, key: &str, json: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, json],
        )?;
        Ok(())
    }

    /// Removes `key`; removing a missing one is not an error.
    pub fn remove_setting(&self, key: &str) -> Result<()> {
        self.conn()
            .execute("DELETE FROM settings WHERE key = ?1", [key])?;
        Ok(())
    }

    /// Keys that start with `prefix`, sorted. An exact prefix, not a `LIKE` pattern.
    pub fn setting_keys(&self, prefix: &str) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT key FROM settings WHERE substr(key, 1, length(?1)) = ?1 ORDER BY key",
        )?;
        let keys = stmt.query_map([prefix], |row| row.get(0))?;
        Ok(keys.collect::<rusqlite::Result<_>>()?)
    }

    /// Reads a JSON-encoded setting; `None` when the key was never set.
    pub fn get_setting<T: serde::de::DeserializeOwned>(
        &self,
        setting: &Setting<T>,
    ) -> Result<Option<T>> {
        self.read_setting(setting.key())?
            .map(|v| serde_json::from_str(&v))
            .transpose()
            .map_err(StoreError::from)
    }

    /// Stores `value` as JSON under `setting`, replacing any previous value.
    pub fn set_setting<T: serde::Serialize>(&self, setting: &Setting<T>, value: &T) -> Result<()> {
        self.write_setting(setting.key(), &serde_json::to_string(value)?)
    }

    /// Removes `setting`; removing a missing one is not an error.
    pub fn delete_setting<T>(&self, setting: &Setting<T>) -> Result<()> {
        self.remove_setting(setting.key())
    }
}
