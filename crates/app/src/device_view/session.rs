//! An opened device as the UI sees it: its info, the latest-frame channel and an ordered input
//! queue. The `Device` itself lives on the input thread, so the UI thread never blocks on
//! `send_input` nor on the backend's `Drop` (which joins capture threads).

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::adapters::devices::{Device, FrameReceiver, LogFilter, LogStream};
use anyhow::Context as _;
use hangar_core::device::{Capabilities, DeviceInfo, Input, Orientation};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;

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

/// What the input thread reports to every subscriber.
#[derive(Debug, Clone)]
pub enum Outcome {
    Input(InputResult),
    Text(TextResult),
    /// `Device::shutdown` finished.
    Shutdown(Result<(), String>),
    /// `Device::set_orientation` finished; carries the requested orientation.
    Rotate(Orientation, Result<(), String>),
}

/// Reports one command's outcome to whoever queued it (in addition to the subscribers).
type Ack = oneshot::Sender<Result<(), String>>;

/// One queued action; inputs and text are delivered in the order they were queued.
#[derive(Debug)]
enum Command {
    Input(Input, Option<Ack>),
    Text(String, Option<Ack>),
    Rotate(Orientation),
    Shutdown(Option<Ack>),
}

impl PartialEq for Command {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Input(a, _), Self::Input(b, _)) => a == b,
            (Self::Text(a, _), Self::Text(b, _)) => a == b,
            (Self::Rotate(a), Self::Rotate(b)) => a == b,
            (Self::Shutdown(_), Self::Shutdown(_)) => true,
            _ => false,
        }
    }
}

/// A shared handle on the device for blocking calls that must not wait behind queued input (log
/// streams, installs). Releasing the last reference of a device can block (backends join
/// threads), so it never happens on the caller's thread.
pub struct DeviceHandle(Option<Arc<dyn Device>>);

impl DeviceHandle {
    fn device(&self) -> crate::adapters::devices::Result<&Arc<dyn Device>> {
        self.0
            .as_ref()
            .ok_or(crate::adapters::devices::Error::Unsupported(
                "device released",
            ))
    }

    /// Spawns the log process. Blocks: call from a background thread.
    pub fn logs(&self, filter: &LogFilter) -> crate::adapters::devices::Result<LogStream> {
        self.device()?.logs(filter)
    }

    /// Installs an `.app` bundle or an `.apk`. Blocks: call from a background thread.
    pub fn install(&self, app: &std::path::Path) -> crate::adapters::devices::Result<()> {
        self.device()?.install(app)
    }

    /// Launches an installed app by bundle id / package name. Blocks.
    pub fn launch(&self, app_id: &str) -> crate::adapters::devices::Result<()> {
        self.device()?.launch(app_id)
    }

    /// The orientation the guest reports (unsupported on iOS). Blocks.
    pub fn orientation(&self) -> crate::adapters::devices::Result<Orientation> {
        self.device()?.orientation()
    }
}

impl Clone for DeviceHandle {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl Drop for DeviceHandle {
    fn drop(&mut self) {
        if let Some(device) = self.0.take() {
            let _ = std::thread::Builder::new()
                .name("device-release".into())
                .spawn(move || drop(device));
        }
    }
}

type Subscribers = Arc<Mutex<Vec<UnboundedSender<Outcome>>>>;

/// An open device shared by the pool and the views showing it: clones talk to the same input
/// thread, which stops (and releases the device) once the last clone is gone.
#[derive(Clone)]
pub struct DeviceSession(Arc<Inner>);

struct Inner {
    info: DeviceInfo,
    capabilities: Capabilities,
    handle: DeviceHandle,
    backend: &'static str,
    frames: FrameReceiver,
    input: mpsc::Sender<Command>,
    subscribers: Subscribers,
}

impl DeviceSession {
    /// Moves the device onto a new input thread. Does not block (only spawns the thread).
    pub fn start(device: Arc<dyn Device>) -> anyhow::Result<Self> {
        let info = device.info().clone();
        let capabilities = device.capabilities();
        let backend = device.backend();
        let frames = device.frames();
        let (input, rx) = mpsc::channel();
        let subscribers: Subscribers = Arc::default();
        let handle = DeviceHandle(Some(device.clone()));
        let worker_subscribers = subscribers.clone();
        std::thread::Builder::new()
            .name("device-input".into())
            .spawn(move || input_worker(device, rx, worker_subscribers))
            .context("failed to start the device input thread")?;
        Ok(Self(Arc::new(Inner {
            info,
            capabilities,
            handle,
            backend,
            frames,
            input,
            subscribers,
        })))
    }

