//! Headless measurements: fps idle / while scrolling, tap → first changed frame, CPU.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use hangar_devices::android::{AndroidSdk, adb::Adb};
use hangar_devices::{Device, Frame, FrameReceiver, Input, SavePng as _};

/// Photos icon on the default Pixel 10 Pro (API 37) home screen, in dp (426.7x952 dp screen).
const TAP_TARGET: (f64, f64) = (263.0, 664.0);
const TAP_PACKAGE: &str = "com.google.android.apps.photos";
const SETTINGS_PACKAGE: &str = "com.android.settings";
/// Pixels-changed ratio above which a frame counts as "changed" (ignores the clock).
const CHANGE_THRESHOLD: f64 = 0.002;

pub fn run(
    sdk: &AndroidSdk,
    device: Arc<dyn Device>,
    animator: Option<Arc<dyn Device>>,
    emulator_pid: u32,
    secs: u64,
    out_dir: &Path,
) -> Result<()> {
    std::fs::create_dir_all(out_dir)?;
    let adb = Adb::new(sdk.adb.clone());
    let info = device.info().clone();
    println!(
        "bench: backend={} device={} ({}) serial={} screen={:?}",
        device.backend(),
        info.name,
        info.runtime,
        info.udid,
        info.screen
    );
    let serial = info.udid.clone();
    go_home(&adb, &serial, device.as_ref());
    std::thread::sleep(Duration::from_millis(1500));

    println!("\n[1] idle ({secs}s, home screen, no input)");
    let cpu = Cpu::start(emulator_pid);
    measure_fps(&device.frames(), Duration::from_secs(secs));
    cpu.report();

    println!("\n[1b] idle ({secs}s, static Settings screen, no input)");
    let _ = adb.shell(&serial, "am start -W -a android.settings.SETTINGS");
    std::thread::sleep(Duration::from_millis(1500));
    let cpu = Cpu::start(emulator_pid);
    measure_fps(&device.frames(), Duration::from_secs(secs));
    cpu.report();

    match &animator {
        Some(anim) => {
            println!("\n[2] scrolling the Settings list ({secs}s, swipes via gRPC sendTouch)");
            let stop = Arc::new(AtomicBool::new(false));
            let (anim, anim_stop) = (anim.clone(), stop.clone());
            let swiper = std::thread::spawn(move || swipe_loop(anim.as_ref(), &anim_stop));
            let cpu = Cpu::start(emulator_pid);
            measure_fps(&device.frames(), Duration::from_secs(secs));
            cpu.report();
            stop.store(true, Ordering::Relaxed);
            let _ = swiper.join();
            go_home(&adb, &serial, device.as_ref());
            std::thread::sleep(Duration::from_millis(1500));
        }
        None => println!("\n[2] skipped: no gRPC animator"),
    }

    println!("\n[3] tap Photos icon {TAP_TARGET:?} dp → first changed frame (3 runs)");
    let mut ok_runs = 0;
    for run in 1..=3 {
        match tap_latency(device.as_ref(), out_dir, run) {
            Ok(()) => ok_runs += 1,
            Err(e) => println!("  run {run}: FAILED: {e:#}"),
        }
        go_home(&adb, &serial, device.as_ref());
        std::thread::sleep(Duration::from_millis(2000));
    }
    if ok_runs == 0 {
        bail!("no tap run succeeded");
    }
    Ok(())
}

fn go_home(adb: &Adb, serial: &str, device: &dyn Device) {
    for pkg in [TAP_PACKAGE, SETTINGS_PACKAGE] {
        let _ = adb.shell(serial, &format!("am force-stop {pkg}"));
    }
    if let Err(e) = device.send_input(Input::Home) {
        println!("  (home press failed: {e})");
    }
}

fn swipe_loop(device: &dyn Device, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        swipe(device, 750.0, 250.0);
        swipe(device, 250.0, 750.0);
    }
}

fn swipe(device: &dyn Device, from: f64, to: f64) {
    let x = 213.0;
    let _ = device.send_input(Input::TouchDown { x, y: from });
    for i in 1..=18 {
        let y = from + (to - from) * f64::from(i) / 18.0;
        std::thread::sleep(Duration::from_millis(16));
        let _ = device.send_input(Input::TouchMove { x, y });
    }
    let _ = device.send_input(Input::TouchUp { x, y: to });
    std::thread::sleep(Duration::from_millis(250));
}

/// CPU time of the emulator process tree and of this process over a phase (`ps -o time`).
struct Cpu {
    pid: u32,
    start: Instant,
    emu: Option<f64>,
    me: Option<f64>,
}

impl Cpu {
    fn start(pid: u32) -> Self {
        let (emu, me) = cpu_seconds(pid);
        Self {
            pid,
            start: Instant::now(),
            emu,
            me,
        }
    }

