//! The changes panel: summary, grouped file list and a virtualized unified diff of the
//! selected file.

mod actions;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::TextareaState;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, StyledExt as _, Theme, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_core::WorkspaceId;
use hangar_worktree::{ChangedPath, FileDiff, FileStatus, Gh, LineKind};

use super::highlight::{RowStyles, highlight_rows};
use super::language::language_for_path;
use super::live::WorkspaceDiff;
use super::model::{
    DiffRow, DisplayLine, ListEntry, file_list, file_title, flatten, hunk_header, list_paths,
    reselect, short_sha, status_letter, widest_row,
};
use super::review::{ReviewInfo, ReviewTarget, StageState};
pub use actions::Busy;

const LIST_ROW_HEIGHT: f32 = 22.;
const DIFF_ROW_HEIGHT: f32 = 18.;
/// File list rows shown before it scrolls.
const LIST_MAX_ROWS: usize = 12;
const GUTTER_WIDTH: f32 = 40.;

pub enum ChangesEvent {
    Close,
}

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
    input: Entity<TextareaState>,
    _change: Subscription,
}

/// The diff on screen. Rows and styles always belong to `file`.
struct Shown {
    file: Arc<FileDiff>,
    rows: Arc<Vec<DiffRow>>,
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
    gh: Option<Gh>,
    info: HashMap<WorkspaceId, ReviewInfo>,
    info_tasks: HashMap<WorkspaceId, Task<()>>,
    busy: HashMap<WorkspaceId, Busy>,
    commit_boxes: HashMap<WorkspaceId, CommitBox>,
    _theme: Subscription,
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
            _theme: theme,
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

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        h_flex()
            .h_8()
            .px_2()
            .gap_2()
            .flex_shrink_0()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .text_xs()
            .child(div().font_semibold().child("Changes"))
            .when_some(self.source.as_ref(), |this, source| {
                this.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(source.name.clone()),
                )
            })
            .child(div().flex_1())
            .when(self.source.is_some(), |this| {
                this.child(
                    Button::new("changes-refresh")
                        .ghost()
                        .xsmall()
                        .icon(IconName::RefreshCw)
                        .tooltip("Recompute the diff")
                        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                )
            })
            .child(
                Button::new("changes-close")
                    .ghost()
                    .xsmall()
                    .icon(IconName::PanelRightClose)
                    .tooltip("Hide changes (⌘⇧D)")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ChangesEvent::Close))),
            )
    }

    fn render_summary(&self, diff: &WorkspaceDiff, cx: &App) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let line = h_flex()
            .px_2()
            .py_1()
            .gap_2()
            .flex_shrink_0()
            .text_xs()
            .text_color(theme.muted_foreground);
        let line = match diff.summary() {
            None => line.child("Computing diff…"),
            Some(summary) => line
                .when_some(summary.status.branch.clone(), |this, branch| {
                    this.child(div().text_color(theme.foreground).truncate().child(branch))
                })
                .child(format!("base {}", short_sha(&summary.merge_base)))
                .child(format!(
                    "{} file{}",
                    summary.files,
                    if summary.files == 1 { "" } else { "s" }
                ))
                .child(
                    div()
                        .text_color(theme.success)
                        .child(format!("+{}", summary.insertions)),
                )
                .child(
                    div()
                        .text_color(theme.danger)
                        .child(format!("−{}", summary.deletions)),
                ),
        };
        v_flex()
            .child(line)
            .when_some(diff.error().cloned(), |this, err| {
                this.child(
                    div()
                        .px_2()
                        .pb_1()
                        .text_xs()
                        .text_color(theme.danger)
                        .child(err),
                )
            })
    }

    fn render_file_list(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let count = self.list.len();
        let height = px(LIST_ROW_HEIGHT * count.min(LIST_MAX_ROWS) as f32);
        div()
            .h(height)
            .flex_shrink_0()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                uniform_list(
                    "changes-files",
                    count,
                    cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                        let list = this.list.clone();
                        range
                            .filter_map(|ix| {
                                list.get(ix).map(|e| this.render_list_entry(ix, e, cx))
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .size_full()
                .track_scroll(&self.list_scroll),
            )
    }

    fn render_list_entry(
        &self,
        ix: usize,
        entry: &ListEntry,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let row = h_flex()
            .id(("changes-file", ix))
            .h(px(LIST_ROW_HEIGHT))
            .w_full()
            .px_2()
            .gap_2()
            .items_center()
            .text_xs();
        match entry {
            ListEntry::Header { group, count } => row
                .text_color(theme.muted_foreground)
                .font_semibold()
                .child(format!("{} ({count})", group.label()))
                .into_any_element(),
            ListEntry::File(file) => {
                let selected = self.selected.as_deref() == Some(file.path.as_path());
                let (dir, name) = split_path(&file.path);
                let path = file.path.clone();
                let change = self.stage.get(&file.path).cloned();
                let state = StageState::of(change.as_ref());
                let busy = self.current_busy().is_some();
                let toggle = change.map(|change| {
                    let tooltip = match state {
                        StageState::Staged => "Staged: click to unstage",
                        StageState::Partial => "Partly staged: click to stage the rest",
                        _ => "Click to stage",
                    };
                    Checkbox::new(("stage", ix))
                        .checked(state.checked())
                        .disabled(busy)
                        .tooltip(tooltip)
                        .on_click(cx.listener(move |this, checked: &bool, window, cx| {
                            this.toggle_stage(&change, *checked, window, cx);
                        }))
                        .into_any_element()
                });
                row.cursor_pointer()
                    .when(selected, |this| this.bg(theme.list_active))
                    .when(!selected, |this| this.hover(|s| s.bg(theme.list_hover)))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(path.clone(), cx)))
                    .child(
                        div()
                            .w_4()
                            .flex_shrink_0()
                            .when(state == StageState::Partial, |this| {
                                this.text_color(theme.warning)
                            })
                            .children(toggle),
                    )
                    .child(
                        div()
                            .w_3()
                            .flex_shrink_0()
                            .font_semibold()
                            .text_color(status_color(file.status, cx))
                            .child(status_letter(file.status)),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(div().flex_shrink_0().child(name))
                            .when(!dir.is_empty(), |this| {
                                this.child(
                                    div()
                                        .ml_2()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(theme.muted_foreground)
                                        .child(dir),
                                )
                            }),
                    )
                    .child(counts(file, cx))
                    .into_any_element()
            }
        }
    }

    fn render_diff(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(shown) = &self.shown else {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(if self.list.is_empty() {
                    ""
                } else {
                    "Select a file to see its diff."
                })
                .into_any_element();
        };
        let file = &shown.file;
        let title = h_flex()
            .h_8()
            .px_2()
            .gap_2()
            .flex_shrink_0()
            .items_center()
            .text_xs()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .font_semibold()
                    .text_color(status_color(file.status, cx))
                    .child(status_letter(file.status)),
            )
            .child(div().flex_1().min_w_0().truncate().child(file_title(file)))
            .child(counts(file, cx));
        let list = uniform_list(
            "changes-diff",
            shown.rows.len(),
            cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                let Some(shown) = &this.shown else {
                    return Vec::new();
                };
                tracing::trace!(?range, total = shown.rows.len(), "diff rows rendered");
                range
                    .filter_map(|ix| render_row(shown, ix, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .size_full()
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .with_width_from_item(Some(shown.widest))
        .track_scroll(&self.diff_scroll);
        v_flex()
            .flex_1()
            .min_h_0()
            .child(title)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .font_family(theme.mono_font_family.clone())
                    .text_size(px(12.))
                    .child(list),
            )
            .into_any_element()
    }
}

