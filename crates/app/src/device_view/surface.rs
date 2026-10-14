//! Zero-copy device frames: wraps the simulator's framebuffer IOSurface in a `CVPixelBuffer` that
//! `Window::paint_surface` hands to Metal as a texture (BGRA support comes from the patched
//! renderer in `third_party/gpui-pre-apple`).

use std::any::Any;
use std::sync::Arc;

use crate::adapters::devices::ios::LiveFramebuffer;
use core_foundation::base::{TCFType as _, kCFAllocatorDefault};
use core_video::pixel_buffer::{CVPixelBuffer, CVPixelBufferRef};
use core_video::pixel_buffer_io_surface::CVPixelBufferCreateWithIOSurface;
use core_video::r#return::kCVReturnSuccess;
use hangar_core::device::LiveSurface;

/// The pixel buffer of the last live surface seen; surfaces only change when the simulator
/// swaps its framebuffer.
#[derive(Default)]
pub struct SurfaceCache(Option<(Arc<dyn LiveSurface>, CVPixelBuffer)>);

impl SurfaceCache {
    /// A pixel buffer over `surface`, or `None` if it is not a simulator framebuffer or CoreVideo
    /// refuses it.
    pub fn buffer(&mut self, surface: &Arc<dyn LiveSurface>) -> Option<CVPixelBuffer> {
        if let Some((cached, buffer)) = &self.0
            && Arc::ptr_eq(cached, surface)
        {
            return Some(buffer.clone());
        }
        let framebuffer = (surface.as_ref() as &dyn Any).downcast_ref::<LiveFramebuffer>()?;
        let buffer = pixel_buffer(framebuffer)?;
        self.0 = Some((surface.clone(), buffer.clone()));
        Some(buffer)
    }
}

fn pixel_buffer(framebuffer: &LiveFramebuffer) -> Option<CVPixelBuffer> {
    let surface = std::ptr::from_ref(framebuffer.io_surface()).cast();
    let mut out: CVPixelBufferRef = std::ptr::null_mut();
    // SAFETY: `surface` is a live IOSurfaceRef (retained by `framebuffer` for the call); null
    // attributes are allowed; `out` is a valid out-pointer.
    let status = unsafe {
        CVPixelBufferCreateWithIOSurface(kCFAllocatorDefault, surface, std::ptr::null(), &mut out)
    };
    if status != kCVReturnSuccess || out.is_null() {
        tracing::warn!(status, "CVPixelBufferCreateWithIOSurface failed");
        return None;
    }
    // SAFETY: a successful Create call returns a +1 reference we now own.
    Some(unsafe { CVPixelBuffer::wrap_under_create_rule(out) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_video::pixel_buffer::kCVPixelFormatType_32BGRA;
    use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString};
    use objc2_io_surface::{
        IOSurfaceRef, kIOSurfaceBytesPerElement, kIOSurfaceHeight, kIOSurfacePixelFormat,
        kIOSurfaceWidth,
    };

    fn bgra_surface(width: i32, height: i32) -> CFRetained<IOSurfaceRef> {
        // SAFETY: framework constants, valid for the process lifetime.
        let keys: [&CFString; 4] = unsafe {
            [
                kIOSurfaceWidth,
                kIOSurfaceHeight,
                kIOSurfaceBytesPerElement,
                kIOSurfacePixelFormat,
            ]
        };
        let values = [
            CFNumber::new_i32(width),
            CFNumber::new_i32(height),
            CFNumber::new_i32(4),
            CFNumber::new_i32(i32::from_be_bytes(*b"BGRA")),
        ];
        let values: Vec<&CFNumber> = values.iter().map(|v| &**v).collect();
        let props = CFDictionary::from_slices(&keys, &values);
        // SAFETY: the dictionary holds IOSurface property keys with CFNumber values.
        unsafe { IOSurfaceRef::new(props.as_ref()) }.unwrap()
    }

    #[derive(Debug)]
    struct NotAFramebuffer;

    impl LiveSurface for NotAFramebuffer {
        fn snapshot(&self) -> Option<Vec<u8>> {
            None
        }
    }

    #[test]
    fn wraps_simulator_framebuffers_once() {
        let surface: Arc<dyn LiveSurface> =
            Arc::new(LiveFramebuffer::new(&bgra_surface(1206, 2622)));
        let mut cache = SurfaceCache::default();
        let buffer = cache.buffer(&surface).unwrap();
        assert_eq!(buffer.get_pixel_format(), kCVPixelFormatType_32BGRA);
        assert_eq!((buffer.get_width(), buffer.get_height()), (1206, 2622));
        let again = cache.buffer(&surface).unwrap();
        assert_eq!(again.as_concrete_TypeRef(), buffer.as_concrete_TypeRef());

        let other: Arc<dyn LiveSurface> = Arc::new(LiveFramebuffer::new(&bgra_surface(4, 4)));
        assert_eq!(cache.buffer(&other).unwrap().get_width(), 4);
        assert!(
            cache
                .buffer(&(Arc::new(NotAFramebuffer) as Arc<dyn LiveSurface>))
                .is_none()
        );
    }
}
