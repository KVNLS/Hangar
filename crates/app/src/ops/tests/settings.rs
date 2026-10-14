use super::*;

#[test]
fn settings_are_loaded_and_bad_ones_reported() {
    let f = fixture();
    let loaded = load(&f.store).unwrap();
    assert!(loaded.layout.is_none() && loaded.agent.is_none());
    assert!(loaded.settings_error.is_none());

    let layout = PaneLayout {
        panes: vec![Some(WorkspaceId::new()), None],
        focused: 1,
        columns: vec![1, 1],
        sizes: vec![0.4, 0.6],
        rows: vec![Vec::new(), Vec::new()],
        split: None,
        panel_width: Some(480.0),
        sidebar_width: Some(300.0),
    };
    save_layout(&f.store, &layout).unwrap();
    save_agent(&f.store, "codex").unwrap();
    let loaded = load(&f.store).unwrap();
    assert_eq!(loaded.layout, Some(layout));
    assert_eq!(loaded.agent.as_deref(), Some("codex"));
    assert!(loaded.changes.is_none());

    let changes = ChangesLayout { open: true };
    save_changes(&f.store, &changes).unwrap();
    assert_eq!(load(&f.store).unwrap().changes, Some(changes));

    f.store.set_setting(&Setting::new("layout"), &42).unwrap();
    let loaded = load(&f.store).unwrap();
    assert!(loaded.layout.is_none());
    assert!(loaded.settings_error.is_some());
    assert_eq!(loaded.entries.len(), 1);
}

#[test]
fn orchestration_round_trips() {
    let f = fixture();
    assert_eq!(load(&f.store).unwrap().orchestration, None);
    save_orchestration(&f.store, true).unwrap();
    assert_eq!(load(&f.store).unwrap().orchestration, Some(true));
    save_orchestration(&f.store, false).unwrap();
    assert_eq!(load(&f.store).unwrap().orchestration, Some(false));
}

#[test]
fn notify_attention_round_trips() {
    let f = fixture();
    assert_eq!(load(&f.store).unwrap().notify_attention, None);
    save_notify_attention(&f.store, false).unwrap();
    assert_eq!(load(&f.store).unwrap().notify_attention, Some(false));
    save_notify_attention(&f.store, true).unwrap();
    assert_eq!(load(&f.store).unwrap().notify_attention, Some(true));
}

#[test]
fn notify_finished_round_trips() {
    let f = fixture();
    assert_eq!(load(&f.store).unwrap().notify_finished, None);
    save_notify_finished(&f.store, false).unwrap();
    assert_eq!(load(&f.store).unwrap().notify_finished, Some(false));
}

#[test]
fn device_settings_round_trip() {
    use crate::device_panel::{DeviceKey, Platform};

    let f = fixture();
    assert!(load(&f.store).unwrap().device.is_none());
    let device = DeviceLayout {
        open: true,
        selected: Some(DeviceKey {
            platform: Platform::Ios,
            id: "E33FE595-67AD-470C-AD94-A38EAC9D0F38".into(),
        }),
        stats: true,
        logs: true,
        log_level: Some("error".into()),
    };
    save_device(&f.store, &device).unwrap();
    let loaded = load(&f.store).unwrap();
    assert_eq!(loaded.device, Some(device.clone()));
    assert!(loaded.settings_error.is_none());

    let android = DeviceLayout {
        selected: Some(DeviceKey {
            platform: Platform::Android,
            id: "emulator-5554".into(),
        }),
        ..device
    };
    save_device(&f.store, &android).unwrap();
    assert_eq!(load(&f.store).unwrap().device, Some(android));

    // A value saved before Android existed (platform written as a plain string).
    #[derive(serde::Serialize)]
    struct Saved {
        open: bool,
        width: f32,
        selected: SavedKey,
        stats: bool,
    }
    #[derive(serde::Serialize)]
    struct SavedKey {
        platform: &'static str,
        id: &'static str,
    }
    f.store
        .set_setting(
            &Setting::new("device_panel"),
            &Saved {
                open: true,
                width: 400.0,
                selected: SavedKey {
                    platform: "ios",
                    id: "U",
                },
                stats: false,
            },
        )
        .unwrap();
    let old = load(&f.store).unwrap().device.unwrap();
    assert_eq!(
        old.selected,
        Some(DeviceKey {
            platform: Platform::Ios,
            id: "U".into()
        })
    );

    // Fields added later default when an older value is read.
    #[derive(serde::Serialize)]
    struct Old {
        open: bool,
        width: f32,
    }
    f.store
        .set_setting(
            &Setting::new("device_panel"),
            &Old {
                open: false,
                width: 300.0,
            },
        )
        .unwrap();
    let old = load(&f.store).unwrap().device.unwrap();
    assert_eq!((old.open, old.selected, old.stats), (false, None, false));
    assert!(!old.logs && old.log_level.is_none());

    // A broken device setting is reported but leaves the other settings alone.
    save_agent(&f.store, "codex").unwrap();
    f.store
        .set_setting(&Setting::new("device_panel"), &"nope")
        .unwrap();
    let loaded = load(&f.store).unwrap();
    assert!(loaded.device.is_none());
    assert!(loaded.settings_error.is_some());
    assert_eq!(loaded.agent.as_deref(), Some("codex"));
}
