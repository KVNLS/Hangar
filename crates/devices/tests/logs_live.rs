//! Live log-stream checks against a running device. Ignored by default.
//!
//! `HANGAR_TEST_ANDROID=<adb serial> HANGAR_TEST_IOS=<udid> cargo test -p hangar-devices \
//!  --test logs_live -- --ignored --nocapture`
//! Each test skips itself when its variable is unset. Nothing is booted or shut down here.

use std::process::Command;
use std::time::{Duration, Instant};

use hangar_devices::{LogFilter, LogLevel, LogStream};

fn wait_for(
    stream: &LogStream,
    needle: &str,
    within: Duration,
) -> Option<hangar_devices::LogEntry> {
    let end = Instant::now() + within;
    while Instant::now() < end {
        if let Some(e) = stream.recv_timeout(Duration::from_millis(100))
            && e.message == needle
        {
            return Some(e);
        }
    }
    None
}

#[test]
#[ignore = "needs HANGAR_TEST_ANDROID=<serial> and a running device"]
fn android_logcat_delivers_a_marker() {
    let Ok(serial) = std::env::var("HANGAR_TEST_ANDROID") else {
        return;
    };
    let filter = LogFilter {
        min_level: Some(LogLevel::Info),
        ..LogFilter::default()
    };
    let stream = hangar_devices::android::logcat(&serial, &filter).unwrap();
    std::thread::sleep(Duration::from_secs(1));
    let mut lat = Vec::new();
    for i in 0..5 {
        let marker = format!("hangar-probe-{i}");
        let sent = Instant::now();
        let status = Command::new(hangar_devices::android::AndroidSdk::locate().unwrap().adb)
            .args([
                "-s",
                &serial,
                "shell",
                "log",
                "-p",
                "i",
                "-t",
                "HangarProbe",
                &marker,
            ])
            .status()
            .unwrap();
        assert!(status.success());
        let done = Instant::now();
        let e = wait_for(&stream, &marker, Duration::from_secs(5)).expect("marker not seen");
        assert_eq!(e.tag, "HangarProbe");
        assert_eq!(e.level, LogLevel::Info);
        lat.push((
            e.received_at.duration_since(sent),
            e.received_at.saturating_duration_since(done),
        ));
    }
    println!("android latency (incl. adb shell spawn, after shell returned): {lat:?}");
    drop(stream);
}

/// Event time of an iOS entry (`YYYY-MM-DD HH:MM:SS.ffffff+ZZZZ`) as microseconds since the epoch.
#[cfg(target_os = "macos")]
fn ios_epoch_micros(ts: &str) -> Option<i64> {
    let (y, rest) = ts.split_once('-')?;
    let (mo, rest) = rest.split_once('-')?;
    let (d, rest) = rest.split_once(' ')?;
    let (h, rest) = rest.split_once(':')?;
    let (mi, rest) = rest.split_once(':')?;
    let (sec, off) = rest.split_at(rest.find(['+', '-'])?);
    let (whole, frac) = sec.split_once('.')?;
    let (y, mo, d): (i64, i64, i64) = (y.parse().ok()?, mo.parse().ok()?, d.parse().ok()?);
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let sign = if off.starts_with('-') { -1 } else { 1 };
    let off_s =
        sign * (off[1..3].parse::<i64>().ok()? * 3600 + off[3..5].parse::<i64>().ok()? * 60);
    let secs = days * 86_400
        + h.parse::<i64>().ok()? * 3600
        + mi.parse::<i64>().ok()? * 60
        + whole.parse::<i64>().ok()?
        - off_s;
    Some(secs * 1_000_000 + format!("{frac:0<6}")[..6].parse::<i64>().ok()?)
}

#[test]
#[ignore = "needs HANGAR_TEST_IOS=<udid> and a booted simulator"]
#[cfg(target_os = "macos")]
fn ios_log_stream_delivers_entries() {
    let Ok(udid) = std::env::var("HANGAR_TEST_IOS") else {
        return;
    };
    let stream = hangar_devices::ios::log_stream(&udid, &LogFilter::default()).unwrap();
    let first = stream
        .recv_timeout(Duration::from_secs(30))
        .expect("no log entry within 30s");
    println!("first iOS entry: {first:?}");
    assert!(first.pid.is_some() && !first.timestamp.is_empty());
    let mut lat_ms = Vec::new();
    while let Some(e) = stream.recv_timeout(Duration::from_secs(2)) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as i64;
        if let Some(t) = ios_epoch_micros(&e.timestamp) {
            lat_ms.push((now - t) as f64 / 1000.0);
        }
        if lat_ms.len() >= 300 {
            break;
        }
    }
    lat_ms.sort_by(f64::total_cmp);
    println!(
        "{} entries, dropped {}, event-to-poll latency ms: p50 {:.1} p95 {:.1} max {:.1}",
        lat_ms.len(),
        stream.dropped(),
        lat_ms[lat_ms.len() / 2],
        lat_ms[lat_ms.len() * 95 / 100],
        lat_ms[lat_ms.len() - 1]
    );
    drop(stream);
}
