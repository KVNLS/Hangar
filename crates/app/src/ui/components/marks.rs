//! Small inline marks: keyboard keys, count badges, agent monograms, diff deltas, chips.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::ui::{ActiveTokens as _, FONT_MONO, RADIUS_BADGE};

/// One key cap (`⌘`, `K`, `esc`).
#[derive(IntoElement)]
pub struct Kbd(SharedString);

impl Kbd {
    pub fn new(key: impl Into<SharedString>) -> Self {
        Self(key.into())
    }
}

impl RenderOnce for Kbd {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.tokens();
        div()
            .flex()
            .items_center()
            .justify_center()
            .h(px(18.))
            .min_w(px(18.))
            .px_1()
            .rounded(RADIUS_BADGE)
            .border_1()
            .border_color(t.line2)
            .bg(t.elev)
            .font_family(FONT_MONO)
            .text_size(px(10.5))
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.tx2)
            .child(self.0)
    }
}

/// A shortcut as separate key caps: `kbd_keys(["⌘", "⇧", "O"])`.
pub fn kbd_keys<I, S>(keys: I) -> Div
where
    I: IntoIterator<Item = S>,
    S: Into<SharedString>,
{
    div()
        .flex()
        .gap(px(3.))
        .children(keys.into_iter().map(Kbd::new))
}

/// Accent count pill (`4`), e.g. on the Changes tab.
#[derive(IntoElement)]
pub struct CountBadge(SharedString);

impl CountBadge {
    pub fn new(count: impl Into<SharedString>) -> Self {
        Self(count.into())
    }
}

impl RenderOnce for CountBadge {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.tokens();
        div()
            .flex()
            .items_center()
            .h(px(16.))
            .px(px(5.))
            .rounded(px(8.))
            .bg(t.accbg)
            .text_color(t.acc)
            .font_family(FONT_MONO)
            .text_size(px(10.))
            .font_weight(FontWeight::SEMIBOLD)
            .child(self.0)
    }
}

/// Neutral agent monogram (`CC`, `CX`, `GM`, `>_`). Vendor logos need a licence; these don't.
#[derive(IntoElement)]
pub struct AgentMark {
    mark: SharedString,
    large: bool,
}

impl AgentMark {
    pub fn new(mark: impl Into<SharedString>) -> Self {
        Self {
            mark: mark.into(),
            large: false,
        }
    }

    /// 22 px instead of 16 (settings rows).
    pub fn large(mut self) -> Self {
        self.large = true;
        self
    }
}

impl RenderOnce for AgentMark {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.tokens();
        let (size, text) = if self.large {
            (px(22.), px(9.5))
        } else {
            (px(16.), px(8.5))
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .h(size)
            .min_w(size)
            .px(px(3.))
            .rounded(if self.large { px(4.) } else { RADIUS_BADGE })
            .bg(t.active)
            .border_1()
            .border_color(t.line2)
            .text_color(t.tx2)
            .font_family(FONT_MONO)
            .text_size(text)
            .font_weight(FontWeight::SEMIBOLD)
            .child(self.mark)
    }
}

/// `+38 −3` in mono; zero counts render in tertiary.
#[derive(IntoElement)]
pub struct Delta {
    added: usize,
    removed: usize,
}

impl Delta {
    pub fn new(added: usize, removed: usize) -> Self {
        Self { added, removed }
    }
}

impl RenderOnce for Delta {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.tokens();
        div()
            .flex()
            .flex_none()
            .gap_1()
            .font_family(FONT_MONO)
            .text_size(px(11.))
            .child(
                div()
                    .text_color(if self.added > 0 { t.grn } else { t.tx3 })
                    .child(format!("+{}", self.added)),
            )
            .child(
                div()
                    .text_color(if self.removed > 0 { t.red } else { t.tx3 })
                    .child(format!("−{}", self.removed)),
            )
    }
}

/// Mono chip on a hover fill: branch names, project tags, composer modes.
#[derive(IntoElement)]
pub struct Chip {
    label: SharedString,
    icon: Option<gpui_kit::component::Icon>,
}

impl Chip {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            icon: None,
        }
    }

    pub fn icon(mut self, icon: impl Into<gpui_kit::component::Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }
}

impl RenderOnce for Chip {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.tokens();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .h(px(20.))
            .px(px(6.))
            .rounded(px(4.))
            .bg(t.hover)
            .font_family(FONT_MONO)
            .text_size(px(11.))
            .text_color(t.tx2)
            .when_some(self.icon, |this, icon| this.child(icon.size(px(11.))))
            .child(self.label)
    }
}
