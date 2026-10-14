//! [`ToolProbe`] over this Mac: git, `gh`, the agent presets, Xcode, the Android SDK, a JDK and
//! Node, looked up with the login shell's `PATH`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use hangar_agents::find_program;
use hangar_core::ports::{CodeHost, ToolProbe};
use hangar_core::review::GhStatus;
use hangar_core::setup::{AgentFact, AndroidFact, GitFact, ToolFacts, XcodeFact};

use crate::agents::{self, AgentPreset, FALLBACK_AGENT};

pub struct SystemProbe {
    /// The `PATH` agents and `gh` run with.
    pub path: String,
    /// Why the login shell's `PATH` could not be read, if it could not.
    pub path_error: Option<String>,
    pub gh: std::sync::Arc<dyn CodeHost>,
}

impl ToolProbe for SystemProbe {
    fn probe(&self) -> ToolFacts {
        let found = |program: &str| find_program(program, Some(OsStr::new(&self.path)));
        ToolFacts {
            path_error: self.path_error.clone(),
            homebrew: found("brew").is_some(),
            git: git(),
            gh: self.gh.status().unwrap_or(GhStatus::Missing),
            agents: agents::all_presets()
                .into_iter()
                .filter(|p| p.id() != FALLBACK_AGENT)
                .map(|preset| agent(&preset, found(preset.program())))
                .collect(),
            xcode: xcode(),
            android: android(),
            jdk: java_home(),
            node: found("node"),
        }
    }
}

fn git() -> GitFact {
    match hangar_worktree::git_version() {
        Ok(version) => {
            let (name, email) = hangar_worktree::identity().unwrap_or_else(|err| {
                tracing::warn!(%err, "cannot read the git identity");
                (None, None)
            });
            GitFact::Installed {
                version: version.to_string(),
                name,
                email,
            }
        }
        Err(hangar_worktree::Error::GitTooOld { found, required }) => GitFact::TooOld {
            version: found.to_string(),
            required: required.to_string(),
        },
        Err(err) => {
            tracing::info!(%err, "git unavailable");
            GitFact::Missing
        }
    }
}

fn agent(preset: &AgentPreset, path: Option<PathBuf>) -> AgentFact {
    let (install_hint, drives) = match preset {
        AgentPreset::Pty(pty) => (pty.install_hint.clone(), None),
        AgentPreset::Acp(acp) => (
            acp.install_hint.clone(),
            acp.id.strip_suffix("-acp").map(str::to_owned),
        ),
    };
    AgentFact {
        id: preset.id().to_owned(),
        name: preset.display_name().to_owned(),
        program: preset.program().to_owned(),
        path,
        install_hint,
        drives,
    }
}

const XCODE_APP: &str = "/Applications/Xcode.app";

#[cfg(target_os = "macos")]
fn xcode() -> XcodeFact {
    use hangar_devices::{ios, simctl};
    let installed = Path::new(XCODE_APP).is_dir();
    let Ok(dir) = ios::developer_dir() else {
        return XcodeFact::Missing;
    };
    let Some((app, _)) = dir.split_once(".app/") else {
        // The Command Line Tools: selecting Xcode only helps if it is there.
        return if installed {
            XcodeFact::CommandLineToolsOnly
        } else {
            XcodeFact::Missing
        };
    };
    XcodeFact::Installed {
        app: PathBuf::from(format!("{app}.app")),
        first_launch_done: ios::first_launch_done(),
        ios_runtimes: simctl::ios_runtime_count().unwrap_or_else(|err| {
            tracing::warn!(%err, "cannot list iOS runtimes");
            0
        }),
    }
}

#[cfg(not(target_os = "macos"))]
fn xcode() -> XcodeFact {
    XcodeFact::Missing
}

fn android() -> AndroidFact {
    let Ok(sdk) = hangar_devices::android::AndroidSdk::locate() else {
        return AndroidFact::Missing;
    };
    let avds = hangar_devices::android::list_avds(&sdk)
        .map(|avds| avds.len())
        .unwrap_or_else(|err| {
            tracing::warn!(%err, "cannot list AVDs");
            0
        });
    AndroidFact::Installed {
        sdk: sdk.root,
        avds,
    }
}

/// The default JDK's home, as `/usr/libexec/java_home` reports it.
fn java_home() -> Option<PathBuf> {
    let out = Command::new("/usr/libexec/java_home")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let home = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !home.is_empty()).then(|| PathBuf::from(home))
}
