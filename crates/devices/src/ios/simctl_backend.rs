//! Public-API fallback: frames by polling `xcrun simctl io <udid> screenshot`, taps by posting
//! CGEvent mouse clicks into the Simulator.app window showing the device.

use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

use crate::{Device, DeviceInfo, Error, Frame, FrameReceiver, FrameSender, Input, Result};

pub struct SimctlSimulator {
    info: DeviceInfo,
    frames: FrameReceiver,
    stop: Arc<AtomicBool>,
    poller: Option<JoinHandle<()>>,
}

impl SimctlSimulator {
    pub fn open(info: DeviceInfo) -> Result<Self> {
        // Fail early (and with the real error) if simctl cannot capture this device.
        let first = screenshot(&info.udid)?;
        let (tx, frames) = tokio::sync::watch::channel(None);
        let mut seq = 1;
        tx.send_replace(Some(Frame { seq, ..first }));
        let stop = Arc::new(AtomicBool::new(false));
        let poll_stop = stop.clone();
        let udid = info.udid.clone();
        let poller = std::thread::Builder::new()
            .name("simctl-screenshot".into())
            .spawn(move || poll(&udid, &tx, &poll_stop, &mut seq))?;
        Ok(Self {
            info,
            frames,
            stop,
            poller: Some(poller),
        })
    }
}

fn poll(udid: &str, tx: &FrameSender, stop: &AtomicBool, seq: &mut u64) {
    while !stop.load(Ordering::Relaxed) {
        match screenshot(udid) {
            Ok(frame) => {
                *seq += 1;
                tx.send_replace(Some(Frame { seq: *seq, ..frame }));
            }
            Err(e) => {
                tracing::warn!("simctl screenshot failed: {e}");
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
    }
}

/// One `simctl io screenshot` round trip, decoded to tightly packed BGRA.
pub fn screenshot(udid: &str) -> Result<Frame> {
    // Xcode 27's simctl treats "-" as a file name (no stdout output), so go through a temp file.
    let path =
        std::env::temp_dir().join(format!("hangar-simctl-{}-{udid}.png", std::process::id()));
    let started = Instant::now();
    let out = Command::new("xcrun")
        .args(["simctl", "io", udid, "screenshot", "--type=png"])
        .arg(&path)
        .output()?;
    if !out.status.success() {
        return Err(Error::Command(format!(
            "simctl io screenshot: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let png = std::fs::read(&path)?;
    let _ = std::fs::remove_file(&path);
    let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
        .map_err(|e| Error::Parse(format!("screenshot PNG: {e}")))?
        .into_rgba8();
    let (width, height) = img.dimensions();
    let mut bgra = img.into_raw();
    for px in bgra.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    Ok(Frame {
        width,
        height,
        stride: width as usize * 4,
        bgra: Arc::from(bgra),
        // simctl captures somewhere inside this window; the start time bounds the latency.
        captured_at: started,
        seq: 0,
    })
}

impl Device for SimctlSimulator {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn logs(&self, filter: &crate::LogFilter) -> Result<crate::LogStream> {
        super::log_stream(&self.info.udid, filter)
    }

    fn backend(&self) -> &'static str {
        "simctl"
    }

    fn frames(&self) -> FrameReceiver {
        self.frames.clone()
    }

    fn send_input(&self, input: Input) -> Result<()> {
        super::sim_window::click(&self.info, input)
    }
}

impl Drop for SimctlSimulator {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(p) = self.poller.take() {
            let _ = p.join();
        }
    }
}
