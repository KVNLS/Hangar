use super::*;

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
