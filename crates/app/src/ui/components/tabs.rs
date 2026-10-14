//! Tabs. Pane tabs (32) mark the active one with an accent line on top; panel tabs (36) with
//! one underneath.

use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::ui::{ActiveTokens as _, PANE_TAB_HEIGHT, PANEL_TAB_HEIGHT, TypeScale as _};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabStyle {
    Pane,
    Panel,
}

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct UiTab {
    id: ElementId,
    style: TabStyle,
    label: SharedString,
    leading: Option<AnyElement>,
    trailing: Option<AnyElement>,
    kbd: Option<SharedString>,
    active: bool,
    on_click: Option<ClickHandler>,
}

impl UiTab {
    pub fn new(id: impl Into<ElementId>, style: TabStyle, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            style,
            label: label.into(),
            leading: None,
            trailing: None,
            kbd: None,
            active: false,
            on_click: None,
        }
    }

    pub fn icon(self, icon: impl Into<Icon>) -> Self {
        self.leading(icon.into().with_size(px(13.)))
    }

    /// Any leading element (state glyph, icon).
    pub fn leading(mut self, element: impl IntoElement) -> Self {
        self.leading = Some(element.into_any_element());
        self
    }

    /// Trailing element (count badge, close button).
    pub fn trailing(mut self, element: impl IntoElement) -> Self {
        self.trailing = Some(element.into_any_element());
        self
    }

    pub fn kbd(mut self, kbd: impl Into<SharedString>) -> Self {
        self.kbd = Some(kbd.into());
        self
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for UiTab {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.tokens();
        let active = self.active;
        let pane = self.style == TabStyle::Pane;
        div()
            .id(self.id)
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap_1p5()
            .h(if pane {
                PANE_TAB_HEIGHT
            } else {
                PANEL_TAB_HEIGHT
            })
            .px(px(10.))
            .max_w(px(340.))
            .text_size(px(12.))
            .when(pane, |this| this.border_r_1().border_color(t.line))
            .when(!pane, |this| this.font_weight(FontWeight::MEDIUM))
            .text_color(if active { t.tx } else { t.tx2 })
            .when(active && pane, |this| this.bg(t.app))
            .when(active, |this| {
                let line = div().absolute().left_0().right_0().h(px(1.)).bg(t.acc);
                this.child(if pane { line.top_0() } else { line.bottom_0() })
            })
            .when_some(self.leading, |this, el| this.child(el))
            .child(div().truncate().child(self.label))
            .when_some(self.trailing, |this, el| this.child(el))
            .when_some(self.kbd, |this, kbd| {
                this.child(
                    div()
                        .font_mono()
                        .text_size(px(10.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.tx3)
                        .child(kbd),
                )
            })
            .when_some(self.on_click.filter(|_| !active), |this, handler| {
                this.cursor_pointer()
                    .hover(move |s| s.text_color(t.tx))
                    .on_click(handler)
            })
    }
}
