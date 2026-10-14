//! Short-lived tool processes with a deadline: a hung tool (an Xcode first-launch prompt, an
//! emulator waiting on a lock) must not hold a caller or outlive the app.

use std::io::Read as _;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::{Error, Result};

/// How long a quick query (a version, a list) may take before it is killed.
pub(crate) const QUERY_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs `command` to completion, or kills it and everything it started once `timeout` passes.
pub(crate) fn output_within(mut command: Command, timeout: Duration) -> Result<Output> {
    let what = format!("{command:?}");
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Its own group, so the kill reaches what it started.
        .process_group(0)
        .spawn()?;
    let read = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut out);
            }
            out
        })
    };
    let stdout = read(child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = read(child.stderr.take().map(|p| Box::new(p) as _));
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            if let Ok(pgid) = libc::pid_t::try_from(child.id()) {
                // SAFETY: plain syscall on a group this process created and has not reaped.
                unsafe { libc::killpg(pgid, libc::SIGKILL) };
            }
            let _ = child.wait();
            return Err(Error::Timeout(what));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Ok(Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_collected() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "echo out; echo err >&2"]);
        let out = output_within(command, Duration::from_secs(5)).unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"out\n");
        assert_eq!(out.stderr, b"err\n");
    }

    #[test]
    fn a_hung_tool_and_its_children_are_killed() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("child.pid");
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            &format!("sleep 60 & echo $! > {}; wait", pid_file.display()),
        ]);
        let started = Instant::now();
        let err = output_within(command, Duration::from_millis(300)).unwrap_err();
        assert!(matches!(err, Error::Timeout(_)), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));
        let pid: libc::pid_t = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        // SAFETY: signal 0 only checks that the process exists.
        assert_ne!(unsafe { libc::kill(pid, 0) }, 0, "grandchild survived");
    }
}
