//! Prints a device log stream.
//!
//! `cargo run -p hangar-examples --example device_logs -- (--android SERIAL | --ios UDID)
//!  [--level debug|info|warn|error|fault] [--process NAME] [--history] [--seconds N]`
//!
//! Runs until Ctrl-C, or for `--seconds`. Reports dropped entries at the end.

use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use hangar_devices::{LogFilter, LogLevel, LogStream};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut filter = LogFilter::default();
    let (mut android, mut ios, mut seconds) = (None, None, None::<u64>);
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().with_context(|| format!("{name} needs a value"));
        match a.as_str() {
            "--android" => android = Some(value("--android")?),
            "--ios" => ios = Some(value("--ios")?),
            "--level" => {
                filter.min_level = Some(
                    value("--level")?
                        .parse::<LogLevel>()
                        .map_err(anyhow::Error::msg)?,
                );
            }
            "--process" => filter.process = Some(value("--process")?),
            "--seconds" => seconds = Some(value("--seconds")?.parse()?),
            "--history" => filter.history = true,
            other => bail!("unknown argument {other:?}"),
        }
    }
    let stream: LogStream = match (android, ios) {
        (Some(serial), None) => hangar_devices::android::logcat(&serial, &filter)?,
        #[cfg(target_os = "macos")]
        (None, Some(udid)) => hangar_devices::ios::log_stream(&udid, &filter)?,
        #[cfg(not(target_os = "macos"))]
        (None, Some(_)) => bail!("iOS needs macOS"),
        _ => bail!("pass exactly one of --android SERIAL or --ios UDID"),
    };

    let deadline = seconds.map(|s| Instant::now() + Duration::from_secs(s));
    let mut count = 0u64;
    while deadline.is_none_or(|d| Instant::now() < d) {
        let Some(e) = stream.recv_timeout(Duration::from_millis(200)) else {
            if stream.is_closed() {
                break;
            }
            continue;
        };
        count += 1;
        let who = if e.process.is_empty() {
            String::new()
        } else {
            format!("{}[{}] ", e.process, e.pid.unwrap_or(0))
        };
        println!(
            "{} {:<7} {who}{}: {}",
            e.timestamp, e.level, e.tag, e.message
        );
    }
    eprintln!("{count} entries, {} dropped", stream.dropped());
    if let Some(err) = stream.error() {
        bail!("log process ended: {err}");
    }
    Ok(())
}
