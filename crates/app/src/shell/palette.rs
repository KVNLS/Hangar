//! Command palette (⌘K): workspaces needing you (all of them while searching), permission
//! requests to allow once, then actions, device and app commands.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::IndexPath;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};

use super::*;
use crate::device_panel::DeviceKey;
use crate::ui::components::StateGlyph;
use crate::ui::{ActiveTokens as _, RADIUS_CARD, TypeScale as _, overlay_shadow};

pub(super) struct Palette {
    state: Entity<CommandState>,
    /// Every active workspace and its state, in sidebar order.
    workspaces: Vec<(AgentState, WorkspaceId)>,
}

/// Section indexes of the palette's groups (the workspace group is always first).
const WORKSPACE_SECTION: usize = 0;
const APPROVE_SECTION: usize = 1;
const DEVICE_SECTION: usize = 3;

/// The first group: workspaces needing you (approval, input, failed) when the query is empty,
/// every workspace while searching; "needs you" order, sidebar order within a state.
pub(super) fn workspace_group<T: Copy>(
    items: &[(AgentState, T)],
    searching: bool,
) -> (&'static str, Vec<T>) {
    let mut rows: Vec<(AgentState, T)> = items
        .iter()
        .copied()
        .filter(|(state, _)| searching || state.needs_you())
        .collect();
    rows.sort_by_key(|(state, _)| *state);
    let label = if searching { "Workspaces" } else { "Needs you" };
    (label, rows.into_iter().map(|(_, id)| id).collect())
}

