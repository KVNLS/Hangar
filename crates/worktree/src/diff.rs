use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::git::{Git, bytes_to_path, bytes_to_string};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStat {
    pub path: PathBuf,
    /// Source path of a rename or copy.
    pub old_path: Option<PathBuf>,
    /// `None` for binary files.
    pub insertions: Option<u32>,
    pub deletions: Option<u32>,
}

impl FileStat {
    pub fn is_binary(&self) -> bool {
        self.insertions.is_none()
    }
}

/// Per-file line counts of everything the worktree changed since it diverged from `base`:
/// commits on HEAD since `merge-base(base, HEAD)` plus staged and unstaged edits.
/// Untracked files are not included (see [`crate::status`]).
pub fn diff_stat(worktree: &Path, base: &str) -> Result<Vec<FileStat>> {
    let git = Git::new(worktree);
    let merge_base = git.run(["merge-base", base, "HEAD"])?;
    let merge_base = merge_base.trim();
    let out = git.run_bytes([
        "diff",
        "--numstat",
        "-z",
        "-M",
        "--no-color",
        "--no-ext-diff",
        merge_base,
        "--",
    ])?;
    parse_numstat(&out)
}

/// Parses `git diff --numstat -z`: `ins\tdel\tpath\0`, or `ins\tdel\t\0old\0new\0` for renames.
pub fn parse_numstat(out: &[u8]) -> Result<Vec<FileStat>> {
    let mut stats = Vec::new();
    let mut fields = out.split(|&b| b == 0);
    while let Some(field) = fields.next() {
        if field.is_empty() {
            continue;
        }
        let bad = || Error::Parse(format!("bad numstat entry: {}", bytes_to_string(field)));
        let mut parts = field.splitn(3, |&b| b == b'\t');
        let (Some(ins), Some(del), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(bad());
        };
        let count = |raw: &[u8]| -> Result<Option<u32>> {
            if raw == b"-" {
                return Ok(None);
            }
            bytes_to_string(raw).parse().map(Some).map_err(|_| bad())
        };
        let (insertions, deletions) = (count(ins)?, count(del)?);
        let (path, old_path) = if path.is_empty() {
            let old = fields.next().filter(|f| !f.is_empty()).ok_or_else(bad)?;
            let new = fields.next().filter(|f| !f.is_empty()).ok_or_else(bad)?;
            (bytes_to_path(new), Some(bytes_to_path(old)))
        } else {
            (bytes_to_path(path), None)
        };
        stats.push(FileStat {
            path,
            old_path,
            insertions,
            deletions,
        });
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_numstat() {
        let out = b"3\t1\tsrc/lib.rs\0-\t-\timage with space.png\0\
0\t0\t\0old.rs\0new.rs\0";
        let stats = parse_numstat(out).unwrap();
        assert_eq!(
            stats,
            vec![
                FileStat {
                    path: "src/lib.rs".into(),
                    old_path: None,
                    insertions: Some(3),
                    deletions: Some(1),
                },
                FileStat {
                    path: "image with space.png".into(),
                    old_path: None,
                    insertions: None,
                    deletions: None,
                },
                FileStat {
                    path: "new.rs".into(),
                    old_path: Some("old.rs".into()),
                    insertions: Some(0),
                    deletions: Some(0),
                },
            ]
        );
        assert!(stats[1].is_binary());
        assert!(parse_numstat(b"").unwrap().is_empty());
    }

    #[test]
    fn rejects_malformed_numstat() {
        assert!(parse_numstat(b"x\t1\tfile\0").is_err());
        assert!(parse_numstat(b"1\t1\0").is_err());
        assert!(parse_numstat(b"1\t1\t\0old\0").is_err());
    }
}
