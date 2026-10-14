//! Headless measurements: frame rate under animation and tap → first changed frame latency.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use hangar_devices::{Device, Frame, FrameReceiver, Input};

/// Settings icon on the default iOS 27 iPhone 17 home screen, in points (402x874 pt screen).
const SETTINGS_ICON: (f64, f64) = (339.0, 422.0);
const SETTINGS_BUNDLE: &str = "com.apple.Preferences";
/// Pixels-changed ratio above which a frame counts as "changed" (ignores clock ticks).
const CHANGE_THRESHOLD: f64 = 0.002;

pub fn run(
    device: Arc<dyn Device>,
    animator: Option<Arc<dyn Device>>,
    secs: u64,
    out_dir: &Path,
) -> Result<()> {
    std::fs::create_dir_all(out_dir)?;
    let info = device.info().clone();
    println!(
        "bench: backend={} device={} ({}, {}) udid={}",
        device.backend(),
        info.name,
        info.runtime,
        info.device_type.as_deref().unwrap_or("?"),
        info.udid
    );

    go_home(&info.udid, animator.as_deref());
    std::thread::sleep(Duration::from_millis(1500));

    println!("\n[1] idle frame rate ({secs}s, no input)");
    measure_fps(&device.frames(), Duration::from_secs(secs));

    match &animator {
        Some(anim) => {
            println!(
                "\n[2] frame rate while swiping home-screen pages ({secs}s, swipes via private HID)"
            );
            let stop = Arc::new(AtomicBool::new(false));
            let anim = anim.clone();
            let anim_stop = stop.clone();
            let swiper = std::thread::spawn(move || swipe_loop(anim.as_ref(), &anim_stop));
            measure_fps(&device.frames(), Duration::from_secs(secs));
            stop.store(true, Ordering::Relaxed);
            let _ = swiper.join();
            go_home(&info.udid, animator.as_deref());
            std::thread::sleep(Duration::from_millis(1500));
        }
        None => println!("\n[2] skipped: no private HID available to animate the screen"),
    }

    println!("\n[3] tap Settings icon → first changed frame (3 runs)");
    let mut ok_runs = 0;
    for run in 1..=3 {
        match tap_latency(device.as_ref(), out_dir, run) {
            Ok(()) => ok_runs += 1,
            Err(e) => println!("  run {run}: FAILED: {e:#}"),
        }
        go_home(&info.udid, animator.as_deref());
        std::thread::sleep(Duration::from_millis(2000));
    }
    if ok_runs == 0 {
        bail!("no tap run succeeded");
    }
    Ok(())
}

/// Back to the first home-screen page: quit Settings, then press Home (private HID) if available.
fn go_home(udid: &str, hid: Option<&dyn Device>) {
    let _ = std::process::Command::new("xcrun")
        .args(["simctl", "terminate", udid, SETTINGS_BUNDLE])
        .output();
    if let Some(hid) = hid {
        std::thread::sleep(Duration::from_millis(800));
        if let Err(e) = hid.send_input(Input::Home) {
            println!("  (home press failed: {e})");
        }
    }
}

fn swipe_loop(device: &dyn Device, stop: &AtomicBool) {
    // Always swipe to page 2 and back so the run ends on the first home-screen page.
    while !stop.load(Ordering::Relaxed) {
        swipe(device, 320.0, 80.0);
        swipe(device, 80.0, 320.0);
    }
}

fn swipe(device: &dyn Device, from: f64, to: f64) {
    let y = 300.0;
    let _ = device.send_input(Input::TouchDown { x: from, y });
    for i in 1..=18 {
        let x = from + (to - from) * f64::from(i) / 18.0;
        std::thread::sleep(Duration::from_millis(16));
        let _ = device.send_input(Input::TouchMove { x, y });
    }
    let _ = device.send_input(Input::TouchUp { x: to, y });
    std::thread::sleep(Duration::from_millis(250));
}

struct FpsStats {
    received: u64,
    changed: u64,
    skipped: u64,
    latencies: Vec<Duration>,
}

