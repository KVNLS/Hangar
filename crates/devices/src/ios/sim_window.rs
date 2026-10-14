//! simctl-backend input: posts CGEvent mouse clicks into the Simulator.app window that shows the
//! device. Public APIs only, but it needs the Accessibility permission, a visible window with
//! device bezels hidden, and it moves the user's real mouse pointer.

use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton,
    CGPreflightPostEventAccess, CGWindowListCopyWindowInfo, CGWindowListOption,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

use crate::geometry::{self, Rect};
use crate::{DeviceInfo, Error, Input, Result};

/// Owner name of Simulator.app windows (Xcode <= 26).
const SIMULATOR_OWNER: &str = "Simulator";
/// Xcode 27 ships no Simulator.app; devices are shown inside Device Hub
/// (`Xcode.app/Contents/Applications/DeviceHub.app`, executable `DevicesTrampoline`).
const DEVICE_HUB_OWNER: &str = "Device Hub";
const CLICK_HOLD: Duration = Duration::from_millis(60);
/// macOS virtual key code for "H" (Simulator's Device ▸ Home is ⇧⌘H).
const KEY_H: u16 = 4;

#[derive(Debug, Clone, PartialEq)]
pub struct SimWindow {
    pub owner: String,
    pub name: Option<String>,
    pub bounds: Rect,
}

pub(crate) fn click(info: &DeviceInfo, input: Input) -> Result<()> {
    if let Some(what) = unsupported(&input) {
        return Err(Error::Unsupported(what));
    }
    if !CGPreflightPostEventAccess() {
        return Err(Error::Permission(
            "posting mouse events needs Accessibility access for this app (System Settings ▸ \
             Privacy & Security ▸ Accessibility)"
                .into(),
        ));
    }
    let screen = info
        .screen
        .ok_or(Error::Unsupported("simctl input without known screen size"))?;
    let windows = simulator_windows();
    let window = pick_window(&windows, &info.name)
        .ok_or_else(|| no_window(&info.name, simulator_app_installed()))?;
    if window.owner == DEVICE_HUB_OWNER {
        // The hub window embeds the device screen next to its own sidebar/toolbar at a position
        // that can only be discovered through Accessibility or screen capture; not mapped.
        return Err(Error::Unsupported(
            "simctl input into Xcode 27 Device Hub windows (screen position inside the hub is unknown)",
        ));
    }
    let to_global = |x: f64, y: f64| -> Result<CGPoint> {
        let (gx, gy) = window_point(&window.bounds, screen.width_pt(), screen.height_pt(), x, y)
            .ok_or(Error::Unsupported(
                "Simulator window does not match the screen aspect; hide device bezels \
                 (Window ▸ Show Device Bezels) for the simctl backend",
            ))?;
        Ok(CGPoint { x: gx, y: gy })
    };
    match input {
        Input::Tap { x, y } => {
            let p = to_global(x, y)?;
            post_mouse(CGEventType::LeftMouseDown, p)?;
            std::thread::sleep(CLICK_HOLD);
            post_mouse(CGEventType::LeftMouseUp, p)
        }
        Input::TouchDown { x, y } => post_mouse(CGEventType::LeftMouseDown, to_global(x, y)?),
        Input::TouchMove { x, y } => post_mouse(CGEventType::LeftMouseDragged, to_global(x, y)?),
        Input::TouchUp { x, y } => post_mouse(CGEventType::LeftMouseUp, to_global(x, y)?),
        Input::Home => {
            // Keyboard shortcuts go to the frontmost app; Simulator must be active.
            post_key(KEY_H, true)?;
            post_key(KEY_H, false)
        }
        Input::Lock
        | Input::VolumeUp
        | Input::VolumeDown
        | Input::AppSwitch
        | Input::Back
        | Input::Key(_) => Err(Error::Unsupported(
            unsupported(&input).unwrap_or("this input through the simctl backend"),
        )),
    }
}

/// What this backend can never send, whatever window is open.
fn unsupported(input: &Input) -> Option<&'static str> {
    match input {
        Input::Lock | Input::VolumeUp | Input::VolumeDown | Input::AppSwitch => {
            Some("this hardware button through the simctl backend (use the private backend)")
        }
        Input::Back => Some("Back button on iOS"),
        Input::Key(_) => Some("keyboard keys through the simctl backend (use the private backend)"),
        _ => None,
    }
}

