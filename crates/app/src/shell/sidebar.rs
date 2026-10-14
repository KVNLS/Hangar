//! Sidebar: workspace filter, one section per project (header, workspace rows, archived group,
//! inline "new workspace" input), and the add-project footer.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::select::Select;
use gpui_kit::component::{Icon, Sizable as _};

use super::status::DiffCounts;
use super::*;
use crate::ui::components::{AgentMark, Delta, StateGlyph, UiButton};
use crate::ui::{
    ActiveTokens as _, FONT_MONO, RADIUS_CONTROL, ROW_HEIGHT, ROW_HEIGHT_DENSE, SIDEBAR_WIDTH,
};

#[derive(Clone)]
pub(super) struct WorkspaceRow {
    pub id: WorkspaceId,
    pub name: SharedString,
    pub branch: SharedString,
    pub state: AgentState,
    /// Monogram of the running (or last) agent.
    pub mark: Option<&'static str>,
    pub counts: Option<DiffCounts>,
    pub active: bool,
    /// An archive/delete/restore is in flight: no further actions.
    pub busy: bool,
}

impl WorkspaceRow {
    fn matches(&self, query: &str) -> bool {
        query.is_empty()
            || self.name.to_lowercase().contains(query)
            || self.branch.to_lowercase().contains(query)
    }
}

/// Runs `f` on the app from a menu callback.
fn with_app(
    app: &WeakEntity<HangarApp>,
    window: &mut Window,
    cx: &mut App,
    f: impl FnOnce(&mut HangarApp, &mut Window, &mut Context<HangarApp>),
) {
    let _ = app.update(cx, |this, cx| f(this, window, cx));
}

fn workspace_menu(
    menu: PopupMenu,
    app: &WeakEntity<HangarApp>,
    id: WorkspaceId,
    archived: bool,
) -> PopupMenu {
    let primary = if archived {
        let app = app.clone();
        PopupMenuItem::new("Restore")
            .icon(IconName::Undo2)
            .on_click(move |_, window, cx| {
                with_app(&app, window, cx, |this, window, cx| {
                    this.restore_workspace(id, window, cx).detach()
                })
            })
    } else {
        let app = app.clone();
        PopupMenuItem::new("Archive")
            .icon(IconName::Inbox)
            .on_click(move |_, window, cx| {
                with_app(&app, window, cx, |this, window, cx| {
                    this.archive_workspace(id, window, cx).detach()
                })
            })
    };
    let app = app.clone();
    menu.item(primary).separator().item(
        PopupMenuItem::new("Delete…")
            .icon(IconName::Delete)
            .on_click(move |_, window, cx| {
                with_app(&app, window, cx, |this, window, cx| {
                    this.confirm_delete_workspace(id, window, cx)
                })
            }),
    )
}

impl HangarApp {
    fn workspace_row(
        &self,
        ws: &Workspace,
        focused: Option<WorkspaceId>,
        cx: &App,
    ) -> WorkspaceRow {
        let live = self.sessions.get(&ws.id);
        WorkspaceRow {
            id: ws.id,
            name: ws.name.clone().into(),
            branch: ws.branch.clone().into(),
            state: self.agent_state(ws.id, cx),
            mark: live.map(|l| l.session.agent.monogram()),
            counts: self.diff_counts(ws.id, cx),
            active: focused == Some(ws.id),
            busy: self.busy.contains(&ws.id),
        }
    }

