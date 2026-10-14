//! Primary Android backend: frames via `streamScreenshot`, input via `sendTouch`/`sendKey`.
//! Owns its tokio runtime so callers need none.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use tokio::runtime::Runtime;

use super::emulator::{self, BootOptions, RunningEmulator};
use super::grpc::{self, Client, keys, proto};
use super::sdk::AndroidSdk;
use crate::geometry::{device_to_pixels, panel_pixels};
use crate::{
    Device, DeviceInfo, Error, Frame, FrameReceiver, FrameSender, Input, Orientation, Platform,
    Result, ScreenSpec,
};

/// Requested frame size; the emulator keeps the aspect ratio and never upscales.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameSize {
    /// 0 = device width.
    pub width: u32,
    /// 0 = device height.
    pub height: u32,
}

struct Conn {
    client: Client,
    running: RunningEmulator,
}

pub struct GrpcEmulator {
    info: DeviceInfo,
    screen: ScreenSpec,
    avd_id: String,
    sdk: AndroidSdk,
    boot_options: BootOptions,
    conn: Arc<RwLock<Option<Conn>>>,
    frames: Arc<FrameSender>,
    /// Shape of the display the input space follows; kept from the latest frame.
    landscape: Arc<AtomicBool>,
    rotation: AtomicU8,
    rt: Option<Runtime>,
}

impl GrpcEmulator {
    pub fn open(sdk: &AndroidSdk, running: RunningEmulator) -> Result<Self> {
        Self::open_with(sdk, running, FrameSize::default())
    }

    pub fn open_with(sdk: &AndroidSdk, running: RunningEmulator, size: FrameSize) -> Result<Self> {
        let screen = running
            .screen()
            .ok_or_else(|| Error::NotFound(format!("screen size of {}", running.serial())))?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("android-grpc")
            .enable_all()
            .build()?;
        let client = connect(&rt, &running)?;
        let frames = Arc::new(tokio::sync::watch::Sender::new(None));
        let conn = Arc::new(RwLock::new(Some(Conn {
            client,
            running: running.clone(),
        })));
        let landscape = Arc::new(AtomicBool::new(false));
        rt.spawn(stream_frames(
            conn.clone(),
            frames.clone(),
            size,
            landscape.clone(),
        ));
        Ok(Self {
            info: running.info.clone(),
            screen,
            avd_id: running.discovery.avd_id.clone(),
            sdk: sdk.clone(),
            boot_options: BootOptions::default(),
            conn,
            frames,
            landscape,
            rotation: AtomicU8::new(0),
            rt: Some(rt),
        })
    }

    /// Options used when [`Device::boot`] relaunches this AVD after a shutdown.
    pub fn set_boot_options(&mut self, options: BootOptions) {
        self.boot_options = options;
    }

    fn client(&self) -> Result<Client> {
        self.conn
            .read()
            .map_err(|_| Error::Command("connection lock poisoned".into()))?
            .as_ref()
            .map(|c| c.client.clone())
            .ok_or_else(|| Error::NotFound(format!("{} is shut down", self.info.name)))
    }

