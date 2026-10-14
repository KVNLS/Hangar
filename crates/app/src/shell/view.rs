//! Rendering of the main window.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Sizable as _, v_resizable};

use super::run::PaneTab;
use super::*;
use crate::ui::components::{CountBadge, EmptyState, StateGlyph, TabStyle, UiButton, UiTab, card};
use crate::ui::{
    ActiveTokens as _, PANE_TAB_HEIGHT, PANEL_TAB_HEIGHT, SPACE_2, SPACE_3, TypeScale as _,
};

impl Render for HangarApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut panes = h_resizable("panes").with_state(&self.layout.pane_groups.columns);
        for (column, range) in self.layout.panes.columns().into_iter().enumerate() {
            let content = match self.layout.pane_groups.rows.get(column) {
                Some(rows) if range.len() > 1 => {
                    let mut group = v_resizable(("pane-column", column)).with_state(rows);
                    for pane in range {
                        group = group.child(resizable_panel().child(self.render_pane(pane, cx)));
                    }
                    group.into_any_element()
                }
                _ => {
                    let mut stack = v_flex().size_full();
                    for pane in range {
                        stack = stack.child(self.render_pane(pane, cx));
                    }
                    stack.into_any_element()
                }
            };
            panes = panes.child(resizable_panel().child(content));
        }
        let main = match self.layout.screen {
            Screen::Review => div().flex_1().min_h_0().child(self.changes_panel.clone()),
            Screen::Files => div().flex_1().min_h_0().child(self.files_panel.clone()),
            Screen::Settings => div()
                .key_context("SettingsScreen")
                .track_focus(&self.layout.screen_focus)
                .flex()
                .flex_1()
                .min_h_0()
                .child(self.render_settings(cx)),
            Screen::Overview => div()
                .key_context("OverviewScreen")
                .track_focus(&self.layout.screen_focus)
                .flex()
                .flex_1()
                .min_h_0()
                .when(self.layout.sidebar_open, |this| {
                    this.child(self.render_resizable_sidebar(cx))
                })
                .child(self.render_overview(cx)),
            Screen::Workspace => {
                let right_panel = self.render_right_panel(cx);
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .when(self.layout.sidebar_open, |this| {
                        this.child(self.render_resizable_sidebar(cx))
                    })
                    .map(|this| {
                        if self.showing_welcome() {
                            this.child(self.render_welcome(cx))
                        } else {
                            this.child(div().flex_1().min_w_0().h_full().child(panes))
                        }
                    })
                    .children(right_panel)
            }
        };
        v_flex()
            .id("hangar")
            .key_context("Hangar")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::toggle_changes))
            .on_action(cx.listener(Self::toggle_device))
            .on_action(cx.listener(Self::toggle_device_stats))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.layout.sidebar_open = !this.layout.sidebar_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &FilterWorkspaces, window, cx| {
                this.layout.sidebar_open = true;
                this.layout
                    .sidebar_filter
                    .update(cx, |state, cx| state.focus(window, cx));
                cx.notify();
            }))
            .on_action(
                cx.listener(|this, _: &AddProject, window, cx| this.pick_project(window, cx)),
            )
            .on_action(cx.listener(|this, _: &Run, window, cx| this.run_focused(window, cx)))
            .on_action(cx.listener(|this, _: &StopRun, _, cx| this.stop_run(cx)))
            .on_action(cx.listener(|this, _: &SplitPane, window, cx| this.split_pane(window, cx)))
            .on_action(
                cx.listener(|this, _: &SplitPaneDown, window, cx| this.split_pane_down(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &FocusPane1, window, cx| {
                    this.focus_pane_number(0, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &FocusPane2, window, cx| {
                    this.focus_pane_number(1, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &FocusPane3, window, cx| {
                    this.focus_pane_number(2, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &FocusPane4, window, cx| {
                    this.focus_pane_number(3, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ClosePane, window, cx| {
                this.close_pane(this.layout.panes.focused(), window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &RestartServer, window, cx| this.restart_server(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ShowDeviceTab, window, cx| {
                this.set_device_open(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowChangesTab, window, cx| {
                this.set_changes_open(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NextNeedingYou, window, cx| {
                this.focus_next_needing_you(window, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowFilesTab, window, cx| {
                this.set_files_open(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenReview, window, cx| {
                this.set_screen(Screen::Review, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenFiles, window, cx| {
                this.set_screen(Screen::Files, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &OpenPalette, window, cx| this.open_palette(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleTheme, window, cx| {
                let next = if cx.theme().is_dark() {
                    crate::ui::theme::Appearance::Light
                } else {
                    crate::ui::theme::Appearance::Dark
                };
                this.set_appearance(next, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                let screen = if this.layout.screen == Screen::Settings {
                    Screen::Workspace
                } else {
                    Screen::Settings
                };
                this.set_screen(screen, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CloseScreen, window, cx| {
                this.set_screen(Screen::Workspace, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ToggleOverview, window, cx| {
                    this.toggle_overview(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &NewWorkspace, window, cx| {
                if let Some(project) = this.current_project() {
                    this.layout.sidebar_open = true;
                    this.open_new_workspace(project, window, cx);
                }
            }))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<SidebarResize>, window, cx| {
                    let width = event.event.position.x - event.bounds.left();
                    this.resize_sidebar(width.as_f32(), window, cx);
                }),
            )
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<PanelResize>, window, cx| {
                    let width = event.bounds.right() - event.event.position.x;
                    this.resize_panel(width.as_f32(), window, cx);
                }),
            )
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_family(crate::ui::FONT_UI)
            .relative()
            .child(self.render_titlebar(cx))
            .child(main)
            .child(self.render_status_bar(cx))
            .children(self.render_new_workspace_sheet(cx))
            .children(self.render_compare(cx))
            .children(self.render_answer_sheet(cx))
            .children(self.render_palette(cx))
            .children(self.render_toast(cx))
    }
}

mod pane;
mod picker;
mod right_panel;

impl HangarApp {
    /// The sidebar with a drag handle on its right edge.
    pub(super) fn render_resizable_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .flex_none()
            .h_full()
            .child(self.render_sidebar(cx))
            .child(
                div()
                    .id("sidebar-resize")
                    .absolute()
                    .right(px(-3.))
                    .top_0()
                    .h_full()
                    .w(px(6.))
                    .cursor_col_resize()
                    .on_drag(SidebarResize, |_, _, _, cx| cx.new(|_| SidebarResize)),
            )
    }
}
