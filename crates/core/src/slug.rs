//! How workspaces are named: slugs, Hangar's branch names and worktree locations.

use std::path::{Path, PathBuf};

/// Prefix of every branch Hangar creates.
pub const BRANCH_PREFIX: &str = "hangar/";

const MAX_LEN: usize = 48;
const FALLBACK: &str = "workspace";

/// Turns a free-form name into a branch- and path-safe slug: lowercase ASCII alphanumerics separated by
/// single dashes, at most 48 characters, never empty.
pub fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len().min(MAX_LEN));
    let mut pending_dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(c.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
        if slug.len() >= MAX_LEN {
            break;
        }
    }
    slug.truncate(MAX_LEN);
    let trimmed = slug.trim_end_matches('-');
    if trimmed.is_empty() {
        FALLBACK.to_owned()
    } else {
        trimmed.to_owned()
    }
}

pub fn branch_name(slug: &str) -> String {
    format!("{BRANCH_PREFIX}{slug}")
}

/// `<root>/<project slug>/<slug>`.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercases_and_dashes() {
        assert_eq!(slugify("Fix Login Bug"), "fix-login-bug");
    }

    #[test]
    fn collapses_and_trims_separators() {
        assert_eq!(slugify("  --Add   OAuth__flow!!  "), "add-oauth-flow");
    }

    #[test]
    fn drops_non_ascii() {
        assert_eq!(slugify("Café déjà vu"), "caf-d-j-vu");
    }

    #[test]
    fn keeps_digits() {
        assert_eq!(slugify("Issue #42 v2"), "issue-42-v2");
    }

    #[test]
    fn truncates_to_max_len_without_trailing_dash() {
        let slug = slugify(&"ab ".repeat(40));
        assert!(slug.len() <= MAX_LEN);
        assert!(!slug.ends_with('-'));
        assert!(slug.starts_with("ab-ab"));
        assert_eq!(slugify(&"x".repeat(100)).len(), MAX_LEN);
    }

    #[test]
    fn falls_back_when_empty() {
        assert_eq!(slugify(""), "workspace");
        assert_eq!(slugify("!!! ---"), "workspace");
        assert_eq!(slugify("日本語"), "workspace");
    }

    #[test]
    fn branch_is_prefixed() {
        assert_eq!(branch_name("fix-login"), "hangar/fix-login");
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
