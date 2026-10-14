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

pub struct DeviceSession {
    info: DeviceInfo,
    backend: &'static str,
    frames: FrameReceiver,
    input: mpsc::Sender<Input>,
    results: Option<UnboundedReceiver<InputResult>>,
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
        self.input.send(input).is_ok()
    }

    pub(super) fn take_results(&mut self) -> Option<UnboundedReceiver<InputResult>> {
        self.results.take()
    }
}

fn input_worker(
    device: Arc<dyn Device>,
    rx: mpsc::Receiver<Input>,
    results: UnboundedSender<InputResult>,
) {
    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        batch.extend(rx.try_iter());
        for input in coalesce(batch) {
            let started = Instant::now();
            let result = device.send_input(input).map_err(|e| e.to_string());
            if let Err(err) = &result {
                tracing::warn!(?input, %err, "device input failed");
            }
            if result.is_err() || !matches!(input, Input::TouchMove { .. }) {
                let _ = results.send(InputResult {
                    input,
                    result,
                    elapsed: started.elapsed(),
                });
            }
        }
    }
    // The last reference to the device is dropped here, off the UI thread.
    drop(device);
}

/// Drops `TouchMove`s that are immediately followed by another one: when the device falls
/// behind, only the latest position of a drag matters.
pub fn coalesce(inputs: Vec<Input>) -> Vec<Input> {
    let mut out: Vec<Input> = Vec::with_capacity(inputs.len());
    for input in inputs {
        if let (Some(Input::TouchMove { .. }), Input::TouchMove { .. }) = (out.last(), input)
            && let Some(last) = out.last_mut()
        {
            *last = input;
            continue;
        }
        out.push(input);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mv(x: f64) -> Input {
        Input::TouchMove { x, y: 0.0 }
    }

    #[test]
    fn coalesces_consecutive_moves_only() {
        let down = Input::TouchDown { x: 0.0, y: 0.0 };
        let up = Input::TouchUp { x: 9.0, y: 0.0 };
        assert_eq!(
            coalesce(vec![down, mv(1.0), mv(2.0), mv(3.0), up, Input::Home]),
            [down, mv(3.0), up, Input::Home]
        );
        assert_eq!(
            coalesce(vec![mv(1.0), down, mv(2.0)]),
            [mv(1.0), down, mv(2.0)]
        );
        assert!(coalesce(Vec::new()).is_empty());
    }
}
