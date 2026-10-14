use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::{Result, TerminalError};
use crate::lock;
use crate::pty::{ExitInfo, PtyEvent, PtySession, PtyWriter, SpawnOptions};
use crate::terminal::{GridSize, Terminal, TerminalConfig};

/// Upper bound of output parsed per lock acquisition, so a flood cannot starve the renderer.
const MAX_BATCH: usize = 256 * 1024;

/// Counters shared with the UI for diagnostics.
#[derive(Debug, Default)]
pub struct SessionStats {
    bytes_read: AtomicU64,
    batches: AtomicU64,
}

impl SessionStats {
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read.load(Ordering::Relaxed)
    }

    /// Number of times the terminal state changed and the update callback fired.
    pub fn batches(&self) -> u64 {
        self.batches.load(Ordering::Relaxed)
    }
}

#[derive(Default)]
struct ExitSlot {
    exit: Mutex<Option<std::result::Result<ExitInfo, String>>>,
    cond: Condvar,
}

/// A PTY child wired to a [`Terminal`]: a pump thread parses program output off the caller's
/// thread, writes emulator replies back to the PTY and calls `on_update` after each batch.
pub struct TerminalSession {
    pty: Arc<PtySession>,
    writer: PtyWriter,
    terminal: Arc<Mutex<Terminal>>,
    stats: Arc<SessionStats>,
    exit: Arc<ExitSlot>,
}

impl TerminalSession {
    pub fn spawn(
        options: &SpawnOptions,
        config: TerminalConfig,
        on_update: impl Fn() + Send + 'static,
    ) -> Result<Self> {
        let pty = Arc::new(PtySession::spawn(options)?);
        let output = pty
            .take_output()
            .ok_or_else(|| TerminalError::PtyIo("PTY output receiver already taken".to_owned()))?;
        let mut terminal = Terminal::new(options.size, config);
        terminal.resize(options.size, options.cell_px);
        let terminal = Arc::new(Mutex::new(terminal));
        let stats = Arc::new(SessionStats::default());
        let exit = Arc::new(ExitSlot::default());
        let writer = pty.writer();

        let pump = Pump {
            pty: Arc::clone(&pty),
            writer: writer.clone(),
            terminal: Arc::clone(&terminal),
            stats: Arc::clone(&stats),
            exit: Arc::clone(&exit),
        };
        thread::Builder::new()
            .name("hangar-term-pump".into())
            .spawn(move || pump.run(output, on_update))
            .map_err(|e| TerminalError::Thread("terminal pump", e))?;

        Ok(Self {
            pty,
            writer,
            terminal,
            stats,
            exit,
        })
    }

    /// Locks the emulator. Keep the guard short: the pump thread needs it to make progress.
    pub fn terminal(&self) -> MutexGuard<'_, Terminal> {
        lock(&self.terminal)
    }

    /// Queues bytes for the program's input (never blocks on the PTY).
    pub fn write(&self, bytes: impl Into<Vec<u8>>) -> Result<()> {
        self.writer.write(bytes)
    }

    /// Resizes the PTY and the grid together.
    pub fn resize(&self, size: GridSize, cell_px: (u16, u16)) -> Result<()> {
        self.terminal().resize(size, cell_px);
        self.pty.resize(size, cell_px)
    }

    pub fn stats(&self) -> &Arc<SessionStats> {
        &self.stats
    }

    pub fn pid(&self) -> Option<u32> {
        self.pty.pid()
    }

    /// `Some` once the program has exited and all of its output has been parsed.
    pub fn exit_status(&self) -> Option<std::result::Result<ExitInfo, String>> {
        lock(&self.exit.exit).clone()
    }

    /// Blocks until the program exits and its output is parsed, or the timeout elapses.
    pub fn wait_for_exit(
        &self,
        timeout: Duration,
    ) -> Option<std::result::Result<ExitInfo, String>> {
        let guard = lock(&self.exit.exit);
        let (guard, _) = self
            .exit
            .cond
            .wait_timeout_while(guard, timeout, |exit| exit.is_none())
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.clone()
    }

    pub fn kill(&self) -> Result<()> {
        self.pty.kill()
    }

    /// Kills the program, blocking until what ignored the hang-up is killed (see
    /// [`PtySession::kill_now`]).
    pub fn kill_now(&self, grace: Duration) {
        self.pty.kill_now(grace)
    }
}

