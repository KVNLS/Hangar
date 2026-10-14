use std::fs;
use std::path::{Path, PathBuf};

/// A runnable app found in a worktree. Paths are relative to the worktree root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stack {
    /// `package.json` depending on `expo`.
    Expo { dir: PathBuf },
    /// `package.json` depending on `react-native` (without Expo).
    ReactNative { dir: PathBuf },
    /// A native iOS app: `container` is the `.xcworkspace`, else the `.xcodeproj`.
    Xcode { container: PathBuf },
    /// A native Android app: `dir` holds `settings.gradle(.kts)`, `module` applies
    /// `com.android.application`.
    Gradle { dir: PathBuf, module: String },
}

impl Stack {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Expo { .. } => "Expo",
            Self::ReactNative { .. } => "React Native",
            Self::Xcode { .. } => "Xcode",
            Self::Gradle { .. } => "Gradle",
        }
    }
}

impl Stack {
    /// Whether this stack builds for `platform` (React Native and Expo build for both).
    pub fn targets(&self, platform: hangar_core::device::Platform) -> bool {
        use hangar_core::device::Platform;
        match self {
            Self::Xcode { .. } => platform == Platform::Ios,
            Self::Gradle { .. } => platform == Platform::Android,
            Self::Expo { .. } | Self::ReactNative { .. } => true,
        }
    }
}

/// The first detected stack that builds for `platform`: a repository with native `ios/` and
/// `android/` apps runs the one matching the open device.
pub fn pick_stack(stacks: &[Stack], platform: hangar_core::device::Platform) -> Option<&Stack> {
    stacks.iter().find(|stack| stack.targets(platform))
}

/// Native subprojects that cross-platform stacks keep next to their sources.
const NATIVE_DIRS: [&str; 2] = ["ios", "android"];

/// Stacks found at `root` and in its `ios/` and `android/` directories. React Native and Expo
/// own those directories, so their native projects are not listed separately.
pub fn detect(root: &Path) -> Vec<Stack> {
    if let Some(stack) = js_stack(root) {
        return vec![stack];
    }
    let mut found = Vec::new();
    for dir in std::iter::once("").chain(NATIVE_DIRS) {
        let rel = PathBuf::from(dir);
        let abs = root.join(&rel);
        if let Some(container) = xcode_container(&abs) {
            found.push(Stack::Xcode {
                container: rel.join(container),
            });
        }
        if let Some(module) = gradle_app_module(&abs) {
            found.push(Stack::Gradle { dir: rel, module });
        }
    }
    found
}

fn js_stack(dir: &Path) -> Option<Stack> {
    let text = fs::read_to_string(dir.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let depends_on = |name: &str| {
        ["dependencies", "devDependencies"]
            .iter()
            .any(|section| json.get(section).and_then(|s| s.get(name)).is_some())
    };
    if depends_on("expo") {
        Some(Stack::Expo {
            dir: PathBuf::new(),
        })
    } else if depends_on("react-native") {
        Some(Stack::ReactNative {
            dir: PathBuf::new(),
        })
    } else {
        None
    }
}

fn xcode_container(dir: &Path) -> Option<PathBuf> {
    let names = sorted_names(dir);
    let with_ext = |ext: &str| names.iter().find(|n| n.ends_with(ext)).map(PathBuf::from);
    with_ext(".xcworkspace").or_else(|| with_ext(".xcodeproj"))
}

fn gradle_app_module(dir: &Path) -> Option<String> {
    let has = |dir: &Path, base: &str| {
        dir.join(base).is_file() || dir.join(format!("{base}.kts")).is_file()
    };
    if !has(dir, "settings.gradle") {
        return None;
    }
    let applies_app_plugin = |module: &str| {
        ["build.gradle", "build.gradle.kts"].iter().any(|file| {
            fs::read_to_string(dir.join(module).join(file)).is_ok_and(|text| {
                text.contains("com.android.application") || text.contains("android.application")
            })
        })
    };
    let names = sorted_names(dir);
    names
        .iter()
        .find(|name| name.as_str() == "app" && applies_app_plugin(name))
        .or_else(|| names.iter().find(|name| applies_app_plugin(name)))
        .cloned()
}

fn sorted_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests;
