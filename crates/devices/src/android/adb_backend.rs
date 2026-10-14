//! Fallback Android backend using only adb: frames by polling `screencap`, input via `input`.
//! Works for any adb device (emulator or physical), no gRPC needed.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::adb::{Adb, escape_input_text};
use crate::geometry::device_to_pixels;
use crate::{
    Device, DeviceInfo, Error, Frame, FrameReceiver, FrameSender, Input, Orientation, Platform,
    Result, ScreenSpec,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Capture {
    /// `screencap` raw RGBA: no PNG encode on the device, but 14.6 MB per 1280x2856 frame over
    /// adb made it ~4x slower than PNG on emulator 36.5 (1.5 vs 6.3 fps while scrolling).
    Raw,
    /// `screencap -p`, decoded with `image`.
    #[default]
    Png,
}

pub struct AdbDevice {
    info: DeviceInfo,
    screen: ScreenSpec,
    adb: Adb,
    frames: Arc<FrameSender>,
    /// Shape of the display the input space follows; kept from the latest frame.
    landscape: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    poller: Option<JoinHandle<()>>,
}

impl AdbDevice {
    pub fn open(adb: Adb, info: DeviceInfo, capture: Capture) -> Result<Self> {
        let screen = info
            .screen
            .ok_or_else(|| Error::NotFound(format!("screen size of {}", info.udid)))?;
        let first = screenshot(&adb, &info.udid, capture)?;
        let landscape = Arc::new(AtomicBool::new(first.width > first.height));
        let frames = Arc::new(tokio::sync::watch::Sender::new(Some(Frame {
            seq: 1,
            ..first
        })));
        let tx = frames.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let poll_stop = stop.clone();
        let poll_landscape = landscape.clone();
        let (poll_adb, serial) = (adb.clone(), info.udid.clone());
        let poller = std::thread::Builder::new()
            .name("adb-screencap".into())
            .spawn(move || {
                poll(
                    &poll_adb,
                    &serial,
                    capture,
                    &tx,
                    &poll_stop,
                    &poll_landscape,
                )
            })?;
        Ok(Self {
            info,
            screen,
            adb,
            frames,
            landscape,
            stop,
            poller: Some(poller),
        })
    }

    fn shell(&self, cmd: &str) -> Result<()> {
        self.adb.shell(&self.info.udid, cmd).map(|_| ())
    }

    fn screen(&self) -> ScreenSpec {
        self.screen.oriented(self.landscape.load(Ordering::Relaxed))
    }
}

/// Polls only while someone holds a receiver from [`Device::frames`].
fn poll(
    adb: &Adb,
    serial: &str,
    capture: Capture,
    tx: &FrameSender,
    stop: &AtomicBool,
    landscape: &AtomicBool,
) {
    let mut seq = 1;
    while !stop.load(Ordering::Relaxed) {
        if tx.receiver_count() == 0 {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        match screenshot(adb, serial, capture) {
            Ok(frame) => {
                landscape.store(frame.width > frame.height, Ordering::Relaxed);
                seq += 1;
                tx.send_replace(Some(Frame { seq, ..frame }));
            }
            Err(e) => {
                tracing::warn!("adb screencap failed: {e}");
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

/// One `adb exec-out screencap` round trip, as packed BGRA.
pub fn screenshot(adb: &Adb, serial: &str, capture: Capture) -> Result<Frame> {
    let started = Instant::now();
    let (width, height, bgra) = match capture {
        Capture::Raw => parse_raw(&adb.exec_out(serial, "screencap")?)?,
        Capture::Png => decode_png(&adb.exec_out(serial, "screencap -p")?)?,
    };
    Ok(Frame {
        width,
        height,
        stride: width as usize * 4,
        bgra: Arc::from(bgra),
        // The capture happens somewhere inside the round trip; the start bounds the latency.
        captured_at: started,
        seq: 0,
    })
}

/// `screencap` raw output: `u32 width, u32 height, u32 format` (+ `u32 colorspace` since
/// Android 9), little endian, then RGBA_8888 rows.
pub fn parse_raw(data: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    let word = |i: usize| -> Result<u32> {
        data.get(i * 4..i * 4 + 4)
            .and_then(|b| b.try_into().ok())
            .map(u32::from_le_bytes)
            .ok_or_else(|| Error::Parse("screencap: short header".into()))
    };
    let (width, height, format) = (word(0)?, word(1)?, word(2)?);
    // PIXEL_FORMAT_RGBA_8888 = 1, RGBX_8888 = 2.
    if format != 1 && format != 2 {
        return Err(Error::Parse(format!("screencap: pixel format {format}")));
    }
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| Error::Parse("screencap: size overflow".into()))?;
    let header = data
        .len()
        .checked_sub(pixels)
        .filter(|h| *h == 12 || *h == 16)
        .ok_or_else(|| {
            Error::Parse(format!(
                "screencap: {} bytes for {width}x{height}",
                data.len()
            ))
        })?;
    let mut bgra = data[header..].to_vec();
    super::grpc::rgba_to_bgra_in_place(&mut bgra);
    Ok((width, height, bgra))
}

pub fn decode_png(png: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .map_err(|e| Error::Parse(format!("screencap PNG: {e}")))?
        .into_rgba8();
    let (w, h) = img.dimensions();
    let mut bgra = img.into_raw();
    for px in bgra.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    Ok((w, h, bgra))
}

fn keycode(key: crate::Key) -> &'static str {
    use crate::Key;
    match key {
        Key::Backspace => "KEYCODE_DEL",
        Key::Enter => "KEYCODE_ENTER",
        Key::Tab => "KEYCODE_TAB",
        Key::Escape => "KEYCODE_ESCAPE",
        Key::ArrowUp => "KEYCODE_DPAD_UP",
        Key::ArrowDown => "KEYCODE_DPAD_DOWN",
        Key::ArrowLeft => "KEYCODE_DPAD_LEFT",
        Key::ArrowRight => "KEYCODE_DPAD_RIGHT",
    }
}

/// `input` command for one [`Input`], coordinates already in device pixels.
pub fn input_command(input: Input, px: impl Fn(f64, f64) -> (i32, i32)) -> String {
    match input {
        Input::Tap { x, y } => {
            let (x, y) = px(x, y);
            format!("input tap {x} {y}")
        }
        Input::TouchDown { x, y } => {
            let (x, y) = px(x, y);
            format!("input motionevent DOWN {x} {y}")
        }
        Input::TouchMove { x, y } => {
            let (x, y) = px(x, y);
            format!("input motionevent MOVE {x} {y}")
        }
        Input::TouchUp { x, y } => {
            let (x, y) = px(x, y);
            format!("input motionevent UP {x} {y}")
        }
        Input::Home => "input keyevent KEYCODE_HOME".to_owned(),
        Input::Back => "input keyevent KEYCODE_BACK".to_owned(),
        Input::Lock => "input keyevent KEYCODE_POWER".to_owned(),
        Input::VolumeUp => "input keyevent KEYCODE_VOLUME_UP".to_owned(),
        Input::VolumeDown => "input keyevent KEYCODE_VOLUME_DOWN".to_owned(),
        Input::AppSwitch => "input keyevent KEYCODE_APP_SWITCH".to_owned(),
        Input::Key(key) => format!("input keyevent {}", keycode(key)),
    }
}

impl Device for AdbDevice {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn logs(&self, filter: &crate::LogFilter) -> Result<crate::LogStream> {
        super::logcat::logcat_with(&self.adb.path, &self.info.udid, filter)
    }

    fn backend(&self) -> &'static str {
        "adb"
    }

    fn platform(&self) -> Platform {
        Platform::Android
    }

    fn frames(&self) -> FrameReceiver {
        let mut rx = self.frames.subscribe();
        rx.mark_changed();
        rx
    }

    fn send_input(&self, input: Input) -> Result<()> {
        let screen = self.screen();
        self.shell(&input_command(input, |x, y| {
            device_to_pixels(x, y, &screen)
        }))
    }

    fn send_text(&self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        self.shell(&format!("input text {}", escape_input_text(text)))
    }

    /// `accelerometer_rotation 0` + `user_rotation N`; works on emulators and physical devices.
    /// Auto-rotate stays off afterwards.
    fn set_orientation(&self, orientation: Orientation) -> Result<()> {
        self.adb.set_orientation(&self.info.udid, orientation)?;
        self.landscape
            .store(orientation.is_landscape(), Ordering::Relaxed);
        Ok(())
    }

    fn orientation(&self) -> Result<Orientation> {
        self.adb.orientation(&self.info.udid)
    }

    /// Emulators only (`adb emu kill`); waits until adb no longer lists the serial.
    fn shutdown(&self) -> Result<()> {
        let serial = &self.info.udid;
        if !serial.starts_with("emulator-") {
            return Err(Error::Unsupported("shutdown of a physical device"));
        }
        self.adb.emu(serial, &["kill"])?;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(30) {
            if !self.adb.devices()?.iter().any(|d| &d.serial == serial) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Err(Error::Command(format!(
            "{serial} still listed 30 s after `adb emu kill`"
        )))
    }

    fn install(&self, app_path: &std::path::Path) -> Result<()> {
        self.adb.install(&self.info.udid, app_path)
    }

    fn launch(&self, bundle_id: &str) -> Result<()> {
        self.adb.launch(&self.info.udid, bundle_id)
    }
}

impl Drop for AdbDevice {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(p) = self.poller.take() {
            let _ = p.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(header_words: &[u32], pixels: &[u8]) -> Vec<u8> {
        let mut v: Vec<u8> = header_words.iter().flat_map(|w| w.to_le_bytes()).collect();
        v.extend_from_slice(pixels);
        v
    }

    #[test]
    fn parses_raw_screencap() -> Result<()> {
        let px = [1, 2, 3, 4, 5, 6, 7, 8];
        for header in [vec![2, 1, 1], vec![2, 1, 1, 0]] {
            let (w, h, bgra) = parse_raw(&raw(&header, &px))?;
            assert_eq!((w, h), (2, 1));
            assert_eq!(bgra, vec![3, 2, 1, 255, 7, 6, 5, 255]);
        }
        assert!(parse_raw(&raw(&[2, 1, 4, 0], &px)).is_err());
        assert!(parse_raw(&raw(&[4, 1, 1, 0], &px)).is_err());
        assert!(parse_raw(&[1, 2]).is_err());
        Ok(())
    }

    #[test]
    fn input_commands_use_pixels() {
        let screen = ScreenSpec {
            width_px: 1280,
            height_px: 2856,
            scale: 3.0,
        };
        let px = |x, y| device_to_pixels(x, y, &screen);
        assert_eq!(
            input_command(Input::Tap { x: 100.0, y: 200.0 }, px),
            "input tap 300 600"
        );
        assert_eq!(
            input_command(Input::TouchMove { x: 1.0, y: 2.0 }, px),
            "input motionevent MOVE 3 6"
        );
        assert_eq!(
            input_command(Input::Back, px),
            "input keyevent KEYCODE_BACK"
        );
        assert_eq!(
            input_command(Input::Key(crate::Key::Backspace), px),
            "input keyevent KEYCODE_DEL"
        );
        assert_eq!(
            input_command(Input::Key(crate::Key::ArrowLeft), px),
            "input keyevent KEYCODE_DPAD_LEFT"
        );
    }
}
