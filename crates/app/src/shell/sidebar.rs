//! One sidebar section per project: its workspaces, an "Archived" group and the inline
//! "new workspace" input.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Escape, Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::sidebar::{SidebarItem, SidebarMenu, SidebarMenuItem};
use gpui_kit::component::{ActiveTheme as _, Collapsible, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_core::{ProjectId, WorkspaceId};

use super::HangarApp;

#[derive(Clone)]
pub(super) struct WorkspaceRow {
    pub id: WorkspaceId,
    pub name: SharedString,
    pub status: Option<SharedString>,
    /// Change badge (`3 +12 −4`) while the workspace's live diff runs.
    pub changes: Option<SharedString>,
    pub active: bool,
    /// An archive/delete/restore is in flight: no further actions.
    pub busy: bool,
}

#[derive(Clone)]
pub(super) struct ProjectSection {
    pub app: WeakEntity<HangarApp>,
    pub project_id: ProjectId,
    pub name: SharedString,
    pub workspaces: Vec<WorkspaceRow>,
    pub archived: Vec<WorkspaceRow>,
    /// Present while the user is naming a new workspace in this project.
    pub input: Option<Entity<InputState>>,
    pub creating: bool,
    pub collapsed: bool,
}

impl Collapsible for ProjectSection {
    fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }

    fn is_collapsed(&self) -> bool {
        self.collapsed
    }
}

/// Runs `f` on the app from a menu or button callback.
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

fn workspace_item(
    app: &WeakEntity<HangarApp>,
    row: WorkspaceRow,
    archived: bool,
) -> SidebarMenuItem {
    let workspace_id = row.id;
    let select = app.clone();
    let menu_app = app.clone();
    SidebarMenuItem::new(row.name)
        .icon(if archived {
            IconName::Inbox
        } else {
            IconName::SquareTerminal
        })
        .active(row.active)
        .disable(row.busy)
        .when(!archived, |item| {
            item.on_click(move |_, window, cx| {
                with_app(&select, window, cx, |this, window, cx| {
                    this.select_workspace(workspace_id, window, cx)
                })
            })
        })
        .when(!row.busy, |item| {
            item.context_menu(move |menu, _, _| {
                workspace_menu(menu, &menu_app, workspace_id, archived)
            })
        })
        .when(row.status.is_some() || row.changes.is_some(), |item| {
            let (status, changes) = (row.status, row.changes);
            item.suffix(move |_, cx| {
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .children(changes.clone())
                    .children(status.clone())
            })
        })
}

impl SidebarItem for ProjectSection {
    fn render(
        self,
        id: impl Into<ElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let id: ElementId = id.into();
        let project_id = self.project_id;

        let app = self.app.clone();
        let new_button = Button::new(SharedString::from(format!("new-ws-{project_id}")))
            .ghost()
            .xsmall()
            .icon(IconName::Plus)
            .tooltip("New workspace")
            .loading(self.creating)
            .on_click(move |_, window, cx| {
                with_app(&app, window, cx, |this, window, cx| {
                    this.open_new_workspace(project_id, window, cx)
                })
            });

        let app = self.app.clone();
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

        let header = h_flex()
            .h_8()
            .px_2()
            .gap_1()
            .justify_between()
            .text_xs()
            .text_color(cx.theme().sidebar_foreground.opacity(0.7))
            .child(div().flex_1().min_w_0().truncate().child(self.name.clone()))
            .child(new_button)
            .child(project_menu);

        let input_row = self.input.clone().map(|input| {
            let cancel = self.app.clone();
            let escape = self.app.clone();
            h_flex()
                .px_1()
                .gap_1()
                // The input lets Escape propagate when it has nothing of its own to dismiss.
                .on_action(move |_: &Escape, window, cx| {
                    with_app(&escape, window, cx, |this, _, cx| {
                        this.close_new_workspace(cx)
                    })
                })
                .child(div().flex_1().child(Input::new(&input).small()))
                .child(
                    Button::new(SharedString::from(format!("cancel-ws-{project_id}")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("Cancel")
                        .on_click(move |_, window, cx| {
                            with_app(&cancel, window, cx, |this, _, cx| {
                                this.close_new_workspace(cx)
                            })
                        }),
                )
        });

        let mut menu = SidebarMenu::new().children(
            self.workspaces
                .into_iter()
                .map(|row| workspace_item(&self.app, row, false)),
        );
        if !self.archived.is_empty() {
            let label = format!("Archived ({})", self.archived.len());
            let rows = self
                .archived
                .into_iter()
                .map(|row| workspace_item(&self.app, row, true));
            menu = menu.child(
                SidebarMenuItem::new(label)
                    .icon(IconName::Inbox)
                    .click_to_toggle(true)
                    .children(rows),
            );
        }

        v_flex()
            .gap_1()
            .child(header)
            .children(input_row)
            .child(SidebarItem::render(
                menu,
                SharedString::from(format!("{id}-menu")),
                window,
                cx,
            ))
    }
}
