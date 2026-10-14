//! Type scale. Geist for UI text, Geist Mono for code, paths, counts and shortcuts.

use gpui_kit::{FontWeight, Styled, px};

pub const FONT_UI: &str = "Geist";
pub const FONT_MONO: &str = "Geist Mono";

/// Type-scale helpers; colors stay with the caller (they come from the tokens).
pub trait TypeScale: Styled + Sized {
    /// 15 / 20 · 600.
    fn type_title(self) -> Self {
        self.text_size(px(15.))
            .line_height(px(20.))
            .font_weight(FontWeight::SEMIBOLD)
    }

    /// 13 / 18 · 400.
    fn type_body(self) -> Self {
        self.text_size(px(13.))
            .line_height(px(18.))
            .font_weight(FontWeight::NORMAL)
    }

    /// 13 / 18 · 600.
    fn type_body_strong(self) -> Self {
        self.text_size(px(13.))
            .line_height(px(18.))
            .font_weight(FontWeight::SEMIBOLD)
    }

    /// 12 / 16 · 400.
    fn type_secondary(self) -> Self {
        self.text_size(px(12.))
            .line_height(px(16.))
            .font_weight(FontWeight::NORMAL)
    }

    /// 11 / 14 · 600, upper-case section labels. GPUI has no letter-spacing; the design's
    /// +0.04em is dropped.
    fn type_label(self) -> Self {
        self.text_size(px(11.))
            .line_height(px(14.))
            .font_weight(FontWeight::SEMIBOLD)
    }

    /// 12 / 18 mono.
    fn type_mono(self) -> Self {
        self.font_family(FONT_MONO)
            .text_size(px(12.))
            .line_height(px(18.))
    }

    /// 11 / 16 mono.
    fn type_mono_small(self) -> Self {
        self.font_family(FONT_MONO)
            .text_size(px(11.))
            .line_height(px(16.))
    }
}

impl<T: Styled> TypeScale for T {}
