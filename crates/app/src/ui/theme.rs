//! Projects the design tokens onto gpui-component's theme so stock widgets (inputs, switches,
//! scrollbars, dialogs, lists) render in the design system. Generated from [`super::tokens`] so
//! the palette has a single source of truth.

use std::rc::Rc;

use anyhow::Context as _;
use gpui_kit::App;
use gpui_kit::component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry};
use serde_json::{Value, json};

use super::metrics::{RADIUS_CARD, RADIUS_CONTROL};
use super::text::{FONT_MONO, FONT_UI};
use hangar_terminal::{Palette as Palette16, Rgb};

use super::tokens::{DARK_HEX, LIGHT_HEX, Palette};

fn hex(v: u32) -> String {
    format!("#{v:08x}")
}

/// `v` at `alpha` (0–255) opacity.
fn alpha(v: u32, alpha: u8) -> String {
    hex((v & 0xFFFF_FF00) | u32::from(alpha))
}

fn colors(p: &Palette<u32>) -> Value {
    let h = hex;
    let pairs: Vec<(&str, String)> = vec![
        ("background", h(p.app)),
        ("foreground", h(p.tx)),
        ("border", h(p.line)),
        ("input.border", h(p.line2)),
        ("ring", h(p.acc)),
        ("caret", h(p.acc)),
        ("selection.background", alpha(p.acc, 0x40)),
        ("muted.background", h(p.hover)),
        ("muted.foreground", h(p.tx2)),
        ("accent.background", h(p.hover)),
        ("accent.foreground", h(p.tx)),
        ("popover.background", h(p.elev)),
        ("popover.foreground", h(p.tx)),
        ("overlay", h(p.scrim)),
        ("window.border", h(p.line2)),
        ("link", h(p.acc)),
        ("link.hover", h(p.acc)),
        ("link.active", h(p.acc)),
        ("drag.border", h(p.acc)),
        ("drop_target.background", alpha(p.acc, 0x26)),
        ("progress.bar.background", h(p.acc)),
        ("skeleton.background", h(p.hover)),
        ("primary.background", h(p.acc)),
        ("primary.hover.background", alpha(p.acc, 0xE6)),
        ("primary.active.background", alpha(p.acc, 0xCC)),
        ("primary.foreground", h(p.accfg)),
        ("secondary.background", h(p.elev)),
        ("secondary.hover.background", h(p.hover)),
        ("secondary.active.background", h(p.active)),
        ("secondary.foreground", h(p.tx)),
        ("danger.background", h(p.red)),
        ("danger.hover.background", alpha(p.red, 0xE6)),
        ("danger.active.background", alpha(p.red, 0xCC)),
        ("danger.foreground", h(p.accfg)),
        ("warning.background", h(p.amb)),
        ("warning.hover.background", alpha(p.amb, 0xE6)),
        ("warning.active.background", alpha(p.amb, 0xCC)),
        ("warning.foreground", h(p.accfg)),
        ("success.background", h(p.grn)),
        ("success.hover.background", alpha(p.grn, 0xE6)),
        ("success.active.background", alpha(p.grn, 0xCC)),
        ("success.foreground", h(p.accfg)),
        ("info.background", h(p.acc)),
        ("info.hover.background", alpha(p.acc, 0xE6)),
        ("info.active.background", alpha(p.acc, 0xCC)),
        ("info.foreground", h(p.accfg)),
        ("list.background", h(p.panel)),
        ("list.even.background", h(p.panel)),
        ("list.head.background", h(p.panel)),
        ("list.hover.background", h(p.hover)),
        ("list.active.background", h(p.active)),
        ("list.active.border", h(p.active)),
        ("table.background", h(p.panel)),
        ("table.even.background", h(p.panel)),
        ("table.head.background", h(p.panel)),
        ("table.head.foreground", h(p.tx3)),
        ("table.hover.background", h(p.hover)),
        ("table.active.background", h(p.active)),
        ("table.active.border", h(p.active)),
        ("table.row.border", h(p.line)),
        ("sidebar.background", h(p.panel)),
        ("sidebar.foreground", h(p.tx)),
        ("sidebar.border", h(p.line)),
        ("sidebar.accent.background", h(p.active)),
        ("sidebar.accent.foreground", h(p.tx)),
        ("sidebar.primary.background", h(p.acc)),
        ("sidebar.primary.foreground", h(p.accfg)),
        ("tab.background", h(p.panel)),
        ("tab.foreground", h(p.tx2)),
        ("tab.active.background", h(p.app)),
        ("tab.active.foreground", h(p.tx)),
        ("tab_bar.background", h(p.panel)),
        ("tab_bar.segmented.background", h(p.hover)),
        ("title_bar.background", h(p.panel)),
        ("title_bar.border", h(p.line)),
        ("status_bar.background", h(p.panel)),
        ("status_bar.border", h(p.line)),
        ("scrollbar.background", alpha(p.app, 0x00)),
        ("scrollbar.thumb.background", h(p.line2)),
        ("scrollbar.thumb.hover.background", h(p.tx3)),
        ("switch.background", h(p.line2)),
        ("switch.thumb.background", "#ffffffff".to_owned()),
        ("slider.background", h(p.acc)),
        ("slider.thumb.background", "#ffffffff".to_owned()),
        ("base.red", h(p.red)),
        ("base.green", h(p.grn)),
        ("base.yellow", h(p.amb)),
        ("base.blue", h(p.acc)),
        ("base.magenta", h(p.vio)),
    ];
    Value::Object(
        pairs
            .into_iter()
            .map(|(k, v)| (k.to_owned(), Value::String(v)))
            .collect(),
    )
}