impl HangarApp {
    pub(super) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.layout.palette.is_some() {
            return self.close_palette(window, cx);
        }
        let state = cx.new(|cx| CommandState::new(window, cx));
        state.update(cx, |state, cx| state.focus(window, cx));
        let workspaces: Vec<(AgentState, WorkspaceId)> = self
            .work
            .projects
            .entries()
            .iter()
            .flat_map(|e| e.active())
            .map(|ws| (self.agent_state(ws.id, cx), ws.id))
            .collect();
        self.layout.palette = Some(Palette { state, workspaces });
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
        let searching = !palette.state.read(cx).query(cx).trim().is_empty();
        let (group_label, workspaces) = workspace_group(&palette.workspaces, searching);
        let rows = workspaces.iter().filter_map(|id| {
            let ws = self.work.projects.workspace(*id)?;
            let project = self.work.projects.project(ws.project_id)?;
            let state = self.agent_state(*id, cx);
            let (name, project): (SharedString, SharedString) =
                (ws.name.clone().into(), project.name.clone().into());
            Some(
                CommandItem::new()
                    .label(name.clone())
                    .keywords([project.clone(), ws.branch.clone().into()])
                    .keywords(ws.task.clone())
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
        let approvals = self.pending_approvals(cx);
        let approve = CommandGroup::new()
            .label("Approve")
            .items(approvals.iter().map(|(_, name, request)| {
                CommandItem::new()
                    .label(super::approvals::approve_label(name, request))
                    .icon(Lucide::Check)
            }));
        let approvals: Vec<WorkspaceId> = approvals.into_iter().map(|(id, _, _)| id).collect();
        let command = |label: &'static str, icon: Lucide, action: Box<dyn Action>| {
            CommandItem::new().label(label).icon(icon).action(action)
        };
        let actions = CommandGroup::new().label("Actions").items([
            command("New workspace…", Lucide::Plus, Box::new(NewWorkspace)),
            command(
                "Open overview",
                Lucide::LayoutGrid,
                Box::new(ToggleOverview),
            ),
            command("Review changes", Lucide::FileDiff, Box::new(OpenReview)),
            command(
                "Next needing you",
                Lucide::ArrowRight,
                Box::new(NextNeedingYou),
            ),
            command("Show device", Lucide::Smartphone, Box::new(ShowDeviceTab)),
            command(
                "Show changes",
                Lucide::GitCommitHorizontal,
                Box::new(ShowChangesTab),
            ),
            command("Show files", Lucide::Folder, Box::new(ShowFilesTab)),
            command("Split pane right", Lucide::Columns2, Box::new(SplitPane)),
            command("Split pane down", Lucide::Rows2, Box::new(SplitPaneDown)),
            command("Close pane", Lucide::X, Box::new(ClosePane)),
            command("Toggle sidebar", Lucide::PanelLeft, Box::new(ToggleSidebar)),
            command(
                "Filter workspaces",
                Lucide::Search,
                Box::new(FilterWorkspaces),
            ),
            command("Add project…", Lucide::FolderOpen, Box::new(AddProject)),
        ]);
        // Shut-down simulators and emulators the device panel can boot. While the query is empty
        // only the panel's selected device is offered, so a long simulator list stays out of the way.
        let picker = self.device_panel.read(cx).picker();
        let selected = picker.selected().cloned();
        let boots: Vec<(DeviceKey, SharedString)> = picker
            .entries()
            .iter()
            .filter(|e| e.key.platform.can_boot(&e.info))
            .filter(|e| searching || selected.as_ref() == Some(&e.key))
            .map(|e| (e.key.clone(), format!("Boot {}", e.info.name).into()))
            .collect();
        let device_commands = [
            command("Build and run", Lucide::Hammer, Box::new(Run)),
            command("Stop the run", Lucide::Square, Box::new(StopRun)),
            command(
                "Restart dev server",
                Lucide::RotateCw,
                Box::new(RestartServer),
            ),
        ];
        let first_boot = device_commands.len();
        let device = CommandGroup::new()
            .label("Device")
            .items(device_commands)
            .items(
                boots
                    .iter()
                    .map(|(_, label)| CommandItem::new().label(label.clone()).icon(Lucide::Power)),
            );
        let app_commands = CommandGroup::new().label("App").items([
            command(
                "Toggle light / dark theme",
                Lucide::SunMoon,
                Box::new(ToggleTheme),
            ),
            command("Settings", Lucide::Settings, Box::new(OpenSettings)),
        ]);
        let app = cx.entity().downgrade();
        let requery = app.clone();
        let cancel = app.clone();
        let command = Command::new(&palette.state)
            .bordered(false)
            .max_h(px(400.))
            .placeholder("Search workspaces and commands…")
            .group(CommandGroup::new().label(group_label).items(rows))
            .group(approve)
            .group(actions)
            .group(device)
            .group(app_commands)
            // The first group changes with the query (needs you / all workspaces).
            .on_query(move |_, _, cx| {
                let _ = requery.update(cx, |_, cx| cx.notify());
            })
            .on_confirm(move |ix: IndexPath, window, cx| {
                let workspace = (ix.section == WORKSPACE_SECTION)
                    .then(|| workspaces.get(ix.row).copied())
                    .flatten();
                let approve = (ix.section == APPROVE_SECTION)
                    .then(|| approvals.get(ix.row).copied())
                    .flatten();
                let boot = (ix.section == DEVICE_SECTION)
                    .then(|| ix.row.checked_sub(first_boot))
                    .flatten()
                    .and_then(|i| boots.get(i))
                    .map(|(key, _)| key.clone());
                let _ = app.update(cx, |this, cx| {
                    this.close_palette(window, cx);
                    if let Some(id) = workspace {
                        this.select_workspace(id, window, cx);
                    }
                    if let Some(id) = approve {
                        this.answer_request(id, true, cx);
                    }
                    if let Some(key) = boot {
                        this.device_panel
                            .update(cx, |panel, cx| panel.boot_device(key, window, cx));
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

#[cfg(test)]
mod tests {
    // Not `super::*`: the shell glob-imports gpui, whose `test` macro would shadow `#[test]`.
    use super::workspace_group;
    use hangar_core::AgentState::*;

    #[test]
    fn empty_query_lists_only_workspaces_needing_you() {
        let items = [
            (Idle, 1),
            (Failed, 2),
            (Review, 3),
            (Approval, 4),
            (Input, 5),
            (Working, 6),
        ];
        assert_eq!(workspace_group(&items, false), ("Needs you", vec![4, 5, 2]));
    }

    #[test]
    fn searching_lists_every_workspace_in_needs_you_order() {
        let items = [
            (Idle, 1),
            (Review, 2),
            (Approval, 3),
            (Idle, 4),
            (Working, 5),
        ];
        assert_eq!(
            workspace_group(&items, true),
            ("Workspaces", vec![3, 2, 5, 1, 4])
        );
    }
}
