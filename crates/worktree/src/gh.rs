//! Thin wrapper over the GitHub CLI (`gh`). Never prompts (`GH_PROMPT_DISABLED`), never asks
//! for or prints tokens. Blocking: run off the UI thread.

use std::ffi::{OsStr, OsString};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use hangar_core::review::{CreatePr, GhStatus, PrInfo};

use crate::error::{Error, Result};

/// How to run `gh`: the program and, optionally, the `PATH` it is looked up in and runs with
/// (a GUI app's own PATH usually lacks Homebrew).
#[derive(Clone, Debug)]
pub struct Gh {
    program: PathBuf,
    path_env: Option<OsString>,
}

impl Default for Gh {
    fn default() -> Self {
        Self {
            program: PathBuf::from("gh"),
            path_env: None,
        }
    }
}

impl Gh {
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs `gh` resolved in (and with) this `PATH`.
    pub fn with_path_env(mut self, path: impl Into<OsString>) -> Self {
        self.path_env = Some(path.into());
        self
    }

    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = program.into();
        self
    }

    fn exec(&self, dir: Option<&Path>, args: &[&OsStr]) -> Result<Output> {
        let mut command = Command::new(&self.program);
        command
            .args(args)
            .env("GH_PROMPT_DISABLED", "1")
            .env("NO_COLOR", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("GH_SPINNER_DISABLED", "1")
            .env("CLICOLOR", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(path) = &self.path_env {
            command.env("PATH", path);
        }
        if let Some(dir) = dir {
            command.current_dir(dir);
        }
        let output = command.output().map_err(|e| match e.kind() {
            ErrorKind::NotFound => Error::GhMissing,
            _ => Error::Io(e),
        })?;
        tracing::trace!(?args, status = %output.status, "gh");
        Ok(output)
    }

    fn failure(args: &[&OsStr], out: &Output) -> Error {
        Error::Gh {
            args: args
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
            status: out.status,
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        }
    }

    /// Whether `gh` is installed and logged in (`gh auth status`, output never logged).
    pub fn status(&self) -> Result<GhStatus> {
        let out = match self.exec(None, &[OsStr::new("auth"), OsStr::new("status")]) {
            Ok(out) => out,
            Err(Error::GhMissing) => return Ok(GhStatus::Missing),
            Err(err) => return Err(err),
        };
        // Older gh prints the report on stderr, newer on stdout.
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(GhStatus::Installed {
            authenticated: out.status.success(),
            account: parse_account(&text),
        })
    }

    /// The pull request whose head is `branch`, or `None` if there is none.
    pub fn pr_for_branch(&self, worktree: &Path, branch: &str) -> Result<Option<PrInfo>> {
        let args = [
            OsStr::new("pr"),
            OsStr::new("view"),
            OsStr::new(branch),
            OsStr::new("--json"),
            OsStr::new("number,url,state,title"),
        ];
        let out = self.exec(Some(worktree), &args)?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("no pull requests found") {
                return Ok(None);
            }
            return Err(Self::failure(&args, &out));
        }
        parse_pr(&out.stdout).map(Some)
    }

    /// Opens a pull request and returns its URL.
    pub fn create_pr(&self, worktree: &Path, pr: &CreatePr) -> Result<String> {
        let mut args = vec![
            OsStr::new("pr"),
            OsStr::new("create"),
            OsStr::new("--base"),
            OsStr::new(&pr.base),
            OsStr::new("--head"),
            OsStr::new(&pr.head),
            OsStr::new("--title"),
            OsStr::new(&pr.title),
            OsStr::new("--body"),
            OsStr::new(&pr.body),
        ];
        if pr.draft {
            args.push(OsStr::new("--draft"));
        }
        let out = self.exec(Some(worktree), &args)?;
        if !out.status.success() {
            return Err(Self::failure(&args, &out));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        stdout
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| l.starts_with("https://") || l.starts_with("http://"))
            .map(str::to_owned)
            .ok_or_else(|| Error::Parse(format!("gh pr create printed no URL: {stdout:?}")))
    }
}

/// [`Gh::status`] with `gh` from the process's own PATH.
pub fn gh_available() -> Result<GhStatus> {
    Gh::default().status()
}

/// Reads the account from `gh auth status`: `Logged in to github.com account NAME (...)` or
/// the older `Logged in to github.com as NAME (...)`.
pub(crate) fn parse_account(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let rest = line.split_once("Logged in to ")?.1;
        let rest = rest
            .split_once(" account ")
            .or_else(|| rest.split_once(" as "))?
            .1;
        let name = rest.split_whitespace().next()?;
        Some(name.to_owned())
    })
}

pub(crate) fn parse_pr(json: &[u8]) -> Result<PrInfo> {
    let value: serde_json::Value =
        serde_json::from_slice(json).map_err(|e| Error::Parse(format!("gh pr view JSON: {e}")))?;
    let field = |name: &str| {
        value
            .get(name)
            .ok_or_else(|| Error::Parse(format!("gh pr view JSON lacks `{name}`")))
    };
    let text = |name: &str| -> Result<String> {
        field(name)?
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| Error::Parse(format!("gh pr view `{name}` is not a string")))
    };
    Ok(PrInfo {
        number: field("number")?
            .as_u64()
            .ok_or_else(|| Error::Parse("gh pr view `number` is not a number".into()))?,
        url: text("url")?,
        state: text("state")?,
        title: text("title")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_accounts() {
        let new = "github.com\n  ✓ Logged in to github.com account octo-cat (keyring)\n  - Token: gho_****";
        assert_eq!(parse_account(new).as_deref(), Some("octo-cat"));
        let old = "github.com\n  ✓ Logged in to github.com as octo (oauth_token)\n";
        assert_eq!(parse_account(old).as_deref(), Some("octo"));
        assert_eq!(
            parse_account("You are not logged into any GitHub hosts."),
            None
        );
    }

    #[test]
    fn parses_pr_json() {
        let pr = parse_pr(br#"{"number":12,"url":"https://x/pull/12","state":"OPEN","title":"T"}"#)
            .unwrap();
        assert_eq!(pr.number, 12);
        assert_eq!(pr.state, "OPEN");
        assert!(parse_pr(br#"{"number":"x"}"#).is_err());
        assert!(parse_pr(b"nope").is_err());
    }
}
