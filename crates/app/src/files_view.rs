//! The Files tab: the focused workspace's worktree as a tree, changed files marked, and a
//! read-only viewer.

mod tree;
mod wide;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, Theme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_core::WorkspaceId;
use hangar_core::changes::{FileDiff, LineKind};

use crate::changes::highlight::{RowStyles, highlight_lines};
use crate::changes::language::language_for_path;
use crate::changes::live::WorkspaceDiff;
use crate::changes::model::DisplayLine;
use crate::ui::components::{EmptyState, UiButton};
use crate::ui::{ActiveTokens as _, TypeScale as _};
pub use tree::{TreeRow, ancestors, tree_rows};

const TREE_ROW_HEIGHT: f32 = 24.;
const VIEW_ROW_HEIGHT: f32 = 19.;
/// Files larger than this are not shown.
const MAX_VIEW_BYTES: u64 = 2 * 1024 * 1024;

pub struct FilesSource {
    pub workspace: WorkspaceId,
    pub root: PathBuf,
    pub diff: Entity<WorkspaceDiff>,
}

struct Source {
    workspace: WorkspaceId,
    root: PathBuf,
    diff: Entity<WorkspaceDiff>,
    _observe: Subscription,
}

enum Viewer {
    None,
    Loading(PathBuf),
    Text {
        path: PathBuf,
        lines: Arc<Vec<SharedString>>,
        /// Syntax styles per line, once highlighted (none for plain text).
        styles: Option<Arc<Vec<RowStyles>>>,
    },
    Message {
        path: PathBuf,
        text: SharedString,
    },
}

pub struct FilesPanel {
    source: Option<Source>,
    files: Arc<Vec<PathBuf>>,
    expanded: HashSet<PathBuf>,
    rows: Arc<Vec<TreeRow>>,
    error: Option<SharedString>,
    viewer: Viewer,
    list_task: Option<Task<()>>,
    read_task: Option<Task<()>>,
    highlight_task: Option<Task<()>>,
    tree_scroll: UniformListScrollHandle,
    view_scroll: UniformListScrollHandle,
    /// Shown as the full-width files screen rather than in the side panel.
    wide: bool,
    focus: FocusHandle,
    _theme: Subscription,
}

actions!(files, [ExitFiles]);

pub enum FilesEvent {
    /// The viewer's expand button.
    OpenWide,
    /// The files screen's back button or Escape.
    ExitWide,
}

impl EventEmitter<FilesEvent> for FilesPanel {}

