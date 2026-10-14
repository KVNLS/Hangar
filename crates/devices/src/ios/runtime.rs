//! Loading Apple's private simulator frameworks and small ObjC runtime helpers.

use std::ffi::{CStr, CString};
use std::process::Command;
use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{msg_send, sel};
use objc2_foundation::{NSArray, NSError, NSString, NSUUID};

use crate::Error;

pub(crate) const CORE_SIMULATOR: &str =
    "/Library/Developer/PrivateFrameworks/CoreSimulator.framework/CoreSimulator";

/// SimulatorKit moved from `Contents/Developer/Library/PrivateFrameworks` (Xcode <= 26) to
/// `Contents/SharedFrameworks` (Xcode 27).
pub(crate) fn simulator_kit_candidates(developer_dir: &str) -> Vec<String> {
    let developer_dir = developer_dir.trim_end_matches('/');
    let contents = developer_dir
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or(developer_dir);
    vec![
        format!("{developer_dir}/Library/PrivateFrameworks/SimulatorKit.framework/SimulatorKit"),
        format!("{contents}/SharedFrameworks/SimulatorKit.framework/SimulatorKit"),
    ]
}

/// The active developer directory (`xcode-select -p`). Runs a process: call it off the UI thread.
pub fn developer_dir() -> Result<String, Error> {
    let out = Command::new("xcode-select").arg("-p").output()?;
    if !out.status.success() {
        return Err(Error::Command(format!(
            "xcode-select -p failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Whether Xcode's license is accepted and its first-launch packages are installed
/// (`xcodebuild -checkFirstLaunchStatus`). Runs a process: call it off the UI thread.
pub fn first_launch_done() -> bool {
    Command::new("xcodebuild")
        .arg("-checkFirstLaunchStatus")
        .stdin(std::process::Stdio::null())
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Handles of the loaded frameworks. Loaded once per process and never unloaded.
pub(crate) struct Frameworks {
    pub developer_dir: String,
    pub simulator_kit_path: String,
    pub simulator_kit: usize,
}

static FRAMEWORKS: OnceLock<Result<Frameworks, String>> = OnceLock::new();

pub(crate) fn frameworks() -> Result<&'static Frameworks, Error> {
    FRAMEWORKS
        .get_or_init(|| load_frameworks().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| Error::PrivateApi(e.clone()))
}

fn load_frameworks() -> Result<Frameworks, Error> {
    let developer_dir = developer_dir()?;
    dlopen(CORE_SIMULATOR)?;
    let simulator_kit_path = simulator_kit_candidates(&developer_dir)
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
        .ok_or_else(|| {
            Error::PrivateApi(format!(
                "SimulatorKit.framework not found under {developer_dir}"
            ))
        })?;
    let simulator_kit = dlopen(&simulator_kit_path)?;
    Ok(Frameworks {
        developer_dir,
        simulator_kit_path,
        simulator_kit,
    })
}

fn dlopen(path: &str) -> Result<usize, Error> {
    let c_path = CString::new(path).map_err(|e| Error::PrivateApi(e.to_string()))?;
    // SAFETY: valid NUL-terminated path; the handle is intentionally leaked.
    let handle = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
    if handle.is_null() {
        // SAFETY: dlerror returns a thread-local C string or null.
        let msg = unsafe {
            let e = libc::dlerror();
            if e.is_null() {
                "unknown".to_owned()
            } else {
                CStr::from_ptr(e).to_string_lossy().into_owned()
            }
        };
        return Err(Error::PrivateApi(format!("dlopen {path}: {msg}")));
    }
    Ok(handle as usize)
}

/// Resolves a C symbol from a dlopen handle (or `RTLD_DEFAULT` when `handle` is `None`).
pub(crate) fn symbol(handle: Option<usize>, name: &str) -> Result<*mut libc::c_void, Error> {
    let c_name = CString::new(name).map_err(|e| Error::PrivateApi(e.to_string()))?;
    let h = handle.map_or(libc::RTLD_DEFAULT, |h| h as *mut libc::c_void);
    // SAFETY: valid handle and NUL-terminated name.
    let ptr = unsafe { libc::dlsym(h, c_name.as_ptr()) };
    if ptr.is_null() {
        return Err(Error::PrivateApi(format!("symbol {name} not found")));
    }
    Ok(ptr)
}

pub(crate) fn class(name: &CStr) -> Result<&'static AnyClass, Error> {
    AnyClass::get(name)
        .ok_or_else(|| Error::PrivateApi(format!("class {} not found", name.to_string_lossy())))
}

/// Sends `respondsToSelector:` (rather than inspecting the class) because IO ports are ROCKit
/// remote proxies that forward to an object in CoreSimulatorService.
pub(crate) fn responds(obj: &AnyObject, sel: Sel) -> bool {
    // SAFETY: every NSObject/NSProxy implements respondsToSelector:.
    unsafe { msg_send![obj, respondsToSelector: sel] }
}

/// Raw `objc_msgSend` for objects whose class does not list the method (ROCKit proxies forward
/// unknown selectors over XPC), where objc2's debug signature verification would reject the send.
pub(crate) mod raw {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Sel};

    type Send0 = unsafe extern "C-unwind" fn(*const AnyObject, Sel) -> *mut AnyObject;
    type Send1 =
        unsafe extern "C-unwind" fn(*const AnyObject, Sel, *const AnyObject) -> *mut AnyObject;
    type Send5 = unsafe extern "C-unwind" fn(
        *const AnyObject,
        Sel,
        *const AnyObject,
        *const AnyObject,
        *const AnyObject,
        *const AnyObject,
        *const AnyObject,
    );

    /// Sends a no-argument message returning an object (or nil), retaining the result.
    ///
    /// # Safety
    /// `sel` must take no arguments and return an object pointer.
    pub(crate) unsafe fn id0(obj: &AnyObject, sel: Sel) -> Option<Retained<AnyObject>> {
        // SAFETY: objc_msgSend is called with the signature the caller guarantees.
        unsafe {
            let f: Send0 =
                std::mem::transmute(objc2::ffi::objc_msgSend as unsafe extern "C-unwind" fn());
            Retained::retain(f(obj, sel))
        }
    }

    /// # Safety
    /// `sel` must take one object argument and return void.
    pub(crate) unsafe fn void1(obj: &AnyObject, sel: Sel, arg: *const AnyObject) {
        // SAFETY: as above; the return register is ignored.
        unsafe {
            let f: Send1 =
                std::mem::transmute(objc2::ffi::objc_msgSend as unsafe extern "C-unwind" fn());
            f(obj, sel, arg);
        }
    }

    /// # Safety
    /// `sel` must take five object (or block) arguments and return void.
    pub(crate) unsafe fn void5(obj: &AnyObject, sel: Sel, args: [*const AnyObject; 5]) {
        // SAFETY: as above.
        unsafe {
            let f: Send5 =
                std::mem::transmute(objc2::ffi::objc_msgSend as unsafe extern "C-unwind" fn());
            f(obj, sel, args[0], args[1], args[2], args[3], args[4]);
        }
    }
}

/// Reads a no-argument property returning an object and renders it with `description`.
pub(crate) fn string_property(obj: &AnyObject, sel: Sel) -> Option<String> {
    if !responds(obj, sel) {
        return None;
    }
    // SAFETY: callers pass property getters (no arguments, object result).
    let value = unsafe { raw::id0(obj, sel) }?;
    // SAFETY: every object responds to `description`, which returns an NSString.
    let desc: Option<Retained<NSString>> = unsafe { msg_send![&value, description] };
    desc.map(|d| d.to_string())
}

pub(crate) fn require(obj: &AnyObject, sel: Sel) -> Result<(), Error> {
    if responds(obj, sel) {
        Ok(())
    } else {
        Err(Error::PrivateApi(format!(
            "{} does not respond to {}",
            obj.class().name().to_string_lossy(),
            sel.name().to_string_lossy()
        )))
    }
}

pub(crate) fn ns_error(e: &NSError) -> Error {
    Error::PrivateApi(e.localizedDescription().to_string())
}

/// `SimServiceContext` → default `SimDeviceSet` → the `SimDevice` with this UDID.
pub(crate) fn sim_device(udid: &str) -> Result<Retained<AnyObject>, Error> {
    let fw = frameworks()?;
    let ctx_class = class(c"SimServiceContext")?;
    let dev_dir = NSString::from_str(&fw.developer_dir);
    // SAFETY: selectors verified on CoreSimulator (Xcode 27); `error:` out-param handled by objc2.
    let ctx: Retained<AnyObject> =
        unsafe { msg_send![ctx_class, sharedServiceContextForDeveloperDir: &*dev_dir, error: _] }
            .map_err(|e: Retained<NSError>| ns_error(&e))?;
    require(&ctx, sel!(defaultDeviceSetWithError:))?;
    // SAFETY: as above.
    let set: Retained<AnyObject> = unsafe { msg_send![&ctx, defaultDeviceSetWithError: _] }
        .map_err(|e: Retained<NSError>| ns_error(&e))?;
    require(&set, sel!(devices))?;
    // SAFETY: `devices` returns an NSArray<SimDevice *>.
    let devices: Retained<NSArray<AnyObject>> = unsafe { msg_send![&set, devices] };
    for device in devices.iter() {
        if !responds(&device, sel!(UDID)) {
            continue;
        }
        // SAFETY: `UDID` returns an NSUUID.
        let id: Retained<NSUUID> = unsafe { msg_send![&device, UDID] };
        if id.UUIDString().to_string().eq_ignore_ascii_case(udid) {
            return Ok(device);
        }
    }
    Err(Error::NotFound(udid.to_owned()))
}

/// Human-readable list of a class's instance methods with their type encodings.
pub(crate) fn describe_class(cls: &AnyClass) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = Some(cls);
    while let Some(c) = current {
        if c.name() == c"NSObject" {
            break;
        }
        for m in c.instance_methods().iter() {
            let args: Vec<String> = (2..m.arguments_count())
                .filter_map(|i| m.argument_type(i).map(|t| t.to_string_lossy().into_owned()))
                .collect();
            out.push(format!(
                "{}  -[{}]  ret={} args=[{}]",
                m.name().name().to_string_lossy(),
                c.name().to_string_lossy(),
                m.return_type().to_string_lossy(),
                args.join(", ")
            ));
        }
        current = c.superclass();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simulator_kit_candidates_cover_old_and_new_layout() {
        let c = simulator_kit_candidates("/Applications/Xcode.app/Contents/Developer");
        assert_eq!(
            c,
            vec![
                "/Applications/Xcode.app/Contents/Developer/Library/PrivateFrameworks/SimulatorKit.framework/SimulatorKit",
                "/Applications/Xcode.app/Contents/SharedFrameworks/SimulatorKit.framework/SimulatorKit",
            ]
        );
    }
}
