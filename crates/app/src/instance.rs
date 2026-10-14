//! One Hangar per database. A second instance on the same database would mark the first one's
//! running sessions as stopped and could not serve agents (the MCP socket is taken), so it is
//! refused before it reads anything.

use std::fs::File;
use std::io::{Read as _, Seek as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;

/// Held for the life of the process; the OS releases the lock when it exits, even on a crash.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

/// Another process holds the lock: the error [`lock`] returns for it, to downcast.
#[derive(Debug)]
pub struct AlreadyRunning {
    pub pid: Option<u32>,
}

impl std::fmt::Display for AlreadyRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Hangar is already running with this database")?;
        match self.pid {
            Some(pid) => write!(f, " (process {pid})"),
            None => Ok(()),
        }
    }
}

impl std::error::Error for AlreadyRunning {}

/// `<db>.lock`, next to the database.
pub fn lock_path(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// Takes the lock for `db`, recording this process id in it. Fails with [`AlreadyRunning`]
/// when another process holds it.
pub fn lock(db: &Path) -> anyhow::Result<InstanceLock> {
    let path = lock_path(db);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let mut file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    // SAFETY: the descriptor is open for the duration of the call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            let mut pid = String::new();
            let _ = file.read_to_string(&mut pid);
            return Err(AlreadyRunning {
                pid: pid.trim().parse().ok(),
            }
            .into());
        }
        return Err(err).with_context(|| format!("cannot lock {}", path.display()));
    }
    (|| {
        file.set_len(0)?;
        file.rewind()?;
        write!(file, "{}", std::process::id())
    })()
    .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(InstanceLock { _file: file })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_lock_on_the_same_database_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("data/hangar.db");
        let held = lock(&db).unwrap();
        let err = lock(&db).unwrap_err();
        let running = err.downcast_ref::<AlreadyRunning>().unwrap();
        assert_eq!(running.pid, Some(std::process::id()));
        assert!(lock(&dir.path().join("other.db")).is_ok());
        drop(held);
        assert!(lock(&db).is_ok());
    }
}
