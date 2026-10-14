//! The Files tab: the focused workspace's worktree as a tree, changed files marked, and a
//! read-only viewer.

mod tree;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_core::WorkspaceId;

use crate::changes::live::WorkspaceDiff;
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
    tree_scroll: UniformListScrollHandle,
    view_scroll: UniformListScrollHandle,
}

impl FilesPanel {
    pub fn new(_: &mut Context<Self>) -> Self {
        Self {
            source: None,
            files: Arc::default(),
            expanded: HashSet::new(),
            rows: Arc::default(),
            error: None,
            viewer: Viewer::None,
            list_task: None,
            read_task: None,
            tree_scroll: UniformListScrollHandle::new(),
            view_scroll: UniformListScrollHandle::new(),
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
                .spawn(async move { hangar_worktree::list_files(&root) })
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
                    },
                    Err(text) => Viewer::Message { path, text },
                };
                this.view_scroll.scroll_to_item(0, ScrollStrategy::Top);
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn render_tree_row(&self, ix: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = cx.tokens();
        let row = self.rows.get(ix)?.clone();
        let changed = !row.dir
            && self
                .source
                .as_ref()
                .is_some_and(|s| s.diff.read(cx).files().contains_key(&row.path));
        let selected = match &self.viewer {
            Viewer::Loading(p) | Viewer::Text { path: p, .. } | Viewer::Message { path: p, .. } => {
                *p == row.path
            }
            Viewer::None => false,
        };
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
            Viewer::Text { path, lines } => {
                let count = lines.len();
                let gutter = px(12. + count.to_string().len() as f32 * 7.5);
                (
                    Some(path),
                    uniform_list(
                        "file-view",
                        count,
                        cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                            let t = cx.tokens();
                            let Viewer::Text { lines, .. } = &this.viewer else {
                                return Vec::new();
                            };
                            range
                                .filter_map(|ix| {
                                    let line = lines.get(ix)?.clone();
                                    Some(
                                        h_flex()
                                            .h(px(VIEW_ROW_HEIGHT))
                                            .whitespace_nowrap()
                                            .child(
                                                div()
                                                    .w(gutter + px(12.))
                                                    .flex_none()
                                                    .pr_3()
                                                    .text_right()
                                                    .text_color(t.tx3)
                                                    .child((ix + 1).to_string()),
                                            )
                                            .child(line),
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
                    .child(div().text_color(t.tx3).child("read-only")),
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

/// The file's lines, or a short message when it is too big, binary or unreadable.
fn read_text(path: &std::path::Path) -> Result<Vec<SharedString>, SharedString> {
    let meta =
        std::fs::metadata(path).map_err(|e| SharedString::from(format!("Cannot read: {e}")))?;
    if meta.len() > MAX_VIEW_BYTES {
        return Err(format!("Too large to show ({} KB).", meta.len() / 1024).into());
    }
    let bytes = std::fs::read(path).map_err(|e| SharedString::from(format!("Cannot read: {e}")))?;
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return Err("Binary file.".into());
    }
    let text = String::from_utf8_lossy(&bytes);
    Ok(text
        .lines()
        .map(|l| SharedString::from(l.replace('\t', "    ")))
        .collect())
}

impl Render for FilesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.tokens();
        let Some(source) = &self.source else {
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
        };
        let changed = source.diff.read(cx).files().len();
        let count = self.rows.len();
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
                    .child(format!("{} files · {changed} changed", self.files.len()))
                    .child(div().flex_1())
                    .child(
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
                    )
                    .child(
                        UiButton::new("files-refresh")
                            .icon(Lucide::RefreshCw)
                            .ghost()
                            .small()
                            .tooltip("Refresh")
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    ),
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
                    .child(
                        uniform_list(
                            "files-tree",
                            count,
                            cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                                range
                                    .filter_map(|ix| this.render_tree_row(ix, cx))
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .size_full()
                        .track_scroll(&self.tree_scroll),
                    ),
            )
            .child(self.render_viewer(cx))
            .into_any_element()
    }
}
