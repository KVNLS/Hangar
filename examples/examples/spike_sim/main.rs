//! Spike B: live iOS Simulator frames in a GPUI window, click to tap.
//!
//! `cargo run -p hangar-examples --example spike_sim -- [--backend private|simctl] [--udid <UDID>]
//!  [--bench <secs>] [--out <dir>]`
//!
//! `HANGAR_SIM_STATS=1` logs received/painted fps and capture→paint latency to stderr.

#[cfg(target_os = "macos")]
mod bench;
#[cfg(target_os = "macos")]
mod view;

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    use std::sync::Arc;

    use anyhow::{Context as _, bail};
    use hangar_devices::Device;
    use hangar_devices::ios::{self, Backend};

    let mut backend = Backend::Private;
    let mut udid: Option<String> = None;
    let mut bench: Option<u64> = None;
    let mut one_input: Option<hangar_devices::Input> = None;
    let mut lifecycle: Vec<(String, String)> = Vec::new();
    let mut list = false;
    let mut out = std::env::temp_dir().join("hangar-spike-b");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--backend" => {
                backend = args
                    .next()
                    .context("--backend needs a value")?
                    .parse()
                    .map_err(anyhow::Error::msg)?;
            }
            "--udid" => udid = Some(args.next().context("--udid needs a value")?),
            "--bench" => bench = Some(args.next().context("--bench needs seconds")?.parse()?),
            "--tap" => {
                let v = args.next().context("--tap needs X,Y (points)")?;
                let (x, y) = v.split_once(',').context("--tap needs X,Y")?;
                one_input = Some(hangar_devices::Input::Tap {
                    x: x.trim().parse()?,
                    y: y.trim().parse()?,
                });
            }
            "--home" => one_input = Some(hangar_devices::Input::Home),
            "--list" => list = true,
            "--boot" | "--shutdown" | "--install" | "--launch" => {
                let v = args.next().with_context(|| format!("{a} needs a value"))?;
                lifecycle.push((a.clone(), v));
            }
            "--out" => out = args.next().context("--out needs a directory")?.into(),
            "-h" | "--help" => {
                println!(
                    "spike_sim [--backend private|simctl] [--udid UDID] [--bench SECS | --tap X,Y | --home] [--out DIR]"
                );
                return Ok(());
            }
            other => bail!("unknown argument {other:?}"),
        }
    }

    if list {
        for d in hangar_devices::simctl::list_all()? {
            let screen = d.screen.map_or_else(
                || "?".to_owned(),
                |s| {
                    format!(
                        "{}x{}@{}x ({}x{} pt)",
                        s.width_px,
                        s.height_px,
                        s.scale,
                        s.width_pt(),
                        s.height_pt()
                    )
                },
            );
            println!(
                "{}  {:<14} {:<26} {}  {screen}",
                d.udid, d.state, d.name, d.runtime
            );
        }
        return Ok(());
    }
    // Lifecycle commands act on --udid (or the first booted simulator for install/launch).
    if !lifecycle.is_empty() {
        let target = match &udid {
            Some(u) => u.clone(),
            None => hangar_devices::simctl::list_booted()?
                .into_iter()
                .next()
                .map(|d| d.udid)
                .context("--udid needed (no booted simulator)")?,
        };
        for (flag, value) in &lifecycle {
            let t0 = std::time::Instant::now();
            match flag.as_str() {
                "--boot" => ios::boot(if value == "-" { &target } else { value })?,
                "--shutdown" => ios::shutdown(if value == "-" { &target } else { value })?,
                "--install" => {
                    hangar_devices::simctl::install(&target, std::path::Path::new(value))?
                }
                _ => {
                    let pid = hangar_devices::simctl::launch(&target, value)?;
                    println!("pid {pid:?}");
                }
            }
            println!("{flag} {value}: ok in {:.1}s", t0.elapsed().as_secs_f64());
        }
        return Ok(());
    }

    let booted = hangar_devices::simctl::list_booted()?;
    for d in &booted {
        eprintln!("booted: {} ({}) {}", d.name, d.runtime, d.udid);
    }
    let info = match &udid {
        Some(u) => booted
            .iter()
            .find(|d| d.udid.eq_ignore_ascii_case(u))
            .cloned()
            .with_context(|| format!("simulator {u} is not booted"))?,
        None => booted.first().cloned().context(
            "no booted simulator; boot one with `xcrun simctl boot <UDID>` (see `xcrun simctl list devices`)",
        )?,
    };

    let device: Arc<dyn Device> = Arc::from(ios::open(info.clone(), backend)?);
    if let Some(input) = one_input {
        return bench::input_once(device.as_ref(), input, &out);
    }
    match bench {
        Some(secs) => {
            // The screen is animated with private HID swipes for both backends (simctl has no input).
            let animator: Option<Arc<dyn Device>> = if backend == Backend::Private {
                Some(device.clone())
            } else {
                match ios::open(info, Backend::Private) {
                    Ok(d) => Some(Arc::from(d)),
                    Err(e) => {
                        eprintln!("no animator: {e}");
                        None
                    }
                }
            };
            bench::run(device, animator, secs, &out)
        }
        None => view::run(device, booted),
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("spike_sim requires macOS");
}
