//! The changes panel: summary, grouped file list and a virtualized unified diff of the
//! selected file.

mod actions;
mod render;
mod rows;
mod wide;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::component::input::{InputState, TextareaState};
use gpui_kit::component::{ActiveTheme as _, Theme};
use gpui_kit::*;
use hangar_core::changes::{ChangedPath, FileDiff};
use hangar_core::ports::{CodeHost, Store};
use hangar_core::{AgentKind, WorkspaceId};

use super::highlight::{RowStyles, highlight_rows};
use super::language::language_for_path;
use super::live::WorkspaceDiff;
use super::model::{
    DiffRow, ListEntry, SplitRow, file_list, flatten, list_paths, reselect, split_rows, widest_row,
};
use super::review::{self, ReviewInfo, ReviewTarget};
pub use actions::Busy;

const LIST_ROW_HEIGHT: f32 = 28.;
const DIFF_ROW_HEIGHT: f32 = 19.;
/// File list rows shown before it scrolls.
const LIST_MAX_ROWS: usize = 12;
const GUTTER_WIDTH: f32 = 36.;

/// What the panel shows: a workspace's live diff and where review actions apply.
pub struct PanelSource {
    pub workspace: WorkspaceId,
    pub name: SharedString,
    pub diff: Entity<WorkspaceDiff>,
    pub target: ReviewTarget,
}

struct Source {
    workspace: WorkspaceId,
    name: SharedString,
    diff: Entity<WorkspaceDiff>,
    target: ReviewTarget,
    _observe: Subscription,
}

/// One commit message box per workspace, so drafts survive switching panes.
struct CommitBox {
    subject: Entity<InputState>,
    body: Entity<TextareaState>,
    _subscriptions: Vec<Subscription>,
}

/// The agent a workspace runs (or last ran), as the shell knows it.
#[derive(Clone, Debug)]
pub struct WorkspaceAgent {
    pub name: SharedString,
    pub kind: AgentKind,
    /// Still running, so a prompt can reach it.
    pub live: bool,
}

/// Looks up a workspace's agent. Called while rendering and from click handlers only, never
/// from inside an update of the shell.
pub type AgentProbe = Box<dyn Fn(WorkspaceId, &App) -> Option<WorkspaceAgent>>;

/// The diff on screen. Rows and styles always belong to `file`.
struct Shown {
    file: Arc<FileDiff>,
    rows: Arc<Vec<DiffRow>>,
    /// Side-by-side layout of `rows` (review screen).
    split: Arc<Vec<SplitRow>>,
    styles: Option<Arc<Vec<Option<RowStyles>>>>,
    /// Row with the longest text, measured for horizontal scrolling.
    widest: usize,
}

pub struct ChangesPanel {
    source: Option<Source>,
    list: Arc<Vec<ListEntry>>,
    paths: Vec<PathBuf>,
    selected: Option<PathBuf>,
    /// Selected as soon as it is listed (dev hook).
    preferred: Option<PathBuf>,
    shown: Option<Shown>,
    /// Bumped on every highlight request; stale results are dropped.
    generation: u64,
    highlight: Option<Task<()>>,
    list_scroll: UniformListScrollHandle,
    diff_scroll: UniformListScrollHandle,
    /// Worktree status of each listed path that is not only committed.
    stage: HashMap<PathBuf, ChangedPath>,
    staged: usize,
    uncommitted: usize,
    /// `None` until the login PATH is known: never run a `gh` found on another PATH.
    gh: Option<Arc<dyn CodeHost>>,
    info: HashMap<WorkspaceId, ReviewInfo>,
    info_tasks: HashMap<WorkspaceId, Task<()>>,
    busy: HashMap<WorkspaceId, Busy>,
    commit_boxes: HashMap<WorkspaceId, CommitBox>,
    /// Credit the workspace's agent with a `Co-authored-by` trailer (saved setting).
    coauthor: bool,
    store: Option<Arc<dyn Store>>,
    agent_probe: Option<AgentProbe>,
    /// Full-width review screen instead of the side panel.
    wide: bool,
    /// Side-by-side diff on the review screen.
    split_view: bool,
    /// Base branch name per workspace (`None`: the base is a commit), once resolved.
    base_names: HashMap<WorkspaceId, Option<String>>,
    base_tasks: HashMap<WorkspaceId, Task<()>>,
    /// Files marked viewed on the review screen, per workspace (not persisted).
    viewed: HashMap<WorkspaceId, HashSet<PathBuf>>,
    /// Focused on the review screen so Escape reaches its key context.
    focus: FocusHandle,
    _theme: Subscription,
}

actions!(changes, [ExitReview, CommitStaged]);

