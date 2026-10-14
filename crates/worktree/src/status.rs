use std::ffi::OsString;
use std::path::{Path, PathBuf};

use hangar_core::changes::{ChangeKind, ChangedPath, Status};

use crate::error::{Error, Result};
use crate::git::{Git, bytes_to_path, bytes_to_string};

pub fn status(worktree: &Path) -> Result<Status> {
    status_paths(worktree, &[])
}

/// [`status`] limited to `paths` (literal pathspecs; empty means all). Much cheaper than a
/// full status on large trees; branch headers are still complete.
pub fn status_paths(worktree: &Path, paths: &[PathBuf]) -> Result<Status> {
    let mut args: Vec<OsString> = [
        "--literal-pathspecs",
        "status",
        "--porcelain=v2",
        "--branch",
        "--untracked-files=all",
        "-z",
        "--",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.extend(paths.iter().map(|p| p.as_os_str().to_owned()));
    let out = Git::new(worktree).run_bytes(&args)?;
    parse_status(&out)
}

/// Parses `git status --porcelain=v2 --branch -z`.
/// Submodules checked out in `worktree` (`git submodule status`; a `-` line is one that was
/// never initialized).
pub fn initialized_submodules(worktree: &Path) -> Result<usize> {
    let out = Git::new(worktree).run(["submodule", "status"])?;
    Ok(count_initialized(&out))
}

fn count_initialized(out: &str) -> usize {
    out.lines()
        .filter(|l| !l.is_empty() && !l.starts_with('-'))
        .count()
}

pub fn parse_status(out: &[u8]) -> Result<Status> {
    let mut status = Status::default();
    let mut fields = out.split(|&b| b == 0).filter(|f| !f.is_empty());
    while let Some(field) = fields.next() {
        let text = || bytes_to_string(field);
        match field.first() {
            Some(b'#') => parse_header(&mut status, &text())?,
            Some(b'1') => {
                let (xy, path) = entry(field, 8)?;
                push_change(&mut status, xy, bytes_to_path(path), None);
            }
            Some(b'2') => {
                let (xy, path) = entry(field, 9)?;
                let orig = fields
                    .next()
                    .ok_or_else(|| Error::Parse(format!("rename without source: {}", text())))?;
                push_change(
                    &mut status,
                    xy,
                    bytes_to_path(path),
                    Some(bytes_to_path(orig)),
                );
            }
            Some(b'u') => {
                let (_, path) = entry(field, 10)?;
                status.conflicted += 1;
                status.changes.push(ChangedPath {
                    path: bytes_to_path(path),
                    orig_path: None,
                    kind: ChangeKind::Conflicted,
                    staged: false,
                    unstaged: false,
                });
            }
            Some(b'?') => {
                let path = field
                    .get(2..)
                    .ok_or_else(|| Error::Parse(format!("bad untracked entry: {}", text())))?;
                status.untracked += 1;
                status.changes.push(ChangedPath {
                    path: bytes_to_path(path),
                    orig_path: None,
                    kind: ChangeKind::Untracked,
                    staged: false,
                    unstaged: false,
                });
            }
            Some(b'!') => {}
            _ => return Err(Error::Parse(format!("unknown status entry: {}", text()))),
        }
    }
    Ok(status)
}

fn parse_header(status: &mut Status, line: &str) -> Result<()> {
    let mut parts = line.splitn(3, ' ');
    let (_, key, value) = (parts.next(), parts.next(), parts.next().unwrap_or(""));
    match key {
        Some("branch.oid") if value != "(initial)" => status.oid = Some(value.to_owned()),
        Some("branch.head") if value != "(detached)" => status.branch = Some(value.to_owned()),
        Some("branch.upstream") => status.upstream = Some(value.to_owned()),
        Some("branch.ab") => {
            let parse = |s: Option<&str>, sign: char| {
                s.and_then(|s| s.strip_prefix(sign))
                    .and_then(|n| n.parse::<u32>().ok())
                    .ok_or_else(|| Error::Parse(format!("bad ahead/behind: {line}")))
            };
            let mut ab = value.split(' ');
            status.ahead = parse(ab.next(), '+')?;
            status.behind = parse(ab.next(), '-')?;
        }
        _ => {}
    }
    Ok(())
}

/// Splits an entry into its `XY` code and the path, which follows `fields_before_path`
/// space-separated fields (the path itself may contain spaces).
fn entry(field: &[u8], fields_before_path: usize) -> Result<([u8; 2], &[u8])> {
    let mut rest = field;
    let mut xy = None;
    for i in 0..fields_before_path {
        let space = rest.iter().position(|&b| b == b' ').ok_or_else(|| {
            Error::Parse(format!("short status entry: {}", bytes_to_string(field)))
        })?;
        if i == 1 {
            xy = match rest[..space] {
                [x, y] => Some([x, y]),
                _ => None,
            };
        }
        rest = &rest[space + 1..];
    }
    let xy = xy.ok_or_else(|| Error::Parse(format!("bad XY: {}", bytes_to_string(field))))?;
    Ok((xy, rest))
}

fn push_change(status: &mut Status, [x, y]: [u8; 2], path: PathBuf, orig_path: Option<PathBuf>) {
    let staged = x != b'.';
    let unstaged = y != b'.';
    status.staged += usize::from(staged);
    status.unstaged += usize::from(unstaged);
    let code = if staged { x } else { y };
    let kind = match code {
        b'A' => ChangeKind::Added,
        b'D' => ChangeKind::Deleted,
        b'R' => ChangeKind::Renamed,
        b'C' => ChangeKind::Copied,
        b'T' => ChangeKind::TypeChanged,
        _ => ChangeKind::Modified,
    };
    status.changes.push(ChangedPath {
        path,
        orig_path,
        kind,
        staged,
        unstaged,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_initialized_submodules() {
        let out = " 1234 lib (heads/main)\n-5678 docs\n+9abc vendor (v1)\nU0000 broken\n";
        assert_eq!(count_initialized(out), 3);
        assert_eq!(count_initialized(""), 0);
    }

    #[test]
    fn parses_branch_headers_and_entries() {
        let out =
            b"# branch.oid 0123abcd\0# branch.head feature/x\0# branch.upstream origin/main\0\
# branch.ab +2 -1\0\
1 M. N... 100644 100644 100644 aaaa bbbb src/lib.rs\0\
1 .M N... 100644 100644 100644 aaaa aaaa dir with space/file.txt\0\
1 AM N... 000000 100644 100644 0000 cccc new.rs\0\
2 R. N... 100644 100644 100644 dddd dddd R100 renamed.rs\0old name.rs\0\
u UU N... 100644 100644 100644 100644 e1 e2 e3 conflict.rs\0\
? untracked file\0";
        let s = parse_status(out).unwrap();
        assert_eq!(s.oid.as_deref(), Some("0123abcd"));
        assert_eq!(s.branch.as_deref(), Some("feature/x"));
        assert_eq!(s.upstream.as_deref(), Some("origin/main"));
        assert_eq!((s.ahead, s.behind), (2, 1));
        assert_eq!(
            (s.staged, s.unstaged, s.untracked, s.conflicted),
            (3, 2, 1, 1)
        );
        assert_eq!(s.changes.len(), 6);
        assert_eq!(s.changes[1].path, PathBuf::from("dir with space/file.txt"));
        assert_eq!(s.changes[1].kind, ChangeKind::Modified);
        assert!(!s.changes[1].staged && s.changes[1].unstaged);
        assert_eq!(s.changes[2].kind, ChangeKind::Added);
        assert!(s.changes[2].staged && s.changes[2].unstaged);
        assert_eq!(s.changes[3].kind, ChangeKind::Renamed);
        assert_eq!(s.changes[3].path, PathBuf::from("renamed.rs"));
        assert_eq!(s.changes[3].orig_path, Some(PathBuf::from("old name.rs")));
        assert_eq!(s.changes[4].kind, ChangeKind::Conflicted);
        assert_eq!(s.changes[5].path, PathBuf::from("untracked file"));
        assert_eq!(s.changes[5].kind, ChangeKind::Untracked);
    }

    #[test]
    fn parses_initial_and_detached() {
        let s = parse_status(b"# branch.oid (initial)\0# branch.head main\0").unwrap();
        assert_eq!(
            (s.oid.as_deref(), s.branch.as_deref()),
            (None, Some("main"))
        );
        assert!(s.is_clean());
        let s = parse_status(b"# branch.oid abcd\0# branch.head (detached)\0").unwrap();
        assert_eq!(s.branch, None);
        assert_eq!(s.upstream, None);
    }

    #[test]
    fn rejects_malformed_entries() {
        assert!(parse_status(b"1 M.\0").is_err());
        assert!(parse_status(b"2 R. N... 1 1 1 a a R100 new\0").is_err());
        assert!(parse_status(b"# branch.ab 2 1\0").is_err());
        assert!(parse_status(b"Z what\0").is_err());
    }
}
