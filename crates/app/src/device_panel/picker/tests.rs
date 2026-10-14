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
