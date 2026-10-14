//! Emulator discovery files (`avd/running/pid_<pid>.ini`), written by each running emulator.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// One running emulator as advertised in its discovery file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    pub pid: u32,
    /// AVD identifier as passed to `-avd` (e.g. `Pixel_10_Pro`).
    pub avd_id: String,
    /// Human name (e.g. `Pixel 10 Pro`).
    pub avd_name: String,
    pub avd_dir: Option<PathBuf>,
    /// Console port; the adb serial is `emulator-<console_port>`.
    pub console_port: u16,
    pub adb_port: Option<u16>,
    pub grpc_port: Option<u16>,
    /// Present when the emulator was started with `-grpc-use-token`.
    pub grpc_token: Option<String>,
    pub emulator_version: Option<String>,
    /// Started with `-read-only` (read from the advertised command line).
    pub read_only: bool,
    pub file: PathBuf,
}

impl Discovery {
    pub fn serial(&self) -> String {
        format!("emulator-{}", self.console_port)
    }
}

/// Parses a discovery file's `key=value` lines.
pub fn parse(text: &str, pid: u32, file: PathBuf) -> Result<Discovery> {
    let kv: BTreeMap<&str, &str> = text
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect();
    let port = |key: &str| -> Result<Option<u16>> {
        kv.get(key)
            .map(|v| {
                v.parse::<u16>()
                    .map_err(|e| Error::Parse(format!("{key}={v}: {e}")))
            })
            .transpose()
    };
    let console_port = port("port.serial")?
        .ok_or_else(|| Error::Parse("discovery file: no port.serial".into()))?;
    let avd_id = kv
        .get("avd.id")
        .ok_or_else(|| Error::Parse("discovery file: no avd.id".into()))?
        .to_string();
    Ok(Discovery {
        pid,
        avd_name: kv
            .get("avd.name")
            .map_or_else(|| avd_id.clone(), |s| s.to_string()),
        avd_id,
        avd_dir: kv.get("avd.dir").map(PathBuf::from),
        console_port,
        adb_port: port("port.adb")?,
        grpc_port: port("grpc.port")?,
        grpc_token: kv
            .get("grpc.token")
            .filter(|t| !t.is_empty())
            .map(|t| t.to_string()),
        emulator_version: kv.get("emulator.version").map(|s| s.to_string()),
        read_only: kv.get("cmdline").is_some_and(|line| {
            line.split_whitespace()
                .any(|word| word.trim_matches('"') == "-read-only")
        }),
        file,
    })
}

/// `pid_1234.ini` → 1234.
pub fn pid_from_file_name(name: &str) -> Option<u32> {
    name.strip_prefix("pid_")?
        .strip_suffix(".ini")?
        .parse()
        .ok()
}

/// Directories where emulators advertise themselves, most likely first.
/// macOS (emulator 36): `~/Library/Caches/TemporaryItems/avd/running`; Linux:
/// `$XDG_RUNTIME_DIR/avd/running`, else `/tmp/android-$USER/avd/running`.
pub fn running_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        if let Some(h) = &home {
            dirs.push(h.join("Library/Caches/TemporaryItems/avd/running"));
        }
        if let Some(t) = std::env::var_os("TMPDIR") {
            dirs.push(PathBuf::from(t).join("avd/running"));
        }
    } else {
        if let Some(x) = std::env::var_os("XDG_RUNTIME_DIR") {
            dirs.push(PathBuf::from(x).join("avd/running"));
        }
        if let Some(u) = std::env::var_os("USER") {
            let mut p = PathBuf::from("/tmp");
            p.push(format!("android-{}", u.to_string_lossy()));
            dirs.push(p.join("avd/running"));
        }
    }
    dirs.dedup();
    dirs
}

/// Reads every discovery file whose process is still alive.
pub fn scan() -> Vec<Discovery> {
    let mut out = Vec::new();
    for dir in running_dirs() {
        out.extend(scan_dir(&dir));
    }
    out.sort_by_key(|d| d.console_port);
    out.dedup_by_key(|d| d.pid);
    out
}

