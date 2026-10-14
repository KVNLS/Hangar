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
/// Where monorepos keep their apps.
const APP_PARENTS: [&str; 2] = ["apps", "packages"];
/// Directories that hold dependencies or build output, never an app of this repository.
const SKIPPED: [&str; 5] = ["node_modules", "Pods", "build", "DerivedData", "Carthage"];

/// Every app found in `root`, as candidates for a run. A React Native or Expo app at the root
/// owns `ios/` and `android/`, so it is the only one. Otherwise: JS apps in `apps/*`,
/// `packages/*` and direct child directories, then Xcode containers and Gradle application
/// modules at the root, in `ios/`, `android/` and direct child directories.
pub fn detect(root: &Path) -> Vec<Stack> {
    if let Some(stack) = js_stack(root, PathBuf::new()) {
        return vec![stack];
    }
    let children = child_dirs(root);
    let nested = APP_PARENTS.iter().flat_map(|parent| {
        child_dirs(&root.join(parent))
            .into_iter()
            .map(move |c| Path::new(parent).join(c))
    });
    let mut found: Vec<Stack> = nested
        .chain(children.iter().map(PathBuf::from))
        .filter_map(|rel| js_stack(&root.join(&rel), rel))
        .collect();
    let native = std::iter::once(String::new())
        .chain(NATIVE_DIRS.map(str::to_owned))
        .chain(
            children
                .into_iter()
                .filter(|c| !NATIVE_DIRS.contains(&c.as_str())),
        );
    for dir in native {
        let rel = PathBuf::from(dir);
        let abs = root.join(&rel);
        found.extend(xcode_containers(&abs).into_iter().map(|c| Stack::Xcode {
            container: rel.join(c),
        }));
        found.extend(
            gradle_app_modules(&abs)
                .into_iter()
                .map(|module| Stack::Gradle {
                    dir: rel.clone(),
                    module,
                }),
        );
    }
    found
}

fn js_stack(dir: &Path, rel: PathBuf) -> Option<Stack> {
    let text = fs::read_to_string(dir.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let depends_on = |name: &str| {
        ["dependencies", "devDependencies"]
            .iter()
            .any(|section| json.get(section).and_then(|s| s.get(name)).is_some())
    };
    if depends_on("expo") {
        Some(Stack::Expo { dir: rel })
    } else if depends_on("react-native") {
        Some(Stack::ReactNative { dir: rel })
    } else {
        None
    }
}

/// `.xcworkspace` directories in `dir`, else its `.xcodeproj` directories: a workspace builds
/// the projects next to it.
fn xcode_containers(dir: &Path) -> Vec<PathBuf> {
    let names = sorted_names(dir);
    let with_ext = |ext: &str| -> Vec<PathBuf> {
        names
            .iter()
            .filter(|n| n.ends_with(ext) && dir.join(n).is_dir())
            .map(PathBuf::from)
            .collect()
    };
    let workspaces = with_ext(".xcworkspace");
    if workspaces.is_empty() {
        with_ext(".xcodeproj")
    } else {
        workspaces
    }
}

/// Gradle paths (`app`, `apps:phone`) of the modules in `dir`'s build that apply the Android
/// application plugin, `app` first. Modules come from the `include`s of `settings.gradle(.kts)`,
/// or are its child directories when it includes none.
fn gradle_app_modules(dir: &Path) -> Vec<String> {
    let Some(settings) = ["settings.gradle", "settings.gradle.kts"]
        .iter()
        .find_map(|f| fs::read_to_string(dir.join(f)).ok())
    else {
        return Vec::new();
    };
    let mut modules = included_modules(&settings);
    if modules.is_empty() {
        modules = sorted_names(dir);
    }
    let applies_app_plugin = |module: &str| {
        let module_dir = dir.join(module.replace(':', "/"));
        ["build.gradle", "build.gradle.kts"].iter().any(|file| {
            fs::read_to_string(module_dir.join(file)).is_ok_and(|text| {
                let code = without_comments(&text);
                code.contains("com.android.application") || code.contains("android.application")
            })
        })
    };
    let mut apps: Vec<String> = modules
        .into_iter()
        .filter(|m| applies_app_plugin(m))
        .collect();
    apps.sort_by_key(|m| m != "app");
    apps
}

/// Module paths named in `include` lines (`include ':a', ':b:c'` or `include(":a")`), without
/// their leading `:`.
fn included_modules(settings: &str) -> Vec<String> {
    let mut modules = Vec::new();
    for line in without_comments(settings).lines() {
        let line = line.trim();
        if !line.starts_with("include") {
            continue;
        }
        let mut rest = line;
        while let Some(start) = rest.find(['"', '\'']) {
            let quote = rest[start..].chars().next().unwrap_or('"');
            let after = &rest[start + 1..];
            let Some(end) = after.find(quote) else {
                break;
            };
            let name = after[..end].trim_start_matches(':');
            if !name.is_empty() {
                modules.push(name.to_owned());
            }
            rest = &after[end + 1..];
        }
    }
    modules
}

/// `text` without `//` line comments and `/* */` block comments (strings are not parsed: good
/// enough to read plugin ids and includes).
fn without_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        match (rest.find("//"), rest.find("/*")) {
            (Some(line), block) if block.is_none_or(|b| line < b) => {
                out.push_str(&rest[..line]);
                rest = rest[line..]
                    .find('\n')
                    .map_or("", |end| &rest[line + end..]);
            }
            (_, Some(block)) => {
                out.push_str(&rest[..block]);
                rest = rest[block..]
                    .find("*/")
                    .map_or("", |end| &rest[block + end + 2..]);
            }
            _ => {
                out.push_str(rest);
                break;
            }
        }
    }
    out
}

/// Child directories of `dir` that may hold an app: not hidden, not dependencies or build
/// output.
fn child_dirs(dir: &Path) -> Vec<String> {
    sorted_names(dir)
        .into_iter()
        .filter(|n| {
            !n.starts_with('.')
                && !SKIPPED.contains(&n.as_str())
                // Xcode containers are directories too.
                && !n.ends_with(".xcodeproj")
                && !n.ends_with(".xcworkspace")
                && dir.join(n).is_dir()
        })
        .collect()
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
