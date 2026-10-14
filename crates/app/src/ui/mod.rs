//! Hangar design system v0.1 (`design/Hangar.dc.html`): tokens, theme, metrics, type.

pub mod components;
pub mod fonts;
pub mod metrics;
pub mod text;
pub mod theme;
pub mod tokens;

pub use metrics::*;
pub use text::{FONT_MONO, FONT_UI, TypeScale, mono_features};
pub use tokens::{ActiveTokens, Tokens};

use gpui_kit::App;
use gpui_kit::component::ThemeMode;

/// Fonts, then the Hangar themes in `mode`. Call right after `gpui_kit::init`.
pub fn init(mode: ThemeMode, cx: &mut App) -> anyhow::Result<()> {
    fonts::init(cx)?;
    theme::init(mode, cx)
}
