//! Android Virtual Devices: listing and `config.ini` parsing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::sdk::AndroidSdk;
use crate::{Error, Result, ScreenSpec};

#[derive(Debug, Clone, PartialEq)]
pub struct Avd {
    /// Identifier used with `emulator -avd` (e.g. `Pixel_10_Pro`).
    pub id: String,
    /// Display name (e.g. `Pixel 10 Pro`).
    pub name: String,
    pub dir: Option<PathBuf>,
    /// `target=android-37.0` → `android-37.0`.
    pub target: Option<String>,
    pub abi: Option<String>,
    pub screen: Option<ScreenSpec>,
}

/// Android's density-independent pixel is 1/160 inch, so dp = px * 160 / dpi.
pub fn screen_from_density(width_px: u32, height_px: u32, dpi: u32) -> Option<ScreenSpec> {
    (width_px > 0 && height_px > 0 && dpi > 0).then(|| ScreenSpec {
        width_px,
        height_px,
        scale: f64::from(dpi) / 160.0,
    })
}

pub fn parse_ini(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect()
}

/// Builds an [`Avd`] from its `config.ini` contents.
pub fn from_config(id: &str, dir: Option<PathBuf>, config: &str) -> Avd {
    let kv = parse_ini(config);
    let num = |k: &str| kv.get(k).and_then(|v| v.parse::<u32>().ok());
    let screen = match (
        num("hw.lcd.width"),
        num("hw.lcd.height"),
        num("hw.lcd.density"),
    ) {
        (Some(w), Some(h), Some(d)) => screen_from_density(w, h, d),
        _ => None,
    };
    Avd {
        id: id.to_owned(),
        name: kv
            .get("avd.ini.displayname")
            .cloned()
            .unwrap_or_else(|| id.replace('_', " ")),
        dir,
        target: kv.get("target").cloned(),
        abi: kv.get("abi.type").cloned(),
        screen,
    }
}

/// What one running instance of an AVD asks of the host, from its `config.ini`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AvdCost {
    pub ram_mb: u64,
    pub cores: u32,
}

impl AvdCost {
    /// Used for keys missing from `config.ini`.
    pub const DEFAULT: Self = Self {
        ram_mb: 2048,
        cores: 2,
    };
}

/// `hw.ramSize` (MB, or with an `M`/`G` suffix) and `hw.cpu.ncore` from `config.ini` contents.
pub fn parse_cost(config: &str) -> AvdCost {
    let kv = parse_ini(config);
    let ram_mb = kv
        .get("hw.ramSize")
        .and_then(|v| parse_ram_mb(v))
        .unwrap_or(AvdCost::DEFAULT.ram_mb);
    let cores = kv
        .get("hw.cpu.ncore")
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(AvdCost::DEFAULT.cores);
    AvdCost { ram_mb, cores }
}

fn parse_ram_mb(value: &str) -> Option<u64> {
    let v = value.trim().to_ascii_uppercase();
    let v = v.strip_suffix('B').unwrap_or(&v);
    let (digits, factor) = match v.strip_suffix('G') {
        Some(d) => (d, 1024),
        None => (v.strip_suffix('M').unwrap_or(v), 1),
    };
    digits
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .map(|n| n * factor)
}

/// [`AvdCost`] of the AVD in `dir` (defaults when `config.ini` cannot be read).
pub fn cost_dir(dir: &Path) -> AvdCost {
    parse_cost(&std::fs::read_to_string(dir.join("config.ini")).unwrap_or_default())
}

/// [`AvdCost`] of AVD `id` under `home` (see [`avd_home`]).
pub fn cost(home: &Path, id: &str) -> AvdCost {
    let ini = std::fs::read_to_string(home.join(format!("{id}.ini"))).unwrap_or_default();
    let dir = parse_ini(&ini)
        .get("path")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(format!("{id}.avd")));
    cost_dir(&dir)
}

/// `android-37.0` → `Android API 37.0`; anything else is returned as is.
pub fn target_label(target: &str) -> String {
    match target.strip_prefix("android-") {
        Some(api) => format!("Android API {api}"),
        None => target.to_owned(),
    }
}

/// Lines of `emulator -list-avds` that are AVD names (the emulator may print log lines too).
pub fn parse_list_avds(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.contains('|')
                && !l.contains(' ')
                && l.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c))
        })
        .map(str::to_owned)
        .collect()
}

/// `ANDROID_AVD_HOME`, `$ANDROID_USER_HOME/avd`, `$ANDROID_EMULATOR_HOME/avd`, `~/.android/avd`.
pub fn avd_home() -> Option<PathBuf> {
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    env("ANDROID_AVD_HOME")
        .or_else(|| env("ANDROID_USER_HOME").map(|p| p.join("avd")))
        .or_else(|| env("ANDROID_EMULATOR_HOME").map(|p| p.join("avd")))
        .or_else(|| env("HOME").map(|p| p.join(".android/avd")))
}

