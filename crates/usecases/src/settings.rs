//! Typed settings over the [`Store`] port: each [`Setting`] is stored as JSON under its key.

use hangar_core::Setting;
use hangar_core::ports::{PortError, Store};

#[derive(Debug, thiserror::Error)]
pub enum SettingError {
    #[error(transparent)]
    Port(#[from] PortError),
    #[error("setting `{key}` is not valid: {source}")]
    Json {
        key: String,
        #[source]
        source: serde_json::Error,
    },
}

/// `get_setting`, `set_setting` and `delete_setting` on any [`Store`], including `dyn Store`.
pub trait SettingsExt {
    /// `None` when the key was never set.
    fn get_setting<T: serde::de::DeserializeOwned>(
        &self,
        setting: &Setting<T>,
    ) -> Result<Option<T>, SettingError>;
    /// Replaces any previous value.
    fn set_setting<T: serde::Serialize>(
        &self,
        setting: &Setting<T>,
        value: &T,
    ) -> Result<(), SettingError>;
    /// Deleting a missing setting is not an error.
    fn delete_setting<T>(&self, setting: &Setting<T>) -> Result<(), SettingError>;
}

impl<S: Store + ?Sized> SettingsExt for S {
    fn get_setting<T: serde::de::DeserializeOwned>(
        &self,
        setting: &Setting<T>,
    ) -> Result<Option<T>, SettingError> {
        self.read_setting(setting.key())?
            .map(|json| serde_json::from_str(&json))
            .transpose()
            .map_err(|source| SettingError::Json {
                key: setting.key().to_owned(),
                source,
            })
    }

    fn set_setting<T: serde::Serialize>(
        &self,
        setting: &Setting<T>,
        value: &T,
    ) -> Result<(), SettingError> {
        let json = serde_json::to_string(value).map_err(|source| SettingError::Json {
            key: setting.key().to_owned(),
            source,
        })?;
        Ok(self.write_setting(setting.key(), &json)?)
    }

    fn delete_setting<T>(&self, setting: &Setting<T>) -> Result<(), SettingError> {
        Ok(self.remove_setting(setting.key())?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::FakeStore;

    #[test]
    fn round_trips_typed_values_through_a_dyn_store() {
        let fake = FakeStore::default();
        let store: &dyn Store = &fake;
        let limit = Setting::<u32>::new("device.limit");
        assert_eq!(store.get_setting(&limit).unwrap(), None);
        store.set_setting(&limit, &3).unwrap();
        assert_eq!(
            store.read_setting("device.limit").unwrap().as_deref(),
            Some("3")
        );
        assert_eq!(store.get_setting(&limit).unwrap(), Some(3));
        store.delete_setting(&limit).unwrap();
        store.delete_setting(&limit).unwrap();
        assert_eq!(store.get_setting(&limit).unwrap(), None);
    }

    #[test]
    fn invalid_json_names_the_key() {
        let store = FakeStore::default();
        store.write_setting("device.limit", "\"three\"").unwrap();
        let err = store
            .get_setting(&Setting::<u32>::new("device.limit"))
            .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("setting `device.limit` is not valid"),
            "{err}"
        );
    }
}
