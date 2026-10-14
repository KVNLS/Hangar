use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use crate::error::{Error, Result};

/// Oldest git accepted: `worktree list -z` and `status --porcelain=v2 -z` behave as parsed here.
pub const MIN_GIT_VERSION: GitVersion = GitVersion {
    major: 2,
    minor: 38,
    patch: 0,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl fmt::Display for GitVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl GitVersion {
    /// Parses `git version 2.39.3 (Apple Git-146)` or `git version 2.45.1.windows.1`.
    pub fn parse(output: &str) -> Result<Self> {
        let raw = output
            .trim()
            .strip_prefix("git version ")
            .ok_or_else(|| Error::Parse(format!("git --version: {output:?}")))?;
        let mut parts = raw
            .split(|c: char| !c.is_ascii_digit())
            .map(str::parse::<u32>);
        let mut next = || parts.next().and_then(|p| p.ok());
        let (Some(major), Some(minor)) = (next(), next()) else {
            return Err(Error::Parse(format!("git --version: {output:?}")));
        };
        Ok(Self {
            major,
            minor,
            patch: next().unwrap_or(0),
        })
    }
}

/// Returns the installed git version, or [`Error::GitTooOld`] below [`MIN_GIT_VERSION`].
pub fn git_version() -> Result<GitVersion> {
    let out = Git::new(".").run(["--version"])?;
    let found = GitVersion::parse(&out)?;
    if found < MIN_GIT_VERSION {
        return Err(Error::GitTooOld {
            found,
            required: MIN_GIT_VERSION,
        });
    }
    Ok(found)
}

/// Synchronous runner for `git -C <dir> ...`. Blocks: call it from a background thread.
#[derive(Clone, Debug)]
pub struct Git {
    dir: PathBuf,
}

impl Git {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Runs git and returns stdout as UTF-8 (lossy).
    pub fn run<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run_bytes(args)
            .map(|out| String::from_utf8_lossy(&out).into_owned())
    }

    /// Runs git and returns raw stdout, for `-z` output whose paths may not be UTF-8.
    pub fn run_bytes<I, S>(&self, args: I) -> Result<Vec<u8>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let out = self.exec(args)?;
        if out.status.success() {
            return Ok(out.stdout);
        }
        Err(self.failure(out))
    }

    /// Runs a yes/no query such as `rev-parse --verify --quiet`: stdout on success, `None` when
    /// git exits with status 1, an error otherwise (e.g. 128 for a missing directory).
    pub fn query<I, S>(&self, args: I) -> Result<Option<String>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let out = self.exec(args)?;
        if out.status.success() {
            return Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()));
        }
        if out.status.code() == Some(1) {
            return Ok(None);
        }
        Err(self.failure(out))
    }

    /// Runs git feeding `input` on stdin; succeeds for any exit code in `ok_codes` (0 is implied),
    /// e.g. 1 for `diff --no-index` with differences or `check-ignore` with no match.
    pub fn run_bytes_with<I, S>(
        &self,
        args: I,
        input: Option<&[u8]>,
        ok_codes: &[i32],
    ) -> Result<Vec<u8>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let out = self.exec_with(args, input)?;
        let code = out.status.code();
        if out.status.success() || code.is_some_and(|c| ok_codes.contains(&c)) {
            return Ok(out.stdout);
        }
        Err(self.failure(out))
    }

    /// Runs git and returns its exit status, stdout and stderr whatever the exit code, for
    /// commands whose failures are parsed (push rejections, commit hooks, merge conflicts).
    pub(crate) fn output<I, S>(&self, args: I, input: Option<&[u8]>) -> Result<RawOutput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.exec_with(args, input)
    }

    fn exec<I, S>(&self, args: I) -> Result<RawOutput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.exec_with(args, None)
    }

    fn exec_with<I, S>(&self, args: I, input: Option<&[u8]>) -> Result<RawOutput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args: Vec<OsString> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(&self.dir)
            .args(&args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("LC_ALL", "C")
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let not_found = |e: io::Error| match e.kind() {
            ErrorKind::NotFound => Error::GitMissing,
            _ => Error::Io(e),
        };
        let mut child = command.spawn().map_err(not_found)?;
        // Write stdin from another thread so a full stdout pipe cannot deadlock us.
        let writer = match (input, child.stdin.take()) {
            (Some(input), Some(mut stdin)) => {
                let input = input.to_vec();
                Some(std::thread::spawn(move || stdin.write_all(&input)))
            }
            _ => None,
        };
        let output = child.wait_with_output().map_err(Error::Io)?;
        if let Some(writer) = writer {
            match writer.join() {
                Ok(Ok(())) => {}
                // git may exit before reading everything; its status tells the real story.
                Ok(Err(e)) if e.kind() == ErrorKind::BrokenPipe => {}
                Ok(Err(e)) => return Err(Error::Io(e)),
                Err(_) => return Err(Error::Io(io::Error::other("stdin writer panicked"))),
            }
        }
        tracing::trace!(dir = %self.dir.display(), ?args, status = %output.status, "git");
        Ok(RawOutput {
            args,
            status: output.status,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    pub(crate) fn failure(&self, out: RawOutput) -> Error {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        if stderr.contains("not a git repository") {
            return Error::NotARepo(self.dir.clone());
        }
        Error::Git {
            args: out
                .args
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
            status: out.status,
            stderr,
        }
    }
}

pub(crate) struct RawOutput {
    pub args: Vec<OsString>,
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl RawOutput {
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_owned()
    }
}

/// Converts a path emitted by git (`-z` output, unquoted) to a `PathBuf`.
pub(crate) fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(OsStr::from_bytes(bytes))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

pub(crate) fn bytes_to_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_version_strings() {
        assert_eq!(
            GitVersion::parse("git version 2.39.3 (Apple Git-146)\n").unwrap(),
            GitVersion {
                major: 2,
                minor: 39,
                patch: 3
            }
        );
        assert_eq!(
            GitVersion::parse("git version 2.45.1.windows.1").unwrap(),
            GitVersion {
                major: 2,
                minor: 45,
                patch: 1
            }
        );
        assert_eq!(
            GitVersion::parse("git version 3.0").unwrap(),
            GitVersion {
                major: 3,
                minor: 0,
                patch: 0
            }
        );
        assert!(GitVersion::parse("hub version 1").is_err());
        assert!(GitVersion::parse("git version x").is_err());
        assert!(
            GitVersion::parse("git version 2.37.9").unwrap() < MIN_GIT_VERSION
                && MIN_GIT_VERSION <= GitVersion::parse("git version 2.38.0").unwrap()
        );
    }

    #[test]
    fn installed_git_is_supported() {
        assert!(git_version().unwrap() >= MIN_GIT_VERSION);
    }
}
