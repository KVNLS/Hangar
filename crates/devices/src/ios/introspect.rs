//! Dumps the private-API surface the private backend relies on, so a new Xcode can be checked quickly.

use std::fmt::Write as _;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{msg_send, sel};
use objc2_foundation::{NSArray, NSString};

use super::runtime::{self, describe_class, responds};
use crate::Error;

const HID_CLIENT: &std::ffi::CStr = c"_TtC12SimulatorKit24SimDeviceLegacyHIDClient";

/// Returns a report of classes, methods and symbols involved in frames and HID for `udid`.
pub fn introspect(udid: &str) -> Result<String, Error> {
    let fw = runtime::frameworks()?;
    let mut out = String::new();
    let _ = writeln!(out, "developer dir: {}", fw.developer_dir);
    let _ = writeln!(out, "SimulatorKit: {}", fw.simulator_kit_path);

    for name in [
        "IndigoHIDMessageForMouseNSEvent",
        "IndigoHIDMessageForButton",
        "IndigoHIDMessageToCreatePointerService",
    ] {
        let found = runtime::symbol(Some(fw.simulator_kit), name).is_ok();
        let _ = writeln!(out, "symbol {name}: {found}");
    }

    let device = runtime::sim_device(udid)?;
    section(&mut out, "SimDevice", device.class());

    if responds(&device, sel!(io)) {
        // SAFETY: `io` returns the device's IO client object.
        let io: Option<Retained<AnyObject>> = unsafe { msg_send![&device, io] };
        if let Some(io) = io {
            section(&mut out, "io", io.class());
            for ports_sel in [sel!(deviceIOPorts), sel!(ioPorts)] {
                if !responds(&io, ports_sel) {
                    continue;
                }
                // SAFETY: both selectors return NSArray of port objects.
                let ports: Option<Retained<NSArray<AnyObject>>> = unsafe {
                    if ports_sel == sel!(deviceIOPorts) {
                        msg_send![&io, deviceIOPorts]
                    } else {
                        msg_send![&io, ioPorts]
                    }
                };
                let Some(ports) = ports else { continue };
                let _ = writeln!(
                    out,
                    "\n{} -> {} ports",
                    ports_sel.name().to_string_lossy(),
                    ports.count()
                );
                for (i, port) in ports.iter().enumerate() {
                    let pid = runtime::string_property(&port, sel!(portIdentifier))
                        .unwrap_or_else(|| String::from("?"));
                    let _ = writeln!(
                        out,
                        "port[{i}] {} id={pid}",
                        port.class().name().to_string_lossy()
                    );
                    if responds(&port, sel!(descriptor)) {
                        // SAFETY: `descriptor` takes no arguments and returns an object.
                        let desc = unsafe { runtime::raw::id0(&port, sel!(descriptor)) };
                        if let Some(desc) = desc {
                            let _ = writeln!(
                                out,
                                "  descriptor {}",
                                desc.class().name().to_string_lossy()
                            );
                            if pid == "com.apple.framebuffer.display" {
                                probe_selectors(&mut out, &desc);
                            }
                        }
                    }
                }
            }
        }
    }

    match AnyClass::get(HID_CLIENT) {
        Some(c) => section(&mut out, "SimDeviceLegacyHIDClient", c),
        None => {
            let _ = writeln!(out, "SimDeviceLegacyHIDClient: class not found");
        }
    }
    Ok(out)
}

/// Remote proxies hide their methods from the class, so ask about each known selector instead.
fn probe_selectors(out: &mut String, obj: &AnyObject) {
    let candidates = [
        sel!(framebufferSurface),
        sel!(ioSurface),
        sel!(registerScreenCallbacksWithUUID:callbackQueue:frameCallback:surfacesChangedCallback:propertiesChangedCallback:),
        sel!(unregisterScreenCallbacksWithUUID:),
        sel!(registerCallbackWithUUID:ioSurfacesChangeCallback:),
        sel!(registerCallbackWithUUID:damageRectanglesCallback:),
        sel!(displayClass),
        sel!(state),
        sel!(defaultWidthForDisplay),
        sel!(defaultHeightForDisplay),
        sel!(defaultPixelFormat),
        sel!(displayScale),
    ];
    for s in candidates {
        let _ = writeln!(
            out,
            "    responds {}: {}",
            s.name().to_string_lossy(),
            responds(obj, s)
        );
    }
    if responds(obj, sel!(description)) {
        // SAFETY: description returns NSString.
        let d: Option<Retained<NSString>> = unsafe { msg_send![obj, description] };
        let _ = writeln!(
            out,
            "    description: {}",
            d.map(|d| d.to_string()).unwrap_or_default()
        );
    }
}

fn section(out: &mut String, title: &str, cls: &AnyClass) {
    let _ = writeln!(out, "\n== {title}: {}", cls.name().to_string_lossy());
    for line in describe_class(cls) {
        let _ = writeln!(out, "  {line}");
    }
}