    /// Runs `fut` on the backend runtime and waits for it from any thread (also from inside
    /// another tokio runtime, where `block_on` would panic).
    fn call<T: Send + 'static>(
        &self,
        fut: impl Future<Output = Result<T>> + Send + 'static,
    ) -> Result<T> {
        let rt = self
            .rt
            .as_ref()
            .ok_or_else(|| Error::Command("runtime stopped".into()))?;
        let (tx, rx) = std::sync::mpsc::channel();
        rt.spawn(async move {
            let _ = tx.send(fut.await);
        });
        rx.recv()
            .map_err(|_| Error::Command("gRPC task dropped".into()))?
    }

    /// Current adb serial (a relaunch may pick another console port).
    fn serial(&self) -> String {
        self.conn
            .read()
            .ok()
            .and_then(|c| c.as_ref().map(|c| c.running.serial()))
            .unwrap_or_else(|| self.info.udid.clone())
    }

    fn px(&self, x: f64, y: f64) -> (i32, i32) {
        let screen = self.screen.oriented(self.landscape.load(Ordering::Relaxed));
        let (fx, fy) = device_to_pixels(x, y, &screen);
        panel_pixels(fx, fy, &self.screen, self.rotation())
    }

    /// Guest orientation for input mapping. Cached from [`Device::set_orientation`]; refreshed
    /// from adb when the frame shape disagrees (rotated by someone else). Portrait and
    /// upside-down share a frame shape, so a foreign 180 degree turn goes unnoticed.
    fn rotation(&self) -> Orientation {
        let landscape = self.landscape.load(Ordering::Relaxed);
        let cached = Orientation::from_android_rotation(self.rotation.load(Ordering::Relaxed))
            .unwrap_or(Orientation::Portrait);
        if cached.is_landscape() == landscape {
            return cached;
        }
        let fresh = super::adb::Adb::new(self.sdk.adb.clone())
            .orientation(&self.serial())
            .ok()
            .filter(|o| o.is_landscape() == landscape)
            .unwrap_or(if landscape {
                Orientation::LandscapeLeft
            } else {
                Orientation::Portrait
            });
        self.rotation
            .store(fresh.android_rotation(), Ordering::Relaxed);
        fresh
    }

    pub fn status(&self) -> Result<proto::EmulatorStatus> {
        let mut c = self.client()?;
        self.call(async move {
            c.get_status(())
                .await
                .map(tonic::Response::into_inner)
                .map_err(|s| grpc::status_err("getStatus", &s))
        })
    }
}

fn connect(rt: &Runtime, running: &RunningEmulator) -> Result<Client> {
    let d = &running.discovery;
    let port = d.grpc_port.ok_or(Error::Unsupported(
        "emulator started without gRPC (relaunch it with -grpc <port>)",
    ))?;
    let token = d.grpc_token.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    rt.spawn(async move {
        let _ = tx.send(grpc::connect(port, token.as_deref()).await);
    });
    rx.recv()
        .map_err(|_| Error::Command("gRPC connect task dropped".into()))?
}

