use super::*;

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn android_app(root: &Path, dir: &str) {
    write(
        root,
        &format!("{dir}settings.gradle.kts"),
        "include(\":app\")",
    );
    write(
        root,
        &format!("{dir}app/build.gradle.kts"),
        "plugins { id(\"com.android.application\") }",
    );
}

#[test]
fn expo_wins_over_react_native_and_hides_native_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "package.json",
        r#"{"dependencies":{"expo":"~52.0.0","react-native":"0.76.0"}}"#,
    );
    fs::create_dir_all(tmp.path().join("ios/App.xcworkspace")).unwrap();
    android_app(tmp.path(), "android/");
    assert_eq!(
        detect(tmp.path()),
        vec![Stack::Expo {
            dir: PathBuf::new()
        }]
    );
}

#[test]
fn react_native_from_dev_dependencies() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "package.json",
        r#"{"devDependencies":{"react-native":"0.76.0"}}"#,
    );
    assert_eq!(
        detect(tmp.path()),
        vec![Stack::ReactNative {
            dir: PathBuf::new()
        }]
    );
}

#[test]
fn plain_node_package_is_not_a_stack() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "package.json",
        r#"{"dependencies":{"react":"19"}}"#,
    );
    assert!(detect(tmp.path()).is_empty());
}

#[test]
fn xcode_prefers_the_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("Demo.xcodeproj/project.xcworkspace")).unwrap();
    fs::create_dir_all(tmp.path().join("Demo.xcworkspace")).unwrap();
    assert_eq!(
        detect(tmp.path()),
        vec![Stack::Xcode {
            container: PathBuf::from("Demo.xcworkspace")
        }]
    );
}

#[test]
fn native_ios_and_android_side_by_side() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("ios/Demo.xcodeproj")).unwrap();
    android_app(tmp.path(), "android/");
    assert_eq!(
        detect(tmp.path()),
        vec![
            Stack::Xcode {
                container: PathBuf::from("ios/Demo.xcodeproj")
            },
            Stack::Gradle {
                dir: PathBuf::from("android"),
                module: "app".into()
            },
        ]
    );
}

#[test]
fn gradle_module_other_than_app() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "settings.gradle", "include ':lib', ':mobile'");
    write(
        tmp.path(),
        "lib/build.gradle",
        "plugins { id 'com.android.library' }",
    );
    write(
        tmp.path(),
        "mobile/build.gradle",
        "plugins { id 'com.android.application' }",
    );
    assert_eq!(
        detect(tmp.path()),
        vec![Stack::Gradle {
            dir: PathBuf::new(),
            module: "mobile".into()
        }]
    );
}

#[test]
fn gradle_library_only_is_not_a_stack() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "settings.gradle", "include ':lib'");
    write(
        tmp.path(),
        "lib/build.gradle",
        "plugins { id 'com.android.library' }",
    );
    assert!(detect(tmp.path()).is_empty());
}

#[test]
fn picks_the_stack_for_the_device_platform() {
    use hangar_core::device::Platform;
    let native = vec![
        Stack::Xcode {
            container: PathBuf::from("ios/Demo.xcodeproj"),
        },
        Stack::Gradle {
            dir: PathBuf::from("android"),
            module: "app".into(),
        },
    ];
    assert_eq!(pick_stack(&native, Platform::Android), native.get(1));
    assert_eq!(pick_stack(&native, Platform::Ios), native.first());
    let expo = vec![Stack::Expo {
        dir: PathBuf::new(),
    }];
    assert_eq!(pick_stack(&expo, Platform::Ios), expo.first());
    assert_eq!(pick_stack(&expo, Platform::Android), expo.first());
    assert_eq!(pick_stack(&native[..1], Platform::Android), None);
}

#[test]
fn empty_repo() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "README.md", "# hi");
    assert!(detect(tmp.path()).is_empty());
}

#[test]
fn js_apps_in_a_monorepo() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "package.json",
        r#"{"private":true,"workspaces":["apps/*"]}"#,
    );
    write(
        tmp.path(),
        "apps/mobile/package.json",
        r#"{"dependencies":{"expo":"~52.0.0"}}"#,
    );
    write(
        tmp.path(),
        "packages/legacy/package.json",
        r#"{"dependencies":{"react-native":"0.76.0"}}"#,
    );
    write(
        tmp.path(),
        "node_modules/expo/package.json",
        r#"{"dependencies":{"expo":"1"}}"#,
    );
    assert_eq!(
        detect(tmp.path()),
        vec![
            Stack::Expo {
                dir: PathBuf::from("apps/mobile")
            },
            Stack::ReactNative {
                dir: PathBuf::from("packages/legacy")
            },
        ]
    );
}

#[test]
fn every_xcode_container_is_a_candidate() {
    let tmp = tempfile::tempdir().unwrap();
    for dir in [
        "Alpha.xcodeproj",
        "Zeta.xcodeproj",
        "ios/App.xcodeproj",
        "Tool/Tool.xcodeproj",
    ] {
        fs::create_dir_all(tmp.path().join(dir)).unwrap();
    }
    // Not containers: a plain file, and projects inside Pods or a hidden directory.
    write(tmp.path(), "Notes.xcodeproj", "");
    fs::create_dir_all(tmp.path().join("Pods/Pods.xcodeproj")).unwrap();
    fs::create_dir_all(tmp.path().join(".build/X.xcodeproj")).unwrap();
    let containers: Vec<PathBuf> = detect(tmp.path())
        .into_iter()
        .map(|s| match s {
            Stack::Xcode { container } => container,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        containers,
        [
            PathBuf::from("Alpha.xcodeproj"),
            PathBuf::from("Zeta.xcodeproj"),
            PathBuf::from("ios/App.xcodeproj"),
            PathBuf::from("Tool/Tool.xcodeproj"),
        ]
    );
}

#[test]
fn gradle_modules_come_from_settings_includes() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "settings.gradle.kts",
        "rootProject.name = \"x\"\ninclude(\":apps:phone\", \":lib\")\n// include(\":old\")",
    );
    write(
        tmp.path(),
        "apps/phone/build.gradle.kts",
        "plugins { id(\"com.android.application\") }",
    );
    // A library whose comment mentions the application plugin is not an app.
    write(
        tmp.path(),
        "lib/build.gradle.kts",
        "// not com.android.application\nplugins { id(\"com.android.library\") }",
    );
    write(
        tmp.path(),
        "old/build.gradle",
        "plugins { id 'com.android.application' }",
    );
    assert_eq!(
        detect(tmp.path()),
        vec![Stack::Gradle {
            dir: PathBuf::new(),
            module: "apps:phone".into()
        }]
    );
}
