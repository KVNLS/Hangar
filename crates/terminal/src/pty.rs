use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{
    Child, ChildKiller, CommandBuilder, MasterPty, PtySize, SlavePty, native_pty_system,
};

use crate::error::{Result, TerminalError};
use crate::lock;
use crate::terminal::GridSize;

const READ_BUFFER: usize = 64 * 1024;
/// Bounded so a program flooding output is throttled by the consumer instead of growing memory.
const OUTPUT_QUEUE: usize = 64;

/// What to run inside the pseudo-terminal.
#[derive(Clone, Debug, Default)]
pub struct SpawnOptions {
    /// Program to run. `None` runs the user's `$SHELL` as a login shell.
    pub program: Option<OsString>,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    /// Extra environment on top of the inherited one.
    pub env: Vec<(OsString, OsString)>,
    pub size: GridSize,
    /// Cell size in pixels, reported to programs via `TIOCGWINSZ` (0 if unknown).
    pub cell_px: (u16, u16),
}

impl SpawnOptions {
    pub fn command(
        program: impl Into<OsString>,
        args: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        Self {
            program: Some(program.into()),
            args: args.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }

    fn display_name(&self) -> String {
        self.program
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "$SHELL".to_owned())
    }
}

/// Output of the PTY reader thread.
#[derive(Debug)]
pub enum PtyEvent {
    Output(Vec<u8>),
    /// The PTY reached end-of-file (all writers to the slave side closed).
    Closed,
}

/// How the child process ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitInfo {
    pub code: u32,
    pub signal: Option<String>,
}

impl ExitInfo {
    pub fn success(&self) -> bool {
        self.code == 0 && self.signal.is_none()
    }
}

impl From<portable_pty::ExitStatus> for ExitInfo {
    fn from(status: portable_pty::ExitStatus) -> Self {
        Self {
            code: status.exit_code(),
            signal: status.signal().map(str::to_owned),
        }
    }
}

/// Non-blocking handle for writing to the PTY. Writes are queued to a dedicated thread so a
/// program that stops reading its input can never block the caller.
#[derive(Clone)]
pub struct PtyWriter {
    tx: Sender<Vec<u8>>,
}

impl PtyWriter {
    pub fn write(&self, bytes: impl Into<Vec<u8>>) -> Result<()> {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return Ok(());
        }
        self.tx.send(bytes).map_err(|_| {
            TerminalError::Write(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "PTY writer thread has stopped",
            ))
        })
    }
}

type SharedMaster = Arc<Mutex<Box<dyn MasterPty + Send>>>;

#[derive(Default)]
struct ExitSlot {
    status: Mutex<Option<std::result::Result<ExitInfo, String>>>,
    cond: Condvar,
}

/// A child process attached to a pseudo-terminal.
pub struct PtySession {
    master: SharedMaster,
    exit: Arc<ExitSlot>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    writer: PtyWriter,
    output: Mutex<Option<Receiver<PtyEvent>>>,
    pid: Option<u32>,
}

