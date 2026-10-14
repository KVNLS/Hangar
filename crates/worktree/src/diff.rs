use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::git::{Git, bytes_to_path, bytes_to_string};
use crate::patch::{DiffOptions, FileDiff, FileStatus, parse_patch};

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

/// Commit the worktree diverged from `base`: `merge-base(base, HEAD)`.
pub fn merge_base(worktree: &Path, base: &str) -> Result<String> {
    let out = Git::new(worktree).run(["merge-base", base, "HEAD"])?;
    Ok(out.trim().to_owned())
}

/// Every change since the worktree diverged from `base`: commits on HEAD since
/// `merge-base(base, HEAD)`, staged and unstaged edits, and untracked (not ignored) files.
/// Never touches the index. Sorted by path.
pub fn diff_files(worktree: &Path, base: &str) -> Result<Vec<FileDiff>> {
    diff_files_with(worktree, base, &DiffOptions::default())
}

pub fn diff_files_with(worktree: &Path, base: &str, opts: &DiffOptions) -> Result<Vec<FileDiff>> {
    let rev = merge_base(worktree, base)?;
    diff_paths(worktree, &rev, &[], opts)
}

/// The diff of one path against `merge-base(base, HEAD)`, or `None` when it is unchanged.
/// Renames are only detected when both names are given, so use [`diff_paths`] for those.
pub fn diff_file(worktree: &Path, base: &str, path: &Path) -> Result<Option<FileDiff>> {
    let rev = merge_base(worktree, base)?;
    let files = diff_paths(
        worktree,
        &rev,
        &[path.to_path_buf()],
        &DiffOptions::default(),
    )?;
    Ok(files.into_iter().find(|f| f.path == path))
}