impl FilesPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // Highlight colours come from the theme.
        let theme = cx.observe_global::<Theme>(|this, cx| this.highlight(cx));
        Self {
            source: None,
            files: Arc::default(),
            expanded: HashSet::new(),
            rows: Arc::default(),
            error: None,
            viewer: Viewer::None,
            list_task: None,
            read_task: None,
            highlight_task: None,
            tree_scroll: UniformListScrollHandle::new(),
            view_scroll: UniformListScrollHandle::new(),
            wide: false,
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

    /// Scrolls the tree to the file in the viewer (the files screen lists more rows).
    fn reveal_selected(&self) {
        let Some(path) = self.viewer_path() else {
            return;
        };
        if let Some(index) = self.rows.iter().position(|r| r.path == *path) {
            self.tree_scroll
                .scroll_to_item(index, ScrollStrategy::Center);
        }
    }

    fn viewer_path(&self) -> Option<&PathBuf> {
        match &self.viewer {
            Viewer::Loading(p) | Viewer::Text { path: p, .. } | Viewer::Message { path: p, .. } => {
                Some(p)
            }
            Viewer::None => None,
        }
    }

    pub fn set_source(&mut self, source: Option<FilesSource>, cx: &mut Context<Self>) {
        let same = match (&self.source, &source) {
            (Some(a), Some(b)) => a.workspace == b.workspace && a.diff == b.diff,
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.files = Arc::default();
        self.rows = Arc::default();
        self.expanded.clear();
        self.error = None;
        self.viewer = Viewer::None;
        self.read_task = None;
        self.highlight_task = None;
        self.source = source.map(|s| Source {
            // The tree follows the worktree: relist when its diff (file events) changes.
            _observe: cx.observe(&s.diff, |this, _, cx| this.reload(cx)),
            workspace: s.workspace,
            root: s.root,
            diff: s.diff,
        });
        self.reload(cx);
        cx.notify();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(source) = &self.source else {
            self.list_task = None;
            return;
        };
        let (root, workspace) = (source.root.clone(), source.workspace);
        self.list_task = Some(cx.spawn(async move |this, cx| {
            let listed = cx
                .background_executor()
                .spawn(async move { crate::adapters::changes().list_files(&root) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.source.as_ref().map(|s| s.workspace) != Some(workspace) {
                    return;
                }
                match listed {
                    Ok(files) => {
                        this.error = None;
                        this.files = Arc::new(files);
                    }
                    Err(err) => this.error = Some(format!("Could not list files: {err}").into()),
                }
                this.rebuild();
                cx.notify();
            });
        }));
    }

    /// Listed files, visible tree rows and lines in the viewer (dev hooks).
    pub fn stats(&self) -> (usize, usize, Option<usize>) {
        let lines = match &self.viewer {
            Viewer::Text { lines, .. } => Some(lines.len()),
            _ => None,
        };
        (self.files.len(), self.rows.len(), lines)
    }

    fn rebuild(&mut self) {
        self.rows = Arc::new(tree_rows(&self.files, &self.expanded));
    }

    fn toggle_dir(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !self.expanded.remove(&path) {
            self.expanded.insert(path);
        }
        self.rebuild();
        cx.notify();
    }

    /// Shows `path` (worktree-relative) in the viewer, revealing it in the tree.
    pub fn open(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(full) = self.source.as_ref().map(|s| s.root.join(&path)) else {
            return;
        };
        self.expanded.extend(ancestors(&path));
        self.rebuild();
        self.viewer = Viewer::Loading(path.clone());
        self.read_task = Some(cx.spawn(async move |this, cx| {
            let read = cx
                .background_executor()
                .spawn(async move { read_text(&full) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if !matches!(&this.viewer, Viewer::Loading(p) if *p == path) {
                    return;
                }
                this.viewer = match read {
                    Ok(lines) => Viewer::Text {
                        path,
                        lines: Arc::new(lines),
                        styles: None,
                    },
                    Err(text) => Viewer::Message { path, text },
                };
                this.view_scroll.scroll_to_item(0, ScrollStrategy::Top);
                this.highlight(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Highlights the file in the viewer off the main thread (again after a theme change).
    fn highlight(&mut self, cx: &mut Context<Self>) {
        let Viewer::Text { path, lines, .. } = &self.viewer else {
            self.highlight_task = None;
            return;
        };
        let Some(language) = language_for_path(path) else {
            self.highlight_task = None;
            return;
        };
        let (path, lines) = (path.clone(), lines.clone());
        let theme = cx.theme().highlight_theme.clone();
        self.highlight_task = Some(cx.spawn(async move |this, cx| {
            let job = lines.clone();
            let styles = cx
                .background_executor()
                .spawn(async move { highlight_lines(&job, language, &theme) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Viewer::Text {
                    path: shown,
                    lines: shown_lines,
                    styles: slot,
                } = &mut this.viewer
                    && *shown == path
                    && Arc::ptr_eq(shown_lines, &lines)
                {
                    *slot = Some(Arc::new(styles));
                    cx.notify();
                }
            });
        }));
    }

    fn render_tree_row(&self, ix: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = cx.tokens();
        let row = self.rows.get(ix)?.clone();
        let changed = !row.dir
            && self
                .source
                .as_ref()
                .is_some_and(|s| s.diff.read(cx).files().contains_key(&row.path));
        let selected = self.viewer_path() == Some(&row.path);
        let icon = if row.dir {
            if row.expanded {
                Lucide::FolderOpen
            } else {
                Lucide::Folder
            }
        } else {
            Lucide::File
        };
        let path = row.path.clone();
        let dir = row.dir;
        Some(
            h_flex()
                .id(("file-row", ix))
                .w_full()
                .h(px(TREE_ROW_HEIGHT))
                .pl(px(6. + row.depth as f32 * 14.))
                .pr_1p5()
                .gap_1p5()
                .items_center()
                .rounded(px(4.))
                .text_size(px(12.))
                .text_color(if dir { t.tx2 } else { t.tx })
                .when(selected, |this| this.bg(t.active))
                .when(!selected, |this| this.hover(|s| s.bg(t.hover)))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    if dir {
                        this.toggle_dir(path.clone(), cx)
                    } else {
                        this.open(path.clone(), cx)
                    }
                }))
                .child(Icon::new(icon).with_size(px(12.)).text_color(t.tx3))
                .child(div().flex_1().min_w_0().truncate().child(row.name))
                .when(changed, |this| {
                    this.child(div().size(px(6.)).rounded_full().bg(t.amb).flex_none())
                })
                .into_any_element(),
        )
    }

    fn render_viewer(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.tokens();
        let (path, body): (Option<&PathBuf>, AnyElement) = match &self.viewer {
            Viewer::None => (
                None,
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(12.))
                    .text_color(t.tx3)
                    .child("Select a file to view it.")
                    .into_any_element(),
            ),
            Viewer::Loading(p) => (
                Some(p),
                div()
                    .p_3()
                    .text_size(px(12.))
                    .text_color(t.tx3)
                    .child("Loading…")
                    .into_any_element(),
            ),
            Viewer::Message { path, text } => (
                Some(path),
                div()
                    .p_3()
                    .text_size(px(12.))
                    .text_color(t.tx2)
                    .child(text.clone())
                    .into_any_element(),
            ),
            Viewer::Text { path, lines, .. } => {
                let count = lines.len();
                let gutter = px(12. + count.to_string().len() as f32 * 7.5);
                let added: Arc<HashSet<u32>> = Arc::new(
                    self.source
                        .as_ref()
                        .and_then(|s| s.diff.read(cx).files().get(path).map(|f| added_lines(f)))
                        .unwrap_or_default(),
                );
                (
                    Some(path),
                    uniform_list(
                        "file-view",
                        count,
                        cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                            let t = cx.tokens();
                            let Viewer::Text { lines, styles, .. } = &this.viewer else {
                                return Vec::new();
                            };
                            range
                                .filter_map(|ix| {
                                    let line = lines.get(ix)?.clone();
                                    let line_styles = styles
                                        .as_ref()
                                        .and_then(|s| s.get(ix))
                                        .filter(|s| !s.is_empty());
                                    let body = match line_styles {
                                        Some(runs) => StyledText::new(line)
                                            .with_highlights(runs.clone())
                                            .into_any_element(),
                                        None => line.into_any_element(),
                                    };
                                    let added = u32::try_from(ix + 1)
                                        .is_ok_and(|number| added.contains(&number));
                                    Some(
                                        h_flex()
                                            .h(px(VIEW_ROW_HEIGHT))
                                            .min_w_full()
                                            .whitespace_nowrap()
                                            .when(added, |this| this.bg(t.addbg))
                                            .child(
                                                div()
                                                    .w(gutter + px(12.))
                                                    .flex_none()
                                                    .pr_3()
                                                    .text_right()
                                                    .text_color(t.tx3)
                                                    .child((ix + 1).to_string()),
                                            )
                                            .child(body),
                                    )
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .size_full()
                    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                    .track_scroll(&self.view_scroll)
                    .into_any_element(),
                )
            }
        };
        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                h_flex()
                    .flex_none()
                    .h(px(28.))
                    .px(px(10.))
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(t.line)
                    .font_mono()
                    .text_size(px(11.))
                    .text_color(t.tx2)
                    .child(Icon::new(Lucide::File).with_size(px(12.)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(path.map(|p| p.display().to_string()).unwrap_or_default()),
                    )
                    .child(div().text_color(t.tx3).child("read-only"))
                    .when(!self.wide, |this| {
                        this.child(
                            UiButton::new("files-wide")
                                .icon(Lucide::Maximize2)
                                .ghost()
                                .small()
                                .tooltip("Open full width (⇧⌘E)")
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(FilesEvent::OpenWide))),
                        )
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .py_1p5()
                    .bg(t.app)
                    .font_mono()
                    .text_size(px(11.5))
                    .child(body),
            )
            .into_any_element()
    }
}

/// Line numbers (1-based, in the new file) the diff adds.
fn added_lines(file: &FileDiff) -> HashSet<u32> {
    file.hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| l.kind == LineKind::Added)
        .filter_map(|l| l.new_lineno)
        .collect()
}

/// The file's lines, or a short message when it is too big, binary or unreadable.
fn read_text(path: &std::path::Path) -> Result<Vec<SharedString>, SharedString> {
    use std::io::Read as _;

    let cannot_read = |e: std::io::Error| SharedString::from(format!("Cannot read: {e}"));
    let meta = std::fs::metadata(path).map_err(cannot_read)?;
    // Devices and FIFOs report a length of 0 and never end (or never open).
    if !meta.is_file() {
        return Err("Not a regular file.".into());
    }
    if meta.len() > MAX_VIEW_BYTES {
        return Err(format!("Too large to show ({} KB).", meta.len() / 1024).into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(MAX_VIEW_BYTES + 1).read_to_end(&mut bytes))
        .map_err(cannot_read)?;
    if bytes.len() as u64 > MAX_VIEW_BYTES {
        return Err("Too large to show (it grew while reading).".into());
    }
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return Err("Binary file.".into());
    }
    let text = String::from_utf8_lossy(&bytes);
    Ok(text
        .lines()
        .map(|l| SharedString::from(DisplayLine::new(l).text))
        .collect())
}

impl Render for FilesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.tokens();
        if self.wide {
            return self.render_wide(cx);
        }
        if self.source.is_none() {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(t.panel)
                .child(
                    EmptyState::new(Lucide::Folder, "No workspace")
                        .hint("Focus a pane that shows a workspace to browse its files."),
                )
                .into_any_element();
        }
        v_flex()
            .size_full()
            .bg(t.panel)
            .child(
                h_flex()
                    .flex_none()
                    .h(px(32.))
                    .px_3()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(t.line)
                    .font_mono()
                    .text_size(px(11.))
                    .text_color(t.tx3)
                    .child(self.summary(cx))
                    .child(div().flex_1())
                    .children(self.tree_buttons(cx)),
            )
            .when_some(self.error.clone(), |this, err| {
                this.child(
                    div()
                        .px_3()
                        .py_1p5()
                        .text_size(px(11.))
                        .text_color(t.red)
                        .child(err),
                )
            })
            .child(
                div()
                    .h(px(300.))
                    .flex_none()
                    .p_1p5()
                    .border_b_1()
                    .border_color(t.line)
                    .child(self.render_tree(cx)),
            )
            .child(self.render_viewer(cx))
            .into_any_element()
    }
}

impl FilesPanel {
    fn summary(&self, cx: &App) -> String {
        let changed = self
            .source
            .as_ref()
            .map_or(0, |s| s.diff.read(cx).files().len());
        format!("{} files · {changed} changed", self.files.len())
    }

    fn tree_buttons(&self, cx: &mut Context<Self>) -> [UiButton; 2] {
        [
            UiButton::new("files-collapse")
                .icon(Lucide::ListCollapse)
                .ghost()
                .small()
                .tooltip("Collapse all")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.expanded.clear();
                    this.rebuild();
                    cx.notify();
                })),
            UiButton::new("files-refresh")
                .icon(Lucide::RefreshCw)
                .ghost()
                .small()
                .tooltip("Refresh")
                .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
        ]
    }

    fn render_tree(&self, cx: &mut Context<Self>) -> impl IntoElement {
        uniform_list(
            "files-tree",
            self.rows.len(),
            cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                range
                    .filter_map(|ix| this.render_tree_row(ix, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .size_full()
        .track_scroll(&self.tree_scroll)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{MAX_VIEW_BYTES, added_lines, read_text};
    use crate::changes::model::MAX_LINE_BYTES;
    use hangar_core::changes::{DiffLine, FileDiff, FileStatus, Hunk, LineKind};

    #[test]
    fn read_text_refuses_devices_and_fifos() {
        let dir = tempfile::tempdir().unwrap();
        let zero = dir.path().join("zero");
        std::os::unix::fs::symlink("/dev/zero", &zero).unwrap();
        assert!(read_text(&zero).is_err());

        let fifo = dir.path().join("fifo");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(read_text(&fifo).is_err());
    }

    #[test]
    fn read_text_cuts_long_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("min.js");
        std::fs::write(
            &path,
            format!("{}\nshort\tline", "x".repeat(MAX_VIEW_BYTES as usize - 20)),
        )
        .unwrap();
        let lines = read_text(&path).unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].len() <= MAX_LINE_BYTES + '…'.len_utf8());
        assert_eq!(lines[1].as_ref(), "short    line");
    }

    #[test]
    fn added_lines_are_new_line_numbers() {
        let line = |kind, old, new| DiffLine {
            kind,
            old_lineno: old,
            new_lineno: new,
            text: String::new(),
        };
        let file = FileDiff {
            path: "a.rs".into(),
            old_path: None,
            status: FileStatus::Modified,
            old_mode: None,
            new_mode: None,
            binary: false,
            truncated: false,
            insertions: 2,
            deletions: 1,
            hunks: vec![Hunk {
                old_start: 1,
                old_lines: 2,
                new_start: 1,
                new_lines: 3,
                header: String::new(),
                lines: vec![
                    line(LineKind::Context, Some(1), Some(1)),
                    line(LineKind::Removed, Some(2), None),
                    line(LineKind::Added, None, Some(2)),
                    line(LineKind::Added, None, Some(3)),
                ],
            }],
        };
        assert_eq!(added_lines(&file), HashSet::from([2, 3]));
    }
}
