use std::path::{Path, PathBuf};

use hangar_core::device::Platform;

use crate::{Error, Result, Stack};

/// The device a run targets. It must be booted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub platform: Platform,
    /// Simulator UDID on iOS, adb serial (`emulator-5554`) on Android.
    pub id: String,
    /// AVD name on Android (Expo picks emulators by name).
    pub avd: Option<String>,
}

/// One program run in a terminal tab, from `cwd` (relative to the worktree root).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}

impl Command {
    fn new(program: &str, args: &[&str], cwd: &Path) -> Self {
        Self {
            program: program.to_owned(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            cwd: cwd.to_owned(),
            env: Vec::new(),
        }
    }

    fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    fn env(mut self, key: &str, value: impl Into<String>) -> Self {
        self.env.push((key.to_owned(), value.into()));
        self
    }

    /// Program and arguments quoted for `sh -c` (environment not included).
    pub fn shell_line(&self) -> String {
        std::iter::once(&self.program)
            .chain(&self.args)
            .map(|word| shell_quote(word))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Steps the app performs itself, before the build tab starts or after it exits 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// `adb reverse tcp:P tcp:P` so the emulator reaches the dev server on the host.
    AdbReverse(u16),
    /// Find what the build produced, install it on the target and launch it.
    InstallAndLaunch(Artifact),
}

/// Where a native build leaves its app. Resolved with [`crate::resolve_artifact`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Artifact {
    Xcode {
        container: PathBuf,
        scheme: String,
        udid: String,
        build_dir: PathBuf,
    },
    Gradle {
        dir: PathBuf,
        module: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPlan {
    /// Long-running dev server (Metro), in its own tab.
    pub server: Option<Command>,
    pub before: Vec<Step>,
    /// The build (and, for the JS CLIs, the install and launch too).
    pub build: Command,
    /// The build command keeps running as the dev server once the app is up (`expo run`):
    /// its tab is the place for reload keys.
    pub build_serves: bool,
    pub after: Vec<Step>,
}

pub struct PlanInput<'a> {
    pub stack: &'a Stack,
    pub target: &'a Target,
    /// The workspace's dev-server port (JS stacks).
    pub port: u16,
    /// Xcode scheme, required for [`Stack::Xcode`].
    pub scheme: Option<&'a str>,
    /// Per-workspace build output directory outside the worktree (Xcode derived data).
    pub build_dir: &'a Path,
}

pub fn plan(input: &PlanInput<'_>) -> Result<RunPlan> {
    let PlanInput {
        stack,
        target,
        port,
        scheme,
        build_dir,
    } = *input;
    let port_str = port.to_string();
    let android = target.platform == Platform::Android;
    let mismatch = |platform: Platform| Error::Mismatch {
        stack: stack.label(),
        platform: platform_label(platform),
    };
    let with_target_env = |command: Command| {
        if android {
            command.env("ANDROID_SERIAL", &target.id)
        } else {
            command
        }
    };
    let js_before = if android {
        vec![Step::AdbReverse(port)]
    } else {
        Vec::new()
    };
    let plan = match stack {
        Stack::Expo { dir } => {
            let device = if android {
                target.avd.as_deref().unwrap_or(&target.id)
            } else {
                &target.id
            };
            let run = if android { "run:android" } else { "run:ios" };
            // `expo run` refuses `--port` with `--no-bundler`, so it starts Metro itself.
            RunPlan {
                server: None,
                before: js_before,
                build: with_target_env(
                    Command::new(
                        "npx",
                        &["expo", run, "--device", device, "--port", &port_str],
                        dir,
                    )
                    .env("RCT_METRO_PORT", &port_str),
                ),
                build_serves: true,
                after: Vec::new(),
            }
        }
        Stack::ReactNative { dir } => {
            let build = if android {
                Command::new(
                    "npx",
                    &["react-native", "run-android", "--deviceId", &target.id],
                    dir,
                )
            } else {
                Command::new(
                    "npx",
                    &["react-native", "run-ios", "--udid", &target.id],
                    dir,
                )
            };
            RunPlan {
                server: Some(
                    Command::new("npx", &["react-native", "start", "--port", &port_str], dir)
                        .env("RCT_METRO_PORT", &port_str),
                ),
                before: js_before,
                build: with_target_env(
                    build
                        .arg("--port")
                        .arg(&port_str)
                        .arg("--no-packager")
                        .env("RCT_METRO_PORT", &port_str),
                ),
                build_serves: false,
                after: Vec::new(),
            }
        }
        Stack::Xcode { container } => {
            if android {
                return Err(mismatch(target.platform));
            }
            let scheme =
                scheme.ok_or_else(|| Error::SchemeRequired(container.display().to_string()))?;
            let build_dir = build_dir.display().to_string();
            RunPlan {
                server: None,
                before: Vec::new(),
                build: Command::new("xcodebuild", &[], Path::new(""))
                    .arg(container_flag(container))
                    .arg(container.display().to_string())
                    .arg("-scheme")
                    .arg(scheme)
                    .arg("-configuration")
                    .arg("Debug")
                    .arg("-destination")
                    .arg(format!("id={}", target.id))
                    .arg("-derivedDataPath")
                    .arg(&build_dir)
                    .arg("build"),
                build_serves: false,
                after: vec![Step::InstallAndLaunch(Artifact::Xcode {
                    container: container.clone(),
                    scheme: scheme.to_owned(),
                    udid: target.id.clone(),
                    build_dir: input.build_dir.to_owned(),
                })],
            }
        }
        Stack::Gradle { dir, module } => {
            if !android {
                return Err(mismatch(target.platform));
            }
            RunPlan {
                server: None,
                before: Vec::new(),
                build: with_target_env(Command::new(
                    "./gradlew",
                    &[&format!(":{module}:assembleDebug")],
                    dir,
                )),
                build_serves: false,
                after: vec![Step::InstallAndLaunch(Artifact::Gradle {
                    dir: dir.clone(),
                    module: module.clone(),
                })],
            }
        }
    };
    Ok(plan)
}

/// Checks the project can be built as planned: a Gradle build needs an executable `gradlew`.
pub fn check(stack: &Stack, root: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    if let Stack::Gradle { dir, .. } = stack {
        let wrapper = root.join(dir).join("gradlew");
        let executable = std::fs::metadata(&wrapper)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
        if !executable {
            return Err(Error::GradleWrapper(wrapper));
        }
    }
    Ok(())
}

/// Dependency install a JS stack needs before Metro or the build can start: worktrees are fresh
/// checkouts without `node_modules`. The lockfile picks the package manager.
pub fn setup(stack: &Stack, root: &Path) -> Option<Command> {
    let dir = match stack {
        Stack::Expo { dir } | Stack::ReactNative { dir } => dir,
        Stack::Xcode { .. } | Stack::Gradle { .. } => return None,
    };
    let abs = root.join(dir);
    if abs.join("node_modules").is_dir() {
        return None;
    }
    let has = |file: &str| abs.join(file).is_file();
    let (program, args): (&str, &[&str]) = if has("bun.lock") || has("bun.lockb") {
        ("bun", &["install"])
    } else if has("pnpm-lock.yaml") {
        ("pnpm", &["install"])
    } else if has("yarn.lock") {
        ("yarn", &["install"])
    } else if has("package-lock.json") {
        ("npm", &["ci"])
    } else {
        ("npm", &["install"])
    };
    Some(Command::new(program, args, dir))
}

pub(crate) fn container_flag(container: &Path) -> &'static str {
    if container
        .extension()
        .is_some_and(|ext| ext == "xcworkspace")
    {
        "-workspace"
    } else {
        "-project"
    }
}

fn platform_label(platform: Platform) -> &'static str {
    match platform {
        Platform::Ios => "iOS",
        Platform::Android => "Android",
    }
}

/// Single-quotes `word` unless it only holds characters the shell leaves alone.
pub fn shell_quote(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests;
