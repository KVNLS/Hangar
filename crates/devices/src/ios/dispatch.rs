//! Minimal libdispatch bindings (libSystem is always linked on macOS).

use std::ffi::CStr;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;

unsafe extern "C" {
    fn dispatch_queue_create(
        label: *const libc::c_char,
        attr: *const libc::c_void,
    ) -> *mut AnyObject;
}

/// Creates a serial dispatch queue. Queues are ObjC objects (`OS_dispatch_queue`) under ARC rules.
pub(crate) fn serial_queue(label: &CStr) -> Option<Retained<AnyObject>> {
    // SAFETY: valid label; a null attribute means serial. The result is returned at +1.
    unsafe { Retained::from_raw(dispatch_queue_create(label.as_ptr(), std::ptr::null())) }
}
