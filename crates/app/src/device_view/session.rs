//! An opened device as the UI sees it: its info, the latest-frame channel and an ordered input
//! queue. The `Device` itself lives on the input thread, so the UI thread never blocks on
//! `send_input` nor on the backend's `Drop` (which joins capture threads).

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use hangar_devices::{Device, DeviceInfo, FrameReceiver, Input};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// Outcome of one input sent to the device. Successful `TouchMove`s are not reported.
#[derive(Debug, Clone)]
pub struct InputResult {
    pub input: Input,
    pub result: Result<(), String>,
    pub elapsed: Duration,
}

/// Outcome of a text batch; only failures are reported.
#[derive(Debug, Clone)]
pub struct TextResult {
    pub chars: usize,
    pub error: String,
}

/// What the input thread reports back to the view.
#[derive(Debug, Clone)]
pub enum Outcome {
    Input(InputResult),
    Text(TextResult),
    /// `Device::shutdown` finished.
    Shutdown(Result<(), String>),
}

/// One queued action; inputs and text are delivered in the order they were queued.
#[derive(Debug, Clone, PartialEq)]
enum Command {
    Input(Input),
    Text(String),
    Shutdown,
}

pub struct DeviceSession {
    info: DeviceInfo,
    backend: &'static str,
    frames: FrameReceiver,
    input: mpsc::Sender<Command>,
    results: Option<UnboundedReceiver<Outcome>>,
}

impl DeviceSession {
    /// Moves the device onto a new input thread. Does not block (only spawns the thread).
    pub fn start(device: Arc<dyn Device>) -> anyhow::Result<Self> {
        let info = device.info().clone();
        let backend = device.backend();
        let frames = device.frames();
        let (input, rx) = mpsc::channel();
        let (results_tx, results) = unbounded_channel();
        std::thread::Builder::new()
            .name("device-input".into())
            .spawn(move || input_worker(device, rx, results_tx))
            .context("failed to start the device input thread")?;
        Ok(Self {
            info,
            backend,
            frames,
            input,
            results: Some(results),
        })
    }

    pub fn info(&self) -> &DeviceInfo {
        &self.info
    }

    pub fn backend(&self) -> &'static str {
        self.backend
    }

    pub fn frames(&self) -> FrameReceiver {
        self.frames.clone()
    }

    /// Queues input; inputs are delivered in order. Returns `false` if the device is gone.
    pub fn send(&self, input: Input) -> bool {
        self.input.send(Command::Input(input)).is_ok()
    }

    /// Queues text for the focused field. Returns `false` if the device is gone.
    pub fn send_text(&self, text: String) -> bool {
        self.input.send(Command::Text(text)).is_ok()
    }

    /// Queues `Device::shutdown` behind the pending input; the outcome is reported once done.
    pub fn shutdown(&self) -> bool {
        self.input.send(Command::Shutdown).is_ok()
    }

    pub(super) fn take_results(&mut self) -> Option<UnboundedReceiver<Outcome>> {
        self.results.take()
    }
}

fn input_worker(
    device: Arc<dyn Device>,
    rx: mpsc::Receiver<Command>,
    results: UnboundedSender<Outcome>,
) {
    let mut unsupported_text_logged = false;
    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        batch.extend(rx.try_iter());
        for command in coalesce(batch) {
            match command {
                Command::Input(input) => {
                    let started = Instant::now();
                    let result = device.send_input(input).map_err(|e| e.to_string());
                    if let Err(err) = &result {
                        tracing::warn!(?input, %err, "device input failed");
                    }
                    if result.is_err() || !matches!(input, Input::TouchMove { .. }) {
                        let _ = results.send(Outcome::Input(InputResult {
                            input,
                            result,
                            elapsed: started.elapsed(),
                        }));
                    }
                }
                Command::Shutdown => {
                    let result = device.shutdown().map_err(|e| e.to_string());
                    if let Err(err) = &result {
                        tracing::warn!(%err, "device shutdown failed");
                    }
                    let _ = results.send(Outcome::Shutdown(result));
                }
                Command::Text(text) => {
                    let Err(err) = device.send_text(&text) else {
                        continue;
                    };
                    let unsupported = matches!(err, hangar_devices::Error::Unsupported(_));
                    if !unsupported || !unsupported_text_logged {
                        tracing::warn!(%err, "device text input failed");
                    }
                    unsupported_text_logged |= unsupported;
                    let _ = results.send(Outcome::Text(TextResult {
                        chars: text.chars().count(),
                        error: err.to_string(),
                    }));
                }
            }
        }
    }
    // The last reference to the device is dropped here, off the UI thread.
    drop(device);
}

/// Drops `TouchMove`s that are immediately followed by another one (when the device falls
/// behind, only the latest position of a drag matters) and joins adjacent text.
fn coalesce(commands: Vec<Command>) -> Vec<Command> {
    let mut out: Vec<Command> = Vec::with_capacity(commands.len());
    for command in commands {
        match (out.last_mut(), &command) {
            (
                Some(Command::Input(Input::TouchMove { .. })),
                Command::Input(Input::TouchMove { .. }),
            ) => {
                if let Some(last) = out.last_mut() {
                    *last = command;
                }
            }
            (Some(Command::Text(prev)), Command::Text(next)) => prev.push_str(next),
            _ => out.push(command),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mv(x: f64) -> Command {
        Command::Input(Input::TouchMove { x, y: 0.0 })
    }

    fn input(input: Input) -> Command {
        Command::Input(input)
    }

    #[test]
    fn coalesces_consecutive_moves_only() {
        let down = input(Input::TouchDown { x: 0.0, y: 0.0 });
        let up = input(Input::TouchUp { x: 9.0, y: 0.0 });
        let home = input(Input::Home);
        assert_eq!(
            coalesce(vec![
                down.clone(),
                mv(1.0),
                mv(2.0),
                mv(3.0),
                up.clone(),
                home.clone()
            ]),
            [down.clone(), mv(3.0), up, home]
        );
        assert_eq!(
            coalesce(vec![mv(1.0), down.clone(), mv(2.0)]),
            [mv(1.0), down, mv(2.0)]
        );
        assert!(coalesce(Vec::new()).is_empty());
    }

    #[test]
    fn joins_adjacent_text_but_keeps_order() {
        let text = |s: &str| Command::Text(s.to_owned());
        let back = input(Input::Back);
        assert_eq!(
            coalesce(vec![text("a"), text("b"), back.clone(), text("c")]),
            [text("ab"), back, text("c")]
        );
    }
}