pub fn scan_dir(dir: &Path) -> Vec<Discovery> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = pid_from_file_name(&name.to_string_lossy()) else {
            continue;
        };
        if !process_alive(pid) {
            // Crashed emulators leave their file behind.
            continue;
        }
        let path = entry.path();
        match std::fs::read_to_string(&path)
            .map_err(Error::from)
            .and_then(|t| parse(&t, pid, path.clone()))
        {
            Ok(d) => out.push(d),
            Err(e) => tracing::debug!("skipping {}: {e}", path.display()),
        }
    }
    out
}

#[cfg(unix)]
pub fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks for existence/permission, nothing is delivered.
    let r = unsafe { libc::kill(pid, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
pub fn process_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"emulator.build=15261927
avd.id=Pixel_10_Pro
emulator.version=36.5.11.0
port.serial=5554
port.adb=5555
avd.name=Pixel 10 Pro
avd.dir=/Users/u/.android/avd/Pixel_10_Pro.avd
cmdline="/sdk/emulator/qemu/darwin-aarch64/qemu-system-aarch64-headless" "-avd" "Pixel_10_Pro" "-grpc" "8554"
grpc.token=ZmFrZS1ncnBjLXRva2Vu+Zm9yL3Rlc3Rz/ZQ==
launcher.dir=/sdk/emulator
grpc.allowlist=/sdk/emulator/lib/emulator_access.json
grpc.port=8554
"#;

    #[test]
    fn parses_discovery_file() -> Result<()> {
        let d = parse(SAMPLE, 34370, "/x/pid_34370.ini".into())?;
        assert_eq!(d.avd_id, "Pixel_10_Pro");
        assert_eq!(d.avd_name, "Pixel 10 Pro");
        assert_eq!(d.console_port, 5554);
        assert_eq!(d.serial(), "emulator-5554");
        assert_eq!(d.adb_port, Some(5555));
        assert_eq!(d.grpc_port, Some(8554));
        // Base64 tokens contain '=', only the first '=' separates key and value.
        assert!(d.grpc_token.as_deref().is_some_and(|t| t.ends_with("ZQ==")));
        assert_eq!(d.emulator_version.as_deref(), Some("36.5.11.0"));
        assert!(!d.read_only);
        let ro = parse(
            &SAMPLE.replace(r#""-grpc" "8554""#, r#""-grpc" "8554" "-read-only""#),
            1,
            "f".into(),
        )?;
        assert!(ro.read_only);
        assert_eq!(
            d.avd_dir,
            Some(PathBuf::from("/Users/u/.android/avd/Pixel_10_Pro.avd"))
        );
        Ok(())
    }

    #[test]
    fn grpc_is_optional_but_console_port_is_not() -> Result<()> {
        let d = parse("avd.id=A\nport.serial=5556\n", 1, "f".into())?;
        assert_eq!(d.grpc_port, None);
        assert_eq!(d.grpc_token, None);
        assert_eq!(d.avd_name, "A");
        assert!(parse("avd.id=A\n", 1, "f".into()).is_err());
        assert!(parse("avd.id=A\nport.serial=x\n", 1, "f".into()).is_err());
        Ok(())
    }

    #[test]
    fn pid_from_name() {
        assert_eq!(pid_from_file_name("pid_34370.ini"), Some(34370));
        assert_eq!(pid_from_file_name("pid_.ini"), None);
        assert_eq!(pid_from_file_name("other.ini"), None);
    }

    #[test]
    fn scan_skips_dead_and_unrelated() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let me = std::process::id();
        std::fs::write(dir.path().join(format!("pid_{me}.ini")), SAMPLE)?;
        // Assumes no process has the maximum pid.
        std::fs::write(dir.path().join("pid_99999999.ini"), SAMPLE)?;
        std::fs::write(dir.path().join("notes.txt"), "x")?;
        let found = scan_dir(dir.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pid, me);
        Ok(())
    }
}
