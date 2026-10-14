//! The changes panel: summary, grouped file list and a virtualized unified diff of the
//! selected file.

mod actions;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::Icon;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::TextareaState;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, StyledExt as _, Theme, h_flex, v_flex,
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

use crate::ui::components::{Delta, UiButton};
use crate::ui::{ActiveTokens as _, FONT_MONO, TypeScale as _};

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

    /// `branch → base` with the totals and a refresh button; errors below it.
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = cx.tokens();
        let diff = self.source.as_ref().map(|s| s.diff.read(cx));
        let summary = diff.and_then(|d| d.summary().cloned());
        let error = diff.and_then(|d| d.error().cloned());
        let line = h_flex()
            .h(px(36.))
            .px_3()
            .gap_2()
            .flex_shrink_0()
            .items_center()
            .font_family(FONT_MONO)
            .text_size(px(11.))
            .text_color(t.tx2)
            .map(|this| match (&self.source, &summary) {
                (None, _) => this.child("No workspace"),
                (Some(_), None) => this.child("Computing diff…"),
                (Some(source), Some(summary)) => this
                    .child(Icon::new(Lucide::GitBranch).size(px(12.)))
                    .child(
                        div().min_w_0().truncate().text_color(t.tx).child(
                            summary
                                .status
                                .branch
                                .clone()
                                .unwrap_or_else(|| source.name.to_string()),
                        ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(t.tx3)
                            .child(format!("→ {}", short_sha(&summary.merge_base))),
                    ),
            })
            .child(div().flex_1())
            .when_some(summary.as_ref(), |this, summary| {
                let n = |v: u64| usize::try_from(v).unwrap_or(usize::MAX);
                this.child(Delta::new(n(summary.insertions), n(summary.deletions)))
            })
            .when(self.source.is_some(), |this| {
                this.child(
                    UiButton::new("changes-refresh")
                        .icon(Lucide::RefreshCw)
                        .ghost()
                        .small()
                        .tooltip("Recompute the diff")
                        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                )
            });
        v_flex()
            .flex_shrink_0()
            .border_b_1()
            .border_color(t.line)
            .child(line)
            .when_some(error, |this, err| {
                this.child(
                    div()
                        .px_3()
                        .pb_1p5()
                        .text_size(px(11.))
                        .text_color(t.red)
                        .child(err),
                )
            })
    }

    fn render_file_list(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let count = self.list.len();
        let height = px(LIST_ROW_HEIGHT * count.min(LIST_MAX_ROWS) as f32);
        div()
            .h(height + px(8.))
            .flex_shrink_0()
            .px_1p5()
            .py_1()
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
            .px_1p5()
            .gap_2()
            .items_center()
            .rounded(px(4.))
            .text_size(px(12.));
        match entry {
            ListEntry::Header { group, count } => row
                .text_color(cx.tokens().tx3)
                .type_label()
                .child(format!("{} ({count})", group.label().to_uppercase()))
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
                            .font_family(FONT_MONO)
                            .text_size(px(10.))
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
                            .when(!dir.is_empty(), |this| {
                                this.child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(cx.tokens().tx3)
                                        .child(format!("{dir}/")),
                                )
                            })
                            .child(div().flex_shrink_0().child(name)),
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
            .h(px(28.))
            .px(px(10.))
            .gap_2()
            .flex_shrink_0()
            .items_center()
            .font_family(FONT_MONO)
            .text_size(px(11.))
            .text_color(cx.tokens().tx2)
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
                    .bg(cx.tokens().app)
                    .font_family(FONT_MONO)
                    .text_size(px(11.5))
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
                let empty = diff.summary().is_some() && self.list.is_empty();
                v_flex()
                    .flex_1()
                    .min_h_0()
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
            .bg(cx.tokens().panel)
            .child(header)
            .child(body)
    }
}

fn render_row(shown: &Shown, ix: usize, cx: &App) -> Option<AnyElement> {
    let t = cx.tokens();
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
            .text_color(t.tx3)
            .children(n.map(|n| n.to_string()))
    };
    let element = match *shown.rows.get(ix)? {
        DiffRow::Hunk { hunk } => {
            let hunk = shown.file.hunks.get(hunk)?;
            row.bg(t.accbg)
                .text_color(t.acc)
                .child(div().w(px(GUTTER_WIDTH * 2. + 14.)).flex_shrink_0())
                .child(hunk_header(hunk))
                .into_any_element()
        }
        DiffRow::Notice(notice) => row
            .px_2()
            .text_color(t.tx3)
            .child(notice.text())
            .into_any_element(),
        DiffRow::Line { hunk, line } => {
            let line = shown.file.hunks.get(hunk)?.lines.get(line)?;
            let (bg, sign, sign_color) = match line.kind {
                LineKind::Added => (Some(t.addbg), "+", t.grn),
                LineKind::Removed => (Some(t.delbg), "−", t.red),
                LineKind::Context | LineKind::NoNewlineAtEof => (None, " ", t.tx3),
            };
            let body: AnyElement = if line.kind == LineKind::NoNewlineAtEof {
                div()
                    .text_color(t.tx3)
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

fn counts(file: &FileDiff, cx: &App) -> AnyElement {
    if file.binary {
        return div()
            .flex_none()
            .font_family(FONT_MONO)
            .text_size(px(11.))
            .text_color(cx.tokens().tx3)
            .child("bin")
            .into_any_element();
    }
    Delta::new(file.insertions as usize, file.deletions as usize).into_any_element()
}

fn status_color(status: FileStatus, cx: &App) -> Hsla {
    let t = cx.tokens();
    match status {
        FileStatus::Added | FileStatus::Untracked | FileStatus::Copied => t.grn,
        FileStatus::Deleted | FileStatus::Unmerged => t.red,
        FileStatus::Modified | FileStatus::Renamed | FileStatus::TypeChanged => t.amb,
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