/// No window shows the device. Xcode 27 has no Simulator.app to open: input is not possible.
fn no_window(device: &str, simulator_app: bool) -> Error {
    if simulator_app {
        Error::NotFound(format!(
            "visible Simulator.app window for {device} (open it with `open -a Simulator`)"
        ))
    } else {
        Error::Unsupported(
            "input on the simctl backend: this Xcode has no Simulator.app, so the device is \
             view only (use the private backend)",
        )
    }
}

fn simulator_app_installed() -> bool {
    super::developer_dir().is_ok_and(|dir| {
        std::path::Path::new(&dir)
            .join("Applications/Simulator.app")
            .is_dir()
    })
}

fn post_mouse(kind: CGEventType, p: CGPoint) -> Result<()> {
    let event = CGEvent::new_mouse_event(None, kind, p, CGMouseButton::Left)
        .ok_or_else(|| Error::Command("CGEventCreateMouseEvent failed".into()))?;
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    Ok(())
}

fn post_key(key: u16, down: bool) -> Result<()> {
    let event = CGEvent::new_keyboard_event(None, key, down)
        .ok_or_else(|| Error::Command("CGEventCreateKeyboardEvent failed".into()))?;
    CGEvent::set_flags(
        Some(&event),
        CGEventFlags::MaskCommand | CGEventFlags::MaskShift,
    );
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    Ok(())
}

/// On-screen windows owned by Simulator.app (global coordinates, top-left origin, points).
/// Window titles are only visible with the Screen Recording permission; `name` is None otherwise.
pub fn simulator_windows() -> Vec<SimWindow> {
    on_screen_windows()
        .into_iter()
        .filter(|w| w.owner == SIMULATOR_OWNER || w.owner == DEVICE_HUB_OWNER)
        .collect()
}

/// All normal-layer on-screen windows (for diagnostics).
pub fn on_screen_windows() -> Vec<SimWindow> {
    let options =
        CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
    let Some(list) = CGWindowListCopyWindowInfo(options, 0) else {
        return Vec::new();
    };
    // SAFETY: CFArray<CFDictionary> is toll-free bridged with NSArray<NSDictionary>.
    let list: Retained<NSArray<NSDictionary<NSString, AnyObject>>> = unsafe {
        let raw = CFRetained::into_raw(list).as_ptr();
        match Retained::from_raw(raw.cast()) {
            Some(l) => l,
            None => return Vec::new(),
        }
    };
    let mut out = Vec::new();
    for w in list.iter() {
        let Some(owner) = string_value(&w, "kCGWindowOwnerName") else {
            continue;
        };
        if number_value(&w, "kCGWindowLayer").unwrap_or(0.0) != 0.0 {
            continue;
        }
        let Some(bounds) = bounds_value(&w) else {
            continue;
        };
        out.push(SimWindow {
            owner,
            name: string_value(&w, "kCGWindowName").filter(|n| !n.is_empty()),
            bounds,
        });
    }
    out
}

fn value(dict: &NSDictionary<NSString, AnyObject>, key: &str) -> Option<Retained<AnyObject>> {
    dict.objectForKey(&NSString::from_str(key))
}

fn string_value(dict: &NSDictionary<NSString, AnyObject>, key: &str) -> Option<String> {
    value(dict, key)?
        .downcast::<NSString>()
        .ok()
        .map(|s| s.to_string())
}

fn number_value(dict: &NSDictionary<NSString, AnyObject>, key: &str) -> Option<f64> {
    value(dict, key)?
        .downcast::<NSNumber>()
        .ok()
        .map(|n| n.doubleValue())
}

fn bounds_value(dict: &NSDictionary<NSString, AnyObject>) -> Option<Rect> {
    let b = value(dict, "kCGWindowBounds")?
        .downcast::<NSDictionary>()
        .ok()?;
    // SAFETY: the bounds dictionary maps NSString keys to NSNumber values.
    let b: Retained<NSDictionary<NSString, AnyObject>> = unsafe { Retained::cast_unchecked(b) };
    Some(Rect {
        x: number_value(&b, "X")?,
        y: number_value(&b, "Y")?,
        width: number_value(&b, "Width")?,
        height: number_value(&b, "Height")?,
    })
}

/// The window titled with the device name, or the only Simulator window when titles are hidden.
pub(crate) fn pick_window<'a>(
    windows: &'a [SimWindow],
    device_name: &str,
) -> Option<&'a SimWindow> {
    let named: Vec<&SimWindow> = windows
        .iter()
        .filter(|w| w.name.as_deref().is_some_and(|n| n.contains(device_name)))
        .collect();
    match named.as_slice() {
        [one] => Some(one),
        [] if windows.len() == 1 => windows.first(),
        _ => None,
    }
}

