//! Embedded Geist and Geist Mono (SIL OFL 1.1, `assets/fonts/OFL.txt`).

use std::borrow::Cow;

use gpui_kit::App;

const FONTS: [&[u8]; 7] = [
    include_bytes!("../../assets/fonts/Geist-Regular.ttf"),
    include_bytes!("../../assets/fonts/Geist-Medium.ttf"),
    include_bytes!("../../assets/fonts/Geist-SemiBold.ttf"),
    include_bytes!("../../assets/fonts/Geist-Bold.ttf"),
    include_bytes!("../../assets/fonts/GeistMono-Regular.ttf"),
    include_bytes!("../../assets/fonts/GeistMono-Medium.ttf"),
    include_bytes!("../../assets/fonts/GeistMono-SemiBold.ttf"),
];

/// Registers the fonts with the text system. Call before the theme names them.
pub fn init(cx: &mut App) -> anyhow::Result<()> {
    cx.text_system()
        .add_fonts(FONTS.iter().map(|f| Cow::Borrowed(*f)).collect())
}