#[derive(Clone, Copy, Debug)]
pub struct ReviewStats {
    pub files: usize,
    pub unified_rows: usize,
    pub split_rows: usize,
    pub wide: bool,
}

pub enum ChangesEvent {
    /// The review screen's back button or Escape.
    ExitReview,
    /// The side panel's "Full review" button.
    OpenReview,
    /// Put `text` in the workspace agent's input (not sent) and focus it.
    AskAgent {
        workspace: WorkspaceId,
        text: String,
    },
}

impl EventEmitter<ChangesEvent> for ChangesPanel {}

impl ChangesPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let theme = cx.observe_global::<Theme>(|this, cx| {
            // Highlight colours come from the theme.
            if let Some(shown) = &this.shown {
                let (file, rows) = (shown.file.clone(), shown.rows.clone());
                this.highlight(file, rows, cx);
            }
        });
        Self {
            source: None,
            list: Arc::default(),
            paths: Vec::new(),
            selected: None,
            preferred: None,
            shown: None,
            generation: 0,
            highlight: None,
            list_scroll: UniformListScrollHandle::new(),
            diff_scroll: UniformListScrollHandle::new(),
            stage: HashMap::new(),
            staged: 0,
            uncommitted: 0,
            gh: None,
            info: HashMap::new(),
            info_tasks: HashMap::new(),
            busy: HashMap::new(),
            commit_boxes: HashMap::new(),
            coauthor: true,
            store: None,
            agent_probe: None,
            wide: false,
            split_view: true,
            base_names: HashMap::new(),
            base_tasks: HashMap::new(),
            viewed: HashMap::new(),
            focus: cx.focus_handle(),
            _theme: theme,
        }
    }

    pub fn set_wide(&mut self, wide: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.wide != wide {
            self.wide = wide;
            if wide {
                self.reveal_selected();
            }
            cx.notify();
        }
        if wide {
            window.focus(&self.focus, cx);
        }
    }

    /// Scrolls the file list to the selected file (the review screen lists more rows).
    fn reveal_selected(&self) {
        let Some(selected) = self.selected.as_deref() else {
            return;
        };
        let index = self
            .list
            .iter()
            .position(|e| matches!(e, ListEntry::File(f) if f.path == selected));
        if let Some(index) = index {
            self.list_scroll
                .scroll_to_item(index, ScrollStrategy::Center);
        }
    }

    pub fn set_split_view(&mut self, split: bool, cx: &mut Context<Self>) {
        self.split_view = split;
        cx.notify();
    }

    /// What the review screen shows (dev hooks).
    pub fn review_stats(&self) -> ReviewStats {
        ReviewStats {
            files: self.paths.len(),
            unified_rows: self.shown.as_ref().map_or(0, |s| s.rows.len()),
            split_rows: self.shown.as_ref().map_or(0, |s| s.split.len()),
            wide: self.wide,
        }
    }

    pub fn prefer(&mut self, path: PathBuf) {
        self.preferred = Some(path);
    }

    pub fn workspace(&self) -> Option<WorkspaceId> {
        self.source.as_ref().map(|s| s.workspace)
    }

    /// Shows the changes of another workspace (or none).
    pub fn set_source(&mut self, source: Option<PanelSource>, cx: &mut Context<Self>) {
        let Some(PanelSource {
            workspace,
            name,
            diff,
            target,
        }) = source
        else {
            if self.source.take().is_some() {
                self.clear();
                cx.notify();
            }
            return;
        };
        if let Some(current) = &mut self.source
            && current.workspace == workspace
            && current.diff == diff
        {
            current.name = name;
            current.target = target;
            return;
        }
        self.clear();
        let observe = cx.observe(&diff, |this, _, cx| this.sync(cx));
        self.source = Some(Source {
            workspace,
            name,
            diff,
            target,
            _observe: observe,
        });
        self.sync(cx);
        self.reload_info(cx);
        self.resolve_base(cx);
    }

    fn resolve_base(&mut self, cx: &mut Context<Self>) {
        let Some(source) = &self.source else { return };
        let workspace = source.workspace;
        if self.base_names.contains_key(&workspace) || self.base_tasks.contains_key(&workspace) {
            return;
        }
        let ws = &source.target.workspace;
        let (worktree, base) = (ws.worktree_path.clone(), ws.base.clone());
        let task = cx.spawn(async move |this, cx| {
            let name = cx
                .background_executor()
                .spawn(async move { review::resolve_base_name(&worktree, &base) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.base_names.insert(workspace, name);
                this.base_tasks.remove(&workspace);
                cx.notify();
            });
        });
        self.base_tasks.insert(workspace, task);
    }

    /// `main`, or the merge-base's short sha while the name is unknown or there is none.
    fn base_label(&self, merge_base: &str) -> String {
        let name = self
            .source
            .as_ref()
            .and_then(|s| self.base_names.get(&s.workspace))
            .and_then(|n| n.as_deref());
        review::base_label(name, merge_base)
    }

    fn clear(&mut self) {
        self.list = Arc::default();
        self.stage.clear();
        self.staged = 0;
        self.uncommitted = 0;
        self.paths.clear();
        self.selected = None;
        self.shown = None;
        self.highlight = None;
        self.generation += 1;
    }

    /// Rebuilds the list from the workspace diff, keeping the selection where possible.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let Some(source) = &self.source else {
            return;
        };
        let diff = source.diff.read(cx);
        let list = file_list(diff.files());
        match diff.summary() {
            Some(summary) => {
                self.stage = summary
                    .status
                    .changes
                    .iter()
                    .map(|c| (c.path.clone(), c.clone()))
                    .collect();
                self.staged = summary.status.staged;
                self.uncommitted = summary.status.changes.len();
            }
            None => {
                self.stage.clear();
                self.staged = 0;
                self.uncommitted = 0;
            }
        }
        let preferred = self.preferred.take_if(|p| list_paths(&list).contains(p));
        let selected = preferred.or_else(|| reselect(&self.paths, &list, self.selected.as_deref()));
        self.paths = list_paths(&list);
        self.list = Arc::new(list);
        tracing::debug!(
            workspace = %source.workspace,
            files = self.paths.len(),
            selected = ?selected,
            "changes panel synced"
        );
        self.select_path(selected, cx);
        cx.notify();
    }

    fn file(&self, path: &Path) -> Option<Arc<FileDiff>> {
        self.list.iter().find_map(|e| match e {
            ListEntry::File(f) if f.path == path => Some(f.clone()),
            _ => None,
        })
    }

    fn select_path(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        let file = path.as_deref().and_then(|p| self.file(p));
        self.selected = path;
        let Some(file) = file else {
            self.shown = None;
            self.highlight = None;
            self.generation += 1;
            return;
        };
        let same_path = self
            .shown
            .as_ref()
            .is_some_and(|s| s.file.path == file.path);
        if same_path
            && self
                .shown
                .as_ref()
                .is_some_and(|s| Arc::ptr_eq(&s.file, &file))
        {
            return;
        }
        let rows = Arc::new(flatten(&file));
        let has_language = language_for_path(&file.path).is_some();
        // A live update of the file on screen keeps the old (highlighted) diff until the new
        // highlights are ready, so it does not flash unstyled.
        if !(same_path && has_language) {
            self.shown = Some(Shown {
                widest: widest_row(&file, &rows),
                split: Arc::new(split_rows(&rows, &file)),
                file: file.clone(),
                rows: rows.clone(),
                styles: None,
            });
        }
        if !same_path {
            self.diff_scroll.scroll_to_item(0, ScrollStrategy::Top);
        }
        if has_language {
            self.highlight(file, rows, cx);
        } else {
            self.highlight = None;
            self.generation += 1;
        }
    }

    fn select(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.selected.as_ref() != Some(&path) {
            self.select_path(Some(path), cx);
            cx.notify();
        }
    }

    /// Highlights off the main thread, then shows `file` with its styles if it is still the
    /// latest request.
    fn highlight(&mut self, file: Arc<FileDiff>, rows: Arc<Vec<DiffRow>>, cx: &mut Context<Self>) {
        let Some(language) = language_for_path(&file.path) else {
            return;
        };
        self.generation += 1;
        let generation = self.generation;
        let theme = cx.theme().highlight_theme.clone();
        self.highlight = Some(cx.spawn(async move |this, cx| {
            let job = (file.clone(), rows.clone());
            let (styles, widest, ms) = cx
                .background_executor()
                .spawn(async move {
                    let started = Instant::now();
                    let (file, rows) = job;
                    let styles = highlight_rows(&file, &rows, language, &theme);
                    let widest = widest_row(&file, &rows);
                    (styles, widest, started.elapsed().as_millis() as u64)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                tracing::debug!(
                    path = %file.path.display(),
                    language,
                    rows = rows.len(),
                    ms,
                    "diff highlighted"
                );
                this.shown = Some(Shown {
                    split: Arc::new(split_rows(&rows, &file)),
                    file,
                    rows,
                    styles: Some(Arc::new(styles)),
                    widest,
                });
                cx.notify();
            });
        }));
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(source) = &self.source {
            source.diff.read(cx).refresh();
        }
    }
}
