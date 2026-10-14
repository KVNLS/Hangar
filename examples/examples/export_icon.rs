//! Writes the app icon (logo 8e) as a 1024 px PNG, the source for a future .app bundle's .icns.
//!
//! `cargo run -p hangar-examples --example export_icon -- [out.png]`
//! (default `crates/app/assets/brand/hangar-app-icon-1024.png`).

use std::sync::Arc;

use anyhow::Context as _;
use gpui_kit::SvgRenderer;
use hangar_app::{app_icon, assets::Assets};

fn main() -> anyhow::Result<()> {
    let out = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../crates/app/assets/brand/hangar-app-icon-1024.png"
        )
        .to_string()
    });
    let png = app_icon::render_png(&SvgRenderer::new(Arc::new(Assets)))?;
    std::fs::write(&out, png).with_context(|| format!("write {out}"))?;
    println!("{out}");
    Ok(())
}