impl Render for ChangesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(cx);
        let review = self
            .source
            .is_some()
            .then(|| self.render_review(window, cx));
        let body = match &self.source {
            None => div()
                .flex_1()
                .p_4()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Focus a pane showing a workspace to see its changes.")
                .into_any_element(),
            Some(source) => {
                let diff = source.diff.read(cx);
                let summary = self.render_summary(diff, cx).into_any_element();
                let empty = diff.summary().is_some() && self.list.is_empty();
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(summary)
                    .children(review)
                    .when(empty, |this| {
                        this.child(
                            div()
                                .p_4()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("No changes since the merge-base."),
                        )
                    })
                    .when(!self.list.is_empty(), |this| {
                        this.child(self.render_file_list(cx))
                    })
                    .child(self.render_diff(cx))
                    .into_any_element()
            }
        };
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(header)
            .child(body)
    }
}

fn render_row(shown: &Shown, ix: usize, cx: &App) -> Option<AnyElement> {
    let theme = cx.theme();
    let row = h_flex()
        .h(px(DIFF_ROW_HEIGHT))
        .min_w_full()
        .flex_none()
        .items_center()
        .whitespace_nowrap();
    let gutter = |n: Option<u32>| {
        div()
            .w(px(GUTTER_WIDTH))
            .flex_shrink_0()
            .pr_1()
            .text_right()
            .text_color(theme.muted_foreground)
            .children(n.map(|n| n.to_string()))
    };
    let element = match *shown.rows.get(ix)? {
        DiffRow::Hunk { hunk } => {
            let hunk = shown.file.hunks.get(hunk)?;
            row.bg(theme.secondary)
                .text_color(theme.info)
                .child(div().w(px(GUTTER_WIDTH * 2. + 14.)).flex_shrink_0())
                .child(hunk_header(hunk))
                .into_any_element()
        }
        DiffRow::Notice(notice) => row
            .px_2()
            .text_color(theme.muted_foreground)
            .child(notice.text())
            .into_any_element(),
        DiffRow::Line { hunk, line } => {
            let line = shown.file.hunks.get(hunk)?.lines.get(line)?;
            let (bg, sign, sign_color) = match line.kind {
                LineKind::Added => (Some(theme.success.opacity(0.14)), "+", theme.success),
                LineKind::Removed => (Some(theme.danger.opacity(0.14)), "−", theme.danger),
                LineKind::Context | LineKind::NoNewlineAtEof => (None, " ", theme.muted_foreground),
            };
            let body: AnyElement = if line.kind == LineKind::NoNewlineAtEof {
                div()
                    .text_color(theme.muted_foreground)
                    .child("\\ No newline at end of file")
                    .into_any_element()
            } else {
                let text = DisplayLine::new(&line.text).text;
                let styles = shown
                    .styles
                    .as_ref()
                    .and_then(|s| s.get(ix).cloned().flatten());
                match styles {
                    Some(styles) if !styles.is_empty() => StyledText::new(text)
                        .with_highlights(styles)
                        .into_any_element(),
                    _ => SharedString::from(text).into_any_element(),
                }
            };
            row.when_some(bg, |this, bg| this.bg(bg))
                .child(gutter(line.old_lineno))
                .child(gutter(line.new_lineno))
                .child(
                    div()
                        .w(px(14.))
                        .flex_shrink_0()
                        .text_color(sign_color)
                        .child(sign),
                )
                .child(body)
                .into_any_element()
        }
    };
    Some(element)
}

fn counts(file: &FileDiff, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    h_flex()
        .gap_1()
        .flex_shrink_0()
        .when(file.binary, |this| {
            this.text_color(theme.muted_foreground).child("bin")
        })
        .when(!file.binary, |this| {
            this.child(
                div()
                    .text_color(theme.success)
                    .child(format!("+{}", file.insertions)),
            )
            .child(
                div()
                    .text_color(theme.danger)
                    .child(format!("−{}", file.deletions)),
            )
        })
}

fn status_color(status: FileStatus, cx: &App) -> Hsla {
    let theme = cx.theme();
    match status {
        FileStatus::Added | FileStatus::Untracked | FileStatus::Copied => theme.success,
        FileStatus::Deleted | FileStatus::Unmerged => theme.danger,
        FileStatus::Modified | FileStatus::Renamed | FileStatus::TypeChanged => theme.warning,
    }
}

/// `("dir/sub", "name.rs")` for display.
fn split_path(path: &Path) -> (String, String) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let dir = path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    (dir, name)
}