    pub fn info(&self) -> &DeviceInfo {
        &self.0.info
    }

    pub fn backend(&self) -> &'static str {
        self.0.backend
    }

    /// What the device's backend implements (read once when it was opened).
    pub fn capabilities(&self) -> Capabilities {
        self.0.capabilities
    }

    pub fn handle(&self) -> DeviceHandle {
        self.0.handle.clone()
    }

    pub fn frames(&self) -> FrameReceiver {
        self.0.frames.clone()
    }

    /// Whether both are the same open device.
    pub fn same(&self, other: &DeviceSession) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Every later outcome of this session (from any clone), until the device is released.
    pub fn subscribe(&self) -> UnboundedReceiver<Outcome> {
        let (tx, rx) = unbounded_channel();
        if let Ok(mut subscribers) = self.0.subscribers.lock() {
            subscribers.push(tx);
        }
        rx
    }

    fn queue(&self, command: Command) -> bool {
        self.0.input.send(command).is_ok()
    }

    /// Queues input; inputs are delivered in order. Returns `false` if the device is gone.
    pub fn send(&self, input: Input) -> bool {
        self.queue(Command::Input(input, None))
    }

    /// Queues text for the focused field. Returns `false` if the device is gone.
    pub fn send_text(&self, text: String) -> bool {
        self.queue(Command::Text(text, None))
    }

    /// Queues input; resolves once the device took it (or failed to). A dropped sender means
    /// the device is gone.
    pub fn send_acked(&self, input: Input) -> oneshot::Receiver<Result<(), String>> {
        let (tx, rx) = oneshot::channel();
        self.queue(Command::Input(input, Some(tx)));
        rx
    }

    /// Queues text; resolves once it was typed (or failed to).
    pub fn send_text_acked(&self, text: String) -> oneshot::Receiver<Result<(), String>> {
        let (tx, rx) = oneshot::channel();
        self.queue(Command::Text(text, Some(tx)));
        rx
    }

    /// Queues `Device::set_orientation` behind the pending input; the outcome is reported once done.
    pub fn rotate(&self, orientation: Orientation) -> bool {
        self.queue(Command::Rotate(orientation))
    }

    /// Queues `Device::shutdown` behind the pending input; the outcome is reported once done.
    pub fn shutdown(&self) -> bool {
        self.queue(Command::Shutdown(None))
    }

    /// [`Self::shutdown`], resolving once the device is off (or failed to shut down).
    pub fn shutdown_acked(&self) -> oneshot::Receiver<Result<(), String>> {
        let (tx, rx) = oneshot::channel();
        self.queue(Command::Shutdown(Some(tx)));
        rx
    }
}

fn broadcast(subscribers: &Subscribers, outcome: &Outcome) {
    if let Ok(mut subscribers) = subscribers.lock() {
        subscribers.retain(|tx| tx.send(outcome.clone()).is_ok());
    }
}

fn reply(ack: Option<Ack>, result: &Result<(), String>) {
    if let Some(ack) = ack {
        let _ = ack.send(result.clone());
    }
}