/// Reads `<home>/<id>.ini` (for `path=`) then the AVD's `config.ini`.
pub fn load(home: &Path, id: &str) -> Avd {
    let ini = std::fs::read_to_string(home.join(format!("{id}.ini"))).unwrap_or_default();
    let dir = parse_ini(&ini)
        .get("path")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(format!("{id}.avd")));
    load_dir(id, &dir)
}

pub fn load_dir(id: &str, dir: &Path) -> Avd {
    let config = std::fs::read_to_string(dir.join("config.ini")).unwrap_or_default();
    from_config(id, Some(dir.to_path_buf()), &config)
}

/// All AVDs known to the emulator (`emulator -list-avds`), with their config.
pub fn list(sdk: &AndroidSdk) -> Result<Vec<Avd>> {
    let out = Command::new(&sdk.emulator)
        .arg("-list-avds")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::Command(format!("{}: {e}", sdk.emulator.display())))?;
    if !out.status.success() {
        return Err(Error::Command(format!(
            "emulator -list-avds: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let ids = parse_list_avds(&String::from_utf8_lossy(&out.stdout));
    let home = avd_home();
    Ok(ids
        .iter()
        .map(|id| match &home {
            Some(h) => load(h, id),
            None => from_config(id, None, ""),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "abi.type=arm64-v8a\navd.ini.displayname=Pixel 10 Pro\nhw.lcd.density=480\n\
        hw.lcd.height=2856\nhw.lcd.width=1280\ntarget=android-37.0\n";

    #[test]
    fn parses_config() {
        let a = from_config("Pixel_10_Pro", None, CONFIG);
        assert_eq!(a.name, "Pixel 10 Pro");
        assert_eq!(a.target.as_deref(), Some("android-37.0"));
        assert_eq!(a.abi.as_deref(), Some("arm64-v8a"));
        let s = a.screen.unwrap_or(ScreenSpec {
            width_px: 0,
            height_px: 0,
            scale: 0.0,
        });
        assert_eq!((s.width_px, s.height_px), (1280, 2856));
        assert!((s.scale - 3.0).abs() < 1e-9);
        assert!((s.width_pt() - 426.666_666).abs() < 1e-3);
        assert!((s.height_pt() - 952.0).abs() < 1e-9);
    }

    #[test]
    fn missing_config_falls_back() {
        let a = from_config("My_Phone", None, "");
        assert_eq!(a.name, "My Phone");
        assert_eq!(a.screen, None);
        assert_eq!(screen_from_density(1, 1, 0), None);
    }

    #[test]
    fn list_avds_output() {
        let text = "INFO    | Storing crashdata in: /tmp/x\nPixel_10_Pro\nMedium_Phone_API_35\n\n";
        assert_eq!(
            parse_list_avds(text),
            vec!["Pixel_10_Pro", "Medium_Phone_API_35"]
        );
    }

    #[test]
    fn avd_costs() {
        let cost = |text: &str| parse_cost(text);
        assert_eq!(
            cost("hw.ramSize=4096\nhw.cpu.ncore=4\n"),
            AvdCost {
                ram_mb: 4096,
                cores: 4
            }
        );
        assert_eq!(cost("hw.ramSize=4096M\n").ram_mb, 4096);
        assert_eq!(cost("hw.ramSize = 4G\n").ram_mb, 4096);
        assert_eq!(cost("hw.ramSize=3g\n").ram_mb, 3072);
        assert_eq!(cost("hw.ramSize=2048MB\n").ram_mb, 2048);
        assert_eq!(cost(""), AvdCost::DEFAULT);
        assert_eq!(cost("hw.ramSize=lots\nhw.cpu.ncore=0\n"), AvdCost::DEFAULT);
    }

    #[test]
    fn avd_cost_from_home() -> Result<(), Box<dyn std::error::Error>> {
        let home = tempfile::tempdir()?;
        let dir = home.path().join("P.avd");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("config.ini"), "hw.ramSize=6144\nhw.cpu.ncore=6\n")?;
        assert_eq!(
            cost(home.path(), "P"),
            AvdCost {
                ram_mb: 6144,
                cores: 6
            }
        );
        assert_eq!(cost(home.path(), "Missing"), AvdCost::DEFAULT);
        Ok(())
    }

    #[test]
    fn target_labels() {
        assert_eq!(target_label("android-37.0"), "Android API 37.0");
        assert_eq!(target_label("weird"), "weird");
    }

    #[test]
    fn loads_from_avd_home() -> Result<(), Box<dyn std::error::Error>> {
        let home = tempfile::tempdir()?;
        let dir = home.path().join("elsewhere/P.avd");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("config.ini"), CONFIG)?;
        std::fs::write(
            home.path().join("P.ini"),
            format!("avd.ini.encoding=UTF-8\npath={}\n", dir.display()),
        )?;
        let a = load(home.path(), "P");
        assert_eq!(a.dir.as_deref(), Some(dir.as_path()));
        assert_eq!(a.name, "Pixel 10 Pro");
        Ok(())
    }
}
