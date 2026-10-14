# M5 (headless) — iOS device capabilities

Environment: Xcode 27 (SimulatorKit in `Contents/SharedFrameworks`), iOS 27.0 iPhone 17 simulator, macOS arm64.
Code: `crates/devices` (`simctl.rs`, `ios/private.rs`, `ios/keymap.rs`, `ios/simctl_backend.rs`).
Driver: `cargo run -p hangar-examples --example spike_sim -- <flags>` (see `--help`).

## Design

### Listing and lifecycle (public `simctl`, both backends)
- `simctl::list_all()` runs `simctl list devices -j` (all *available* devices in any state; `isAvailable == false`
  entries dropped) and fills `ScreenSpec` from each device type's `capabilities.plist` (read once per device type).
  `parse_all` is the pure, fixture-tested part. Entries of all runtimes are returned (`runtime` says "iOS 27.0");
  filtering to iOS is the caller's business.
- `simctl::boot(udid)` / `ios::boot(udid)` = `simctl bootstatus <udid> -b` with a 180 s kill timeout: boots if needed
  and blocks until SpringBoard finished booting; an already booted device returns at once.
  `simctl::shutdown` / `ios::shutdown` = `simctl shutdown`, "already shut down" is not an error.
- `Device::{boot,shutdown,install,launch}` on `PrivateSimulator` and `SimctlSimulator` call the same functions
  (`install` = `simctl install`, `launch` = `simctl launch`; `simctl::launch` also returns the pid).
  After `shutdown` an open `Device` stops getting frames; reopen after `boot`.

### Hardware buttons (private backend, Indigo HID)
`IndigoHIDMessageForButton(source, direction, target 0x33)`: source `0x0` Home (existing), `0x1` Lock/side button.
Direction 1 = down, 2 = up. Lock verified: home screen -> lock screen; pressing again wakes it.

The volume rocker is not a "button" message: it is a consumer-page HID usage sent with
`IndigoHIDMessageForHIDArbitrary(target 0x33, usage_page 0x0c, usage, direction)`
(found by disassembling SimulatorKit: event type 0x2711, fields `[+0x34]=direction, [+0x38]=target, [+0x3c]=usage,
[+0x44]=page`). Usage `0xe9` = Volume Increment, `0xea` = Volume Decrement. Verified: the iOS volume HUD appears
on the left edge, and drains after Volume Down.

New `Input` variants: `Lock`, `VolumeUp`, `VolumeDown`, `AppSwitch`.
- iOS private: Lock/VolumeUp/VolumeDown work; `AppSwitch` and `Back` -> `Unsupported` (Face ID devices have no
  such button; the app switcher is a swipe-up-and-hold gesture the caller can build from touches).
- iOS simctl backend: only `Home` (Cmd-Shift-H into the Simulator window); the others -> `Unsupported`.
- Android: gRPC `sendKey` names `Power`, `AudioVolumeUp`, `AudioVolumeDown`, `AppSwitch`; adb `input keyevent`
  `KEYCODE_POWER|VOLUME_UP|VOLUME_DOWN|APP_SWITCH`. Not run against an emulator in this task (no AVD was
  booted); only the command mapping is unit-tested/compiled.

### Keyboard text (private backend)
`IndigoHIDMessageForKeyboardArbitrary(usage, direction)` takes a keyboard-page (0x07) HID usage directly
(the `...NSEvent` variant maps mac virtual key codes through a table first). `ios/keymap.rs` maps printable ASCII,
`\n` and `\t` on a US layout to `(usage, shift)`; shift is Left Shift (usage `0xe1`) pressed around the key.
`send_text` validates the whole string first and returns `Unsupported` for any other character (nothing is typed
in that case). Because the host "keyboard" is a hardware keyboard, iOS shows no soft keyboard while typing.
Verified in Safari's address bar: `Hello World! 42 @#$ a/b?c=d` appears exactly (upper case, digits, shifted symbols).
simctl backend: `Unsupported`. `simctl pbcopy` only fills the pasteboard; pasting needs a Cmd-V key event or a tap
on the paste callout, i.e. exactly the input path that backend does not have on Xcode 27.

### Rotation: not feasible (documented, `set_orientation` returns `Unsupported`)
`Device::set_orientation(Orientation)` exists with a default `Unsupported` so callers can already code against it.
What was tried on Xcode 27:
- Public: `simctl` has no orientation/rotate command (`simctl ui` only does appearance, contrast, content size).
- CoreSimulator `SimDevice` and the framebuffer descriptors expose no orientation/rotation selector
  (probed `uiOrientation`, `setOrientation:`, `deviceOrientation`, `rotation`, `deviceRotation`, ... : none respond;
  the only rotation API is `updateNearbyInteractionDeviceRect:deviceRotation:`, which is for Nearby Interaction).
  SimulatorKit only *reacts* to guest rotation (`RotateWindowWhenSignaledByGuest`, `SimDisplayUIOrientationChangeDelegate`).
- Indigo accelerometer: `IndigoHIDMessageForDeviceMotionLiteEvent(type, data, len 0x18|0x28)`. Sent with event types
  1..10 and a gravity-like payload: no rotation (type 1 produced a spurious full-screen change, others nothing).
  The payload semantics are unknown and nothing in SimulatorKit calls it.
- GSEvent over `PurpleWorkspacePort`: `-[SimDevice lookup:error:]` does return a port, and `mach_msg` accepts a
  0x6c-byte message with id 0x7b, but without the real GSEvent record layout (not reconstructed; no Simulator.app
  in Xcode 27 to disassemble) a brute-force of event-type/orientation offsets (4 x 20 combinations) never rotated
  Safari. Continuing means reverse engineering backboardd's Purple event handler in the runtime dyld cache.
Geometry (`geometry.rs`) therefore still assumes portrait; nothing to change until rotation is possible.

## Measurements (iPhone 17, iOS 27.0, debug build)
| Operation | Time |
|---|---|
| `boot` from shut down (`bootstatus -b`) | 1.4 - 2.4 s |
| `boot` when already booted | 0.2 s |
| `shutdown` | 3.1 s (0.1 s if already down) |
| `install` of a small re-signed system app copy | 3.6 s |
| `launch` | 0.3 - 0.5 s |
| `send_input(Lock)` / `VolumeUp` / `VolumeDown` | 61 - 67 ms (60 ms is the key hold) |
| `send_text`, 28 characters with shifted symbols | 396 ms (~14 ms per character) |
| `list_all` (22 simulators, 11 device types) | ~0.2 s (2 simctl calls + one plutil per device type) |

## Gaps
- Rotation (above). Non-ASCII text (would need the pasteboard + paste, or an input method).
- simctl backend: no lock, volume, text.
- Android key mappings for the new buttons were not exercised on a live emulator.
- `Device::boot` on an open device does not refresh `info().state`.

## Credits
Learned from, not copied: idb (`FBSimulatorIndigoHID` button/source constants and the Indigo message idea),
baguette and SimView (HID client usage). The keyboard/consumer-page signatures above were derived by
disassembling SimulatorKit with `llvm-objdump` and confirmed on a running simulator.
