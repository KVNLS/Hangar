//! The login shell's `PATH`. Apps launched from Finder inherit launchd's minimal `PATH`, so
//! agents installed by Homebrew, npm, mise etc. would not be found without this.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};

pub const TIMEOUT: Duration = Duration::from_secs(5);

// Markers keep the value apart from anything the user's profile prints.
const START: &str = "__HANGAR_PATH_START__";
const END: &str = "__HANGAR_PATH_END__";

/// Where the effective `PATH` came from, for the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathSource {
    LoginShell,
    /// The login shell could not be queried; the reason is kept for display.
    Process(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPath {
    pub value: String,
    pub source: PathSource,
}

/// Login shell `PATH`, else this process's. Blocks up to [`TIMEOUT`]; call off the UI thread.
pub fn resolve() -> ResolvedPath {
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/zsh".to_owned());
    match login_shell_path(Path::new(&shell), TIMEOUT) {
        Ok(value) => ResolvedPath {
            value,
            source: PathSource::LoginShell,
        },
        Err(err) => {
            tracing::warn!("using process PATH: {err:#}");
            ResolvedPath {
                value: std::env::var("PATH").unwrap_or_default(),
                source: PathSource::Process(format!("{err:#}")),
            }
        }
    }
}

pub fn login_shell_path(shell: &Path, timeout: Duration) -> anyhow::Result<String> {
    let script = format!(r#"printf '%s%s%s' '{START}' "$PATH" '{END}'"#);
    let mut child = Command::new(shell)
        .args(["-l", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to start {}", shell.display()))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("no stdout pipe"))?;

    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("login-path".into())
        .spawn(move || {
            let mut out = String::new();
            let read = stdout.read_to_string(&mut out).map(|_| out);
            let _ = tx.send(read);
        })
        .context("failed to start reader thread")?;

    let out = match rx.recv_timeout(timeout) {
        Ok(read) => read.context("failed to read login shell output")?,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "{} -l did not answer within {}s",
                shell.display(),
                timeout.as_secs()
            );
        }
    };
    let status = child.wait().context("failed to wait for login shell")?;
    parse_marked_path(&out)
        .ok_or_else(|| anyhow!("{} -l printed no PATH ({status})", shell.display()))
}

/// Extracts the value between the markers; `None` if missing or empty.
pub fn parse_marked_path(output: &str) -> Option<String> {
    let start = output.rfind(START)? + START.len();
    let len = output[start..].find(END)?;
    let value = output[start..start + len].trim();
    (!value.is_empty()).then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{END, START, login_shell_path, parse_marked_path};
    use std::path::Path;
    use std::time::Duration;

    #[test]
    fn parses_value_between_markers() {
        let out = format!("{START}/usr/bin:/opt/homebrew/bin{END}");
        assert_eq!(
            parse_marked_path(&out).as_deref(),
            Some("/usr/bin:/opt/homebrew/bin")
        );
    }

    #[test]
    fn ignores_profile_noise() {
        let out = format!("Welcome!\nlast login\n{START}/a b/bin:/c{END}\ntrailing");
        assert_eq!(parse_marked_path(&out).as_deref(), Some("/a b/bin:/c"));
    }

    #[test]
    fn rejects_missing_or_empty() {
        assert_eq!(parse_marked_path("no markers"), None);
        assert_eq!(parse_marked_path(&format!("{START}{END}")), None);
        assert_eq!(parse_marked_path(&format!("{START}/usr/bin")), None);
    }

    #[test]
    fn sh_login_path_is_non_empty() {
        let path = login_shell_path(Path::new("/bin/sh"), Duration::from_secs(10)).unwrap();
        assert!(path.contains("/usr/bin") || path.contains("/bin"), "{path}");
    }

    #[test]
    fn hanging_shell_times_out() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("slow-shell");
        std::fs::write(&script, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = std::time::Instant::now();
        let err = login_shell_path(&script, Duration::from_millis(300)).unwrap_err();
        assert!(err.to_string().contains("did not answer"), "{err:#}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn missing_shell_is_an_error() {
        assert!(login_shell_path(Path::new("/nonexistent/shell"), Duration::from_secs(1)).is_err());
    }
}
