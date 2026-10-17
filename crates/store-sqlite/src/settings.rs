use rusqlite::{OptionalExtension, params};

use crate::{Result, Store};

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
}

#[cfg(test)]
mod tests {
    use crate::fixtures::store;

    #[test]
    fn settings_round_trip_and_overwrite() {
        let store = store();
        assert_eq!(store.read_setting("layout").unwrap(), None);
        store.write_setting("layout", r#"{"split":0.4}"#).unwrap();
        assert_eq!(
            store.read_setting("layout").unwrap().as_deref(),
            Some(r#"{"split":0.4}"#)
        );
        store.write_setting("layout", "null").unwrap();
        assert_eq!(
            store.read_setting("layout").unwrap().as_deref(),
            Some("null")
        );
    }

    #[test]
    fn delete_setting_removes_the_key() {
        let store = store();
        store.write_setting("run:x", "8100").unwrap();
        store.remove_setting("run:x").unwrap();
        assert_eq!(store.read_setting("run:x").unwrap(), None);
        store.remove_setting("missing").unwrap();
    }

    #[test]
    fn setting_keys_match_an_exact_prefix() {
        let store = store();
        for key in [
            "device:b",
            "device:a",
            "device_pool:limit",
            "devicex",
            "layout",
        ] {
            store.write_setting(key, "1").unwrap();
        }
        assert_eq!(
            store.setting_keys("device:").unwrap(),
            ["device:a", "device:b"]
        );
        // `_` is not a wildcard.
        store.write_setting("a_b", "1").unwrap();
        store.write_setting("axb", "1").unwrap();
        assert_eq!(store.setting_keys("a_").unwrap(), ["a_b"]);
    }
}
