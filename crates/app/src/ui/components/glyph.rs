//! Agent-state glyphs. Shape carries the meaning without color (color-blind safe): ring spinning,
//! diamond, triangle, circle ✓, square ×, hollow circle.

use gpui_kit::assets::IconName;
use gpui_kit::component::{Icon, Sizable as _, Size, spinner::Spinner};
use gpui_kit::*;
use hangar_core::AgentState;

use crate::ui::{ActiveTokens as _, Tokens};

/// Foreground and subtle background colors of a state.
pub fn state_colors(state: AgentState, t: &Tokens) -> (Hsla, Hsla) {
    match state {
        AgentState::Working => (t.acc, t.accbg),
        AgentState::Approval => (t.amb, t.ambbg),
        AgentState::Input => (t.vio, t.viobg),
        AgentState::Review => (t.grn, t.grnbg),
        AgentState::Failed => (t.red, t.redbg),
        AgentState::Idle => (t.tx3, t.hover),
    }
}

#[derive(IntoElement)]
pub struct StateGlyph {
    state: AgentState,
    size: Pixels,
}

impl StateGlyph {
    /// 10 px, the sidebar/tab size.
    pub fn new(state: AgentState) -> Self {
        Self {
            state,
            size: px(10.),
        }
    }

    pub fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }
}

/// A filled polygon in a `z`×`z` box; `points` are fractions of the box.
fn polygon(z: Pixels, color: Hsla, points: &'static [(f32, f32)]) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let at = |(x, y): (f32, f32)| {
                point(
                    bounds.origin.x + bounds.size.width * x,
                    bounds.origin.y + bounds.size.height * y,
                )
            };
            let mut path = PathBuilder::fill();
            path.move_to(at(points[0]));
            for p in &points[1..] {
                path.line_to(at(*p));
            }
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        },
    )
    .size(z)
}

impl RenderOnce for StateGlyph {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.tokens();
        let z = self.size;
        let (color, _) = state_colors(self.state, t);
        let mark = |icon: IconName| Icon::new(icon).size(z * 0.8).text_color(t.panel);
        let inner = match self.state {
            AgentState::Working => Spinner::new()
                .icon(IconName::LoaderCircle)
                .ease(|t| t)
                .color(color)
                .with_size(Size::Size(z + px(1.)))
                .into_any_element(),
            // A square rotated 45° at 78% of the box.
            AgentState::Approval => polygon(
                z,
                color,
                &[(0.5, 0.05), (0.95, 0.5), (0.5, 0.95), (0.05, 0.5)],
            )
            .into_any_element(),
            AgentState::Input => {
                polygon(z, color, &[(0.5, 0.05), (1.0, 0.95), (0.0, 0.95)]).into_any_element()
            }
            AgentState::Review => div()
                .flex()
                .items_center()
                .justify_center()
                .size(z + px(1.))
                .rounded_full()
                .bg(color)
                .child(mark(IconName::Check))
                .into_any_element(),
            AgentState::Failed => div()
                .flex()
                .items_center()
                .justify_center()
                .size(z)
                .rounded(px(2.))
                .bg(color)
                .child(mark(IconName::X))
                .into_any_element(),
            AgentState::Idle => div()
                .size(z)
                .rounded_full()
                .border(px(1.5))
                .border_color(color)
                .into_any_element(),
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(z + px(2.))
            .child(inner)
    }
}
