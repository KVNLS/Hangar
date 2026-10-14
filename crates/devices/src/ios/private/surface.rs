//! The simulator framebuffer as a [`LiveSurface`]: the UI hands the IOSurface to the GPU, readers
//! that need bytes (screenshots, diffs) take a snapshot.

use objc2_core_foundation::CFRetained;
use objc2_io_surface::{IOSurfaceLockOptions, IOSurfaceRef};

use crate::LiveSurface;

pub struct LiveFramebuffer(CFRetained<IOSurfaceRef>);

// SAFETY: IOSurface objects are thread-safe (IOSurfaceRef.h); this type only reads the surface,
// under a read-only lock, and retains/releases it through atomic CF reference counting.
unsafe impl Send for LiveFramebuffer {}
// SAFETY: as above; `&self` only exposes reads.
unsafe impl Sync for LiveFramebuffer {}

impl LiveFramebuffer {
    pub fn new(surface: &IOSurfaceRef) -> Self {
        Self(CFRetained::from(surface))
    }

    /// The BGRA framebuffer surface CoreSimulator draws into.
    pub fn io_surface(&self) -> &IOSurfaceRef {
        &self.0
    }
}

impl std::fmt::Debug for LiveFramebuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LiveFramebuffer({}x{})", self.0.width(), self.0.height())
    }
}

impl LiveSurface for LiveFramebuffer {
    fn snapshot(&self) -> Option<Vec<u8>> {
        let s = &*self.0;
        let mut seed = 0u32;
        // SAFETY: read-only lock of a live IOSurface; `seed` is a valid out-pointer.
        if unsafe { s.lock(IOSurfaceLockOptions::ReadOnly, &mut seed) } != 0 {
            return None;
        }
        let (width, height, stride) = (s.width(), s.height(), s.bytes_per_row());
        let row = width * 4;
        let mut bgra = Vec::with_capacity(row * height);
        let base = s.base_address().as_ptr().cast::<u8>().cast_const();
        for y in 0..height {
            // SAFETY: the surface is locked and spans `stride * height` bytes from `base`.
            let src = unsafe { std::slice::from_raw_parts(base.add(y * stride), row) };
            bgra.extend_from_slice(src);
        }
        // SAFETY: matching unlock for the lock above.
        unsafe { s.unlock(IOSurfaceLockOptions::ReadOnly, &mut seed) };
        Some(bgra)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_core_foundation::{CFDictionary, CFNumber, CFString};
    use objc2_io_surface::{
        kIOSurfaceBytesPerElement, kIOSurfaceHeight, kIOSurfacePixelFormat, kIOSurfaceWidth,
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

    #[test]
    fn snapshot_packs_rows() {
        let surface = bgra_surface(3, 2);
        let mut seed = 0;
        unsafe { surface.lock(IOSurfaceLockOptions::empty(), &mut seed) };
        let stride = surface.bytes_per_row();
        let base = surface.base_address().as_ptr().cast::<u8>();
        for y in 0..2 {
            for x in 0..12 {
                unsafe { *base.add(y * stride + x) = (y * 12 + x) as u8 };
            }
        }
        unsafe { surface.unlock(IOSurfaceLockOptions::empty(), &mut seed) };
        let fb = LiveFramebuffer::new(&surface);
        let expected: Vec<u8> = (0..24).collect();
        assert_eq!(fb.snapshot().unwrap(), expected);
        assert_eq!(format!("{fb:?}"), "LiveFramebuffer(3x2)");
    }
}
