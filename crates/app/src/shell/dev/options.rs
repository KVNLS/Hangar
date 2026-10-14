//! `HANGAR_DEV_*` variables parsed into [`DevOptions`].

use super::*;

/// Debug/automation hooks, read once at startup (documented in `main.rs`).
#[derive(Clone, Debug, Default)]
pub struct DevOptions {
    pub open: Option<PathBuf>,
    pub workspace: String,
    /// Several workspaces to create instead of `workspace` (comma-separated names).
    pub workspaces: Vec<String>,
    /// Base device of the created workspaces (`ios:<UDID>` or `android:<AVD id>`).
    pub target: Option<crate::device_panel::DeviceKey>,
    /// "Devices at once" for this run (`auto` or 1–8), not saved.
    pub device_limit: Option<crate::device_pool::policy::Limit>,
    /// After the runs: save each dev workspace's latest frame here and log its device status.
    pub pool_frames: Option<PathBuf>,
    pub agent: String,
    /// Ollama host whose models OpenCode gets for this run (not saved).
    pub ollama: Option<String>,
    pub input: Option<String>,
    pub archive: Option<String>,
    pub restore: Option<String>,
    pub delete: Option<String>,
    pub delete_branch: bool,
    pub delete_force: bool,
    pub remove_project: Option<PathBuf>,
    /// `1`, or a worktree-relative path to select once it changes.
    pub changes: Option<String>,
    /// Commit message: stage everything in the dev workspace, then commit.
    pub commit: Option<String>,
    pub push: bool,
    /// PR title (`1` = the default title).
    pub pr: Option<String>,
    /// Open the full-width review screen once the changes panel lists files.
    pub review: bool,
    /// Open the overview, log what it counts, then go back.
    pub overview: bool,
    /// Worktree-relative path to open in the Files tab.
    pub files: Option<String>,
    /// `dark`, `light` or `system`: render every settings page, then set the appearance.
    pub appearance: Option<String>,
    /// Screen to leave the window on once every other hook has run (for screenshots).
    pub show: Option<String>,
    /// After `show`: wait for the workspace device's first frame, then take the ⌘S screenshot.
    pub screenshot: bool,
    /// After `show`: wait for a permission request, then allow it once after this many seconds
    /// (as the overview card and the palette do).
    pub approve: Option<u64>,
    /// After `show`: raw keys typed into the focused agent, each step after its delay
    /// (`HANGAR_DEV_KEYS="8|\e[B\r||3|hello\r"`: seconds, `|`, text with `\r`, `\n`, `\e`).
    pub keys: Vec<(u64, String)>,
    /// After `show`: answer the first question an agent asks (`ask_user`), `<seconds>|<text>`; a
    /// text that is a number picks that choice (0-based).
    pub answer: Option<(u64, String)>,
    /// Build and run on their devices: `1` = the workspaces `HANGAR_DEV_OPEN` created (else the
    /// focused one), or comma-separated slugs.
    pub run: Option<String>,
    /// Slug of a workspace to show in the focused pane after the runs.
    pub select: Option<String>,
    pub merge: Option<MergeStrategy>,
    /// Device id (simulator UDID) to open in the device panel.
    pub device: Option<String>,
    /// Press Home once the device shows frames (before the tap).
    pub device_home: bool,
    /// Tap at these device points once the device shows frames, then log whether the painted
    /// frame changed.
    pub device_tap: Option<(f64, f64)>,
    /// Type this text into the device once it shows frames (same check as the tap).
    pub device_text: Option<String>,
    /// Boot the device first when it is a shut-down virtual device (simulator UDID, AVD id).
    pub device_boot: bool,
    /// Comma-separated hardware controls to press after the text (`lock`, `volume-up`, ...).
    pub device_buttons: Vec<hangar_devices::Input>,
    /// Comma-separated keys to press after the buttons (`Backspace`, `Enter`, `ArrowUp`, ...).
    pub device_keys: Vec<hangar_devices::Input>,
    /// Shut the device down after all other device hooks.
    pub device_shutdown: bool,
    /// Open the logs pane under the device screen.
    pub device_logs: bool,
}

/// Parses a comma-separated list; unknown names are logged and skipped.
fn parse_list(
    hook: &str,
    value: Option<String>,
    parse: impl Fn(&str) -> Option<hangar_devices::Input>,
) -> Vec<hangar_devices::Input> {
    let value = value.unwrap_or_default();
    value
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .filter_map(|name| {
            let parsed = parse(name);
            if parsed.is_none() {
                tracing::error!(%name, "{hook}: unknown name");
            }
            parsed
        })
        .collect()
}

fn parse_button(name: &str) -> Option<hangar_devices::Input> {
    use hangar_devices::Input;
    Some(match name {
        "home" => Input::Home,
        "back" => Input::Back,
        "lock" | "power" => Input::Lock,
        "volume-up" => Input::VolumeUp,
        "volume-down" => Input::VolumeDown,
        "app-switch" => Input::AppSwitch,
        _ => return None,
    })
}

fn parse_key(name: &str) -> Option<hangar_devices::Input> {
    use hangar_devices::Key;
    Some(hangar_devices::Input::Key(
        match name.to_ascii_lowercase().as_str() {
            "backspace" => Key::Backspace,
            "enter" => Key::Enter,
            "tab" => Key::Tab,
            "escape" => Key::Escape,
            "arrowup" | "up" => Key::ArrowUp,
            "arrowdown" | "down" => Key::ArrowDown,
            "arrowleft" | "left" => Key::ArrowLeft,
            "arrowright" | "right" => Key::ArrowRight,
            _ => return None,
        },
    ))
}

