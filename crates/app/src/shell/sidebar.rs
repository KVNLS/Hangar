//! One sidebar section per project: its workspaces and the inline "new workspace" input.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
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
    pub active: bool,
}

#[derive(Clone)]
pub(super) struct ProjectSection {
    pub app: WeakEntity<HangarApp>,
    pub project_id: ProjectId,
    pub name: SharedString,
    pub workspaces: Vec<WorkspaceRow>,
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
                let _ = app.update(cx, |this, cx| {
                    this.open_new_workspace(project_id, window, cx);
                });
            });

        let header = h_flex()
            .h_8()
            .px_2()
            .gap_1()
            .justify_between()
            .text_xs()
            .text_color(cx.theme().sidebar_foreground.opacity(0.7))
            .child(div().flex_1().min_w_0().truncate().child(self.name.clone()))
            .child(new_button);

        let input_row = self.input.clone().map(|input| {
            let app = self.app.clone();
            h_flex()
                .px_1()
                .gap_1()
                .child(div().flex_1().child(Input::new(&input).small()))
                .child(
                    Button::new(SharedString::from(format!("cancel-ws-{project_id}")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("Cancel")
                        .on_click(move |_, _, cx| {
                            let _ = app.update(cx, |this, cx| this.close_new_workspace(cx));
                        }),
                )
        });

        let menu = SidebarMenu::new().children(self.workspaces.into_iter().map(|row| {
            let app = self.app.clone();
            let workspace_id = row.id;
            SidebarMenuItem::new(row.name)
                .icon(IconName::SquareTerminal)
                .active(row.active)
                .on_click(move |_, window, cx| {
                    let _ = app.update(cx, |this, cx| {
                        this.select_workspace(workspace_id, window, cx);
                    });
                })
                .when_some(row.status, |item, status| {
                    item.suffix(move |_, cx| {
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(status.clone())
                    })
                })
        }));

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
