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

#[cfg(test)]
mod tests {
    use hangar_core::{Setting, WorkspaceId};

    use crate::fixtures::store;
    use crate::{Store, StoreError};

    #[test]
    fn settings_round_trip_and_overwrite() {
        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        struct Layout {
            panes: Vec<Option<WorkspaceId>>,
            split: Option<f32>,
        }
        let store = store();
        let layout = Layout {
            panes: vec![Some(WorkspaceId::new()), None],
            split: Some(0.4),
        };
        const LAYOUT: Setting<Layout> = Setting::new("layout");
        store.set_setting(&LAYOUT, &layout).unwrap();
        assert_eq!(store.get_setting(&LAYOUT).unwrap(), Some(layout));

        let empty = Layout {
            panes: vec![],
            split: None,
        };
        store.set_setting(&LAYOUT, &empty).unwrap();
        assert_eq!(store.get_setting(&LAYOUT).unwrap(), Some(empty));

        // A value of the wrong shape is an error, not a silent default.
        store
            .set_setting(&Setting::new("layout"), &"not a layout")
            .unwrap();
        assert!(matches!(
            store.get_setting(&LAYOUT),
            Err(StoreError::Serde(_))
        ));
    }

    #[test]
    fn delete_setting_removes_the_key() {
        let store = Store::open_in_memory().unwrap();
        let run = Setting::<u16>::scoped("run", "x");
        store.set_setting(&run, &8100).unwrap();
        store.delete_setting(&run).unwrap();
        assert_eq!(store.get_setting(&run).unwrap(), None);
        store
            .delete_setting(&Setting::<u16>::new("missing"))
            .unwrap();
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