impl PtySession {
    pub fn spawn(options: &SpawnOptions) -> Result<Self> {
        let pty = native_pty_system()
            .openpty(pty_size(options.size, options.cell_px))
            .map_err(|e| TerminalError::OpenPty(format!("{e:#}")))?;

        let mut cmd = match &options.program {
            Some(program) => {
                let mut cmd = CommandBuilder::new(program);
                cmd.args(&options.args);
                cmd
            }
            None => CommandBuilder::new_default_prog(),
        };
        let cwd = match &options.cwd {
            Some(cwd) => Some(cwd.clone()),
            None => std::env::current_dir().ok(),
        };
        if let Some(cwd) = cwd {
            cmd.cwd(cwd);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "Hangar");
        for (key, value) in &options.env {
            cmd.env(key, value);
        }

        let child = pty
            .slave
            .spawn_command(cmd)
            .map_err(|e| TerminalError::Spawn {
                program: options.display_name(),
                reason: format!("{e:#}"),
            })?;
        let reader = pty
            .master
            .try_clone_reader()
            .map_err(|e| TerminalError::PtyIo(format!("{e:#}")))?;
        let writer = pty
            .master
            .take_writer()
            .map_err(|e| TerminalError::PtyIo(format!("{e:#}")))?;

        let (out_tx, out_rx) = mpsc::sync_channel(OUTPUT_QUEUE);
        thread::Builder::new()
            .name("hangar-pty-reader".into())
            .spawn(move || read_loop(reader, out_tx))
            .map_err(|e| TerminalError::Thread("PTY reader", e))?;

        let (in_tx, in_rx) = mpsc::channel::<Vec<u8>>();
        thread::Builder::new()
            .name("hangar-pty-writer".into())
            .spawn(move || write_loop(writer, in_rx))
            .map_err(|e| TerminalError::Thread("PTY writer", e))?;

        let master: SharedMaster = Arc::new(Mutex::new(pty.master));
        let exit = Arc::new(ExitSlot::default());
        let pid = child.process_id();
        let killer = child.clone_killer();
        {
            let master = Arc::clone(&master);
            let exit = Arc::clone(&exit);
            let slave = pty.slave;
            thread::Builder::new()
                .name("hangar-pty-waiter".into())
                .spawn(move || wait_loop(child, slave, &master, &exit))
                .map_err(|e| TerminalError::Thread("PTY waiter", e))?;
        }

        Ok(Self {
            pid,
            killer: Mutex::new(killer),
            exit,
            master,
            writer: PtyWriter { tx: in_tx },
            output: Mutex::new(Some(out_rx)),
        })
    }

    /// Takes the receiver of program output. Returns `None` after the first call.
    pub fn take_output(&self) -> Option<Receiver<PtyEvent>> {
        lock(&self.output).take()
    }

    pub fn writer(&self) -> PtyWriter {
        self.writer.clone()
    }

    pub fn write(&self, bytes: impl Into<Vec<u8>>) -> Result<()> {
        self.writer.write(bytes)
    }

    pub fn resize(&self, size: GridSize, cell_px: (u16, u16)) -> Result<()> {
        lock(&self.master)
            .resize(pty_size(size, cell_px))
            .map_err(|e| TerminalError::Resize(format!("{e:#}")))
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Non-blocking exit check.
    pub fn try_wait(&self) -> Result<Option<ExitInfo>> {
        lock(&self.exit.status)
            .clone()
            .transpose()
            .map_err(TerminalError::Wait)
    }

    /// Blocks until the child exits.
    pub fn wait(&self) -> Result<ExitInfo> {
        let guard = lock(&self.exit.status);
        let guard = self
            .exit
            .cond
            .wait_while(guard, |status| status.is_none())
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match guard.clone() {
            Some(status) => status.map_err(TerminalError::Wait),
            None => Err(TerminalError::Wait("exit status missing".to_owned())),
        }
    }

    pub fn kill(&self) -> Result<()> {
        lock(&self.killer).kill().map_err(TerminalError::Kill)
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        if matches!(self.try_wait(), Ok(None))
            && let Err(err) = self.kill()
        {
            tracing::debug!(%err, "failed to kill PTY child on drop");
        }
    }
}

fn pty_size(size: GridSize, cell_px: (u16, u16)) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: size.cols.saturating_mul(cell_px.0),
        pixel_height: size.rows.saturating_mul(cell_px.1),
    }
}

/// Waits for the child, then closes our copy of the slave once its output has been read.
///
/// The slave stays open in this process while the child runs because macOS discards unread
/// PTY output when the last slave descriptor closes; a fast-exiting child (e.g. `echo`)
/// would otherwise lose its output. Closing it after the drain makes the reader see EOF.
fn wait_loop(
    mut child: Box<dyn Child + Send + Sync>,
    slave: Box<dyn SlavePty + Send>,
    master: &SharedMaster,
    exit: &ExitSlot,
) {
    let status = child.wait().map(ExitInfo::from).map_err(|e| e.to_string());
    *lock(&exit.status) = Some(status);
    exit.cond.notify_all();

    let deadline = Instant::now() + DRAIN_TIMEOUT;
    while pending_output(master) > 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(1));
    }
    drop(slave);
}