/// Streams only while someone holds a receiver from [`Device::frames`]: an unwatched device costs
/// no emulator encoding, copies or conversion.
async fn stream_frames(
    conn: Arc<RwLock<Option<Conn>>>,
    tx: Arc<FrameSender>,
    size: FrameSize,
    landscape: Arc<AtomicBool>,
) {
    let mut seq = 0u64;
    loop {
        let client = conn
            .read()
            .ok()
            .and_then(|c| c.as_ref().map(|c| c.client.clone()));
        let (Some(mut client), true) = (client, tx.receiver_count() > 0) else {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let request = proto::ImageFormat {
            format: proto::image_format::ImgFormat::Rgba8888 as i32,
            width: size.width,
            height: size.height,
            ..Default::default()
        };
        let mut stream = match client.stream_screenshot(request).await {
            Ok(resp) => resp.into_inner(),
            Err(s) => {
                tracing::debug!("streamScreenshot: {s}");
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
        };
        let mut watchers = tokio::time::interval(Duration::from_millis(250));
        loop {
            let msg = tokio::select! {
                m = stream.message() => m,
                _ = watchers.tick() => {
                    if tx.receiver_count() == 0 {
                        break;
                    }
                    continue;
                }
            };
            match msg {
                Ok(Some(img)) => {
                    let captured_at = produced_at(img.timestamp_us);
                    let (w, h) = img.format.as_ref().map_or((0, 0), |f| (f.width, f.height));
                    // An inactive display sends empty images.
                    if w == 0 || h == 0 {
                        continue;
                    }
                    let got = img.image.len();
                    let Some(bgra) = grpc::rgba_to_bgra(img.image, w, h) else {
                        tracing::warn!("short frame: {got} bytes for {w}x{h}");
                        continue;
                    };
                    landscape.store(w > h, Ordering::Relaxed);
                    seq += 1;
                    tx.send_replace(Some(Frame {
                        width: w,
                        height: h,
                        stride: w as usize * 4,
                        bgra: Arc::from(bgra),
                        captured_at,
                        seq,
                    }));
                }
                Ok(None) => break,
                Err(s) => {
                    tracing::debug!("screenshot stream: {s}");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    break;
                }
            }
        }
    }
}

/// Maps the emulator's frame timestamp (unix microseconds, same host clock) to an `Instant`, so
/// `captured_at` measures from frame production rather than from arrival.
fn produced_at(timestamp_us: u64) -> Instant {
    let now = Instant::now();
    let Ok(since_epoch) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return now;
    };
    let age = since_epoch.saturating_sub(Duration::from_micros(timestamp_us));
    // A missing or bogus timestamp falls back to arrival time.
    if timestamp_us == 0 || age > Duration::from_secs(5) {
        return now;
    }
    now.checked_sub(age).unwrap_or(now)
}

impl Device for GrpcEmulator {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn logs(&self, filter: &crate::LogFilter) -> Result<crate::LogStream> {
        super::logcat::logcat_with(&self.sdk.adb, &self.serial(), filter)
    }

    fn backend(&self) -> &'static str {
        "grpc"
    }

    fn capabilities(&self) -> crate::Capabilities {
        crate::Capabilities::ALL
    }

    fn platform(&self) -> Platform {
        Platform::Android
    }

    fn frames(&self) -> FrameReceiver {
        let mut rx = self.frames.subscribe();
        // The latest frame (if any) counts as new for this receiver.
        rx.mark_changed();
        rx
    }

    fn send_input(&self, input: Input) -> Result<()> {
        let mut c = self.client()?;
        let touch = |c: &mut Client, x: f64, y: f64, down: bool| {
            let (px, py) = self.px(x, y);
            let mut c = c.clone();
            async move {
                c.send_touch(grpc::touch(px, py, down))
                    .await
                    .map(|_| ())
                    .map_err(|s| grpc::status_err("sendTouch", &s))
            }
        };
        match input {
            Input::Tap { x, y } => {
                let down = touch(&mut c, x, y, true);
                let up = touch(&mut c, x, y, false);
                self.call(async move {
                    down.await?;
                    // A zero-length press is sometimes dropped by the input reader.
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    up.await
                })
            }
            Input::TouchDown { x, y } | Input::TouchMove { x, y } => {
                self.call(touch(&mut c, x, y, true))
            }
            Input::TouchUp { x, y } => self.call(touch(&mut c, x, y, false)),
            Input::Home
            | Input::Back
            | Input::Lock
            | Input::VolumeUp
            | Input::VolumeDown
            | Input::AppSwitch
            | Input::Key(_) => {
                let code = match input {
                    Input::Key(k) => keys::named(k),
                    Input::Home => keys::HOME,
                    Input::Back => keys::BACK,
                    Input::Lock => keys::POWER,
                    Input::VolumeUp => keys::VOLUME_UP,
                    Input::VolumeDown => keys::VOLUME_DOWN,
                    _ => keys::APP_SWITCH,
                };
                self.call(async move {
                    c.send_key(grpc::key_press(code))
                        .await
                        .map(|_| ())
                        .map_err(|s| grpc::status_err("sendKey", &s))
                })
            }
        }
    }

    fn send_text(&self, text: &str) -> Result<()> {
        let mut c = self.client()?;
        let event = grpc::text_event(text);
        self.call(async move {
            c.send_key(event)
                .await
                .map(|_| ())
                .map_err(|s| grpc::status_err("sendKey(text)", &s))
        })
    }

    /// Turns the emulator's virtual sensor (`setPhysicalModel(ROTATION)`) with auto-rotate on, so
    /// the guest rotates and the emulator hands out upright landscape frames and maps touches
    /// itself. The adb `user_rotation` lock would rotate the guest but leave `streamScreenshot`
    /// in panel space (sideways content, no rotation info), see spikes/m4-android-notes.md.
    /// Apps that ignore the sensor (the Pixel launcher) stay portrait: an error after 5 s.
    fn set_orientation(&self, orientation: Orientation) -> Result<()> {
        let adb = super::adb::Adb::new(self.sdk.adb.clone());
        let serial = self.serial();
        adb.shell(&serial, "settings put system accelerometer_rotation 1")?;
        let mut c = self.client()?;
        let [x, y, z] = grpc::rotation_angles(orientation);
        self.call(async move {
            c.set_physical_model(grpc::rotation_model(x, y, z))
                .await
                .map(|_| ())
                .map_err(|s| grpc::status_err("setPhysicalModel", &s))
        })?;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            if adb.orientation(&serial).ok() == Some(orientation) {
                super::adb::settle();
                self.landscape
                    .store(orientation.is_landscape(), Ordering::Relaxed);
                self.rotation
                    .store(orientation.android_rotation(), Ordering::Relaxed);
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(Error::Command(format!(
            "{serial} did not rotate to {orientation:?} within 5 s (the foreground app may ignore the sensor, e.g. the Pixel launcher; upside-down is off by default on phones)"
        )))
    }

    fn orientation(&self) -> Result<Orientation> {
        super::adb::Adb::new(self.sdk.adb.clone()).orientation(&self.serial())
    }

    /// Relaunches the AVD if it was shut down; a running emulator is left alone.
    fn boot(&self) -> Result<()> {
        let alive = self
            .conn
            .read()
            .map_err(|_| Error::Command("connection lock poisoned".into()))?
            .as_ref()
            .is_some_and(|c| super::discovery::process_alive(c.running.discovery.pid));
        if alive {
            return Ok(());
        }
        let running = emulator::boot(&self.sdk, &self.avd_id, &self.boot_options, &|_| {})?;
        let rt = self
            .rt
            .as_ref()
            .ok_or_else(|| Error::Command("runtime stopped".into()))?;
        let client = connect(rt, &running)?;
        *self
            .conn
            .write()
            .map_err(|_| Error::Command("connection lock poisoned".into()))? =
            Some(Conn { client, running });
        Ok(())
    }

    /// Graceful shutdown via `setVmState(SHUTDOWN)`, falling back to `adb emu kill`.
    fn shutdown(&self) -> Result<()> {
        let taken = self
            .conn
            .write()
            .map_err(|_| Error::Command("connection lock poisoned".into()))?
            .take();
        let Some(Conn {
            mut client,
            running,
        }) = taken
        else {
            return Ok(());
        };
        let d = running.discovery;
        let sent = self.call(async move {
            client
                .set_vm_state(proto::VmRunState {
                    state: proto::vm_run_state::RunState::Shutdown as i32,
                })
                .await
                .map(|_| ())
                .map_err(|s| grpc::status_err("setVmState", &s))
        });
        let timeout = Duration::from_secs(30);
        match sent {
            Ok(()) if emulator::wait_exit(d.pid, timeout) => Ok(()),
            Ok(()) => {
                tracing::warn!("{} ignored SHUTDOWN, using adb emu kill", d.serial());
                emulator::kill_via_adb(&self.sdk, &d, timeout)
            }
            Err(e) => {
                tracing::warn!("setVmState failed ({e}), using adb emu kill");
                emulator::kill_via_adb(&self.sdk, &d, timeout)
            }
        }
    }

    fn install(&self, app_path: &std::path::Path) -> Result<()> {
        super::adb::Adb::new(self.sdk.adb.clone()).install(&self.serial(), app_path)
    }

    fn launch(&self, bundle_id: &str) -> Result<()> {
        super::adb::Adb::new(self.sdk.adb.clone()).launch(&self.serial(), bundle_id)
    }
}

impl Drop for GrpcEmulator {
    fn drop(&mut self) {
        if let Some(rt) = self.rt.take() {
            // Never blocks, and is allowed inside another runtime (unlike dropping it).
            rt.shutdown_background();
        }
    }
}
