use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use notify::event::ModifyKind;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::error::Result;

#[derive(Clone, Debug)]
pub struct WatcherConfig {
    /// Quiet period after the last relevant event before a batch is emitted.
    pub debounce: Duration,
    /// Upper bound on how long a batch may be held back while events keep arriving.
    pub max_delay: Duration,
    /// Directory names skipped anywhere in the tree without asking git.
    pub skip_dirs: Vec<String>,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(150),
            max_delay: Duration::from_secs(1),
            skip_dirs: [
                "node_modules",
                "Pods",
                "build",
                "DerivedData",
                ".gradle",
                "target",
            ]
            .map(String::from)
            .to_vec(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeBatch {
    /// Changed paths relative to the worktree, git-ignored ones removed. May name directories.
    pub paths: BTreeSet<PathBuf>,
    /// HEAD, the index, refs or merge state changed (commit, checkout, stage, reset...).
    pub git_meta_changed: bool,
    /// The OS dropped events (e.g. FSEvents `MustScanSubDirs`): treat everything as changed.
    pub rescan: bool,
    /// When the first event of this batch arrived.
    pub at: Instant,
}

/// Recursive watch of a worktree plus its git directories (which live outside the worktree
/// for linked worktrees). Batches are debounced on a dedicated thread. Dropping the
/// `Watcher` stops it and closes the channel.
pub struct Watcher {
    _inner: RecommendedWatcher,
    root: PathBuf,
}

impl Watcher {
    /// Starts watching `worktree`. Batches arrive on a tokio unbounded channel: its receiver
    /// needs no tokio runtime, so a GPUI background task can `recv().await` it directly.
    pub fn start(
        worktree: &Path,
        config: WatcherConfig,
    ) -> Result<(Self, UnboundedReceiver<ChangeBatch>)> {
        let (tx, rx) = unbounded_channel();
        let watcher = Self::with_sink(worktree, config, move |batch| tx.send(batch).is_ok())?;
        Ok((watcher, rx))
    }

    /// Like [`Watcher::start`] but hands batches to `sink`; returning `false` stops the thread.
    pub(crate) fn with_sink(
        worktree: &Path,
        config: WatcherConfig,
        sink: impl FnMut(ChangeBatch) -> bool + Send + 'static,
    ) -> Result<Self> {
        let root = worktree.canonicalize()?;
        let dirs = hangar_worktree::git_dirs(&root)?;
        let classifier = Classifier {
            git_dir: canonical(dirs.git_dir),
            common_dir: canonical(dirs.common_dir),
            root: root.clone(),
            skip_dirs: config.skip_dirs.clone(),
        };

        let (raw_tx, raw_rx) = mpsc::channel();
        let mut inner = notify::recommended_watcher(raw_tx)?;
        inner.watch(&root, RecursiveMode::Recursive)?;
        if !classifier.git_dir.starts_with(&root) {
            inner.watch(&classifier.git_dir, RecursiveMode::NonRecursive)?;
        }
        if classifier.common_dir != classifier.git_dir && !classifier.common_dir.starts_with(&root)
        {
            inner.watch(&classifier.common_dir, RecursiveMode::NonRecursive)?;
            let refs = classifier.common_dir.join("refs");
            if refs.is_dir() {
                inner.watch(&refs, RecursiveMode::Recursive)?;
            }
        }

        thread::Builder::new()
            .name(format!("hangar-watch {}", root.display()))
            .spawn(move || debounce_loop(raw_rx, classifier, config, sink))?;
        Ok(Self {
            _inner: inner,
            root,
        })
    }

    /// Canonical worktree path that batch paths are relative to.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

fn is_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

fn canonical(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

type RawEvent = notify::Result<notify::Event>;

#[derive(Default)]
struct Pending {
    candidates: BTreeSet<PathBuf>,
    git_meta_changed: bool,
    rescan: bool,
}

fn debounce_loop(
    raw_rx: mpsc::Receiver<RawEvent>,
    classifier: Classifier,
    config: WatcherConfig,
    mut sink: impl FnMut(ChangeBatch) -> bool,
) {
    loop {
        let mut pending = Pending::default();
        let first = loop {
            match raw_rx.recv() {
                Ok(event) => {
                    if classifier.add(&mut pending, event) {
                        break Instant::now();
                    }
                }
                Err(_) => return,
            }
        };
        let mut last = first;
        let mut disconnected = false;
        loop {
            let deadline = (last + config.debounce).min(first + config.max_delay);
            let wait = deadline.saturating_duration_since(Instant::now());
            match raw_rx.recv_timeout(wait) {
                Ok(event) => {
                    if classifier.add(&mut pending, event) {
                        last = Instant::now();
                    }
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if disconnected {
            return;
        }
        if let Some(batch) = classifier.finish(pending, first)
            && !sink(batch)
        {
            return;
        }
    }
}

struct Classifier {
    root: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
    skip_dirs: Vec<String>,
}

impl Classifier {
    /// Records `event`; returns whether it was relevant.
    fn add(&self, pending: &mut Pending, event: RawEvent) -> bool {
        let event = match event {
            Ok(event) => event,
            Err(e) => {
                tracing::warn!(error = %e, root = %self.root.display(), "file watcher error");
                pending.rescan = true;
                return true;
            }
        };
        if event.need_rescan() {
            pending.rescan = true;
            return true;
        }
        if matches!(event.kind, EventKind::Access(_)) {
            return false;
        }
        // Git does not track directories: creating or touching one only matters through the
        // files inside, which get events of their own. Renames and removals still count.
        let dir_noise = !matches!(
            event.kind,
            EventKind::Modify(ModifyKind::Name(_)) | EventKind::Remove(_)
        );
        let mut relevant = false;
        for path in &event.paths {
            match self.classify(path) {
                Class::Ignored => {}
                Class::GitMeta => {
                    pending.git_meta_changed = true;
                    relevant = true;
                }
                Class::Candidate(_) if dir_noise && is_dir(path) => {}
                Class::Candidate(rel) => {
                    pending.candidates.insert(rel);
                    relevant = true;
                }
            }
        }
        relevant
    }

    fn classify(&self, path: &Path) -> Class {
        if let Ok(rel) = path.strip_prefix(&self.git_dir) {
            return git_meta_class(rel, true);
        }
        if let Ok(rel) = path.strip_prefix(&self.common_dir) {
            // Shared refs move the base branch or ours; the main checkout's HEAD/index do not.
            return git_meta_class(rel, false);
        }
        let Ok(rel) = path.strip_prefix(&self.root) else {
            return Class::Ignored;
        };
        let mut components = rel.components().peekable();
        if components.peek().is_none() {
            return Class::Ignored;
        }
        for component in components {
            let Component::Normal(name) = component else {
                return Class::Ignored;
            };
            if name == ".git" || self.skip_dirs.iter().any(|d| name == d.as_str()) {
                return Class::Ignored;
            }
        }
        Class::Candidate(rel.to_path_buf())
    }

    fn finish(&self, pending: Pending, at: Instant) -> Option<ChangeBatch> {
        let Pending {
            mut candidates,
            git_meta_changed,
            rescan,
        } = pending;
        if !candidates.is_empty() {
            let list: Vec<PathBuf> = candidates.iter().cloned().collect();
            match hangar_worktree::check_ignored(&self.root, &list) {
                Ok(ignored) => {
                    for path in ignored {
                        candidates.remove(&path);
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, root = %self.root.display(), "git check-ignore failed");
                }
            }
        }
        if candidates.is_empty() && !git_meta_changed && !rescan {
            return None;
        }
        Some(ChangeBatch {
            paths: candidates,
            git_meta_changed,
            rescan,
            at,
        })
    }
}

enum Class {
    Ignored,
    GitMeta,
    Candidate(PathBuf),
}

fn git_meta_class(rel: &Path, own_git_dir: bool) -> Class {
    let mut components = rel.components();
    let Some(Component::Normal(first)) = components.next() else {
        return Class::Ignored;
    };
    let is_lock = rel
        .file_name()
        .is_some_and(|n| n.as_encoded_bytes().ends_with(b".lock"));
    if is_lock {
        return Class::Ignored;
    }
    let shared = first == "refs" || first == "packed-refs";
    let own = components.next().is_none()
        && [
            "HEAD",
            "index",
            "MERGE_HEAD",
            "REBASE_HEAD",
            "CHERRY_PICK_HEAD",
        ]
        .iter()
        .any(|name| first == *name);
    if shared || (own_git_dir && own) {
        Class::GitMeta
    } else {
        Class::Ignored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classifier() -> Classifier {
        Classifier {
            root: "/wt".into(),
            git_dir: "/repo/.git/worktrees/wt".into(),
            common_dir: "/repo/.git".into(),
            skip_dirs: WatcherConfig::default().skip_dirs,
        }
    }

    fn class(path: &str) -> &'static str {
        match classifier().classify(Path::new(path)) {
            Class::Ignored => "ignored",
            Class::GitMeta => "meta",
            Class::Candidate(_) => "candidate",
        }
    }

    #[test]
    fn classifies_paths() {
        assert_eq!(class("/wt/src/lib.rs"), "candidate");
        assert_eq!(class("/wt/my build.rs"), "candidate");
        assert_eq!(class("/wt"), "ignored");
        assert_eq!(class("/wt/.git"), "ignored");
        assert_eq!(class("/wt/app/node_modules/x/index.js"), "ignored");
        assert_eq!(class("/wt/ios/Pods/A.h"), "ignored");
        assert_eq!(class("/wt/target/debug/x"), "ignored");
        assert_eq!(class("/elsewhere/file"), "ignored");

        assert_eq!(class("/repo/.git/worktrees/wt/HEAD"), "meta");
        assert_eq!(class("/repo/.git/worktrees/wt/index"), "meta");
        assert_eq!(class("/repo/.git/worktrees/wt/index.lock"), "ignored");
        assert_eq!(class("/repo/.git/worktrees/wt/logs/HEAD"), "ignored");
        assert_eq!(class("/repo/.git/refs/heads/main"), "meta");
        assert_eq!(class("/repo/.git/refs/heads/main.lock"), "ignored");
        assert_eq!(class("/repo/.git/packed-refs"), "meta");
        assert_eq!(class("/repo/.git/HEAD"), "ignored");
        assert_eq!(class("/repo/.git/index"), "ignored");
        assert_eq!(class("/repo/.git/objects/ab/cdef"), "ignored");
    }

    #[test]
    fn main_checkout_git_dir_inside_root() {
        let c = Classifier {
            root: "/repo".into(),
            git_dir: "/repo/.git".into(),
            common_dir: "/repo/.git".into(),
            skip_dirs: Vec::new(),
        };
        assert!(matches!(
            c.classify(Path::new("/repo/.git/HEAD")),
            Class::GitMeta
        ));
        assert!(matches!(
            c.classify(Path::new("/repo/.git/objects/x")),
            Class::Ignored
        ));
        assert!(matches!(
            c.classify(Path::new("/repo/a.rs")),
            Class::Candidate(_)
        ));
    }
}
