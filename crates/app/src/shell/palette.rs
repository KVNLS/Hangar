//! Command palette (⌘K): workspaces (those needing you first) and the app's commands.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::IndexPath;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};

use super::*;
use crate::ui::components::StateGlyph;
use crate::ui::{ActiveTokens as _, RADIUS_CARD, TypeScale as _, overlay_shadow};

pub(super) struct Palette {
    state: Entity<CommandState>,
    /// Workspaces in the order of the first group, to resolve a confirmed row.
    workspaces: Vec<WorkspaceId>,
}

impl HangarApp {
    pub(super) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.layout.palette.is_some() {
            return self.close_palette(window, cx);
        }
        let state = cx.new(|cx| CommandState::new(window, cx));
        state.update(cx, |state, cx| state.focus(window, cx));
        let mut workspaces: Vec<(AgentState, WorkspaceId)> = self
            .work
            .projects
            .entries()
            .iter()
            .flat_map(|e| e.active())
            .map(|ws| (self.agent_state(ws.id, cx), ws.id))
            .collect();
        // Stable: within a state, sidebar order.
        workspaces.sort_by_key(|(state, _)| *state);
        self.layout.palette = Some(Palette {
            state,
            workspaces: workspaces.into_iter().map(|(_, id)| id).collect(),
        });
        cx.notify();
    }

    pub(super) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.layout.palette.take().is_some() {
            window.focus(&self.focus, cx);
            self.focus_terminal(window, cx);
            cx.notify();
        }
    }

    pub(super) fn render_palette(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let palette = self.layout.palette.as_ref()?;
        let t = cx.tokens();
        let rows = palette.workspaces.iter().filter_map(|id| {
            let ws = self.work.projects.workspace(*id)?;
            let project = self.work.projects.project(ws.project_id)?;
            let state = self.agent_state(*id, cx);
            let (name, project): (SharedString, SharedString) =
                (ws.name.clone().into(), project.name.clone().into());
            Some(
                CommandItem::new()
                    .label(name.clone())
                    .keywords([project.clone(), ws.branch.clone().into()])
                    .child(move |_, cx| {
                        let t = cx.tokens();
                        h_flex()
                            .w_full()
                            .gap(px(10.))
                            .items_center()
                            .child(
                                div()
                                    .flex()
                                    .w(px(16.))
                                    .justify_center()
                                    .child(StateGlyph::new(state)),
                            )
                            .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                            .child(
                                div()
                                    .font_mono()
                                    .text_size(px(11.))
                                    .text_color(t.tx3)
                                    .child(project.clone()),
                            )
                    }),
            )
        });
        let command = |label: &'static str, icon: Lucide, action: Box<dyn Action>| {
            CommandItem::new().label(label).icon(icon).action(action)
        };
        let commands = CommandGroup::new().label("Commands").items([
            command("New workspace…", Lucide::Plus, Box::new(NewWorkspace)),
            command("Review changes", Lucide::FileDiff, Box::new(OpenReview)),
            command("Build and run", Lucide::Hammer, Box::new(Run)),
            command("Stop the run", Lucide::Square, Box::new(StopRun)),
            command(
                "Restart dev server",
                Lucide::RotateCw,
                Box::new(RestartServer),
            ),
            command("Overview", Lucide::LayoutGrid, Box::new(ToggleOverview)),
            command("Settings", Lucide::Settings, Box::new(OpenSettings)),
            command("Show device", Lucide::Smartphone, Box::new(ShowDeviceTab)),
            command(
                "Show changes",
                Lucide::GitCommitHorizontal,
                Box::new(ShowChangesTab),
            ),
            command("Show files", Lucide::Folder, Box::new(ShowFilesTab)),
            command(
                "Next workspace needing you",
                Lucide::ArrowRight,
                Box::new(NextNeedingYou),
            ),
            command("Split pane right", Lucide::Columns2, Box::new(SplitPane)),
            command("Close pane", Lucide::X, Box::new(ClosePane)),
            command("Toggle sidebar", Lucide::PanelLeft, Box::new(ToggleSidebar)),
            command(
                "Filter workspaces",
                Lucide::Search,
                Box::new(FilterWorkspaces),
            ),
            command("Add project…", Lucide::FolderOpen, Box::new(AddProject)),
            command(
                "Toggle light / dark theme",
                Lucide::SunMoon,
                Box::new(ToggleTheme),
            ),
        ]);
        let app = cx.entity().downgrade();
        let workspaces = palette.workspaces.clone();
        let cancel = app.clone();
        let command = Command::new(&palette.state)
            .bordered(false)
            .max_h(px(400.))
            .placeholder("Search workspaces and commands…")
            .group(CommandGroup::new().label("Workspaces").items(rows))
            .group(commands)
            .on_confirm(move |ix: IndexPath, window, cx| {
                let selected = (ix.section == 0)
                    .then(|| workspaces.get(ix.row).copied())
                    .flatten();
                let _ = app.update(cx, |this, cx| {
                    this.close_palette(window, cx);
                    if let Some(id) = selected {
                        this.select_workspace(id, window, cx);
                    }
                });
            })
            .on_cancel(move |window, cx| {
                let _ = cancel.update(cx, |this, cx| this.close_palette(window, cx));
            })
            .footer(move |_, _, cx| {
                let t = cx.tokens();
                h_flex()
                    .h(px(30.))
                    .px(px(14.))
                    .gap(px(14.))
                    .items_center()
                    .border_t_1()
                    .border_color(t.line)
                    .font_mono()
                    .text_size(px(11.))
                    .text_color(t.tx3)
                    .child("↑↓ navigate")
                    .child("⏎ run")
                    .child("esc close")
            });
        Some(
            div()
                .id("palette-scrim")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(88.))
                .bg(t.scrim)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_palette(window, cx)),
                )
                .child(
                    div()
                        .id("palette")
                        .w(px(620.))
                        .bg(t.elev)
                        .border_1()
                        .border_color(t.line2)
                        .rounded(RADIUS_CARD)
                        .overflow_hidden()
                        .shadow(overlay_shadow())
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(command),
                )
                .into_any_element(),
        )
    }
}
