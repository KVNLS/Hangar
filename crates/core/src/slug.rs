//! How workspaces are named: slugs, Hangar's branch names and worktree locations.

use std::path::{Path, PathBuf};

/// Prefix of every branch Hangar creates.
pub const BRANCH_PREFIX: &str = "hangar/";

const MAX_LEN: usize = 48;
const FALLBACK: &str = "workspace";

/// Turns a free-form name into a branch- and path-safe slug: lowercase ASCII alphanumerics separated by
/// single dashes, at most 48 characters, never empty. Accented Latin letters are folded (`é` → `e`,
/// `ß` → `ss`); a name whose letters have no ASCII form (Japanese, Greek…) becomes `ws-` and a
/// stable hash of it, so different names stay apart.
pub fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len().min(MAX_LEN));
    let mut pending_dash = false;
    let mut letters = false;
    for c in name.chars().flat_map(char::to_lowercase) {
        letters |= c.is_alphanumeric();
        let ascii = if c.is_ascii_alphanumeric() {
            Some(c.encode_utf8(&mut [0; 4]).to_owned())
        } else {
            fold(c).map(str::to_owned)
        };
        match ascii {
            Some(ascii) => {
                if pending_dash && !slug.is_empty() {
                    slug.push('-');
                }
                pending_dash = false;
                slug.push_str(&ascii);
            }
            None => pending_dash = true,
        }
        if slug.len() >= MAX_LEN {
            break;
        }
    }
    slug.truncate(MAX_LEN);
    let trimmed = slug.trim_end_matches('-');
    match (trimmed.is_empty(), letters) {
        (false, _) => trimmed.to_owned(),
        (true, true) => format!("ws-{:06x}", fnv1a(name) & 0xff_ffff),
        (true, false) => FALLBACK.to_owned(),
    }
}

/// The ASCII spelling of a lowercase accented Latin letter.
fn fold(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ď' | 'đ' | 'ð' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
        'ĵ' => "j",
        'ķ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
        'œ' => "oe",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' | 'ț' => "t",
        'þ' => "th",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        _ => return None,
    })
}

/// FNV-1a: a small stable hash (the same on every run and build, unlike `DefaultHasher`).
fn fnv1a(text: &str) -> u32 {
    text.bytes().fold(0x811c_9dc5, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    })
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
    fn folds_accents() {
        assert_eq!(slugify("Café déjà vu"), "cafe-deja-vu");
        assert_eq!(slugify("🚀 Émoji ünïcode"), "emoji-unicode");
        assert_eq!(slugify("Straße Ærø Œuvre"), "strasse-aero-oeuvre");
        assert_eq!(slugify("Łódź Ñandú"), "lodz-nandu");
    }

    #[test]
    fn names_without_latin_letters_get_a_stable_id() {
        let slug = slugify("日本語のワークスペース");
        assert!(slug.starts_with("ws-") && slug.len() == 9, "{slug}");
        assert_eq!(slug, slugify("日本語のワークスペース"));
        assert_ne!(slug, slugify("别的名字"));
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
