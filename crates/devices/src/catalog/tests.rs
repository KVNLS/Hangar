//! The catalogs through their port, on the paths that need no simulator or emulator.

use hangar_core::device::catalog::{Conflict, Target};
use hangar_core::device::{DeviceCost, DeviceInfo, Platform};
use hangar_core::ports::{DeviceCatalog, PortError};

use super::android::openable;
use super::ios::sort_ios;
use super::*;

fn device(udid: &str, name: &str, state: &str, avd: Option<&str>) -> DeviceInfo {
    DeviceInfo {
        udid: udid.into(),
        name: name.into(),
        runtime: "R".into(),
        state: state.into(),
        device_type: avd.map(Into::into),
        screen: None,
    }
}

fn unsupported(err: &PortError) -> bool {
    matches!(
        err.inner().downcast_ref::<crate::Error>(),
        Some(crate::Error::Unsupported(_))
    )
}

#[test]
fn base_choices_skip_clones_and_physical_devices() {
    let ios = base_choices(
        Platform::Ios,
        vec![
            device("A", "iPhone 17", "Shutdown", None),
            device("B", "Hangar · app/x", "Booted", None),
            device("C", "iPhone Air", "Booted", None),
        ],
    );
    let ids: Vec<&str> = ios.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["C", "A"]);
    let android = base_choices(
        Platform::Android,
        vec![
            device("Small", "Small", "Shutdown", Some("Small")),
            device("emulator-5554", "Pixel", "Booted", Some("Pixel")),
            device("emulator-5556", "Pixel", "Booted", Some("Pixel")),
            device("R5CT", "Galaxy", "Booted", Some("dm3q")),
        ],
    );
    let ids: Vec<&str> = android.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["Pixel", "Small"]);
}

#[test]
fn openable_drops_the_avd_id() {
    let open = openable(device(
        "emulator-5556",
        "Pixel",
        "Booted",
        Some("Pixel_10_Pro"),
    ));
    assert_eq!(open.device_type, None);
    assert_eq!(open.udid, "emulator-5556");
}

#[test]
fn fallback_only_when_primary_fails() {
    let fail = |m: &str| -> PortResult<i32> { Err(PortError::new(m.to_owned())) };
    let (v, why) = open_with_fallback("a", || Ok(1), "b", || Ok(2)).unwrap();
    assert_eq!((v, why), (1, None));

    let (v, why) = open_with_fallback("a", || fail("boom"), "b", || Ok(2)).unwrap();
    assert_eq!(v, 2);
    assert_eq!(why.as_deref(), Some("a backend failed: boom"));

    let err = open_with_fallback("a", || fail("boom"), "b", || fail("bust")).unwrap_err();
    assert_eq!(
        err.to_string(),
        "a backend failed: boom; b fallback failed: bust"
    );
}

#[test]
fn errors_print_their_sources() {
    let io = std::io::Error::other("disk gone");
    let err = PortError::new(crate::Error::Io(io));
    assert_eq!(chain(&err), "io: disk gone: disk gone");
}

#[test]
fn forced_backend_applies_per_platform() {
    assert_eq!(forced_for(Platform::Ios, Some("simctl")), Some("simctl"));
    assert_eq!(forced_for(Platform::Ios, Some("adb")), None);
    assert_eq!(forced_for(Platform::Android, Some("grpc")), Some("grpc"));
    assert_eq!(forced_for(Platform::Android, Some("private")), None);
    assert_eq!(forced_for(Platform::Android, Some("bogus")), Some("bogus"));
    assert_eq!(forced_for(Platform::Android, None), None);
}

#[test]
fn ios_list_sorts_booted_then_newest_runtime() {
    let sim = |name: &str, runtime: &str, state: &str| DeviceInfo {
        runtime: runtime.into(),
        ..device("U", name, state, None)
    };
    let mut devices = vec![
        sim("a", "iOS 26.5", "Shutdown"),
        sim("b", "iOS 9.3", "Shutdown"),
        sim("c", "iOS 27.0", "Shutdown"),
        sim("d", "iOS 26.5", "Booted"),
        sim("e", "iOS 27.0", "Shutdown"),
    ];
    sort_ios(&mut devices);
    let names: Vec<_> = devices.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["d", "c", "e", "a", "b"]);
}

#[test]
fn each_platform_has_its_catalog() {
    for platform in Platform::ALL {
        assert_eq!(for_platform(platform).platform(), platform);
    }
    assert_eq!(Simulators.cost(None), DeviceCost::ios_simulator());
    assert_eq!(Emulators.cost(None), DeviceCost::android(2048, 2));
}

#[test]
fn targets_of_the_other_platform_are_unsupported() {
    let android = Target::Android {
        avd: "Pixel".into(),
        serial: None,
        shared: true,
    };
    let own = Target::IosOwn {
        udid: None,
        base: "BASE".into(),
        name: "Hangar · app/x".into(),
    };
    assert!(unsupported(&Simulators.probe(&android).unwrap_err()));
    assert!(unsupported(
        &Simulators.boot(&android, &|_| {}).unwrap_err()
    ));
    assert!(unsupported(&Emulators.probe(&own).unwrap_err()));
    assert!(unsupported(&Emulators.boot(&own, &|_| {}).unwrap_err()));
    assert!(unsupported(&Emulators.create_if_missing(&own).unwrap_err()));
    // Only a workspace's own simulator is ever created.
    assert_eq!(Emulators.create_if_missing(&android).unwrap(), None);
    assert_eq!(Simulators.create_if_missing(&android).unwrap(), None);
    // Not created yet: nothing runs, nothing is asked of simctl.
    assert_eq!(Simulators.probe(&own).unwrap(), None);
}

#[test]
fn clones_and_conflicts_belong_to_one_platform() {
    let conflict = Conflict {
        avd: "Pixel".into(),
        serial: "emulator-5554".into(),
    };
    assert!(unsupported(
        &Simulators.clear_conflict(&conflict).unwrap_err()
    ));
    assert!(unsupported(&Emulators.delete_clone("UDID").unwrap_err()));
    assert!(unsupported(&Emulators.list_clones().unwrap_err()));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn simulators_need_macos() {
    assert!(unsupported(&Simulators.list().unwrap_err()));
    assert!(unsupported(&Simulators.running().unwrap_err()));
    assert!(unsupported(&SystemHost.detect().unwrap_err()));
}
