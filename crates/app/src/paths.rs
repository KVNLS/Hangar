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
        .or_else(hangar_core::default_db_path)
}

#[cfg(test)]
mod tests {
    use super::{branch_name, unique_slug, worktree_path, worktrees_root_for};
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
}