/// Height of Simulator.app's title/toolbar strip above the screen, in points.
pub(crate) const TITLE_BAR_PT: f64 = 28.0;
/// Accepted aspect-ratio mismatch between the window content and the device screen.
const ASPECT_TOLERANCE: f64 = 0.03;

/// Device point → global screen point inside a bezel-less Simulator window, or None when the
/// window content does not have the screen's aspect ratio (bezels shown, or unknown chrome).
pub(crate) fn window_point(
    window: &Rect,
    screen_w_pt: f64,
    screen_h_pt: f64,
    x: f64,
    y: f64,
) -> Option<(f64, f64)> {
    let content_h = window.height - TITLE_BAR_PT;
    if content_h <= 0.0 || window.width <= 0.0 || screen_w_pt <= 0.0 || screen_h_pt <= 0.0 {
        return None;
    }
    let content_aspect = window.width / content_h;
    let screen_aspect = screen_w_pt / screen_h_pt;
    if ((content_aspect - screen_aspect) / screen_aspect).abs() > ASPECT_TOLERANCE {
        return None;
    }
    let r = geometry::contain(window.width, content_h, screen_w_pt, screen_h_pt);
    let (ux, uy) = geometry::device_to_unit(x, y, screen_w_pt, screen_h_pt);
    Some((
        window.x + r.x + ux * r.width,
        window.y + TITLE_BAR_PT + r.y + uy * r.height,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_and_keys_are_unsupported_before_any_window_lookup() {
        for input in [Input::Lock, Input::VolumeUp, Input::AppSwitch, Input::Back] {
            assert!(unsupported(&input).is_some(), "{input:?}");
        }
        assert!(unsupported(&Input::Tap { x: 1.0, y: 1.0 }).is_none());
        assert!(unsupported(&Input::Home).is_none());
    }

    #[test]
    fn without_simulator_app_input_is_unsupported() {
        assert!(matches!(no_window("iPhone", false), Error::Unsupported(_)));
        assert!(matches!(no_window("iPhone", true), Error::NotFound(_)));
    }

    fn win(name: Option<&str>, w: f64, h: f64) -> SimWindow {
        SimWindow {
            owner: "Simulator".into(),
            name: name.map(str::to_owned),
            bounds: Rect {
                x: 100.0,
                y: 50.0,
                width: w,
                height: h,
            },
        }
    }

    #[test]
    fn maps_device_points_into_bezel_less_window() {
        // 402x874 pt device shown at scale 1 below the title bar.
        let w = win(None, 402.0, 874.0 + TITLE_BAR_PT);
        assert_eq!(
            window_point(&w.bounds, 402.0, 874.0, 0.0, 0.0),
            Some((100.0, 50.0 + TITLE_BAR_PT))
        );
        assert_eq!(
            window_point(&w.bounds, 402.0, 874.0, 201.0, 437.0),
            Some((301.0, 50.0 + TITLE_BAR_PT + 437.0))
        );
        // Half scale.
        let w = win(None, 201.0, 437.0 + TITLE_BAR_PT);
        assert_eq!(
            window_point(&w.bounds, 402.0, 874.0, 402.0, 874.0),
            Some((301.0, 50.0 + TITLE_BAR_PT + 437.0))
        );
    }

    #[test]
    fn rejects_windows_with_bezels() {
        let w = win(None, 470.0, 960.0);
        assert_eq!(window_point(&w.bounds, 402.0, 874.0, 10.0, 10.0), None);
    }

    #[test]
    fn picks_window_by_name_or_single() {
        let ws = vec![
            win(Some("iPhone 17 – iOS 27.0"), 1.0, 1.0),
            win(Some("iPad Air"), 2.0, 2.0),
        ];
        assert_eq!(pick_window(&ws, "iPhone 17"), Some(&ws[0]));
        assert_eq!(pick_window(&ws, "iPhone 18"), None);
        let ws = vec![win(None, 1.0, 1.0)];
        assert_eq!(pick_window(&ws, "iPhone 17"), Some(&ws[0]));
        let ws = vec![win(None, 1.0, 1.0), win(None, 2.0, 2.0)];
        assert_eq!(pick_window(&ws, "iPhone 17"), None);
    }
}
