//! Buttons: primary (one per surface), secondary, ghost, danger, and icon-only.

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::ui::{
    ActiveTokens as _, BUTTON_HEIGHT, BUTTON_HEIGHT_SHEET, BUTTON_HEIGHT_SMALL, FONT_MONO,
};
use crate::ui::{RADIUS_CONTROL, Tokens};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonKind {
    /// Accent fill; the one main action of a surface.
    Primary,
    /// Elevated with a strong border.
    #[default]
    Secondary,
    /// Text only; hover shows a fill.
    Ghost,
    /// Secondary look with red text, for destructive actions.
    Danger,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonSize {
    /// 24, inside cards and prompts.
    Small,
    /// 26, toolbars and panels.
    #[default]
    Medium,
    /// 28, sheets and empty states.
    Large,
}

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct UiButton {
    id: ElementId,
    label: Option<SharedString>,
    icon: Option<Icon>,
    kbd: Option<SharedString>,
    kind: ButtonKind,
    size: ButtonSize,
    disabled: bool,
    selected: bool,
    full_width: bool,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

impl UiButton {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            label: None,
            icon: None,
            kbd: None,
            kind: ButtonKind::default(),
            size: ButtonSize::default(),
            disabled: false,
            selected: false,
            full_width: false,
            tooltip: None,
            on_click: None,
        }
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Shortcut hint after the label (`⌘⏎`, `esc`).
    pub fn kbd(mut self, kbd: impl Into<SharedString>) -> Self {
        self.kbd = Some(kbd.into());
        self
    }

    pub fn kind(mut self, kind: ButtonKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn primary(self) -> Self {
        self.kind(ButtonKind::Primary)
    }

    pub fn ghost(self) -> Self {
        self.kind(ButtonKind::Ghost)
    }

    pub fn danger(self) -> Self {
        self.kind(ButtonKind::Danger)
    }

    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    pub fn small(self) -> Self {
        self.size(ButtonSize::Small)
    }

    pub fn large(self) -> Self {
        self.size(ButtonSize::Large)
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Toggled-on state for ghost/icon buttons (shows the selected fill).
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
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

struct Look {
    bg: Hsla,
    hover: Hsla,
    fg: Hsla,
    border: Option<Hsla>,
    weight: FontWeight,
}

fn look(kind: ButtonKind, selected: bool, t: &Tokens) -> Look {
    match kind {
        ButtonKind::Primary => Look {
            bg: t.acc,
            hover: t.acc.opacity(0.9),
            fg: t.accfg,
            border: None,
            weight: FontWeight::SEMIBOLD,
        },
        ButtonKind::Secondary | ButtonKind::Danger => Look {
            bg: if selected { t.active } else { t.elev },
            hover: t.hover,
            fg: if kind == ButtonKind::Danger {
                t.red
            } else {
                t.tx
            },
            border: Some(t.line2),
            weight: FontWeight::MEDIUM,
        },
        ButtonKind::Ghost => Look {
            bg: if selected {
                t.active
            } else {
                gpui_kit::transparent_black()
            },
            hover: if selected { t.active } else { t.hover },
            fg: if selected { t.tx } else { t.tx2 },
            border: None,
            weight: FontWeight::MEDIUM,
        },
    }
}

impl RenderOnce for UiButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.tokens();
        let look = look(self.kind, self.selected, t);
        let (height, text, pad) = match self.size {
            ButtonSize::Small => (BUTTON_HEIGHT_SMALL, px(11.5), px(8.)),
            ButtonSize::Medium => (BUTTON_HEIGHT, px(12.), px(10.)),
            ButtonSize::Large => (BUTTON_HEIGHT_SHEET, px(12.), px(12.)),
        };
        let icon_only = self.label.is_none();
        let tooltip = self.tooltip;
        div()
            .id(self.id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap_1p5()
            .h(height)
            .when(icon_only, |this| this.w(height))
            .when(!icon_only, |this| this.px(pad))
            .when(self.full_width, |this| this.flex_1())
            .rounded(RADIUS_CONTROL)
            .bg(look.bg)
            .when_some(look.border, |this, border| {
                this.border_1().border_color(border)
            })
            .text_color(look.fg)
            .text_size(text)
            .font_weight(look.weight)
            .when_some(self.icon, |this, icon| {
                this.child(icon.with_size(px(if icon_only { 14. } else { 13. })))
            })
            .when_some(self.label, |this, label| this.child(label))
            .when_some(self.kbd, |this, kbd| {
                this.child(
                    div()
                        .font_family(FONT_MONO)
                        .text_size(px(10.))
                        .font_weight(FontWeight::MEDIUM)
                        .opacity(0.7)
                        .child(kbd),
                )
            })
            .when_some(tooltip, |this, tooltip| {
                this.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            })
            .map(|this| match (self.disabled, self.on_click) {
                (true, _) => this.opacity(0.5),
                (false, Some(handler)) => this
                    .cursor_pointer()
                    // Keep focus where it is (e.g. in the terminal), as gpui-component buttons do.
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .hover(move |style| style.bg(look.hover))
                    .on_click(handler),
                (false, None) => this,
            })
    }
}
