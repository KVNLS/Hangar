//! Spacing, radii and fixed sizes from the design system.

use gpui_kit::{BoxShadow, Hsla, Pixels, point, px};

pub const SPACE_1: Pixels = px(4.);
pub const SPACE_2: Pixels = px(8.);
pub const SPACE_3: Pixels = px(12.);
pub const SPACE_4: Pixels = px(16.);
pub const SPACE_5: Pixels = px(20.);
pub const SPACE_6: Pixels = px(24.);
pub const SPACE_8: Pixels = px(32.);

/// Badge, kbd.
pub const RADIUS_BADGE: Pixels = px(3.);
/// Row, button, input.
pub const RADIUS_CONTROL: Pixels = px(5.);
/// Card, popover.
pub const RADIUS_CARD: Pixels = px(8.);
/// Window, sheet.
pub const RADIUS_SHEET: Pixels = px(10.);

pub const TITLEBAR_HEIGHT: Pixels = px(38.);
pub const STATUS_BAR_HEIGHT: Pixels = px(24.);
pub const ROW_HEIGHT: Pixels = px(30.);
pub const ROW_HEIGHT_DENSE: Pixels = px(28.);
pub const PANE_TAB_HEIGHT: Pixels = px(32.);
pub const PANEL_TAB_HEIGHT: Pixels = px(36.);
pub const BUTTON_HEIGHT: Pixels = px(26.);
pub const BUTTON_HEIGHT_SHEET: Pixels = px(28.);
pub const BUTTON_HEIGHT_SMALL: Pixels = px(24.);
pub const RIGHT_PANEL_WIDTH: Pixels = px(440.);

/// The single overlay shadow: `0 8 24 #000 35%`.
pub fn overlay_shadow() -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: Hsla {
            h: 0.,
            s: 0.,
            l: 0.,
            a: 0.35,
        },
        offset: point(px(0.), px(8.)),
        blur_radius: px(24.),
        spread_radius: px(0.),
        inset: false,
    }]
}
