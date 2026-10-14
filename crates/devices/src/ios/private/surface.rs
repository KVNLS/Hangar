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
    pub(super) fn new(surface: &IOSurfaceRef) -> Self {
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
