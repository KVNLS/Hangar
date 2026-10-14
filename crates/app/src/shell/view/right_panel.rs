//! The right panel: device, changes and files tabs.

use super::*;

impl HangarApp {
    /// Device, changes and files as tabs of one right panel of one width.
    pub(super) fn render_right_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let device_tab = self.layout.device.open;
        let changes_tab = !device_tab && self.layout.changes.open;
        let files_tab = !device_tab && !changes_tab && self.layout.files_open;
        if !device_tab && !changes_tab && !files_tab {
            return None;
        }
        let t = cx.tokens();
        let width = self.layout.panel_width;
        let files = self
            .layout
            .panes
            .slot(self.layout.panes.focused())
            .and_then(|id| self.diff_counts(id, cx))
            .map(|c| c.files)
            .filter(|n| *n > 0);
        let tabs = h_flex()
            .flex_none()
            .h(PANEL_TAB_HEIGHT)
            .px_1p5()
            .gap(px(2.))
            .items_center()
            .border_b_1()
            .border_color(t.line)
            .child(
                UiTab::new("tab-device", TabStyle::Panel, "Device")
                    .icon(Lucide::Smartphone)
                    .kbd("⌘1")
                    .active(device_tab)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.set_device_open(true, window, cx)),
                    ),
            )
            .child(
                UiTab::new("tab-changes", TabStyle::Panel, "Changes")
                    .icon(Lucide::GitCommitHorizontal)
                    .when_some(files, |this, n| {
                        this.trailing(CountBadge::new(n.to_string()))
                    })
                    .kbd("⌘2")
                    .active(changes_tab)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.set_changes_open(true, window, cx)),
                    ),
            )
            .child(
                UiTab::new("tab-files", TabStyle::Panel, "Files")
                    .icon(Lucide::Folder)
                    .kbd("⌘3")
                    .active(files_tab)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.set_files_open(true, window, cx)),
                    ),
            )
            .child(div().flex_1())
            .child(
                UiButton::new("right-panel-close")
                    .icon(Lucide::PanelRightClose)
                    .ghost()
                    .small()
                    .tooltip("Hide panel ⇧⌘D")
                    .on_click(cx.listener(|this, _, window, cx| this.hide_panel(window, cx))),
            );
        let handle = div()
            .id("right-panel-resize")
            .absolute()
            .left(px(-3.))
            .top_0()
            .h_full()
            .w(px(6.))
            .cursor_col_resize()
            .on_drag(PanelResize, |_, _, _, cx| cx.new(|_| PanelResize));
        Some(
            v_flex()
                .relative()
                .h_full()
                .w(px(width))
                .flex_none()
                .bg(t.panel)
                .border_l_1()
                .border_color(t.line)
                .child(tabs)
                .child(div().flex_1().min_h_0().map(|this| {
                    if device_tab {
                        this.child(self.device_panel.clone())
                    } else if changes_tab {
                        this.child(self.changes_panel.clone())
                    } else {
                        this.child(self.files_panel.clone())
                    }
                }))
                .child(handle)
                .into_any_element(),
        )
    }
}
