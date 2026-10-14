//! Working-tree changes and diffs as plain data, produced by a [`crate::ports::Changes`] adapter.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
    Conflicted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedPath {
    pub path: PathBuf,
    /// Source path of a rename or copy.
    pub orig_path: Option<PathBuf>,
    pub kind: ChangeKind,
    pub staged: bool,
    pub unstaged: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// `None` when HEAD is detached.
    pub branch: Option<String>,
    /// `None` before the first commit.
    pub oid: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub staged: usize,
    pub unstaged: usize,
    pub untracked: usize,
    pub conflicted: usize,
    pub changes: Vec<ChangedPath>,
}

impl Status {
    pub fn is_clean(&self) -> bool {
        self.changes.is_empty()
    }
}

impl Status {
    /// Replaces the changes under `paths` with those of `partial` (a status limited to the same paths
    /// for the same paths) and takes its branch headers.
    pub fn merge_partial(&mut self, partial: Status, paths: &[PathBuf]) {
        let covered = |p: &Path| paths.iter().any(|spec| p.starts_with(spec));
        self.changes
            .retain(|c| !covered(&c.path) && !c.orig_path.as_deref().is_some_and(covered));
        self.changes.extend(partial.changes);
        self.changes.sort_by(|a, b| a.path.cmp(&b.path));
        self.changes.dedup_by(|a, b| a.path == b.path);
        self.branch = partial.branch;
        self.oid = partial.oid;
        self.upstream = partial.upstream;
        self.ahead = partial.ahead;
        self.behind = partial.behind;
        self.staged = self.changes.iter().filter(|c| c.staged).count();
        self.unstaged = self.changes.iter().filter(|c| c.unstaged).count();
        self.untracked = self.count(ChangeKind::Untracked);
        self.conflicted = self.count(ChangeKind::Conflicted);
    }

    fn count(&self, kind: ChangeKind) -> usize {
        self.changes.iter().filter(|c| c.kind == kind).count()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
    /// Merge conflict in progress; git prints no hunks for it.
    Unmerged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LineKind {
    Context,
    Added,
    Removed,
    /// `\ No newline at end of file`, applying to the line before it.
    NoNewlineAtEof,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_lineno: Option<u32>,
    pub new_lineno: Option<u32>,
    /// Line content without its prefix and line terminator (`\n` or `\r\n`).
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    /// Function context git prints after the second `@@`, possibly empty.
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    pub path: PathBuf,
    /// Source path of a rename or copy.
    pub old_path: Option<PathBuf>,
    pub status: FileStatus,
    /// Octal git modes (`0o100644`, `0o120000`, ...); `None` on the missing side.
    pub old_mode: Option<u32>,
    pub new_mode: Option<u32>,
    pub binary: bool,
    /// Hunks were cut at [`DiffOptions::max_lines_per_file`] or the file was too large to diff.
    pub truncated: bool,
    /// Counted over the whole patch, including lines dropped by truncation.
    pub insertions: u32,
    pub deletions: u32,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    pub fn new(path: PathBuf, status: FileStatus) -> Self {
        Self {
            path,
            old_path: None,
            status,
            old_mode: None,
            new_mode: None,
            binary: false,
            truncated: false,
            insertions: 0,
            deletions: 0,
            hunks: Vec::new(),
        }
    }

    /// An untracked file as a regular-file addition, before its contents are read.
    pub fn untracked(path: PathBuf) -> Self {
        let mut file = Self::new(path, FileStatus::Untracked);
        file.new_mode = Some(0o100644);
        file
    }

    pub fn line_count(&self) -> usize {
        self.hunks.iter().map(|h| h.lines.len()).sum()
    }
}

/// What a session cost, as its agent reports it (a running total in one currency).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Cost {
    pub amount: f64,
    /// ISO 4217 code, e.g. `USD`.
    pub currency: String,
}

/// Size of a workspace's diff: changed files, inserted and deleted lines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiffStat {
    pub files: u32,
    pub insertions: u32,
    pub deletions: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffOptions {
    /// Stop collecting lines past this many per file and set [`FileDiff::truncated`].
    pub max_lines_per_file: Option<usize>,
    /// Untracked files larger than this are not diffed (reported truncated, without hunks).
    pub max_untracked_bytes: Option<u64>,
    pub detect_renames: bool,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            max_lines_per_file: Some(20_000),
            max_untracked_bytes: Some(8 * 1024 * 1024),
            detect_renames: true,
        }
    }
}

/// Absolute git directories of a worktree. For a linked worktree `git_dir` is
/// `<repo>/.git/worktrees/<name>` (HEAD, index) and `common_dir` is `<repo>/.git` (refs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitDirs {
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
}
