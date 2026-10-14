use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Instant;

use hangar_worktree::{ChangeKind, DiffOptions, FileDiff, FileStatus, Status};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::error::{Error, Result};
use crate::watcher::{ChangeBatch, Watcher, WatcherConfig};

#[derive(Clone, Debug)]
pub struct LiveDiffConfig {
    pub watcher: WatcherConfig,
    pub diff: DiffOptions,
    /// Batches naming more paths than this trigger a full recompute instead.
    pub max_incremental_paths: usize,
}

impl Default for LiveDiffConfig {
    fn default() -> Self {
        Self {
            watcher: WatcherConfig::default(),
            diff: DiffOptions::default(),
            max_incremental_paths: 256,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffSummary {
    pub status: Status,
    /// Merge-base the diff is computed against.
    pub merge_base: String,
    pub files: usize,
    pub insertions: u64,
    pub deletions: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffUpdate {
    /// Files that appeared or whose diff changed, sorted by path.
    pub changed: Vec<FileDiff>,
    /// Paths that no longer differ from the merge-base.
    pub removed: Vec<PathBuf>,
    pub summary: DiffSummary,
    /// This update came from a full recompute (startup, git metadata change, rescan, refresh).
    pub full: bool,
    /// Arrival of the earliest file event behind this update; `None` for startup and refresh.
    pub trigger: Option<Instant>,
}

/// Live diff of a worktree against `merge-base(base, HEAD)`. A worker thread owns the git work:
/// it emits an initial full update, then one update per watcher batch that changed something.
/// Dropping the `LiveDiff` stops the watcher and the worker.
pub struct LiveDiff {
    _watcher: Watcher,
    input: mpsc::Sender<Input>,
    shared: Arc<Mutex<Shared>>,
}

#[derive(Default)]
struct Shared {
    files: BTreeMap<PathBuf, FileDiff>,
    summary: Option<DiffSummary>,
}

enum Input {
    Batch(ChangeBatch),
    Refresh,
    SetBase(String),
}

impl LiveDiff {
    /// Updates (or errors, after which the worker keeps going) arrive on an unbounded tokio
    /// channel whose receiver works without a tokio runtime.
    pub fn start(
        worktree: &Path,
        base: impl Into<String>,
        config: LiveDiffConfig,
    ) -> Result<(Self, UnboundedReceiver<Result<DiffUpdate>>)> {
        let (input, input_rx) = mpsc::channel();
        let batch_tx = input.clone();
        let watcher = Watcher::with_sink(worktree, config.watcher.clone(), move |batch| {
            batch_tx.send(Input::Batch(batch)).is_ok()
        })?;
        let (out_tx, out_rx) = unbounded_channel();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let worker = Worker {
            worktree: watcher.root().to_path_buf(),
            base: base.into(),
            config,
            merge_base: None,
            shared: shared.clone(),
            out: out_tx,
        };
        thread::Builder::new()
            .name(format!("hangar-livediff {}", worktree.display()))
            .spawn(move || worker.run(input_rx))?;
        Ok((
            Self {
                _watcher: watcher,
                input,
                shared,
            },
            out_rx,
        ))
    }

    /// Requests a full recompute.
    pub fn refresh(&self) -> Result<()> {
        self.input.send(Input::Refresh).map_err(|_| Error::Stopped)
    }

    /// Diffs against a different base from now on (full recompute).
    pub fn set_base(&self, base: impl Into<String>) -> Result<()> {
        self.input
            .send(Input::SetBase(base.into()))
            .map_err(|_| Error::Stopped)
    }

    /// Current per-file diffs, as of the last emitted update.
    pub fn snapshot(&self) -> BTreeMap<PathBuf, FileDiff> {
        lock(&self.shared).files.clone()
    }

    pub fn file(&self, path: &Path) -> Option<FileDiff> {
        lock(&self.shared).files.get(path).cloned()
    }

    /// `None` until the first update is computed.
    pub fn summary(&self) -> Option<DiffSummary> {
        lock(&self.shared).summary.clone()
    }
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

/// Coalesced work drained from the input channel.
#[derive(Default)]
struct Job {
    paths: BTreeSet<PathBuf>,
    full: bool,
    trigger: Option<Instant>,
}

impl Job {
    fn add(&mut self, input: Input, base: &mut String) {
        match input {
            Input::Batch(batch) => {
                self.full |= batch.git_meta_changed || batch.rescan;
                self.paths.extend(batch.paths);
                self.trigger = Some(self.trigger.map_or(batch.at, |t| t.min(batch.at)));
            }
            Input::Refresh => self.full = true,
            Input::SetBase(new) => {
                *base = new;
                self.full = true;
            }
        }
    }
}

struct Worker {
    worktree: PathBuf,
    base: String,
    config: LiveDiffConfig,
    merge_base: Option<String>,
    shared: Arc<Mutex<Shared>>,
    out: UnboundedSender<Result<DiffUpdate>>,
}

impl Worker {
    fn run(mut self, input: mpsc::Receiver<Input>) {
        let mut job = Job {
            full: true,
            ..Job::default()
        };
        let mut first = true;
        loop {
            while let Ok(more) = input.try_recv() {
                job.add(more, &mut self.base);
            }
            let result = self.process(std::mem::take(&mut job), first);
            let sent = match result {
                Ok(Some(update)) => {
                    first = false;
                    self.out.send(Ok(update))
                }
                Ok(None) => Ok(()),
                Err(e) => {
                    tracing::warn!(error = %e, worktree = %self.worktree.display(), "live diff");
                    // Retry from scratch next time: the merge-base may be what failed.
                    self.merge_base = None;
                    self.out.send(Err(e))
                }
            };
            if sent.is_err() {
                return;
            }
            match input.recv() {
                Ok(next) => job.add(next, &mut self.base),
                Err(_) => return,
            }
        }
    }

    fn process(&mut self, job: Job, force_emit: bool) -> Result<Option<DiffUpdate>> {
        let full = job.full
            || self.merge_base.is_none()
            || job.paths.len() > self.config.max_incremental_paths;
        if full {
            self.merge_base = Some(hangar_worktree::merge_base(&self.worktree, &self.base)?);
        }
        let merge_base = self.merge_base.clone().unwrap_or_default();
        let (mut files, previous_status) = {
            let shared = lock(&self.shared);
            (
                shared.files.clone(),
                shared.summary.as_ref().map(|s| s.status.clone()),
            )
        };
        let (pathspecs, covered): (Vec<PathBuf>, Vec<PathBuf>) = if full {
            (Vec::new(), files.keys().cloned().collect())
        } else {
            let mut specs: BTreeSet<PathBuf> = job.paths.clone();
            // Re-diff both sides of a known rename when either side is touched.
            for file in files.values() {
                if let Some(old) = &file.old_path
                    && (job.paths.contains(&file.path) || job.paths.contains(old))
                {
                    specs.insert(file.path.clone());
                    specs.insert(old.clone());
                }
            }
            let covered = files
                .keys()
                .filter(|k| specs.iter().any(|s| k.starts_with(s)))
                .cloned()
                .collect();
            (specs.into_iter().collect(), covered)
        };
        if !full && pathspecs.is_empty() {
            return Ok(None);
        }

        // A full `git status` stats every tracked file (~200 ms at 40k files): only do it
        // when everything is recomputed, otherwise patch the previous one.
        let status = match previous_status {
            Some(mut status) if !full => {
                let partial = hangar_worktree::status_paths(&self.worktree, &pathspecs)?;
                status.merge_partial(partial, &pathspecs);
                status
            }
            _ => hangar_worktree::status(&self.worktree)?,
        };
        let conflicted: BTreeSet<&Path> = status
            .changes
            .iter()
            .filter(|c| c.kind == ChangeKind::Conflicted)
            .map(|c| c.path.as_path())
            .collect();

        let fresh = hangar_worktree::diff_paths(
            &self.worktree,
            &merge_base,
            &pathspecs,
            &self.config.diff,
        )?;
        let mut changed = Vec::new();
        let mut seen = BTreeSet::new();
        for mut file in fresh {
            if conflicted.contains(file.path.as_path()) {
                file.status = FileStatus::Unmerged;
            }
            seen.insert(file.path.clone());
            if files.get(&file.path) != Some(&file) {
                files.insert(file.path.clone(), file.clone());
                changed.push(file);
            }
        }
        let removed: Vec<PathBuf> = covered.into_iter().filter(|p| !seen.contains(p)).collect();
        for path in &removed {
            files.remove(path);
        }

        let summary = DiffSummary {
            merge_base,
            files: files.len(),
            insertions: files.values().map(|f| u64::from(f.insertions)).sum(),
            deletions: files.values().map(|f| u64::from(f.deletions)).sum(),
            status,
        };
        let mut shared = lock(&self.shared);
        let summary_changed = shared.summary.as_ref() != Some(&summary);
        shared.files = files;
        shared.summary = Some(summary.clone());
        drop(shared);

        if changed.is_empty() && removed.is_empty() && !summary_changed && !force_emit {
            return Ok(None);
        }
        Ok(Some(DiffUpdate {
            changed,
            removed,
            summary,
            full,
            trigger: job.trigger,
        }))
    }
}