fn config(
    name: &str,
    mode: &str,
    palette: &Palette<u32>,
    base: &ThemeConfig,
) -> anyhow::Result<ThemeConfig> {
    let mut config: ThemeConfig = serde_json::from_value(json!({
        "name": name,
        "mode": mode,
        "font.family": FONT_UI,
        "font.size": 13.0,
        "mono_font.family": FONT_MONO,
        "mono_font.size": 12.0,
        "radius": RADIUS_CONTROL.as_f32() as usize,
        "radius.lg": RADIUS_CARD.as_f32() as usize,
        "shadow": false,
        "colors": colors(palette),
    }))
    .with_context(|| format!("theme {name}"))?;
    // Syntax colors stay gpui-component's; the design does not define them.
    config.highlight = base.highlight.clone();
    Ok(config)
}

/// Installs the Hangar dark and light themes and switches to `mode`. Call after
/// `gpui_kit::init`.
pub fn init(mode: ThemeMode, cx: &mut App) -> anyhow::Result<()> {
    let registry = ThemeRegistry::global(cx);
    let dark = config(
        "Hangar Dark",
        "dark",
        &DARK_HEX,
        registry.default_dark_theme(),
    )?;
    let light = config(
        "Hangar Light",
        "light",
        &LIGHT_HEX,
        registry.default_light_theme(),
    )?;
    let theme = Theme::global_mut(cx);
    theme.dark_theme = Rc::new(dark);
    theme.light_theme = Rc::new(light);
    Theme::change(mode, None, cx);
    Ok(())
}

/// Terminal default colors for the current appearance: `term` background, `tx` text. Dark keeps
/// the terminal crate's ANSI set; light uses darker hues that read on a near-white background.
pub fn terminal_palette(dark: bool) -> Palette16 {
    let p = if dark { &DARK_HEX } else { &LIGHT_HEX };
    let rgb = |v: u32| Rgb::from_hex(v >> 8);
    let mut palette = Palette16::default();
    if !dark {
        palette.ansi = [
            0x17181B, 0xC9362D, 0x1F8049, 0xA8650C, 0x3F5BE0, 0x7647C9, 0x0E7C86, 0x6E7781,
            0x878B93, 0xE5584F, 0x2A9D5C, 0xC17A12, 0x5B74F0, 0x9466DE, 0x1A98A4, 0x8C959F,
        ]
        .map(Rgb::from_hex);
    }
    palette.background = rgb(p.term);
    palette.foreground = rgb(p.tx);
    palette.cursor = rgb(p.tx2);
    palette
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_configs_parse_with_every_color() {
        let base = ThemeConfig::default();
        for (mode, palette) in [("dark", DARK_HEX), ("light", LIGHT_HEX)] {
            let config = config("t", mode, &palette, &base).unwrap();
            assert_eq!(config.font_family.as_deref(), Some(FONT_UI));
            assert_eq!(config.radius, Some(5));
            // A misspelled key is silently dropped by serde; every key must survive the round trip.
            let json = serde_json::to_value(&config.colors).unwrap();
            let set: Vec<&String> = json
                .as_object()
                .unwrap()
                .iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, _)| k)
                .collect();
            let input = colors(&palette);
            let missing: Vec<&String> = input
                .as_object()
                .unwrap()
                .keys()
                .filter(|k| !set.contains(k))
                .collect();
            assert!(missing.is_empty(), "unknown theme keys: {missing:?}");
        }
    }

    #[test]
    fn alpha_replaces_the_alpha_byte() {
        assert_eq!(alpha(0x7B93FFFF, 0x40), "#7b93ff40");
    }
}
