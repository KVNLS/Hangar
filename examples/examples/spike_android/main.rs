//! M4 spike: live Android emulator frames in a GPUI window, click to tap; headless bench.
//!
//! `cargo run -p hangar-examples --example spike_android -- [--backend grpc|adb] [--serial S | --avd ID]
//!  [--boot AVD [--cold] [--window host|none|hidden]] [--bench SECS | --tap X,Y | --back | --home | --text T]
//!  [--capture raw|png] [--out DIR] [--shutdown] [--relaunch]`
//!
//! Coordinates are dp (Android's points). `HANGAR_SIM_STATS=1` logs fps and capture→paint latency.

mod bench;
#[cfg(target_os = "macos")]
#[path = "../spike_sim/view.rs"]
mod view;

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context as _, bail};
use hangar_devices::android::{
    self, AndroidSdk, Backend, BootOptions, Capture, RunningEmulator, WindowMode, adb::Adb,
};
use hangar_devices::{Device, Input, Orientation};

fn main() -> anyhow::Result<()> {
    let mut backend = Backend::Grpc;
    let mut select: Option<String> = None;
    let mut boot: Option<String> = None;
    let mut opts = BootOptions::default();
    let mut bench: Option<u64> = None;
    let mut one_input: Option<Input> = None;
    let mut text: Option<String> = None;
    let mut capture = Capture::Png;
    let mut shutdown = false;
    let mut relaunch = false;
    let mut rotate: Option<Orientation> = None;
    let mut out = std::env::temp_dir().join("hangar-spike-android");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--backend" => {
                backend = args
                    .next()
                    .context("--backend needs grpc|adb")?
                    .parse()
                    .map_err(anyhow::Error::msg)?;
            }
            "--serial" | "--avd" => select = Some(args.next().context("needs a value")?),
            "--boot" => boot = Some(args.next().context("--boot needs an AVD id")?),
            "--cold" => opts.cold_boot = true,
            "--emu-arg" => opts
                .extra_args
                .push(args.next().context("--emu-arg needs a value")?),
            "--window" => {
                opts.window = match args.next().as_deref() {
                    Some("host") => WindowMode::NoWindowHostGpu,
                    Some("none") => WindowMode::NoWindow,
                    Some("hidden") => WindowMode::HiddenWindow,
                    other => bail!("--window host|none|hidden, got {other:?}"),
                }
            }
            "--bench" => bench = Some(args.next().context("--bench needs seconds")?.parse()?),
            "--tap" => {
                let v = args.next().context("--tap needs X,Y (dp)")?;
                let (x, y) = v.split_once(',').context("--tap needs X,Y")?;
                one_input = Some(Input::Tap {
                    x: x.trim().parse()?,
                    y: y.trim().parse()?,
                });
            }
            "--home" => one_input = Some(Input::Home),
            "--back" => one_input = Some(Input::Back),
            "--text" => text = Some(args.next().context("--text needs a value")?),
            "--capture" => {
                capture = match args.next().as_deref() {
                    Some("raw") => Capture::Raw,
                    Some("png") => Capture::Png,
                    other => bail!("--capture raw|png, got {other:?}"),
                }
            }
            "--shutdown" => shutdown = true,
            "--relaunch" => relaunch = true,
            "--rotate" => {
                rotate = Some(match args.next().as_deref() {
                    Some("portrait") => Orientation::Portrait,
                    Some("left") => Orientation::LandscapeLeft,
                    Some("right") => Orientation::LandscapeRight,
                    Some("upside-down") => Orientation::PortraitUpsideDown,
                    other => bail!("--rotate portrait|left|right|upside-down, got {other:?}"),
                })
            }
            "--out" => out = args.next().context("--out needs a directory")?.into(),
            "-h" | "--help" => {
                println!(
                    "spike_android [--backend grpc|adb] [--serial S | --avd ID] [--boot AVD [--cold] \
                     [--window host|none|hidden]] [--bench SECS | --tap X,Y | --back | --home | --text T] \
                     [--capture raw|png] [--out DIR] [--shutdown] [--relaunch] \
                     [--rotate portrait|left|right|upside-down]"
                );
                return Ok(());
            }
            other => bail!("unknown argument {other:?}"),
        }
    }

    let sdk = AndroidSdk::locate()?;
    eprintln!("sdk: {}", sdk.root.display());
    for avd in android::list_avds(&sdk)? {
        eprintln!(
            "avd: {} ({}) {} {:?}",
            avd.id,
            avd.name,
            avd.target.as_deref().unwrap_or("?"),
            avd.screen
        );
    }

    let mut running = android::list_running(&sdk)?;
    if let Some(avd) = &boot
        && !running.iter().any(|r| &r.discovery.avd_id == avd)
    {
        eprintln!(
            "booting {avd} ({:?}, cold={})…",
            opts.window, opts.cold_boot
        );
        let t0 = Instant::now();
        let r = android::boot_with(&sdk, avd, &opts)?;
        println!(
            "boot: {avd} booted as {} in {:.1} s (window={:?}, cold={})",
            r.serial(),
            t0.elapsed().as_secs_f64(),
            opts.window,
            opts.cold_boot
        );
        running = android::list_running(&sdk)?;
    }
    for r in &running {
        eprintln!(
            "running: {} {} [{}] {} grpc={:?} screen={:?}",
            r.serial(),
            r.info.name,
            r.info.state,
            r.info.runtime,
            r.discovery.grpc_port,
            r.info.screen
        );
    }
    let selected: RunningEmulator = match select.as_deref().or(boot.as_deref()) {
        Some(s) => running
            .iter()
            .find(|r| r.serial() == s || r.discovery.avd_id == s)
            .cloned()
            .with_context(|| format!("{s} is not running (use --boot <AVD>)"))?,
        None => running
            .first()
            .cloned()
            .context("no running emulator; start one with --boot <AVD>")?,
    };
    let pid = selected.discovery.pid;

    let device: Arc<dyn Device> = match backend {
        Backend::Grpc => Arc::new(android::GrpcEmulator::open(&sdk, selected.clone())?),
        Backend::Adb => Arc::new(android::AdbDevice::open(
            Adb::new(sdk.adb.clone()),
            selected.info.clone(),
            capture,
        )?),
    };

    let result = if let Some(o) = rotate {
        rotate_once(device.as_ref(), o, &out)
    } else if let Some(t) = text {
        let t0 = Instant::now();
        device.send_text(&t).context("send_text")?;
        println!("text sent in {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0);
        Ok(())
    } else if let Some(input) = one_input {
        bench::input_once(device.as_ref(), input, &out)
    } else if let Some(secs) = bench {
        // The screen is animated with gRPC swipes for both backends (consistent and fast).
        let animator: Option<Arc<dyn Device>> = if backend == Backend::Grpc {
            Some(device.clone())
        } else {
            match android::GrpcEmulator::open(&sdk, selected.clone()) {
                Ok(d) => Some(Arc::new(d)),
                Err(e) => {
                    eprintln!("no gRPC animator: {e}");
                    None
                }
            }
        };
        bench::run(&sdk, device.clone(), animator, pid, secs, &out)
    } else {
        run_view(
            device.clone(),
            running.iter().map(|r| r.info.clone()).collect(),
        )
    };

    if relaunch {
        // Device::shutdown then Device::boot on the same handle; frames must resume.
        let t0 = Instant::now();
        device.shutdown().context("shutdown")?;
        println!("relaunch: shut down in {:.1} s", t0.elapsed().as_secs_f64());
        let t0 = Instant::now();
        device.boot().context("boot")?;
        println!("relaunch: booted in {:.1} s", t0.elapsed().as_secs_f64());
        let rx = device.frames();
        let t0 = Instant::now();
        while rx.borrow().is_none() || rx.borrow().as_ref().is_some_and(|f| f.captured_at < t0) {
            if t0.elapsed() > std::time::Duration::from_secs(10) {
                bail!("no new frame 10 s after relaunch");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        println!(
            "relaunch: first new frame after {:.0} ms",
            t0.elapsed().as_secs_f64() * 1000.0
        );
    }
    if shutdown {
        let t0 = Instant::now();
        device.shutdown().context("shutdown")?;
        println!(
            "shutdown: {} stopped in {:.1} s",
            selected.serial(),
            t0.elapsed().as_secs_f64()
        );
    }
    result
}

#[cfg(target_os = "macos")]
fn run_view(
    device: Arc<dyn Device>,
    running: Vec<hangar_devices::DeviceInfo>,
) -> anyhow::Result<()> {
    view::run(device, running)
}

#[cfg(not(target_os = "macos"))]
fn run_view(_: Arc<dyn Device>, _: Vec<hangar_devices::DeviceInfo>) -> anyhow::Result<()> {
    anyhow::bail!("the window is macOS-only here; use --bench / --tap")
}

/// Rotates, then reports the guest orientation and the size of a frame captured afterwards.
fn rotate_once(
    device: &dyn Device,
    target: Orientation,
    out: &std::path::Path,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(out)?;
    let mut rx = device.frames();
    let t0 = Instant::now();
    device.set_orientation(target).context("set_orientation")?;
    println!(
        "rotated to {target:?} in {:.0} ms",
        t0.elapsed().as_secs_f64() * 1000.0
    );
    println!(
        "orientation(): {:?}",
        device.orientation().context("orientation")?
    );
    let want_landscape = target.is_landscape();
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(f) = rx.borrow_and_update().clone()
            && (f.width > f.height) == want_landscape
        {
            println!("frame {}x{}", f.width, f.height);
            f.save_png(&out.join(format!("rotate_{target:?}.png")))?;
            return Ok(());
        }
        if Instant::now() > deadline {
            bail!(
                "no {} frame within 5 s",
                if want_landscape {
                    "landscape"
                } else {
                    "portrait"
                }
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
