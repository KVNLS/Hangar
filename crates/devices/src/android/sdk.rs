//! Android SDK location and tool paths.

use std::path::{Path, PathBuf};

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AndroidSdk {
    pub root: PathBuf,
    pub adb: PathBuf,
    pub emulator: PathBuf,
    /// `cmdline-tools/latest/bin/avdmanager` (or `tools/bin`); optional, only needed to create AVDs.
    pub avdmanager: Option<PathBuf>,
}

impl AndroidSdk {
    /// `ANDROID_HOME`, then `ANDROID_SDK_ROOT`, then the platform default
    /// (`~/Library/Android/sdk` on macOS, `~/Android/Sdk` on Linux).
    pub fn locate() -> Result<Self> {
        let candidates = candidate_roots(
            std::env::var_os("ANDROID_HOME").map(PathBuf::from),
            std::env::var_os("ANDROID_SDK_ROOT").map(PathBuf::from),
            std::env::var_os("HOME").map(PathBuf::from),
        );
        let mut tried = Vec::new();
        for root in candidates {
            match Self::at(&root) {
                Ok(sdk) => return Ok(sdk),
                Err(e) => tried.push(e.to_string()),
            }
        }
        Err(Error::NotFound(format!(
            "Android SDK (set ANDROID_HOME): {}",
            if tried.is_empty() {
                "no candidate location".to_owned()
            } else {
                tried.join("; ")
            }
        )))
    }

    /// Validates an SDK root: `platform-tools/adb` and `emulator/emulator` must exist.
    pub fn at(root: &Path) -> Result<Self> {
        let adb = root.join("platform-tools").join(exe("adb"));
        let emulator = root.join("emulator").join(exe("emulator"));
        for (what, p) in [("adb", &adb), ("emulator", &emulator)] {
            if !p.is_file() {
                return Err(Error::NotFound(format!("{what} at {}", p.display())));
            }
        }
        let avdmanager = [
            root.join("cmdline-tools/latest/bin"),
            root.join("tools/bin"),
        ]
        .into_iter()
        .map(|d| d.join(exe("avdmanager")))
        .find(|p| p.is_file());
        Ok(Self {
            root: root.to_path_buf(),
            adb,
            emulator,
            avdmanager,
        })
    }
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

/// Ordered, de-duplicated SDK root candidates (pure, for tests).
pub fn candidate_roots(
    android_home: Option<PathBuf>,
    android_sdk_root: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Vec<PathBuf> {
    let default = home.map(|h| {
        if cfg!(target_os = "macos") {
            h.join("Library/Android/sdk")
        } else {
            h.join("Android/Sdk")
        }
    });
    let mut out: Vec<PathBuf> = Vec::new();
    for p in [android_home, android_sdk_root, default]
        .into_iter()
        .flatten()
        .filter(|p| !p.as_os_str().is_empty())
    {
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_are_ordered_and_deduplicated() {
        let c = candidate_roots(Some("/a".into()), Some("/a".into()), Some("/home/u".into()));
        let default = if cfg!(target_os = "macos") {
            PathBuf::from("/home/u/Library/Android/sdk")
        } else {
            PathBuf::from("/home/u/Android/Sdk")
        };
        assert_eq!(c, vec![PathBuf::from("/a"), default]);
        assert!(candidate_roots(Some("".into()), None, None).is_empty());
    }

    #[test]
    fn validates_tools() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let err = AndroidSdk::at(dir.path());
        assert!(matches!(err, Err(Error::NotFound(_))));
        std::fs::create_dir_all(dir.path().join("platform-tools"))?;
        std::fs::create_dir_all(dir.path().join("emulator"))?;
        std::fs::write(dir.path().join("platform-tools/adb"), "")?;
        std::fs::write(dir.path().join("emulator/emulator"), "")?;
        let sdk = AndroidSdk::at(dir.path())?;
        assert_eq!(sdk.adb, dir.path().join("platform-tools/adb"));
        assert_eq!(sdk.avdmanager, None);
        std::fs::create_dir_all(dir.path().join("cmdline-tools/latest/bin"))?;
        std::fs::write(dir.path().join("cmdline-tools/latest/bin/avdmanager"), "")?;
        assert!(AndroidSdk::at(dir.path())?.avdmanager.is_some());
        Ok(())
    }
}
