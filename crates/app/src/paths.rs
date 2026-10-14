//! Where Hangar puts things on disk and how workspaces are named in Git.

use std::path::{Path, PathBuf};

/// Prefix of every branch Hangar creates.
pub const BRANCH_PREFIX: &str = "hangar/";

/// Overrides [`worktrees_root`], e.g. for tests and headless runs.
pub const WORKTREES_ENV: &str = "HANGAR_WORKTREES";

/// Overrides the database location.
pub const DB_ENV: &str = "HANGAR_DB";

pub fn branch_name(slug: &str) -> String {
    format!("{BRANCH_PREFIX}{slug}")
}

/// Directory holding every worktree Hangar creates. Where worktrees live is still an open
/// question (PLAN.md); this is the only place that decides it.
pub fn worktrees_root() -> Option<PathBuf> {
    worktrees_root_for(
        cfg!(target_os = "macos"),
        std::env::var_os(WORKTREES_ENV).map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

fn worktrees_root_for(
    macos: bool,
    override_dir: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(dir) = override_dir.filter(|p| p.is_absolute()) {
        return Some(dir);
    }
    let home = home.filter(|p| p.is_absolute())?;
    Some(if macos {
        home.join("Library/Application Support/Hangar/worktrees")
    } else {
        home.join(".local/share/hangar/worktrees")
    })
}

/// Per-workspace build output (Xcode derived data), next to the worktrees and outside them so
/// builds never show up as changes.
pub fn builds_root() -> Option<PathBuf> {
    Some(worktrees_root()?.parent()?.join("builds"))
}

pub fn worktree_path(root: &Path, project_slug: &str, slug: &str) -> PathBuf {
    root.join(project_slug).join(slug)
}

/// `base`, else `base-2`, `base-3`, … — the first candidate `is_taken` rejects.
pub fn unique_slug(base: &str, mut is_taken: impl FnMut(&str) -> bool) -> String {
    if !is_taken(base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| !is_taken(candidate))
        .unwrap_or_else(|| base.to_owned())
}

/// Database path: `$HANGAR_DB`, else the core default.
pub fn db_path() -> Option<PathBuf> {
    std::env::var_os(DB_ENV)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(hangar_store_sqlite::default_db_path)
}

/// Hangar's data directory: the database's directory.
pub fn data_dir() -> Option<PathBuf> {
    Some(db_path()?.parent()?.to_path_buf())
}

/// Where per-workspace MCP config files for agents live (never inside a worktree).
pub fn mcp_config_dir() -> Option<PathBuf> {
    Some(data_dir()?.join("mcp"))
}

/// Longest Unix socket path macOS accepts (`sun_path` is 104 bytes with the NUL).
pub const MAX_SOCKET_PATH: usize = 103;

const SOCKET_NAME: &str = "hangar.sock";

/// Socket path candidates in order of preference, keeping only those short enough to bind:
/// `<data>/run/hangar.sock`, then `$TMPDIR/hangar-<uid>/hangar.sock`, then
/// `/tmp/hangar-<uid>/hangar.sock`.
pub fn socket_candidates(data_dir: Option<&Path>, tmpdir: Option<&Path>, uid: u32) -> Vec<PathBuf> {
    let private = format!("hangar-{uid}");
    let all = [
        data_dir
            .filter(|p| p.is_absolute())
            .map(|d| d.join("run").join(SOCKET_NAME)),
        tmpdir
            .filter(|p| p.is_absolute())
            .map(|t| t.join(&private).join(SOCKET_NAME)),
        Some(Path::new("/tmp").join(&private).join(SOCKET_NAME)),
    ];
    let mut out: Vec<PathBuf> = Vec::new();
    for path in all.into_iter().flatten() {
        if path.as_os_str().len() <= MAX_SOCKET_PATH && !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// Why a socket directory is unsafe to use, if it is: it must be a real directory (not a
/// symlink) owned by `uid` that nobody else can enter.
pub fn private_dir_problem(
    is_dir: bool,
    is_symlink: bool,
    owner: u32,
    mode: u32,
    uid: u32,
) -> Option<String> {
    if is_symlink {
        Some("is a symbolic link".to_owned())
    } else if !is_dir {
        Some("is not a directory".to_owned())
    } else if owner != uid {
        Some(format!("is owned by uid {owner}, not {uid}"))
    } else if mode & 0o077 != 0 {
        Some(format!("has mode {:o}, expected 700", mode & 0o777))
    } else {
        None
    }
}

/// Creates `dir` with mode 0700 if missing (parents with default modes) and checks it with
/// [`private_dir_problem`]. An existing directory with a wider mode is refused, not fixed.
pub fn ensure_private_dir(dir: &Path, uid: u32) -> anyhow::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("cannot create {}: {e}", parent.display()))?;
    }
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => anyhow::bail!("cannot create {}: {e}", dir.display()),
    }
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|e| anyhow::anyhow!("cannot inspect {}: {e}", dir.display()))?;
    match private_dir_problem(
        meta.is_dir(),
        meta.file_type().is_symlink(),
        meta.uid(),
        meta.mode(),
        uid,
    ) {
        None => Ok(()),
        Some(problem) => anyhow::bail!(
            "refusing to put the MCP socket in {}: it {problem}",
            dir.display()
        ),
    }
}