const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(unix)]
fn pending_output(master: &SharedMaster) -> usize {
    let master = lock(master);
    let Some(fd) = master.as_raw_fd() else {
        return 0;
    };
    let mut pending: libc::c_int = 0;
    // SAFETY: `fd` belongs to the PTY master, which the held guard keeps open for the whole
    // call; FIONREAD writes a single c_int into `pending`.
    let rc = unsafe { libc::ioctl(fd, libc::FIONREAD, &mut pending) };
    if rc == 0 {
        usize::try_from(pending).unwrap_or(0)
    } else {
        0
    }
}

#[cfg(not(unix))]
fn pending_output(_master: &SharedMaster) -> usize {
    0
}

fn read_loop(mut reader: Box<dyn Read + Send>, tx: SyncSender<PtyEvent>) {
    let mut buf = vec![0u8; READ_BUFFER];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if tx.send(PtyEvent::Output(buf[..n].to_vec())).is_err() {
                    return;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            // macOS reports EIO once the child side is gone; treat every error as end of stream.
            Err(e) => {
                tracing::debug!(err = %e, "PTY read ended");
                break;
            }
        }
    }
    let _ = tx.send(PtyEvent::Closed);
}

fn write_loop(mut writer: Box<dyn Write + Send>, rx: Receiver<Vec<u8>>) {
    while let Ok(bytes) = rx.recv() {
        if let Err(err) = writer.write_all(&bytes).and_then(|()| writer.flush()) {
            tracing::debug!(%err, "PTY write failed; stopping writer");
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn collect_output(rx: &Receiver<PtyEvent>, timeout: Duration) -> Vec<u8> {
        let deadline = Instant::now() + timeout;
        let mut out = Vec::new();
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(PtyEvent::Output(bytes)) => out.extend(bytes),
                Ok(PtyEvent::Closed) | Err(_) => break,
            }
        }
        out
    }

    #[test]
    fn echo_output_and_exit_status() {
        let session = PtySession::spawn(&SpawnOptions::command("/bin/echo", ["hello"])).unwrap();
        let rx = session.take_output().unwrap();
        assert!(session.take_output().is_none());
        let out = collect_output(&rx, Duration::from_secs(10));
        assert_eq!(String::from_utf8_lossy(&out).trim_end(), "hello");
        let exit = session.wait().unwrap();
        assert!(exit.success(), "{exit:?}");
    }

    #[test]
    fn environment_and_size_are_applied() {
        let mut opts = SpawnOptions::command(
            "/bin/sh",
            ["-c", "echo $TERM $COLORTERM $HANGAR_TEST; stty size"],
        );
        opts.env.push(("HANGAR_TEST".into(), "yes".into()));
        opts.size = GridSize::new(100, 30).unwrap();
        let session = PtySession::spawn(&opts).unwrap();
        let rx = session.take_output().unwrap();
        let out =
            String::from_utf8_lossy(&collect_output(&rx, Duration::from_secs(10))).into_owned();
        assert!(out.contains("xterm-256color truecolor yes"), "{out:?}");
        assert!(out.contains("30 100"), "{out:?}");
    }

    #[test]
    fn input_is_written_and_resize_is_seen() {
        let session = PtySession::spawn(&SpawnOptions::command(
            "/bin/sh",
            ["-c", "read line; echo got:$line; stty size"],
        ))
        .unwrap();
        let rx = session.take_output().unwrap();
        session
            .resize(GridSize::new(70, 11).unwrap(), (0, 0))
            .unwrap();
        session.write(b"ping\r".to_vec()).unwrap();
        let out =
            String::from_utf8_lossy(&collect_output(&rx, Duration::from_secs(10))).into_owned();
        assert!(out.contains("got:ping"), "{out:?}");
        assert!(out.contains("11 70"), "{out:?}");
    }

    #[test]
    fn non_zero_exit_is_reported() {
        let session =
            PtySession::spawn(&SpawnOptions::command("/bin/sh", ["-c", "exit 3"])).unwrap();
        let rx = session.take_output().unwrap();
        collect_output(&rx, Duration::from_secs(10));
        assert_eq!(session.wait().unwrap().code, 3);
    }

    #[test]
    fn missing_program_is_an_error() {
        let result = PtySession::spawn(&SpawnOptions::command(
            "/nonexistent/hangar-binary",
            Vec::<String>::new(),
        ));
        assert!(matches!(result, Err(TerminalError::Spawn { .. })));
    }
}
