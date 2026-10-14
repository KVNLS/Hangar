//! Primary Android backend: frames via `streamScreenshot`, input via `sendTouch`/`sendKey`.
//! Owns its tokio runtime so callers need none.

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use tokio::runtime::Runtime;

use super::emulator::{self, BootOptions, RunningEmulator};
use super::grpc::{self, Client, keys, proto};
use super::sdk::AndroidSdk;
use crate::geometry::device_to_pixels;
use crate::{
    Device, DeviceInfo, Error, Frame, FrameReceiver, FrameSender, Input, Platform, Result,
    ScreenSpec,
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
    frames: FrameReceiver,
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
        let (tx, frames) = tokio::sync::watch::channel(None);
        let conn = Arc::new(RwLock::new(Some(Conn {
            client,
            running: running.clone(),
        })));
        rt.spawn(stream_frames(conn.clone(), tx, size));
        Ok(Self {
            info: running.info.clone(),
            screen,
            avd_id: running.discovery.avd_id.clone(),
            sdk: sdk.clone(),
            boot_options: BootOptions::default(),
            conn,
            frames,
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
        device_to_pixels(x, y, &self.screen)
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

async fn stream_frames(conn: Arc<RwLock<Option<Conn>>>, tx: FrameSender, size: FrameSize) {
    let mut seq = 0u64;
    loop {
        if tx.is_closed() {
            return;
        }
        let client = conn
            .read()
            .ok()
            .and_then(|c| c.as_ref().map(|c| c.client.clone()));
        let Some(mut client) = client else {
            tokio::time::sleep(Duration::from_millis(250)).await;
            continue;
        };
        let request = proto::ImageFormat {
            format: proto::image_format::ImgFormat::Rgba8888 as i32,
            width: size.width,
            height: size.height,
            ..Default::default()
        };
        match client.stream_screenshot(request).await {
            Ok(resp) => {
                let mut stream = resp.into_inner();
                loop {
                    match stream.message().await {
                        Ok(Some(img)) => {
                            let captured_at = produced_at(img.timestamp_us);
                            let (w, h) =
                                img.format.as_ref().map_or((0, 0), |f| (f.width, f.height));
                            // An inactive display sends empty images.
                            if w == 0 || h == 0 {
                                continue;
                            }
                            let got = img.image.len();
                            let Some(bgra) = grpc::rgba_to_bgra(img.image, w, h) else {
                                tracing::warn!("short frame: {got} bytes for {w}x{h}");
                                continue;
                            };
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
                            break;
                        }
                    }
                }
            }
            Err(s) => tracing::debug!("streamScreenshot: {s}"),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
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

    fn backend(&self) -> &'static str {
        "grpc"
    }

    fn platform(&self) -> Platform {
        Platform::Android
    }

    fn frames(&self) -> FrameReceiver {
        self.frames.clone()
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
            Input::Home | Input::Back => {
                let code = if input == Input::Home {
                    keys::HOME
                } else {
                    keys::BACK
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
        let running = emulator::boot(&self.sdk, &self.avd_id, &self.boot_options)?;
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