/// The app's MCP socket path, its private directory created and checked.
pub fn socket_path() -> anyhow::Result<PathBuf> {
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    let tmpdir = std::env::var_os("TMPDIR").map(PathBuf::from);
    let path = socket_candidates(data_dir().as_deref(), tmpdir.as_deref(), uid)
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no MCP socket path fits in {MAX_SOCKET_PATH} bytes"))?;
    let dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("socket path {} has no parent", path.display()))?;
    ensure_private_dir(dir, uid)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_SOCKET_PATH, branch_name, ensure_private_dir, private_dir_problem, socket_candidates,
        unique_slug, worktree_path, worktrees_root_for,
    };
    use std::path::{Path, PathBuf};

    #[test]
    fn branch_is_prefixed() {
        assert_eq!(branch_name("fix-login"), "hangar/fix-login");
    }

    #[test]
    fn worktree_root_defaults_per_platform() {
        let home = Some(PathBuf::from("/Users/me"));
        assert_eq!(
            worktrees_root_for(true, None, home.clone()),
            Some(PathBuf::from(
                "/Users/me/Library/Application Support/Hangar/worktrees"
            ))
        );
        assert_eq!(
            worktrees_root_for(false, None, home),
            Some(PathBuf::from("/Users/me/.local/share/hangar/worktrees"))
        );
    }

    #[test]
    fn worktree_root_override_and_missing_home() {
        assert_eq!(
            worktrees_root_for(true, Some("/tmp/wt".into()), None),
            Some(PathBuf::from("/tmp/wt"))
        );
        // A relative override is ignored rather than resolved against the cwd.
        assert_eq!(
            worktrees_root_for(true, Some("rel".into()), Some("/h".into())),
            Some(PathBuf::from(
                "/h/Library/Application Support/Hangar/worktrees"
            ))
        );
        assert_eq!(worktrees_root_for(true, None, None), None);
        assert_eq!(worktrees_root_for(true, None, Some("rel".into())), None);
    }

    #[test]
    fn worktree_path_nests_project_and_slug() {
        assert_eq!(
            worktree_path(Path::new("/root"), "my-app", "fix-login"),
            PathBuf::from("/root/my-app/fix-login")
        );
    }

    #[test]
    fn unique_slug_appends_counter() {
        assert_eq!(unique_slug("a", |_| false), "a");
        let taken = ["a", "a-2"];
        assert_eq!(unique_slug("a", |s| taken.contains(&s)), "a-3");
    }

    #[test]
    fn socket_prefers_data_dir_then_tmpdir_then_tmp() {
        let data = Path::new("/Users/me/Library/Application Support/Hangar");
        let tmp = Path::new("/var/folders/ab/cd/T");
        assert_eq!(
            socket_candidates(Some(data), Some(tmp), 501),
            vec![
                PathBuf::from("/Users/me/Library/Application Support/Hangar/run/hangar.sock"),
                PathBuf::from("/var/folders/ab/cd/T/hangar-501/hangar.sock"),
                PathBuf::from("/tmp/hangar-501/hangar.sock"),
            ]
        );
    }

    #[test]
    fn socket_skips_long_relative_and_duplicate_dirs() {
        let long = PathBuf::from(format!("/private/var/{}", "x".repeat(100)));
        let got = socket_candidates(Some(&long), Some(Path::new("rel")), 7);
        assert_eq!(got, vec![PathBuf::from("/tmp/hangar-7/hangar.sock")]);
        let got = socket_candidates(None, Some(Path::new("/tmp/")), 7);
        assert_eq!(got, vec![PathBuf::from("/tmp/hangar-7/hangar.sock")]);
        for p in socket_candidates(Some(&long), Some(&long), u32::MAX) {
            assert!(p.as_os_str().len() <= MAX_SOCKET_PATH);
        }
    }

    #[test]
    fn private_dir_rules() {
        assert_eq!(private_dir_problem(true, false, 501, 0o40700, 501), None);
        assert!(private_dir_problem(true, false, 501, 0o40755, 501).is_some());
        assert!(private_dir_problem(true, false, 501, 0o40710, 501).is_some());
        assert!(private_dir_problem(true, false, 0, 0o40700, 501).is_some());
        assert!(private_dir_problem(false, false, 501, 0o100600, 501).is_some());
        assert!(private_dir_problem(false, true, 501, 0o120777, 501).is_some());
    }

    #[test]
    fn ensure_private_dir_creates_and_refuses() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let root = tempfile::tempdir().unwrap();
        let uid = std::fs::metadata(root.path()).unwrap().uid();
        let dir = root.path().join("a/run");
        ensure_private_dir(&dir, uid).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        ensure_private_dir(&dir, uid).unwrap();
        assert!(ensure_private_dir(&dir, uid + 1).is_err());

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(ensure_private_dir(&dir, uid).is_err());

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(ensure_private_dir(&link, uid).is_err());
    }
}
