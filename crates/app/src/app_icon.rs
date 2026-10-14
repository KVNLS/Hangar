//! The app icon (logo 8e): rasterized from `brand/hangar-app-icon.svg`, set on the Dock at launch so
//! `cargo run` builds show it without an .app bundle.

use anyhow::Context as _;
use gpui_kit::{App, SMOOTH_SVG_SCALE_FACTOR, SvgRenderer};

use crate::assets::APP_ICON_SVG;

/// Edge of the rasterized icon, in pixels (the macOS 512@2x slot).
pub const ICON_PX: u32 = 1024;

/// The app icon as a `ICON_PX`-square PNG.
pub fn render_png(renderer: &SvgRenderer) -> anyhow::Result<Vec<u8>> {
    // The SVG canvas is ICON_PX wide and the renderer multiplies the scale by its smoothing factor.
    let image = renderer
        .render_single_frame(APP_ICON_SVG, 1.0 / SMOOTH_SVG_SCALE_FACTOR)
        .context("rasterize the app icon")?;
    let size = image.size(0);
    let mut rgba = image.as_bytes(0).context("app icon has no frame")?.to_vec();
    for px in rgba.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    let (w, h) = (size.width.0 as u32, size.height.0 as u32);
    let buffer = image::RgbaImage::from_raw(w, h, rgba).context("app icon buffer size")?;
    let mut png = Vec::new();
    buffer
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .context("encode the app icon")?;
    Ok(png)
}

/// Shows the app icon in the Dock (and the app switcher) for this process.
#[cfg(target_os = "macos")]
pub fn set_dock_icon(cx: &App) -> anyhow::Result<()> {
    use objc2::AllocAnyThread as _;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    let png = render_png(&cx.svg_renderer())?;
    let mtm = MainThreadMarker::new().context("the Dock icon must be set on the main thread")?;
    let image = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(&png))
        .context("AppKit rejected the app icon")?;
    // SAFETY: a valid, retained NSImage, set on the main thread.
    unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn set_dock_icon(_cx: &App) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_square_png() {
        let renderer = SvgRenderer::new(std::sync::Arc::new(crate::assets::Assets));
        let png = render_png(&renderer).unwrap();
        let image = image::load_from_memory(&png).unwrap();
        assert_eq!((image.width(), image.height()), (ICON_PX, ICON_PX));
        // Transparent margin around the tile, opaque tile centre.
        let rgba = image.to_rgba8();
        assert_eq!(rgba.get_pixel(4, 4)[3], 0);
        assert_eq!(rgba.get_pixel(ICON_PX / 2, ICON_PX / 2)[3], 255);
    }
}
