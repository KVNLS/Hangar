//! Frame to PNG for the `screenshot` tool.

use hangar_core::device::Frame;
use image::codecs::png::PngEncoder;
use image::imageops::{self, FilterType};
use image::{ExtendedColorType, ImageEncoder, RgbImage};

/// Longest side of the image sent to agents: large enough to read UI text, small enough to keep
/// the model's image token cost low.
pub const MAX_SIDE: u32 = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screenshot {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum ScreenshotError {
    #[error("invalid frame: {0}")]
    InvalidFrame(String),
    #[error("png encode: {0}")]
    Encode(String),
}

/// Encodes `frame` (BGRA) as an opaque RGB PNG whose longest side is at most `max_side`,
/// keeping the aspect ratio. Alpha is dropped: device frames are opaque. A live surface is
/// snapshotted first.
pub fn encode_screenshot(frame: &Frame, max_side: u32) -> Result<Screenshot, ScreenshotError> {
    let frame = frame
        .snapshot()
        .ok_or_else(|| ScreenshotError::InvalidFrame("device surface could not be read".into()))?;
    let (stride, bgra) = frame
        .bytes()
        .ok_or_else(|| ScreenshotError::InvalidFrame("no captured bytes".into()))?;
    let (w, h) = (frame.width, frame.height);
    if w == 0 || h == 0 || max_side == 0 {
        return Err(ScreenshotError::InvalidFrame(format!(
            "empty size {w}x{h} (max side {max_side})"
        )));
    }
    let row_bytes = w as usize * 4;
    if stride < row_bytes {
        return Err(ScreenshotError::InvalidFrame(format!(
            "stride {stride} shorter than a {w} px row"
        )));
    }
    let needed = stride * (h as usize - 1) + row_bytes;
    if bgra.len() < needed {
        return Err(ScreenshotError::InvalidFrame(format!(
            "{} bytes for {w}x{h} with stride {stride} (need {needed})",
            bgra.len()
        )));
    }

    let mut rgb = Vec::with_capacity(w as usize * h as usize * 3);
    for y in 0..h as usize {
        let row = &bgra[y * stride..y * stride + row_bytes];
        for px in row.as_chunks::<4>().0 {
            rgb.extend_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    let mut img = RgbImage::from_raw(w, h, rgb)
        .ok_or_else(|| ScreenshotError::InvalidFrame("pixel buffer size mismatch".into()))?;

    let (tw, th) = fit(w, h, max_side);
    if (tw, th) != (w, h) {
        img = imageops::resize(&img, tw, th, FilterType::Triangle);
    }

    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(img.as_raw(), tw, th, ExtendedColorType::Rgb8)
        .map_err(|e| ScreenshotError::Encode(e.to_string()))?;
    Ok(Screenshot {
        png,
        width: tw,
        height: th,
    })
}

fn fit(w: u32, h: u32, max_side: u32) -> (u32, u32) {
    let long = w.max(h);
    if long <= max_side {
        return (w, h);
    }
    let scale = |v: u32| {
        let scaled = (u64::from(v) * u64::from(max_side) + u64::from(long) / 2) / u64::from(long);
        (scaled as u32).max(1)
    };
    (scale(w), scale(h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hangar_core::device::FramePixels;
    use std::sync::Arc;

    fn frame(width: u32, height: u32, stride: usize) -> Frame {
        let mut bgra = vec![0u8; stride * height as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                // Pure blue in BGRA order.
                bgra[y * stride + x * 4..y * stride + x * 4 + 4].copy_from_slice(&[255, 0, 0, 255]);
            }
        }
        Frame::bgra(width, height, stride, Arc::from(bgra), 1)
    }

    fn decode(png: &[u8]) -> image::DynamicImage {
        image::load_from_memory_with_format(png, image::ImageFormat::Png).unwrap()
    }

    #[test]
    fn downscales_keeping_aspect_ratio() {
        let shot = encode_screenshot(&frame(1179, 2556, 1179 * 4), 1024).unwrap();
        assert_eq!((shot.width, shot.height), (472, 1024));
        let img = decode(&shot.png);
        assert_eq!((img.width(), img.height()), (472, 1024));
        let px = img.to_rgb8().get_pixel(100, 100).0;
        assert_eq!(px, [0, 0, 255], "BGRA must become RGB");
    }

    #[test]
    fn landscape_and_small_frames() {
        let shot = encode_screenshot(&frame(2400, 1080, 2400 * 4), 1024).unwrap();
        assert_eq!((shot.width, shot.height), (1024, 461));
        let shot = encode_screenshot(&frame(300, 200, 300 * 4 + 16), 1024).unwrap();
        assert_eq!((shot.width, shot.height), (300, 200));
        assert_eq!(
            decode(&shot.png).to_rgb8().get_pixel(299, 199).0,
            [0, 0, 255]
        );
    }

    #[test]
    fn rejects_bad_frames() {
        let mut f = frame(10, 10, 40);
        f.pixels = FramePixels::Bgra {
            stride: 40,
            data: Arc::from(vec![0u8; 100]),
        };
        assert!(matches!(
            encode_screenshot(&f, 1024),
            Err(ScreenshotError::InvalidFrame(_))
        ));
        let mut f = frame(10, 10, 40);
        if let FramePixels::Bgra { stride, .. } = &mut f.pixels {
            *stride = 20;
        }
        assert!(encode_screenshot(&f, 1024).is_err());
        assert!(encode_screenshot(&frame(0, 10, 0), 1024).is_err());
    }
}