/// Diffs the working tree against the commit `rev`, limited to `paths` (literal pathspecs,
/// relative to the worktree; a directory covers everything below it). Empty `paths` means all.
pub fn diff_paths(
    worktree: &Path,
    rev: &str,
    paths: &[PathBuf],
    opts: &DiffOptions,
) -> Result<Vec<FileDiff>> {
    let git = Git::new(worktree);
    let mut args: Vec<OsString> = ["--literal-pathspecs", "diff"]
        .into_iter()
        .chain(PATCH_ARGS)
        .map(OsString::from)
        .collect();
    args.push(
        if opts.detect_renames {
            "-M"
        } else {
            "--no-renames"
        }
        .into(),
    );
    args.push(rev.into());
    args.push("--".into());
    args.extend(paths.iter().map(|p| p.as_os_str().to_owned()));

    let mut small = Vec::new();
    let mut files = Vec::new();
    for path in untracked_files(worktree, paths)? {
        match std::fs::symlink_metadata(worktree.join(&path)) {
            Ok(meta) if opts.max_untracked_bytes.is_some_and(|max| meta.len() > max) => {
                let mut file = FileDiff::untracked(path);
                file.truncated = true;
                files.push(file);
            }
            Ok(_) => small.push(path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    // Untracked files go into one diff through a scratch copy of the index where they are
    // intent-to-add: no git process per file, and a file moved without `git mv` pairs with its
    // deleted original as a rename. The real index is never written.
    let scratch = match small.is_empty() {
        true => None,
        false => ScratchIndex::with_intent_to_add(&git, &small)
            .inspect_err(|e| tracing::debug!("diff: scratch index failed, diffing one by one: {e}"))
            .ok(),
    };
    let runner = match &scratch {
        Some(index) => git
            .clone()
            .with_env("GIT_INDEX_FILE", index.path.as_os_str()),
        None => git.clone(),
    };
    let mut diffed = parse_patch(&runner.run_bytes(&args)?, opts)?;
    match scratch {
        Some(_) => {
            let untracked: std::collections::HashSet<&PathBuf> = small.iter().collect();
            for file in &mut diffed {
                if file.old_path.is_none() && untracked.contains(&file.path) {
                    file.status = FileStatus::Untracked;
                }
            }
        }
        None => {
            for path in &small {
                diffed.extend(diff_untracked(&git, worktree, path, opts)?);
            }
        }
    }
    files.extend(diffed);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// A copy of the worktree's index next to it, removed on drop.
struct ScratchIndex {
    path: PathBuf,
}

impl ScratchIndex {
    fn with_intent_to_add(git: &Git, paths: &[PathBuf]) -> Result<Self> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let index = git.run(["rev-parse", "--git-path", "index"])?;
        let index = git.dir().join(index.trim());
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = index.with_file_name(format!("index.hangar-diff-{}-{n}", std::process::id()));
        std::fs::copy(&index, &path)?;
        let scratch = Self { path };
        let mut list = Vec::new();
        for p in paths {
            list.extend_from_slice(p.as_os_str().as_encoded_bytes());
            list.push(0);
        }
        git.clone()
            .with_env("GIT_INDEX_FILE", scratch.path.as_os_str())
            .run_bytes_with(
                [
                    "--literal-pathspecs",
                    "add",
                    "--intent-to-add",
                    "--pathspec-from-file=-",
                    "--pathspec-file-nul",
                ],
                Some(&list),
                &[],
            )?;
        Ok(scratch)
    }
}

impl Drop for ScratchIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let mut lock = self.path.clone().into_os_string();
        lock.push(".lock");
        let _ = std::fs::remove_file(lock);
    }
}

/// Untracked, non-ignored files under `paths` (all when empty), relative to the worktree.
pub fn untracked_files(worktree: &Path, paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut args: Vec<OsString> = [
        "--literal-pathspecs",
        "ls-files",
        "--others",
        "--exclude-standard",
        "-z",
        "--",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.extend(paths.iter().map(|p| p.as_os_str().to_owned()));
    let out = Git::new(worktree).run_bytes(&args)?;
    Ok(out
        .split(|&b| b == 0)
        // A trailing slash marks a nested repository, which git does not diff into.
        .filter(|f| !f.is_empty() && !f.ends_with(b"/"))
        .map(bytes_to_path)
        .collect())
}

/// Every file in the worktree that is tracked or untracked-but-not-ignored, sorted, relative to
/// the worktree. Tracked files deleted from disk are left out.
pub fn list_files(worktree: &Path) -> Result<Vec<PathBuf>> {
    let out = Git::new(worktree).run_bytes([
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
        "-z",
    ])?;
    let mut files: Vec<PathBuf> = out
        .split(|&b| b == 0)
        // A trailing slash marks a nested repository.
        .filter(|f| !f.is_empty() && !f.ends_with(b"/"))
        .map(bytes_to_path)
        .filter(|p| worktree.join(p).symlink_metadata().is_ok())
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}

const PATCH_ARGS: [&str; 10] = [
    "--raw",
    "--patch",
    "-z",
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "--submodule=short",
    "--src-prefix=a/",
    "--dst-prefix=b/",
    "--no-relative",
];

/// `None` when the file vanished since it was listed.
fn diff_untracked(
    git: &Git,
    worktree: &Path,
    path: &Path,
    opts: &DiffOptions,
) -> Result<Option<FileDiff>> {
    let full = worktree.join(path);
    let meta = match std::fs::symlink_metadata(&full) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if opts.max_untracked_bytes.is_some_and(|max| meta.len() > max) {
        let mut file = FileDiff::untracked(path.to_path_buf());
        file.truncated = true;
        return Ok(Some(file));
    }
    let mut args: Vec<OsString> = ["diff", "--no-index"]
        .into_iter()
        .chain(PATCH_ARGS)
        .map(OsString::from)
        .collect();
    args.extend(["--".into(), "/dev/null".into(), path.as_os_str().to_owned()]);
    let out = match git.run_bytes_with(&args, None, &[1]) {
        Ok(out) => out,
        Err(_) if std::fs::symlink_metadata(&full).is_err() => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut file = parse_patch(&out, opts)?
        .into_iter()
        .next()
        .unwrap_or_else(|| FileDiff::untracked(path.to_path_buf()));
    file.path = path.to_path_buf();
    file.old_path = None;
    file.status = FileStatus::Untracked;
    Ok(Some(file))
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
