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
}
