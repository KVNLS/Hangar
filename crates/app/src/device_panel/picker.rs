//! Device picker state, independent of the UI: the listed devices, the user's selection and
//! which device should be open. Every change returns the [`Effect`] the panel must carry out.

use hangar_core::device::DeviceInfo;
pub use hangar_core::device::DeviceKey;

use super::platform::Platform;

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceEntry {
    pub key: DeviceKey,
    pub info: DeviceInfo,
    pub ready: bool,
}

impl DeviceEntry {
    pub fn new(platform: Platform, info: DeviceInfo) -> Self {
        Self {
            key: DeviceKey {
                platform,
                id: stable_id(platform, &info).to_owned(),
            },
            ready: platform.is_ready(&info),
            info,
        }
    }
}

/// The key id: the UDID, except for running Android emulators, whose adb serial changes with
/// every boot. They use their AVD id, which is also the udid of the same AVD when shut down.
fn stable_id(platform: Platform, info: &DeviceInfo) -> &str {
    match (platform, info.device_type.as_deref()) {
        (Platform::Android, Some(avd)) if info.udid.starts_with("emulator-") => avd,
        _ => &info.udid,
    }
}

/// What the panel has to do after a state change: close the open device first, then open one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Effect {
    pub close: bool,
    pub open: Option<DeviceEntry>,
}

#[derive(Debug, Default)]
pub struct Picker {
    entries: Vec<DeviceEntry>,
    errors: Vec<(Platform, String)>,
    listed: bool,
    listing: bool,
    visible: bool,
    selected: Option<DeviceKey>,
    /// Open or being opened.
    active: Option<DeviceKey>,
    /// Failed to open; not retried until it is selected again or the list is refreshed.
    failed: Option<DeviceKey>,
}

impl Picker {
    pub fn entries(&self) -> &[DeviceEntry] {
        &self.entries
    }

    pub fn errors(&self) -> &[(Platform, String)] {
        &self.errors
    }

    pub fn listed(&self) -> bool {
        self.listed
    }

    pub fn is_listing(&self) -> bool {
        self.listing
    }

    pub fn selected(&self) -> Option<&DeviceKey> {
        self.selected.as_ref()
    }

    pub fn selected_entry(&self) -> Option<&DeviceEntry> {
        let key = self.selected.as_ref()?;
        self.entries.iter().find(|e| &e.key == key)
    }

    #[cfg(test)]
    pub fn active(&self) -> Option<&DeviceKey> {
        self.active.as_ref()
    }

    pub fn entry_by_value(&self, value: &str) -> Option<&DeviceEntry> {
        self.entries.iter().find(|e| e.key.value() == value)
    }

    /// Finds a listed device by key id or UDID (adb serial) on any platform (dev hooks).
    pub fn find_id(&self, id: &str) -> Option<DeviceKey> {
        self.entries
            .iter()
            .find(|e| e.key.id.eq_ignore_ascii_case(id) || e.info.udid.eq_ignore_ascii_case(id))
            .map(|e| e.key.clone())
    }

    /// Restores the saved selection (before the first listing).
    pub fn restore(&mut self, selected: Option<DeviceKey>) {
        self.selected = selected;
    }

    pub fn set_visible(&mut self, visible: bool) -> Effect {
        self.visible = visible;
        self.reconcile()
    }

    pub fn listing_started(&mut self) {
        self.listing = true;
        self.failed = None;
    }

    pub fn list_done(
        &mut self,
        entries: Vec<DeviceEntry>,
        errors: Vec<(Platform, String)>,
    ) -> Effect {
        self.entries = entries;
        self.errors = errors;
        self.listing = false;
        self.listed = true;
        if self.selected.is_none() {
            self.selected = self.entries.iter().find(|e| e.ready).map(|e| e.key.clone());
        }
        self.reconcile()
    }

    pub fn select(&mut self, key: DeviceKey) -> Effect {
        if self.failed.as_ref() == Some(&key) {
            self.failed = None;
        }
        self.selected = Some(key);
        self.reconcile()
    }

    /// The device could not be opened: it stays selected but is not retried automatically.
    pub fn open_failed(&mut self, key: &DeviceKey) {
        if self.active.as_ref() == Some(key) {
            self.active = None;
        }
        self.failed = Some(key.clone());
    }

    /// The open device stopped on its own (e.g. shut down); the next listing decides again.
    pub fn closed(&mut self, key: &DeviceKey) {
        if self.active.as_ref() == Some(key) {
            self.active = None;
        }
    }

    fn reconcile(&mut self) -> Effect {
        let target = self
            .visible
            .then(|| self.selected_entry())
            .flatten()
            .filter(|e| e.ready && self.failed.as_ref() != Some(&e.key))
            .cloned();
        let effect = match (&self.active, target) {
            (Some(active), Some(target)) if *active == target.key => Effect::default(),
            (active, Some(target)) => Effect {
                close: active.is_some(),
                open: Some(target),
            },
            (Some(_), None) => Effect {
                close: true,
                open: None,
            },
            (None, None) => Effect::default(),
        };
        if effect.close {
            self.active = None;
        }
        if let Some(open) = &effect.open {
            self.active = Some(open.key.clone());
        }
        effect
    }
}

#[cfg(test)]
mod tests;
