//! Device picker state, independent of the UI: the listed devices, the user's selection and
//! which device should be open. Every change returns the [`Effect`] the panel must carry out.

use hangar_devices::DeviceInfo;
use serde::{Deserialize, Serialize};

use super::platform::Platform;

/// Identifies a device across launches (saved in settings).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeviceKey {
    pub platform: Platform,
    pub id: String,
}

impl DeviceKey {
    /// Value used by the picker's select (`ios:<UDID>`).
    pub fn value(&self) -> String {
        format!("{}:{}", self.platform.id(), self.id)
    }
}

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
mod tests {
    use super::*;

    fn entry(id: &str, state: &str) -> DeviceEntry {
        DeviceEntry::new(
            Platform::Ios,
            DeviceInfo {
                udid: id.into(),
                name: format!("Phone {id}"),
                runtime: "iOS 27.0".into(),
                state: state.into(),
                device_type: None,
                screen: None,
            },
        )
    }

    fn key(id: &str) -> DeviceKey {
        DeviceKey {
            platform: Platform::Ios,
            id: id.into(),
        }
    }

    fn opens(effect: &Effect) -> Option<&str> {
        effect.open.as_ref().map(|e| e.key.id.as_str())
    }

    #[test]
    fn first_listing_selects_the_first_ready_device() {
        let mut p = Picker::default();
        assert_eq!(p.set_visible(true), Effect::default());
        p.listing_started();
        assert!(p.is_listing());
        let fx = p.list_done(vec![entry("A", "Shutdown"), entry("B", "Booted")], vec![]);
        assert_eq!(opens(&fx), Some("B"));
        assert!(!fx.close);
        assert_eq!(p.selected(), Some(&key("B")));
        assert_eq!(p.active(), Some(&key("B")));
        assert!(p.listed() && !p.is_listing());
        // Listing again with the same device keeps it open.
        assert_eq!(
            p.list_done(vec![entry("B", "Booted")], vec![]),
            Effect::default()
        );
    }

    #[test]
    fn hidden_panel_opens_nothing_and_closes_on_hide() {
        let mut p = Picker::default();
        assert_eq!(
            p.list_done(vec![entry("A", "Booted")], vec![]),
            Effect::default()
        );
        assert_eq!(opens(&p.set_visible(true)), Some("A"));
        assert_eq!(
            p.set_visible(false),
            Effect {
                close: true,
                open: None
            }
        );
        assert_eq!(p.active(), None);
        assert_eq!(p.selected(), Some(&key("A")));
    }

    #[test]
    fn restored_selection_wins_and_waits_for_its_device() {
        let mut p = Picker::default();
        p.restore(Some(key("Z")));
        p.set_visible(true);
        // The saved device is not booted: nothing opens, the selection is kept.
        assert_eq!(
            p.list_done(vec![entry("A", "Booted")], vec![]),
            Effect::default()
        );
        assert_eq!(p.selected(), Some(&key("Z")));
        assert!(p.selected_entry().is_none());
        // Once it shows up booted it opens.
        let fx = p.list_done(vec![entry("A", "Booted"), entry("Z", "Booted")], vec![]);
        assert_eq!(opens(&fx), Some("Z"));
    }

    #[test]
    fn switching_devices_closes_then_opens() {
        let mut p = Picker::default();
        p.set_visible(true);
        p.list_done(vec![entry("A", "Booted"), entry("B", "Booted")], vec![]);
        let fx = p.select(key("B"));
        assert!(fx.close);
        assert_eq!(opens(&fx), Some("B"));
        assert_eq!(p.select(key("B")), Effect::default());
        // Selecting a device that is not ready closes the current one.
        p.list_done(
            vec![
                entry("A", "Booted"),
                entry("B", "Booted"),
                entry("C", "Shutdown"),
            ],
            vec![],
        );
        assert_eq!(
            p.select(key("C")),
            Effect {
                close: true,
                open: None
            }
        );
    }

    #[test]
    fn vanished_device_closes_and_reopens_when_back() {
        let mut p = Picker::default();
        p.set_visible(true);
        p.list_done(vec![entry("A", "Booted")], vec![]);
        let fx = p.list_done(vec![], vec![(Platform::Ios, "xcrun failed".into())]);
        assert!(fx.close && fx.open.is_none());
        assert_eq!(p.errors().len(), 1);
        assert_eq!(
            opens(&p.list_done(vec![entry("A", "Booted")], vec![])),
            Some("A")
        );
        assert!(p.errors().is_empty());
    }