fn measure_fps(rx: &FrameReceiver, dur: Duration) -> FpsStats {
    let mut rx = rx.clone();
    rx.mark_changed();
    let start = Instant::now();
    let mut stats = FpsStats {
        received: 0,
        changed: 0,
        skipped: 0,
        latencies: Vec::new(),
    };
    let mut prev: Option<Frame> = None;
    while start.elapsed() < dur {
        if !rx.has_changed().unwrap_or(false) {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        let Some(frame) = rx.borrow_and_update().clone() else {
            continue;
        };
        if frame.captured_at >= start {
            stats.latencies.push(frame.captured_at.elapsed());
        }
        if let Some(p) = &prev {
            stats.skipped += frame.seq.saturating_sub(p.seq + 1);
            if frame.diff_ratio(p) > 0.0 {
                stats.changed += 1;
            }
        }
        stats.received += 1;
        prev = Some(frame);
    }
    let secs = dur.as_secs_f64();
    stats.latencies.sort();
    let pct = |p: f64| -> f64 {
        stats
            .latencies
            .get(((stats.latencies.len() as f64 - 1.0) * p).round() as usize)
            .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
    };
    println!(
        "  received {:.1} fps ({} frames), content-changed {:.1} fps, dropped-before-consume {}",
        stats.received as f64 / secs,
        stats.received,
        stats.changed as f64 / secs,
        stats.skipped
    );
    println!(
        "  capture→consumer latency ms: p50 {:.2}  p95 {:.2}  max {:.2}",
        pct(0.5),
        pct(0.95),
        pct(1.0)
    );
    stats
}

fn latest(rx: &mut FrameReceiver) -> Option<Frame> {
    rx.borrow_and_update().clone()
}

/// Waits until the screen has been visually stable for `quiet`, returns the last frame.
fn wait_stable(rx: &mut FrameReceiver, quiet: Duration, timeout: Duration) -> Result<Frame> {
    let start = Instant::now();
    let mut base = loop {
        if let Some(f) = latest(rx) {
            break f;
        }
        if start.elapsed() > timeout {
            bail!("no frame within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut since = Instant::now();
    while start.elapsed() < timeout {
        std::thread::sleep(Duration::from_millis(5));
        if rx.has_changed().unwrap_or(false)
            && let Some(f) = latest(rx)
        {
            if f.diff_ratio(&base) > CHANGE_THRESHOLD {
                since = Instant::now();
            }
            base = f;
        }
        if since.elapsed() >= quiet {
            return Ok(base);
        }
    }
    Ok(base)
}

fn tap_latency(device: &dyn Device, out_dir: &Path, run: u32) -> Result<()> {
    let mut rx = device.frames();
    let baseline = wait_stable(&mut rx, Duration::from_millis(700), Duration::from_secs(5))?;
    baseline.save_png(&out_dir.join(format!("tap{run}_before.png")))?;

    let (x, y) = SETTINGS_ICON;
    let t0 = Instant::now();
    // Tap on another thread: send_input blocks for the touch hold, and frames must be watched
    // meanwhile (the channel only keeps the latest frame).
    let (sent, first_change) = std::thread::scope(|s| {
        let tap = s.spawn(|| {
            let r = device.send_input(Input::Tap { x, y });
            (r, t0.elapsed())
        });
        let mut first_change: Option<(Duration, Duration, f64)> = None;
        while t0.elapsed() < Duration::from_secs(3) {
            if rx.has_changed().unwrap_or(false)
                && let Some(f) = latest(&mut rx)
            {
                let d = f.diff_ratio(&baseline);
                if f.captured_at >= t0 && d > CHANGE_THRESHOLD {
                    first_change = Some((f.captured_at - t0, t0.elapsed(), d));
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        (tap.join(), first_change)
    });
    let (sent, send_done) = sent.map_err(|_| anyhow::anyhow!("tap thread panicked"))?;
    sent.context("send_input(Tap)")?;
    std::thread::sleep(Duration::from_millis(1500));
    let after = latest(&mut rx).context("no frame after tap")?;
    after.save_png(&out_dir.join(format!("tap{run}_after.png")))?;
    let final_diff = after.diff_ratio(&baseline);
    match first_change {
        Some((capture, seen, d)) => println!(
            "  run {run}: tap call {:.1} ms; first changed frame captured +{:.1} ms, seen +{:.1} ms \
             (diff {:.1}%); after 1.5 s diff {:.1}%",
            ms(send_done),
            ms(capture),
            ms(seen),
            d * 100.0,
            final_diff * 100.0
        ),
        None => bail!(
            "no changed frame within 3 s (tap call {:.1} ms, final diff {:.1}%)",
            ms(send_done),
            final_diff * 100.0
        ),
    }
    Ok(())
}

/// One input, with before/after frames saved and the change reported (manual verification).
pub fn input_once(device: &dyn Device, input: Input, out_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(out_dir)?;
    let mut rx = device.frames();
    let before = wait_stable(&mut rx, Duration::from_millis(500), Duration::from_secs(3))?;
    before.save_png(&out_dir.join("input_before.png"))?;
    let t0 = Instant::now();
    device.send_input(input).context("send_input")?;
    println!("{input:?} sent in {:.1} ms", ms(t0.elapsed()));
    std::thread::sleep(Duration::from_millis(1500));
    let after = latest(&mut rx).context("no frame")?;
    after.save_png(&out_dir.join("input_after.png"))?;
    println!(
        "screen changed {:.1}% (frames in {})",
        after.diff_ratio(&before) * 100.0,
        out_dir.display()
    );
    Ok(())
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}