struct Pump {
    pty: Arc<PtySession>,
    writer: PtyWriter,
    terminal: Arc<Mutex<Terminal>>,
    stats: Arc<SessionStats>,
    exit: Arc<ExitSlot>,
}

impl Pump {
    fn run(self, output: Receiver<PtyEvent>, on_update: impl Fn()) {
        loop {
            let deadline = lock(&self.terminal).sync_deadline();
            let event = match deadline {
                Some(deadline) => {
                    output.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                }
                None => output.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match event {
                Ok(PtyEvent::Output(bytes)) => {
                    if self.process(bytes, &output) {
                        self.finish(&on_update);
                        return;
                    }
                    self.notify(&on_update);
                }
                Err(RecvTimeoutError::Timeout) => {
                    lock(&self.terminal).flush_expired_sync();
                    self.notify(&on_update);
                }
                Ok(PtyEvent::Closed) | Err(RecvTimeoutError::Disconnected) => {
                    self.finish(&on_update);
                    return;
                }
            }
        }
    }

    /// Parses `first` plus any output already queued, up to `MAX_BATCH`. Returns true when
    /// the end of the stream was reached.
    fn process(&self, first: Vec<u8>, output: &Receiver<PtyEvent>) -> bool {
        let mut terminal = lock(&self.terminal);
        let mut parsed = first.len();
        self.stats
            .bytes_read
            .fetch_add(first.len() as u64, Ordering::Relaxed);
        terminal.advance(&first);
        let mut closed = false;
        while parsed < MAX_BATCH {
            match output.try_recv() {
                Ok(PtyEvent::Output(bytes)) => {
                    parsed += bytes.len();
                    self.stats
                        .bytes_read
                        .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                    terminal.advance(&bytes);
                }
                Ok(PtyEvent::Closed) => {
                    closed = true;
                    break;
                }
                Err(_) => break,
            }
        }
        let replies = terminal.take_replies();
        drop(terminal);
        if let Err(err) = self.writer.write(replies) {
            tracing::debug!(%err, "dropping terminal reply");
        }
        closed
    }

    fn notify(&self, on_update: &impl Fn()) {
        // While a synchronized update is pending nothing visible changed.
        if lock(&self.terminal).sync_deadline().is_none() {
            self.stats.batches.fetch_add(1, Ordering::Relaxed);
            on_update();
        }
    }

    fn finish(&self, on_update: &impl Fn()) {
        {
            let mut terminal = lock(&self.terminal);
            if terminal.sync_deadline().is_some() {
                // The program exited mid-update; show what it drew.
                terminal.advance(b"\x1b[?2026l");
            }
        }
        let status = self.pty.wait().map_err(|e| e.to_string());
        *lock(&self.exit.exit) = Some(status);
        self.exit.cond.notify_all();
        self.stats.batches.fetch_add(1, Ordering::Relaxed);
        on_update();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn session_renders_program_output() {
        let updates = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&updates);
        let session = TerminalSession::spawn(
            &SpawnOptions::command("/bin/sh", ["-c", "printf 'hello\\033[31m red'"]),
            TerminalConfig::default(),
            move || {
                counter.fetch_add(1, Ordering::Relaxed);
            },
        )
        .unwrap();
        let exit = session
            .wait_for_exit(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        assert!(exit.success());
        let snapshot = session.terminal().snapshot();
        assert_eq!(snapshot.row_text(0), "hello red");
        assert!(updates.load(Ordering::Relaxed) >= 1);
        assert_eq!(
            session.stats().bytes_read(),
            "hello\x1b[31m red".len() as u64
        );
    }

    #[test]
    fn emulator_replies_reach_the_program() {
        // The shell asks for the cursor position and prints the reply it reads back.
        let script = "stty raw -echo; printf 'ab\\033[6n'; dd bs=1 count=6 2>/dev/null | od -An -c";
        let session = TerminalSession::spawn(
            &SpawnOptions::command("/bin/sh", ["-c", script]),
            TerminalConfig::default(),
            || {},
        )
        .unwrap();
        session
            .wait_for_exit(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        let snapshot = session.terminal().snapshot();
        let text: String = (0..snapshot.rows)
            .map(|r| snapshot.row_text(r) + "\n")
            .collect();
        assert!(text.contains("[   1   ;   3   R"), "{text}");
    }
}