    #[test]
    fn failed_open_is_not_retried_until_refresh_or_reselect() {
        let mut p = Picker::default();
        p.set_visible(true);
        p.list_done(vec![entry("A", "Booted")], vec![]);
        p.open_failed(&key("A"));
        assert_eq!(p.active(), None);
        assert_eq!(
            p.list_done(vec![entry("A", "Booted")], vec![]),
            Effect::default()
        );
        assert_eq!(p.set_visible(true), Effect::default());
        // Explicit reselect retries.
        assert_eq!(opens(&p.select(key("A"))), Some("A"));
        p.open_failed(&key("A"));
        // So does a refresh.
        p.listing_started();
        assert_eq!(
            opens(&p.list_done(vec![entry("A", "Booted")], vec![])),
            Some("A")
        );
    }

    #[test]
    fn closed_device_reopens_on_next_listing() {
        let mut p = Picker::default();
        p.set_visible(true);
        p.list_done(vec![entry("A", "Booted")], vec![]);
        p.closed(&key("B"));
        assert_eq!(p.active(), Some(&key("A")));
        p.closed(&key("A"));
        assert_eq!(p.active(), None);
        assert_eq!(
            opens(&p.list_done(vec![entry("A", "Booted")], vec![])),
            Some("A")
        );
    }

    /// An AVD: shut down it is listed under its id, running under its adb serial.
    fn avd(avd_id: &str, serial: Option<&str>, state: &str) -> DeviceEntry {
        DeviceEntry::new(
            Platform::Android,
            DeviceInfo {
                udid: serial.unwrap_or(avd_id).into(),
                name: format!("Pixel {avd_id}"),
                runtime: "Android 17".into(),
                state: state.into(),
                device_type: Some(avd_id.into()),
                screen: None,
            },
        )
    }

    fn android_key(id: &str) -> DeviceKey {
        DeviceKey {
            platform: Platform::Android,
            id: id.into(),
        }
    }

    #[test]
    fn android_devices_mix_with_ios() {
        let mut p = Picker::default();
        p.set_visible(true);
        // A shut-down AVD is listed but never opened or auto-selected.
        let fx = p.list_done(
            vec![avd("Pixel_10", None, "Shutdown"), entry("A", "Booted")],
            vec![],
        );
        assert_eq!(opens(&fx), Some("A"));
        assert!(!p.entries()[0].ready);
        assert!(p.select(android_key("Pixel_10")).close);
        assert_eq!(p.active(), None);
        // Once booted it keeps its key (the AVD id), so the selection holds and it opens.
        let fx = p.list_done(
            vec![
                avd("Pixel_10", Some("emulator-5554"), "Booted"),
                entry("A", "Booted"),
            ],
            vec![],
        );
        assert_eq!(opens(&fx), Some("Pixel_10"));
        assert_eq!(
            fx.open.map(|e| e.info.udid),
            Some("emulator-5554".to_owned())
        );
        assert_eq!(android_key("Pixel_10").value(), "android:Pixel_10");
        assert_eq!(
            p.entry_by_value("android:Pixel_10").map(|e| e.key.platform),
            Some(Platform::Android)
        );
        // An Android failure does not hide the iOS devices.
        let fx = p.list_done(
            vec![entry("A", "Booted")],
            vec![(Platform::Android, "no SDK".into())],
        );
        assert!(fx.close);
        assert_eq!(p.errors(), [(Platform::Android, "no SDK".to_owned())]);
    }

    #[test]
    fn saved_avd_selection_survives_a_new_serial() {
        let mut p = Picker::default();
        p.restore(Some(android_key("Pixel_10")));
        p.set_visible(true);
        let fx = p.list_done(
            vec![
                entry("A", "Booted"),
                avd("Pixel_10", Some("emulator-5556"), "Booted"),
            ],
            vec![],
        );
        assert_eq!(opens(&fx), Some("Pixel_10"));
        // The dev hooks still find it by serial.
        assert_eq!(p.find_id("emulator-5556"), Some(android_key("Pixel_10")));
    }

    #[test]
    fn physical_android_device_keeps_its_serial() {
        // `device_type` is the adb product name for physical devices, not an AVD id.
        let phone = DeviceEntry::new(
            Platform::Android,
            DeviceInfo {
                udid: "R5CT12345".into(),
                name: "Galaxy".into(),
                runtime: "Android 16".into(),
                state: "Booted".into(),
                device_type: Some("r0q".into()),
                screen: None,
            },
        );
        assert_eq!(phone.key, android_key("R5CT12345"));
    }

    #[test]
    fn lookups() {
        let mut p = Picker::default();
        p.list_done(vec![entry("abc-1", "Booted")], vec![]);
        assert_eq!(p.find_id("ABC-1"), Some(key("abc-1")));
        assert_eq!(p.find_id("nope"), None);
        assert_eq!(key("abc-1").value(), "ios:abc-1");
        assert_eq!(
            p.entry_by_value("ios:abc-1").map(|e| e.info.name.as_str()),
            Some("Phone abc-1")
        );
        assert!(p.entry_by_value("abc-1").is_none());
    }
}
