//! `adb` invocation and output parsing.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use crate::{Error, Result};

/// One line of `adb devices -l`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdbDevice {
    pub serial: String,
    /// `device`, `offline`, `unauthorized`, `authorizing`, ...
    pub state: String,
    pub product: Option<String>,
    pub model: Option<String>,
    pub device: Option<String>,
    pub transport_id: Option<u32>,
}

impl AdbDevice {
    pub fn is_emulator(&self) -> bool {
        self.serial.starts_with("emulator-")
    }

    pub fn is_online(&self) -> bool {
        self.state == "device"
    }
}

/// Parses `adb devices -l`. Daemon start-up chatter (`* daemon ...`) and the header are ignored.
pub fn parse_devices(text: &str) -> Vec<AdbDevice> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('*') || line.starts_with("List of devices") {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(serial), Some(state)) = (parts.next(), parts.next()) else {
            continue;
        };
        let mut d = AdbDevice {
            serial: serial.to_owned(),
            state: state.to_owned(),
            product: None,
            model: None,
            device: None,
            transport_id: None,
        };
        // `no permissions (...)` style states contain spaces; key:value pairs follow them.
        for p in parts {
            match p.split_once(':') {
                Some(("product", v)) => d.product = Some(v.to_owned()),
                Some(("model", v)) => d.model = Some(v.to_owned()),
                Some(("device", v)) => d.device = Some(v.to_owned()),
                Some(("transport_id", v)) => d.transport_id = v.parse().ok(),
                _ => {}
            }
        }
        out.push(d);
    }
    out
}

/// Parses `wm size`: prefers `Override size:` over `Physical size:`. Returns (width, height) px.
pub fn parse_wm_size(text: &str) -> Option<(u32, u32)> {
    let find = |prefix: &str| {
        text.lines().find_map(|l| {
            let v = l.trim().strip_prefix(prefix)?.trim();
            let (w, h) = v.split_once('x')?;
            Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
        })
    };
    find("Override size:").or_else(|| find("Physical size:"))
}

/// Parses `wm density`: prefers `Override density:` over `Physical density:`.
pub fn parse_wm_density(text: &str) -> Option<u32> {
    let find = |prefix: &str| {
        text.lines()
            .find_map(|l| l.trim().strip_prefix(prefix)?.trim().parse().ok())
    };
    find("Override density:").or_else(|| find("Physical density:"))
}