    fn report(&self) {
        let wall = self.start.elapsed().as_secs_f64();
        let (emu, me) = cpu_seconds(self.pid);
        let pct = |a: Option<f64>, b: Option<f64>| match (a, b) {
            (Some(a), Some(b)) => format!("{:.0}%", (b - a) / wall * 100.0),
            _ => "?".to_owned(),
        };
        println!(
            "  CPU (100% = one core): emulator tree {}, this process {}",
            pct(self.emu, emu),
            pct(self.me, me)
        );
    }
}

/// (emulator pid + descendants, this process) cumulative CPU seconds.
fn cpu_seconds(emu_pid: u32) -> (Option<f64>, Option<f64>) {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,time="])
        .output()
    else {
        return (None, None);
    };
    let rows: Vec<(u32, u32, f64)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((
                it.next()?.parse().ok()?,
                it.next()?.parse().ok()?,
                parse_cpu_time(it.next()?)?,
            ))
        })
        .collect();
    let mut tree = vec![emu_pid];
    let mut i = 0;
    while i < tree.len() {
        let parent = tree[i];
        tree.extend(rows.iter().filter(|r| r.1 == parent).map(|r| r.0));
        i += 1;
    }
    let sum = |pids: &[u32]| -> Option<f64> {
        let v: Vec<f64> = rows
            .iter()
            .filter(|r| pids.contains(&r.0))
            .map(|r| r.2)
            .collect();
        (!v.is_empty()).then(|| v.iter().sum())
    };
    (sum(&tree), sum(&[std::process::id()]))
}

/// `ps` time: `[[D-]HH:]MM:SS.ss`.
fn parse_cpu_time(s: &str) -> Option<f64> {
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<f64>().ok()?, r),
        None => (0.0, s),
    };
    let mut total = 0.0;
    for part in rest.split(':') {
        total = total * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(days * 86400.0 + total)
}

fn measure_fps(rx: &FrameReceiver, dur: Duration) {
    let mut rx = rx.clone();
    rx.mark_changed();
    let start = Instant::now();
    let (mut received, mut changed, mut skipped) = (0u64, 0u64, 0u64);
    let mut latencies = Vec::new();
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
            latencies.push(frame.captured_at.elapsed());
        }
        if let Some(p) = &prev {
            skipped += frame.seq.saturating_sub(p.seq + 1);
            if frame.diff_ratio(p) > 0.0 {
                changed += 1;
            }
        }
        received += 1;
        prev = Some(frame);
    }
    let secs = dur.as_secs_f64();
    latencies.sort();
    let pct = |p: f64| -> f64 {
        latencies
            .get(((latencies.len() as f64 - 1.0) * p).round() as usize)
            .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
    };
    println!(
        "  received {:.1} fps ({received} frames), content-changed {:.1} fps, dropped-before-consume {skipped}",
        received as f64 / secs,
        changed as f64 / secs,
    );
    println!(
        "  capture→consumer latency ms: p50 {:.2}  p95 {:.2}  max {:.2}",
        pct(0.5),
        pct(0.95),
        pct(1.0)
    );
}

fn latest(rx: &mut FrameReceiver) -> Option<Frame> {
    rx.borrow_and_update().clone()
}

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
    let baseline = wait_stable(&mut rx, Duration::from_millis(700), Duration::from_secs(8))?;
    baseline.save_png(&out_dir.join(format!("tap{run}_before.png")))?;
    let (x, y) = TAP_TARGET;
    let t0 = Instant::now();
    let (sent, first_change) = std::thread::scope(|s| {
        let tap = s.spawn(|| {
            let r = device.send_input(Input::Tap { x, y });
            (r, t0.elapsed())
        });
        let mut first_change: Option<(Duration, Duration, f64)> = None;
        while t0.elapsed() < Duration::from_secs(5) {
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
    std::thread::sleep(Duration::from_millis(2000));
    let after = latest(&mut rx).context("no frame after tap")?;
    after.save_png(&out_dir.join(format!("tap{run}_after.png")))?;
    let final_diff = after.diff_ratio(&baseline);
    match first_change {
        Some((capture, seen, d)) => println!(
            "  run {run}: tap call {:.1} ms; first changed frame captured +{:.1} ms, seen +{:.1} ms \
             (diff {:.1}%); after 2 s diff {:.1}%",
            ms(send_done),
            ms(capture),
            ms(seen),
            d * 100.0,
            final_diff * 100.0
        ),
        None => bail!(
            "no changed frame within 5 s (tap call {:.1} ms, final diff {:.1}%)",
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
    let before = wait_stable(&mut rx, Duration::from_millis(500), Duration::from_secs(5))?;
    before.save_png(&out_dir.join("input_before.png"))?;
    let t0 = Instant::now();
    device.send_input(input).context("send_input")?;
    println!("{input:?} sent in {:.1} ms", ms(t0.elapsed()));
    std::thread::sleep(Duration::from_millis(2000));
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

#[cfg(test)]
mod tests {
    #[test]
    fn cpu_time_formats() {
        assert_eq!(super::parse_cpu_time("0:01.50"), Some(1.5));
        assert_eq!(super::parse_cpu_time("1:02:03.00"), Some(3723.0));
        assert_eq!(super::parse_cpu_time("1-00:00:01.00"), Some(86401.0));
    }
}
