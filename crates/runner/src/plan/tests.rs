use super::*;

fn ios() -> Target {
    Target {
        platform: Platform::Ios,
        id: "UDID-1".into(),
        avd: None,
    }
}

fn android() -> Target {
    Target {
        platform: Platform::Android,
        id: "emulator-5554".into(),
        avd: Some("Pixel_10_Pro".into()),
    }
}

fn run(stack: &Stack, target: &Target, scheme: Option<&str>) -> Result<RunPlan> {
    plan(&PlanInput {
        stack,
        target,
        port: 8100,
        scheme,
        build_dir: Path::new("/build/ws"),
    })
}

fn root() -> PathBuf {
    PathBuf::new()
}

#[test]
fn expo_on_ios_builds_and_serves_on_the_workspace_port() {
    let plan = run(&Stack::Expo { dir: root() }, &ios(), None).unwrap();
    assert!(plan.server.is_none());
    assert!(plan.build_serves);
    assert_eq!(
        plan.build.shell_line(),
        "npx expo run:ios --device UDID-1 --port 8100"
    );
    assert!(
        plan.build
            .env
            .contains(&("RCT_METRO_PORT".into(), "8100".into()))
    );
    assert!(plan.before.is_empty());
    assert!(plan.after.is_empty());
}

#[test]
fn expo_on_android_picks_the_avd_and_reverses_the_port() {
    let plan = run(&Stack::Expo { dir: root() }, &android(), None).unwrap();
    assert_eq!(
        plan.build.shell_line(),
        "npx expo run:android --device Pixel_10_Pro --port 8100"
    );
    assert_eq!(plan.before, vec![Step::AdbReverse(8100)]);
    assert!(
        plan.build
            .env
            .contains(&("ANDROID_SERIAL".into(), "emulator-5554".into()))
    );
}

#[test]
fn react_native_cli_both_platforms() {
    let stack = Stack::ReactNative { dir: root() };
    let ios_plan = run(&stack, &ios(), None).unwrap();
    assert_eq!(
        ios_plan.build.shell_line(),
        "npx react-native run-ios --udid UDID-1 --port 8100 --no-packager"
    );
    assert_eq!(
        ios_plan.server.unwrap().shell_line(),
        "npx react-native start --port 8100"
    );
    let android_plan = run(&stack, &android(), None).unwrap();
    assert_eq!(
        android_plan.build.shell_line(),
        "npx react-native run-android --deviceId emulator-5554 --port 8100 --no-packager"
    );
    assert_eq!(android_plan.before, vec![Step::AdbReverse(8100)]);
}

#[test]
fn xcode_builds_into_the_workspace_build_dir_then_installs() {
    let stack = Stack::Xcode {
        container: PathBuf::from("ios/Demo.xcworkspace"),
    };
    let plan = run(&stack, &ios(), Some("Demo App")).unwrap();
    assert_eq!(
        plan.build.shell_line(),
        "xcodebuild -workspace ios/Demo.xcworkspace -scheme 'Demo App' -configuration Debug \
         -destination id=UDID-1 -derivedDataPath /build/ws build"
    );
    assert_eq!(
        plan.after,
        vec![Step::InstallAndLaunch(Artifact::Xcode {
            container: PathBuf::from("ios/Demo.xcworkspace"),
            scheme: "Demo App".into(),
            udid: "UDID-1".into(),
            build_dir: PathBuf::from("/build/ws"),
        })]
    );
}

#[test]
fn xcode_needs_a_scheme_and_an_ios_target() {
    let stack = Stack::Xcode {
        container: PathBuf::from("Demo.xcodeproj"),
    };
    assert!(matches!(
        run(&stack, &ios(), None),
        Err(Error::SchemeRequired(_))
    ));
    assert!(matches!(
        run(&stack, &android(), Some("Demo")),
        Err(Error::Mismatch { .. })
    ));
    let plan = run(&stack, &ios(), Some("Demo")).unwrap();
    assert!(plan.build.args.starts_with(&["-project".into()]));
}

#[test]
fn gradle_assembles_then_installs_on_android_only() {
    let stack = Stack::Gradle {
        dir: PathBuf::from("android"),
        module: "app".into(),
    };
    let plan = run(&stack, &android(), None).unwrap();
    assert_eq!(plan.build.shell_line(), "./gradlew :app:assembleDebug");
    assert_eq!(plan.build.cwd, PathBuf::from("android"));
    assert_eq!(
        plan.after,
        vec![Step::InstallAndLaunch(Artifact::Gradle {
            dir: PathBuf::from("android"),
            module: "app".into(),
        })]
    );
    assert!(matches!(
        run(&stack, &ios(), None),
        Err(Error::Mismatch { .. })
    ));
}

#[test]
fn js_stacks_install_dependencies_first() {
    let tmp = tempfile::tempdir().unwrap();
    let expo = Stack::Expo { dir: root() };
    assert_eq!(
        setup(&expo, tmp.path()).unwrap().shell_line(),
        "npm install"
    );
    std::fs::write(tmp.path().join("yarn.lock"), "").unwrap();
    assert_eq!(
        setup(&expo, tmp.path()).unwrap().shell_line(),
        "yarn install"
    );
    std::fs::write(tmp.path().join("bun.lock"), "").unwrap();
    assert_eq!(
        setup(&expo, tmp.path()).unwrap().shell_line(),
        "bun install"
    );
    std::fs::create_dir(tmp.path().join("node_modules")).unwrap();
    assert!(setup(&expo, tmp.path()).is_none());
    let gradle = Stack::Gradle {
        dir: root(),
        module: "app".into(),
    };
    assert!(setup(&gradle, Path::new("/nowhere")).is_none());
}

#[test]
fn npm_ci_with_a_package_lock() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("package-lock.json"), "{}").unwrap();
    let rn = Stack::ReactNative { dir: root() };
    assert_eq!(setup(&rn, tmp.path()).unwrap().shell_line(), "npm ci");
}

#[test]
fn quoting() {
    assert_eq!(shell_quote("plain-word_1.0"), "plain-word_1.0");
    assert_eq!(shell_quote("two words"), "'two words'");
    assert_eq!(shell_quote("it's"), r"'it'\''s'");
    assert_eq!(shell_quote(""), "''");
}