    fn render_row(&self, row: WorkspaceRow, archived: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.tokens();
        let id = row.id;
        let app = cx.entity().downgrade();
        let element_id = SharedString::from(format!("ws-{id}"));
        let leading = if archived {
            Icon::new(IconName::Inbox)
                .size(px(12.))
                .text_color(t.tx3)
                .into_any_element()
        } else {
            StateGlyph::new(row.state).into_any_element()
        };
        let trailing = if row.busy {
            div()
                .font_family(FONT_MONO)
                .text_size(px(11.))
                .text_color(t.tx3)
                .child("…")
                .into_any_element()
        } else {
            let counts = row.counts.unwrap_or_default();
            div()
                .flex()
                .flex_none()
                .w(px(58.))
                .justify_end()
                .when(row.counts.is_some(), |this| {
                    this.child(Delta::new(counts.insertions, counts.deletions))
                })
                .into_any_element()
        };
        let busy = row.busy;
        let element = div()
            .id(element_id)
            .flex()
            .items_center()
            .gap_2()
            .h(if archived {
                ROW_HEIGHT_DENSE
            } else {
                ROW_HEIGHT
            })
            .pl(px(10.))
            .pr_2()
            .rounded(RADIUS_CONTROL)
            .text_size(px(13.))
            .text_color(if archived { t.tx2 } else { t.tx })
            .when(row.active, |this| this.bg(t.active))
            .when(!row.active, |this| this.hover(|s| s.bg(t.hover)))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .size(px(14.))
                    .items_center()
                    .justify_center()
                    .child(leading),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(row.active, |this| this.font_weight(FontWeight::SEMIBOLD))
                    .child(row.name),
            )
            .children(row.mark.map(AgentMark::new))
            .child(trailing)
            .when(!archived && !busy, |this| {
                this.cursor_pointer().on_click(
                    cx.listener(move |this, _, window, cx| this.select_workspace(id, window, cx)),
                )
            });
        if busy {
            element.into_any_element()
        } else {
            element
                .context_menu(move |menu, _, _| workspace_menu(menu, &app, id, archived))
                .into_any_element()
        }
    }

    fn render_project(
        &self,
        entry: &ProjectEntry,
        query: &str,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let t = cx.tokens();
        let project_id = entry.project.id;
        let focused = self.panes.slot(self.panes.focused());
        let rows: Vec<WorkspaceRow> = entry
            .active()
            .map(|ws| self.workspace_row(ws, focused, cx))
            .filter(|row| row.matches(query))
            .collect();
        let archived: Vec<WorkspaceRow> = entry
            .archived()
            .map(|ws| self.workspace_row(ws, focused, cx))
            .filter(|row| row.matches(query))
            .collect();

        if !query.is_empty() && rows.is_empty() && archived.is_empty() {
            return None;
        }
        let collapsed = self.collapsed_projects.contains(&project_id) && query.is_empty();
        let show_archived = self.show_archived.contains(&project_id) || !query.is_empty();

        let app = cx.entity().downgrade();
        let project_menu = Button::new(SharedString::from(format!("project-menu-{project_id}")))
            .ghost()
            .xsmall()
            .icon(IconName::Ellipsis)
            .tooltip("Project actions")
            .dropdown_menu(move |menu, _, _| {
                let app = app.clone();
                menu.item(
                    PopupMenuItem::new("Remove from Hangar…")
                        .icon(IconName::CircleX)
                        .on_click(move |_, window, cx| {
                            with_app(&app, window, cx, |this, window, cx| {
                                this.confirm_remove_project(project_id, window, cx)
                            })
                        }),
                )
            });
        let header = div()
            .id(SharedString::from(format!("project-{project_id}")))
            .flex()
            .items_center()
            .gap_1p5()
            .h(ROW_HEIGHT_DENSE)
            .mt_1p5()
            .pl_1p5()
            .pr_1()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.collapsed_projects.remove(&project_id) {
                    this.collapsed_projects.insert(project_id);
                }
                cx.notify();
            }))
            .child(
                Icon::new(if collapsed {
                    Lucide::ChevronRight
                } else {
                    Lucide::ChevronDown
                })
                .size(px(12.))
                .text_color(t.tx3),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.tx)
                    .child(entry.project.name.clone()),
            )
            .child(
                UiButton::new(SharedString::from(format!("new-ws-{project_id}")))
                    .icon(Lucide::Plus)
                    .ghost()
                    .small()
                    .disabled(self.creating.contains(&project_id))
                    .tooltip("New workspace")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_new_workspace(project_id, window, cx)
                    })),
            )
            .child(project_menu);

        let archived_count = archived.len();
        let archived_group = (archived_count > 0).then(|| {
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .id(SharedString::from(format!("archived-{project_id}")))
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .h(px(24.))
                        .pl(px(10.))
                        .text_size(px(11.))
                        .text_color(t.tx3)
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.show_archived.remove(&project_id) {
                                this.show_archived.insert(project_id);
                            }
                            cx.notify();
                        }))
                        .child(
                            Icon::new(if show_archived {
                                Lucide::ChevronDown
                            } else {
                                Lucide::ChevronRight
                            })
                            .size(px(11.)),
                        )
                        .child(format!("Archived ({archived_count})")),
                )
                .when(show_archived, |this| {
                    this.children(
                        archived
                            .into_iter()
                            .map(|row| self.render_row(row, true, cx)),
                    )
                })
        });

        Some(
            div()
                .flex()
                .flex_col()
                .child(header)
                .when(!collapsed, |this| {
                    this.children(rows.into_iter().map(|row| self.render_row(row, false, cx)))
                        .children(archived_group)
                }),
        )
    }

    pub(super) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.tokens();
        let query = self.sidebar_filter.read(cx).value().trim().to_lowercase();
        let sections: Vec<Div> = self
            .projects
            .entries()
            .iter()
            .filter_map(|entry| self.render_project(entry, &query, cx))
            .collect();
        let notice = if self.loading {
            Some(SharedString::from("Loading…"))
        } else if let Some(err) = &self.load_error {
            Some(err.clone())
        } else if self.projects.is_empty() {
            Some("No projects yet. Add a Git repository to start.".into())
        } else if sections.is_empty() {
            Some("No workspace matches the filter.".into())
        } else {
            None
        };
        let notice_color = if self.load_error.is_some() {
            t.red
        } else {
            t.tx3
        };

        v_flex()
            .w(SIDEBAR_WIDTH)
            .flex_none()
            .h_full()
            .bg(t.panel)
            .border_r_1()
            .border_color(t.line)
            .child(
                div().p_2().pb_1p5().child(
                    Input::new(&self.sidebar_filter)
                        .small()
                        .prefix(Icon::new(IconName::Search).size(px(12.)).text_color(t.tx3))
                        .suffix(
                            div()
                                .font_family(FONT_MONO)
                                .text_size(px(10.))
                                .text_color(t.tx3)
                                .child("⌘P"),
                        ),
                ),
            )
            .child(
                div()
                    .id("sidebar-sections")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_1p5()
                    .pb_2()
                    .children(sections)
                    .when_some(notice, |this, notice| {
                        this.child(
                            div()
                                .px_2()
                                .py_3()
                                .text_size(px(12.))
                                .text_color(notice_color)
                                .child(notice),
                        )
                    }),
            )
            .child(
                h_flex()
                    .px(px(10.))
                    .py_1p5()
                    .gap_2()
                    .border_t_1()
                    .border_color(t.line)
                    .text_size(px(12.))
                    .text_color(t.tx2)
                    .child("Agent")
                    .child(
                        div()
                            .flex_1()
                            .child(Select::new(&self.agent_select).small()),
                    ),
            )
            .child(
                div()
                    .id("add-project")
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1p5()
                    .h(px(36.))
                    .px(px(10.))
                    .border_t_1()
                    .border_color(t.line)
                    .text_size(px(12.))
                    .text_color(t.tx2)
                    .cursor_pointer()
                    .hover(|s| s.bg(t.hover))
                    .on_click(cx.listener(|this, _, window, cx| this.pick_project(window, cx)))
                    .child(Icon::new(IconName::FolderOpen).size(px(13.)))
                    .child(div().flex_1().child("Add project"))
                    .child(
                        div()
                            .font_family(FONT_MONO)
                            .text_size(px(10.))
                            .text_color(t.tx3)
                            .child("⌘O"),
                    ),
            )
    }
}