fn input_worker(device: Arc<dyn Device>, rx: mpsc::Receiver<Command>, results: Subscribers) {
    let mut unsupported_text_logged = false;
    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        batch.extend(rx.try_iter());
        for command in coalesce(batch) {
            match command {
                Command::Input(input, ack) => {
                    let started = Instant::now();
                    let result = device.send_input(input).map_err(|e| e.to_string());
                    if let Err(err) = &result {
                        tracing::warn!(?input, %err, "device input failed");
                    }
                    reply(ack, &result);
                    if result.is_err() || !matches!(input, Input::TouchMove { .. }) {
                        broadcast(
                            &results,
                            &Outcome::Input(InputResult {
                                input,
                                result,
                                elapsed: started.elapsed(),
                            }),
                        );
                    }
                }
                Command::Rotate(orientation) => {
                    let result = device
                        .set_orientation(orientation)
                        .map_err(|e| e.to_string());
                    if let Err(err) = &result {
                        tracing::warn!(%err, ?orientation, "device rotation failed");
                    }
                    broadcast(&results, &Outcome::Rotate(orientation, result));
                }
                Command::Shutdown(ack) => {
                    let result = device.shutdown().map_err(|e| e.to_string());
                    if let Err(err) = &result {
                        tracing::warn!(%err, "device shutdown failed");
                    }
                    reply(ack, &result);
                    broadcast(&results, &Outcome::Shutdown(result));
                }
                Command::Text(text, ack) => {
                    let result = device.send_text(&text);
                    reply(ack, &result.as_ref().map_err(ToString::to_string).copied());
                    let Err(err) = result else {
                        continue;
                    };
                    let unsupported =
                        matches!(err, crate::adapters::devices::Error::Unsupported(_));
                    if !unsupported || !unsupported_text_logged {
                        tracing::warn!(%err, "device text input failed");
                    }
                    unsupported_text_logged |= unsupported;
                    broadcast(
                        &results,
                        &Outcome::Text(TextResult {
                            chars: text.chars().count(),
                            error: err.to_string(),
                        }),
                    );
                }
            }
        }
    }
    // The last reference to the device is dropped here, off the UI thread.
    drop(device);
}

/// Drops `TouchMove`s that are immediately followed by another one (when the device falls
/// behind, only the latest position of a drag matters) and joins adjacent text. Commands whose
/// sender waits for their own outcome are never merged.
fn coalesce(commands: Vec<Command>) -> Vec<Command> {
    let mut out: Vec<Command> = Vec::with_capacity(commands.len());
    for command in commands {
        match (out.last_mut(), command) {
            (
                Some(last @ Command::Input(Input::TouchMove { .. }, None)),
                next @ Command::Input(Input::TouchMove { .. }, None),
            ) => *last = next,
            (Some(Command::Text(prev, None)), Command::Text(next, None)) => prev.push_str(&next),
            (_, command) => out.push(command),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mv(x: f64) -> Command {
        Command::Input(Input::TouchMove { x, y: 0.0 }, None)
    }

    fn input(input: Input) -> Command {
        Command::Input(input, None)
    }

    #[test]
    fn coalesces_consecutive_moves_only() {
        let down = || input(Input::TouchDown { x: 0.0, y: 0.0 });
        let up = || input(Input::TouchUp { x: 9.0, y: 0.0 });
        let home = || input(Input::Home);
        assert_eq!(
            coalesce(vec![down(), mv(1.0), mv(2.0), mv(3.0), up(), home()]),
            [down(), mv(3.0), up(), home()]
        );
        assert_eq!(
            coalesce(vec![mv(1.0), down(), mv(2.0)]),
            [mv(1.0), down(), mv(2.0)]
        );
        assert!(coalesce(Vec::new()).is_empty());
    }

    #[test]
    fn joins_adjacent_text_but_keeps_order() {
        let text = |s: &str| Command::Text(s.to_owned(), None);
        let back = || input(Input::Back);
        assert_eq!(
            coalesce(vec![text("a"), text("b"), back(), text("c")]),
            [text("ab"), back(), text("c")]
        );
    }

    #[test]
    fn acknowledged_commands_are_never_merged() {
        let (tx, _rx) = oneshot::channel();
        let acked = Command::Text("b".into(), Some(tx));
        let out = coalesce(vec![Command::Text("a".into(), None), acked]);
        assert_eq!(out.len(), 2);
    }
}
