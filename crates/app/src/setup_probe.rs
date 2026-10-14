//! [`ToolProbe`] over this Mac: git, `gh`, the agent presets, Xcode, the Android SDK, a JDK and
//! Node, looked up with the login shell's `PATH`; and an agent's `--version`.

use std::ffi::OsStr;
use std::io::Read as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use hangar_agents::find_program;
use hangar_core::ports::{CodeHost, ToolProbe};
use hangar_core::review::{GhStatus, ReviewError};
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
    /// The slow probes run side by side, each with a deadline, so one hung tool neither holds
    /// the others nor the checklist.
    fn probe(&self) -> ToolFacts {
        let found = |program: &str| find_program(program, Some(OsStr::new(&self.path)));
        let (git, gh, xcode, android, jdk) = std::thread::scope(|scope| {
            let git = scope.spawn(git);
            let gh = scope.spawn(|| self.gh.status());
            let xcode = scope.spawn(xcode);
            let android = scope.spawn(android);
            let jdk = scope.spawn(java_home);
            (
                git.join().unwrap_or(GitFact::Missing),
                gh.join().unwrap_or(Ok(GhStatus::Missing)),
                xcode.join().unwrap_or(Err(Late)),
                android.join().unwrap_or(Err(Late)),
                jdk.join().unwrap_or(Err(Late)),
            )
        });
        let mut not_answering = Vec::new();
        let gh = match gh {
            Ok(status) => status,
            Err(ReviewError::GhNotAnswering) => {
                not_answering.push("gh".to_owned());
                GhStatus::Missing
            }
            Err(err) => {
                tracing::warn!(%err, "gh status unknown");
                GhStatus::Missing
            }
        };
        let xcode = or_late(
            xcode,
            "xcode-select",
            XcodeFact::Missing,
            &mut not_answering,
        );
        let android = or_late(
            android,
            "emulator",
            AndroidFact::Missing,
            &mut not_answering,
        );
        let jdk = or_late(jdk, "java_home", None, &mut not_answering);
        ToolFacts {
            path_error: self.path_error.clone(),
            not_answering,
            homebrew: found("brew").is_some(),
            git,
            gh,
            agents: agents::all_presets()
                .into_iter()
                .filter(|p| p.id() != FALLBACK_AGENT)
                .map(|preset| agent(&preset, found(preset.program())))
                .collect(),
            xcode,
            android,
            jdk,
            node: found("node"),
        }
    }
}

/// A probe's tool did not answer before its deadline (or the probe panicked).
struct Late;

fn or_late<T>(probed: Result<T, Late>, tool: &str, fallback: T, late: &mut Vec<String>) -> T {
    probed.unwrap_or_else(|Late| {
        late.push(tool.to_owned());
        fallback
    })
}

/// How long a quick query may take before it is killed.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

fn git() -> GitFact {
    match crate::adapters::vcs::git_version() {
        Ok(version) => {
            let (name, email) = crate::adapters::vcs::identity().unwrap_or_else(|err| {
                tracing::warn!(%err, "cannot read the git identity");
                (None, None)
            });
            GitFact::Installed {
                version: version.to_string(),
                name,
                email,
            }
        }
        Err(crate::adapters::vcs::Error::GitTooOld { found, required }) => GitFact::TooOld {
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
fn xcode() -> Result<XcodeFact, Late> {
    use crate::adapters::devices::{ios, simctl};
    let installed = Path::new(XCODE_APP).is_dir();
    let dir = match ios::developer_dir() {
        Ok(dir) => dir,
        Err(crate::adapters::devices::Error::Timeout(_)) => return Err(Late),
        Err(_) => return Ok(XcodeFact::Missing),
    };
    let Some((app, _)) = dir.split_once(".app/") else {
        // The Command Line Tools: selecting Xcode only helps if it is there.
        return Ok(if installed {
            XcodeFact::CommandLineToolsOnly
        } else {
            XcodeFact::Missing
        });
    };
    Ok(XcodeFact::Installed {
        app: PathBuf::from(format!("{app}.app")),
        first_launch_done: ios::first_launch_done(),
        ios_runtimes: simctl::ios_runtime_count().unwrap_or_else(|err| {
            tracing::warn!(%err, "cannot list iOS runtimes");
            0
        }),
    })
}

#[cfg(not(target_os = "macos"))]
fn xcode() -> Result<XcodeFact, Late> {
    Ok(XcodeFact::Missing)
}

fn android() -> Result<AndroidFact, Late> {
    let Ok(sdk) = crate::adapters::devices::android::AndroidSdk::locate() else {
        return Ok(AndroidFact::Missing);
    };
    let avds = match crate::adapters::devices::android::list_avds(&sdk) {
        Ok(avds) => avds.len(),
        Err(crate::adapters::devices::Error::Timeout(_)) => return Err(Late),
        Err(err) => {
            tracing::warn!(%err, "cannot list AVDs");
            0
        }
    };
    Ok(AndroidFact::Installed {
        sdk: sdk.root,
        avds,
    })
}

/// The default JDK's home, as `/usr/libexec/java_home` reports it.
fn java_home() -> Result<Option<PathBuf>, Late> {
    let Some(out) = output_within(Command::new("/usr/libexec/java_home"), PROBE_TIMEOUT)? else {
        return Ok(None);
    };
    let home = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    Ok((out.status.success() && !home.is_empty()).then(|| PathBuf::from(home)))
}

/// Runs `command`, killing it and what it started after `timeout`. `Ok(None)` when it cannot
/// start.
fn output_within(mut command: Command, timeout: Duration) -> Result<Option<Output>, Late> {
    let Ok(mut child) = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
    else {
        return Ok(None);
    };
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        if let Some(pipe) = stdout.as_mut() {
            let _ = std::io::Read::read_to_end(pipe, &mut out);
        }
        out
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = reader.join().unwrap_or_default();
                return Ok(Some(Output {
                    status,
                    stdout,
                    stderr: Vec::new(),
                }));
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                if let Ok(pgid) = libc::pid_t::try_from(child.id()) {
                    // SAFETY: plain syscall on a group this process created and has not reaped.
                    unsafe { libc::killpg(pgid, libc::SIGKILL) };
                }
                let _ = child.wait();
                return Err(Late);
            }
        }
    }
}