/// Escapes text for `adb shell input text`: the device shell re-parses the line, and `input`
/// itself turns `%s` into a space.
pub fn escape_input_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        match c {
            ' ' => out.push_str("%s"),
            '\\' | '\'' | '"' | '`' | '$' | '&' | '|' | ';' | '<' | '>' | '(' | ')' | '*' | '?'
            | '~' | '#' | '!' | '[' | ']' | '{' | '}' | '%' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct Adb {
    pub path: PathBuf,
}

impl Adb {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn run(&self, args: &[&str]) -> Result<Output> {
        let out = Command::new(&self.path)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| Error::Command(format!("{}: {e}", self.path.display())))?;
        if !out.status.success() {
            return Err(Error::Command(format!(
                "adb {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(out)
    }

    pub fn devices(&self) -> Result<Vec<AdbDevice>> {
        let out = self.run(&["devices", "-l"])?;
        Ok(parse_devices(&String::from_utf8_lossy(&out.stdout)))
    }

    /// `adb -s <serial> shell <cmd>`; returns stdout. Fails on a non-zero remote exit status.
    pub fn shell(&self, serial: &str, cmd: &str) -> Result<String> {
        let out = self.run(&["-s", serial, "shell", cmd])?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// `adb -s <serial> exec-out <cmd>`: raw stdout bytes (no tty, no CRLF mangling).
    pub fn exec_out(&self, serial: &str, cmd: &str) -> Result<Vec<u8>> {
        Ok(self.run(&["-s", serial, "exec-out", cmd])?.stdout)
    }

    pub fn getprop(&self, serial: &str, prop: &str) -> Result<String> {
        Ok(self
            .shell(serial, &format!("getprop {prop}"))?
            .trim()
            .to_owned())
    }

    pub fn boot_completed(&self, serial: &str) -> bool {
        self.getprop(serial, "sys.boot_completed")
            .is_ok_and(|v| v == "1")
    }

    /// Screen size in px and density (dpi), honouring `wm` overrides.
    pub fn screen(&self, serial: &str) -> Result<(u32, u32, u32)> {
        let text = self.shell(serial, "wm size; wm density")?;
        let (w, h) = parse_wm_size(&text)
            .ok_or_else(|| Error::Parse(format!("wm size: {}", text.trim())))?;
        let dpi = parse_wm_density(&text)
            .ok_or_else(|| Error::Parse(format!("wm density: {}", text.trim())))?;
        Ok((w, h, dpi))
    }

    /// (release, sdk) e.g. ("17", "37").
    pub fn version(&self, serial: &str) -> Result<(String, String)> {
        let text = self.shell(
            serial,
            "getprop ro.build.version.release; getprop ro.build.version.sdk",
        )?;
        let mut lines = text.lines().map(str::trim);
        Ok((
            lines.next().unwrap_or_default().to_owned(),
            lines.next().unwrap_or_default().to_owned(),
        ))
    }

    /// Console command through adb (`adb emu kill`, ...).
    pub fn emu(&self, serial: &str, args: &[&str]) -> Result<()> {
        let mut all = vec!["-s", serial, "emu"];
        all.extend_from_slice(args);
        self.run(&all).map(|_| ())
    }

    pub fn install(&self, serial: &str, apk: &std::path::Path) -> Result<()> {
        let apk = apk
            .to_str()
            .ok_or_else(|| Error::Parse(format!("non UTF-8 path {}", apk.display())))?;
        let out = self.run(&["-s", serial, "install", "-r", apk])?;
        let text = String::from_utf8_lossy(&out.stdout);
        if text.contains("Success") {
            Ok(())
        } else {
            Err(Error::Command(format!("adb install: {}", text.trim())))
        }
    }

    /// Starts the launcher activity of `package` (no need to know the activity name).
    pub fn launch(&self, serial: &str, package: &str) -> Result<()> {
        if !package
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
        {
            return Err(Error::Parse(format!("invalid package name {package:?}")));
        }
        let text = self.shell(
            serial,
            &format!("monkey -p {package} -c android.intent.category.LAUNCHER 1"),
        )?;
        if text.contains("No activities found") || text.contains("monkey aborted") {
            return Err(Error::NotFound(format!("launcher activity of {package}")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_devices_list() {
        let text = "* daemon not running; starting now at tcp:5037\n* daemon started successfully\n\
            List of devices attached\n\
            emulator-5554          device product:sdk_gphone16k_arm64 model:sdk_gphone16k_arm64 device:emu64a16k transport_id:1\n\
            emulator-5556\toffline transport_id:2\n\
            R58M123ABC             unauthorized usb:1-1 transport_id:3\n\n";
        let d = parse_devices(text);
        assert_eq!(d.len(), 3);
        assert_eq!(d[0].serial, "emulator-5554");
        assert!(d[0].is_online() && d[0].is_emulator());
        assert_eq!(d[0].model.as_deref(), Some("sdk_gphone16k_arm64"));
        assert_eq!(d[0].device.as_deref(), Some("emu64a16k"));
        assert_eq!(d[0].transport_id, Some(1));
        assert_eq!(d[1].state, "offline");
        assert!(!d[1].is_online());
        assert_eq!(d[2].state, "unauthorized");
        assert!(!d[2].is_emulator());
        assert!(parse_devices("List of devices attached\n\n").is_empty());
    }

    #[test]
    fn parses_wm_output() {
        assert_eq!(
            parse_wm_size("Physical size: 1280x2856\n"),
            Some((1280, 2856))
        );
        assert_eq!(
            parse_wm_size("Physical size: 1280x2856\nOverride size: 720x1606\n"),
            Some((720, 1606))
        );
        assert_eq!(parse_wm_density("Physical density: 480\r\n"), Some(480));
        assert_eq!(
            parse_wm_density("Physical density: 480\nOverride density: 320\n"),
            Some(320)
        );
        assert_eq!(parse_wm_size("garbage"), None);
    }

    #[test]
    fn escapes_input_text() {
        assert_eq!(escape_input_text("hello world"), "hello%sworld");
        assert_eq!(escape_input_text("a&b;c"), "a\\&b\\;c");
        assert_eq!(escape_input_text("it's $5"), "it\\'s%s\\$5");
        assert_eq!(escape_input_text("100%"), "100\\%");
    }
}