impl DevOptions {
    pub fn from_env() -> Self {
        let var = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
        let flag = |key: &str| var(key).is_some_and(|v| v == "1");
        Self {
            open: var("HANGAR_DEV_OPEN").map(PathBuf::from),
            workspace: var("HANGAR_DEV_WORKSPACE").unwrap_or_else(|| "dev".to_owned()),
            workspaces: var("HANGAR_DEV_WORKSPACES")
                .map(|v| {
                    v.split(',')
                        .map(str::trim)
                        .filter(|n| !n.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            target: var("HANGAR_DEV_TARGET").and_then(|v| {
                let parsed = v.split_once(':').and_then(|(p, id)| {
                    let platform = match p {
                        "ios" => crate::device_panel::Platform::Ios,
                        "android" => crate::device_panel::Platform::Android,
                        _ => return None,
                    };
                    Some(crate::device_panel::DeviceKey {
                        platform,
                        id: id.to_owned(),
                    })
                });
                if parsed.is_none() {
                    tracing::error!(value = %v, "HANGAR_DEV_TARGET: expected ios:<UDID> or android:<AVD id>");
                }
                parsed
            }),
            device_limit: var("HANGAR_DEV_DEVICE_LIMIT").and_then(|v| {
                let parsed = crate::device_pool::policy::Limit::parse(&v);
                if parsed.is_none() {
                    tracing::error!(value = %v, "HANGAR_DEV_DEVICE_LIMIT: expected auto or 1-8");
                }
                parsed
            }),
            pool_frames: var("HANGAR_DEV_POOL_FRAMES").map(PathBuf::from),
            agent: var("HANGAR_DEV_AGENT").unwrap_or_else(|| agents::FALLBACK_AGENT.to_owned()),
            ollama: var("HANGAR_DEV_OLLAMA"),
            input: var("HANGAR_DEV_INPUT"),
            archive: var("HANGAR_DEV_ARCHIVE"),
            restore: var("HANGAR_DEV_RESTORE"),
            delete: var("HANGAR_DEV_DELETE"),
            delete_branch: flag("HANGAR_DEV_DELETE_BRANCH"),
            delete_force: flag("HANGAR_DEV_DELETE_FORCE"),
            remove_project: var("HANGAR_DEV_REMOVE_PROJECT").map(PathBuf::from),
            changes: var("HANGAR_DEV_CHANGES"),
            commit: var("HANGAR_DEV_COMMIT"),
            push: flag("HANGAR_DEV_PUSH"),
            pr: var("HANGAR_DEV_PR"),
            review: flag("HANGAR_DEV_REVIEW"),
            overview: flag("HANGAR_DEV_OVERVIEW"),
            files: var("HANGAR_DEV_FILES"),
            appearance: var("HANGAR_DEV_APPEARANCE"),
            show: var("HANGAR_DEV_SHOW"),
            screenshot: flag("HANGAR_DEV_SCREENSHOT"),
            approve: var("HANGAR_DEV_APPROVE").and_then(|v| v.parse().ok()),
            keys: var("HANGAR_DEV_KEYS").map(|v| key_steps(&v)).unwrap_or_default(),
            answer: var("HANGAR_DEV_ANSWER").and_then(|v| {
                let (delay, text) = v.split_once('|')?;
                Some((delay.trim().parse().ok()?, text.to_owned()))
            }),
            run: var("HANGAR_DEV_RUN"),
            select: var("HANGAR_DEV_SELECT"),
            merge: var("HANGAR_DEV_MERGE").and_then(|v| match v.as_str() {
                "noff" => Some(MergeStrategy::NoFf),
                "squash" => Some(MergeStrategy::Squash),
                other => {
                    tracing::error!(value = other, "HANGAR_DEV_MERGE: expected noff or squash");
                    None
                }
            }),
            device: var("HANGAR_DEV_DEVICE"),
            device_home: flag("HANGAR_DEV_DEVICE_HOME"),
            device_text: var("HANGAR_DEV_DEVICE_TEXT"),
            device_boot: flag("HANGAR_DEV_DEVICE_BOOT"),
            device_buttons: parse_list(
                "HANGAR_DEV_DEVICE_BUTTONS",
                var("HANGAR_DEV_DEVICE_BUTTONS"),
                parse_button,
            ),
            device_keys: parse_list(
                "HANGAR_DEV_DEVICE_KEYS",
                var("HANGAR_DEV_DEVICE_KEYS"),
                parse_key,
            ),
            device_shutdown: flag("HANGAR_DEV_DEVICE_SHUTDOWN"),
            device_logs: flag("HANGAR_DEV_DEVICE_LOGS"),
            device_tap: var("HANGAR_DEV_DEVICE_TAP").and_then(|v| {
                let parsed = v
                    .split_once(',')
                    .and_then(|(x, y)| Some((x.trim().parse().ok()?, y.trim().parse().ok()?)));
                if parsed.is_none() {
                    tracing::error!(value = %v, "HANGAR_DEV_DEVICE_TAP: expected X,Y in points");
                }
                parsed
            }),
        }
    }
}

/// `8|\e[B\r||3|hi\r` → `[(8, "\x1b[B\r"), (3, "hi\r")]`.
fn key_steps(spec: &str) -> Vec<(u64, String)> {
    spec.split("||")
        .filter_map(|step| {
            let (delay, text) = step.split_once('|')?;
            let text = text
                .replace("\\r", "\r")
                .replace("\\n", "\n")
                .replace("\\e", "\x1b");
            Some((delay.trim().parse().ok()?, text))
        })
        .collect()
}

#[cfg(test)]
mod key_tests {
    use super::key_steps;

    #[test]
    fn key_steps_parse_delays_and_escapes() {
        assert_eq!(
            key_steps(r"8|\e[B\r||3|hi\r||x|ignored"),
            [(8, "\x1b[B\r".to_owned()), (3, "hi\r".to_owned())]
        );
    }
}