/// How long `<agent> --version` may take before it is given up on.
const VERSION_TIMEOUT: Duration = Duration::from_secs(3);

/// The first version-looking word of `--version` output (`2.1.4 (Claude Code)` → `2.1.4`,
/// `zsh 5.9 (arm64-apple-darwin24.0)` → `5.9`, `v0.3.1` → `0.3.1`).
fn parse_version(output: &str) -> Option<String> {
    output
        .lines()
        .find(|l| !l.trim().is_empty())?
        .split_whitespace()
        .find_map(|word| {
            let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
            let word = word.strip_prefix('v').unwrap_or(word);
            (word.starts_with(|c: char| c.is_ascii_digit()) && word.contains('.'))
                .then(|| word.to_owned())
        })
}

/// Runs `<program> --version` with `path` as `PATH`; `None` when it fails, prints no version or
/// outlasts [`VERSION_TIMEOUT`]. Blocking: call it off the UI thread.
pub fn program_version(program: &Path, path: Option<&str>) -> Option<String> {
    let mut command = Command::new(program);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(path) = path {
        command.env("PATH", path);
    }
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("agent-version".into())
        .spawn(move || {
            let mut out = String::new();
            let _ = stdout.read_to_string(&mut out);
            let _ = tx.send(out);
        })
        .ok()?;
    let out = rx.recv_timeout(VERSION_TIMEOUT).ok();
    let _ = child.kill();
    let _ = child.wait();
    parse_version(&out?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hung_probe_is_late_and_killed() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 60"]);
        let started = Instant::now();
        assert!(output_within(command, Duration::from_millis(200)).is_err());
        assert!(started.elapsed() < Duration::from_secs(5));

        let mut command = Command::new("/bin/sh");
        command.args(["-c", "echo /jdk"]);
        let out = output_within(command, Duration::from_secs(5))
            .ok()
            .flatten()
            .unwrap();
        assert_eq!(out.stdout, b"/jdk\n");
        assert!(
            output_within(Command::new("/no/such/tool"), PROBE_TIMEOUT).is_ok_and(|o| o.is_none())
        );
    }

    #[test]
    fn versions() {
        assert_eq!(
            parse_version("2.1.4 (Claude Code)\n").as_deref(),
            Some("2.1.4")
        );
        assert_eq!(
            parse_version("zsh 5.9 (arm64-apple-darwin24.0)").as_deref(),
            Some("5.9")
        );
        assert_eq!(parse_version("\nv0.3.1").as_deref(), Some("0.3.1"));
        assert_eq!(parse_version("codex-cli 0.46.0").as_deref(), Some("0.46.0"));
        assert_eq!(parse_version("usage: foo [options]"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn a_program_version_is_read_and_a_missing_program_has_none() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("tool");
        std::fs::write(&tool, "#!/bin/sh\necho \"tool v1.2.3 ($PATH)\"\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            program_version(&tool, Some("/usr/bin:/bin")).as_deref(),
            Some("1.2.3")
        );
        assert_eq!(program_version(&dir.path().join("missing"), None), None);
    }
}
