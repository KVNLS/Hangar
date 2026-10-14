# Spike B — iOS Simulator frames + tap

Machine: macOS 27.0.1 arm64, Xcode 27.0, iPhone 17 simulator on iOS 27.0
(UDID 96D76D05-5A8F-483B-815A-DBBF1C32E4DB), Rust 1.99.0.

## How to run

```sh
xcrun simctl list devices                 # pick a UDID
xcrun simctl boot <UDID>                  # boot it
xcrun simctl bootstatus <UDID> -b         # wait until it has booted

cargo run -p hangar-examples --example spike_sim                      # private backend, first booted device
cargo run -p hangar-examples --example spike_sim -- --backend simctl --udid <UDID>
HANGAR_SIM_STATS=1 cargo run -p hangar-examples --example spike_sim   # fps / latency on stderr, once per second

# headless
cargo run -p hangar-examples --example spike_sim -- --bench 10 [--backend simctl] [--out DIR]
cargo run -p hangar-examples --example spike_sim -- --tap 339,422     # one tap (points), saves before/after PNGs
cargo run -p hangar-examples --example spike_sim -- --home            # Home button
cargo run -p hangar-examples --example sim_probe [-- <UDID>]          # dumps the private API surface + on-screen windows

cargo test -p hangar-devices -- --ignored                             # tests that need a booted simulator
xcrun simctl shutdown <UDID>
```

Permissions:
- Private backend: none. Simulator.app / Device Hub does not need to be open.
- simctl backend frames: none.
- simctl backend taps: Accessibility permission for the process that posts CGEvents (the terminal running
  cargo), a visible Simulator.app window with bezels hidden, and Simulator must be frontmost for Home (⇧⌘H).
  On Xcode 27 this path does not work, see below.

## Design

- `hangar-devices` is headless. `Device` trait: `info()`, `backend()`, `frames()` (a `tokio::sync::watch`
  receiver holding the latest `Frame`, so slow consumers skip frames instead of queueing), `send_input(Input)`
  with `Input::{Tap, TouchDown, TouchMove, TouchUp, Home}` in device points. `boot`, `install`, `launch`,
  `logs` return an explicit `Error::Unsupported` for now (deliberate, not a silent stub; to be listed in PLAN.md).
- Frame: `width`, `height`, `stride`, `bgra: Arc<[u8]>`, `captured_at: Instant`, `seq`.
- Listing: `xcrun simctl list devices booted -j` (serde_json). Screen size/scale comes from the device type's
  `capabilities.plist` (`simctl list devicetypes -j` → bundle path → `plutil -convert json`), e.g. iPhone 17 =
  1206×2622 px @3x = 402×874 pt.
- Everything iOS-specific is under `#[cfg(target_os = "macos")]` (`devices::ios`); the JSON parsing and
  geometry modules compile and are tested everywhere.
- Private backend runs two dedicated threads (capture, HID) that own all ObjC objects; the rest of the crate
  only sees channels. The GUI never blocks: frames are converted to a GPUI `RenderImage` on the background
  executor, taps are sent from the background executor.
- View (`examples/examples/spike_sim/view.rs`): sidebar with the booted list, a `canvas` that paints the latest
  `RenderImage` with `Window::paint_image` (contain-fit), previous images are freed with `Window::drop_image`
  (otherwise every frame leaks an atlas texture). Click → `geometry::view_to_device` (letterbox-aware) →
  `Input::Tap` in points; the backend converts points to unit coordinates.

## Private-API surface used (Xcode 27.0)

Frameworks (dlopen, never linked):
- `/Library/Developer/PrivateFrameworks/CoreSimulator.framework/CoreSimulator`
- SimulatorKit moved in Xcode 27: `Xcode.app/Contents/SharedFrameworks/SimulatorKit.framework` (was
  `Contents/Developer/Library/PrivateFrameworks/` up to Xcode 26). Both paths are probed.

Frames:
- `+[SimServiceContext sharedServiceContextForDeveloperDir:error:]`, `-defaultDeviceSetWithError:`,
  `-[SimDeviceSet devices]`, `-[SimDevice UDID]`, `-[SimDevice io]` (→ `SimDeviceIOClient`), `-ioPorts`.
- Each port is a ROCKit remote proxy (`ROCKRemoteProxy-…-SimDeviceIOPortInterface`) that forwards over XPC to
  CoreSimulatorService. Its methods are not on the class, so objc2's debug signature check (`msg_send!`)
  panics; these calls go through raw `objc_msgSend` casts after `respondsToSelector:`.
- `-portIdentifier` == `com.apple.framebuffer.display` → `-descriptor` (a `SimScreen` proxy implementing
  `SimDisplayIOSurfaceRenderable`) → `-framebufferSurface` (IOSurface, BGRA, 1206×2622).
- `-registerScreenCallbacksWithUUID:callbackQueue:frameCallback:surfacesChangedCallback:propertiesChangedCallback:`
  / `-unregisterScreenCallbacksWithUUID:`. ROCKit serialises the blocks and asserts they carry a type signature
  (`rock_blockSignature`, ROCKBlock.h:135), so blocks are created with `block2::RcBlock::with_encoding`
  (`v8@?0`). The queue is a `dispatch_queue_create` serial queue (libSystem, no extra crate).
- On a callback: compare `IOSurfaceGetSeed`, lock read-only, copy rows into a packed BGRA buffer, unlock.
  The current surface is published once at start because an idle screen produces no callbacks.

Touch / buttons:
- `_TtC12SimulatorKit24SimDeviceLegacyHIDClient` (Swift class, ObjC-exposed): `alloc` +
  `initWithDevice:error:`, and `sendWithMessage:freeWhenDone:completionQueue:completion:`
  (type `v@:^{IndigoHIDMessageStruct…}B@@?`); the completion is an `NSError *` block (`v16@?0@"NSError"8`).
- `IndigoHIDMessageForMouseNSEvent(CGPoint *p1, CGPoint *p2, IndigoHIDTarget target, NSEventType type,
  NSSize size, IndigoHIDEdge edge)`. Verified by disassembly of Xcode 27's SimulatorKit (`otool -tV`):
  x0/x1 points, w2 target, x3 event type, x4 edge (bounds-checked `cmp x24, #0x4`), d0/d1 size (the point is
  divided by it). Called with unit coordinates, size (1,1), target 0x32, type 1 (down) / 6 (dragged) / 2 (up),
  edge 0.
- `IndigoHIDMessageForButton(source, direction, target)`: Home = (0, 1 then 2, 0x33).
- All of this is plain C functions + ObjC messages: no Swift-only types, no C++ ABI. **No helper is needed.**

About the "5-argument call broke in Xcode 26" report: the function always had 6 logical arguments (the NSSize
is passed in two FP registers, so on arm64 integer and FP arguments are counted separately). Code that passed a
"direction" value in the 5th integer slot was actually setting `IndigoHIDEdge` (1 = left, 2 = top …), which
explains taps turning into edge/Home gestures. Baguette switched to building IOHIDEvent digitizer events and
wrapping them with `IndigoHIDMessageForTrackpadEventFromHIDEventRef` plus byte patches; SimView and current
idb call the same builder with edge = 0. The simple call with edge = 0 works on Xcode 27 / iOS 27 (verified
below), so the byte-patching path was not needed.

## What worked / what did not

| | private | simctl |
|---|---|---|
| list booted devices + screen spec | yes (shared) | yes (shared) |
| frames | yes, event-driven, ~60 fps | yes, polled, ~2 fps |
| tap | **yes, verified** | **no on this machine** (see below) |
| Home | yes | ⇧⌘H to Simulator.app only (untested, see below) |

Tap verification (private): on the home screen, `--tap 339,422` (Settings icon, points) → frame diff 91.9 %
after 1.5 s; an independent `xcrun simctl io … screenshot` afterwards showed the Settings app (Apple Account,
General, Accessibility …). The bench repeats this 3 times and saves `tap{N}_before/after.png`; all three runs
opened Settings. Swipes (TouchDown/Move/Up) page the home screen; Home returns to the home screen.

simctl backend input does **not** work here, for three independent reasons:
1. Xcode 27 ships no Simulator.app. Devices are shown inside Device Hub
   (`Xcode.app/Contents/Applications/DeviceHub.app`, window owner "Device Hub"), whose device screen sits inside
   a larger window with its own sidebar/toolbar; its position cannot be derived from `CGWindowListCopyWindowInfo`
   alone. The code returns `Error::Unsupported` for Device Hub windows instead of guessing.
2. This process has no Accessibility permission (`CGPreflightPostEventAccess() == false`) → `Error::Permission`.
3. Even where it works (Xcode ≤ 26, bezels hidden, window visible), it moves the user's real pointer and needs
   the window unobscured. It is not a usable product path.
The window-to-point mapping for bezel-less Simulator.app windows is implemented and unit-tested, but was not
exercised end-to-end on a real window.

Other Xcode 27 findings: `simctl io <udid> screenshot --type=png -` no longer writes to stdout (it creates a
file named `-`), so the fallback writes to a temp file.

## Measurements

All numbers from iPhone 17 / iOS 27.0, debug build (dependencies at opt-level 2), Apple Silicon.

Method (`--bench N`):
- fps: consumer polls the frame channel for N s; "received" = distinct frames seen, "content-changed" =
  frames whose sampled pixels differ from the previous one. Animation is produced by private-HID horizontal
  swipes paging the home screen continuously (used for both backends, since simctl has no input).
- capture→consumer latency: `Instant` at IOSurface lock (private) or at the start of the `simctl` call
  (simctl) → time the consumer reads it.
- tap→first changed frame: wait for a stable home screen, send `Tap` on the Settings icon at t0, watch for the
  first frame captured after t0 that differs > 0.2 % from the baseline (the icon highlight / launch zoom).
- GUI stats (`HANGAR_SIM_STATS=1`): received = frames converted to `RenderImage`, painted = distinct frames
  painted, latency = capture → paint callback.

| | private | simctl |
|---|---|---|
| idle (no change) | 0.1 fps (callbacks only on change) | 2.0 fps (each poll = new frame) |
| while swiping, received | 53–62 fps (3 runs, 5–10 s) | 2.0–2.1 fps |
| while swiping, content-changed | 50–59 fps | 1.9 fps |
| capture→consumer p50 / p95 | 1.5 / 2.3 ms | ~505 / ~525 ms (one screenshot round trip) |
| tap call duration | 64–74 ms (includes 60 ms hold) | n/a |
| tap → first changed frame captured | 12.7–26.8 ms (6 runs) | not measurable (no tap) |
| GUI: received / painted fps | 58–68 / 56–60 fps while animating | not measured |
| GUI: capture→paint | avg ~8 ms, max 11–17 ms | not measured |

The GUI row comes from one interactive run on 2026-10-03 with the display awake (before a fix that counts each
painted frame once; painted ≤ received in that run anyway). A rerun on 2026-10-04 printed no stats because the
display was asleep (`screencapture` failed), so GPUI did not paint; worth re-checking with the screen on.

## Licences and credits (learned from, no code copied)

- **baguette** (tddworks/baguette, Apache-2.0): framebuffer approach (`io` → `ioPorts` → port
  `com.apple.framebuffer.display` → `descriptor.framebufferSurface` + `registerScreenCallbacks…`), the
  SimulatorKit path moving to `SharedFrameworks` in Xcode 27, `SimDeviceLegacyHIDClient` +
  `sendWithMessage:freeWhenDone:completionQueue:completion:`, the Home button codes, and the report that
  mouse-event taps broke in Xcode 26 (with their IOHIDEvent/trackpad workaround).
- **SimView** (ToolingTools/SimView, Apache-2.0): the same framebuffer callbacks plus seed-based change
  detection; the `IndigoHIDMessageForMouseNSEvent` call shape with unit coordinates, target 0x32 and edge 0.
- **idb / FBSimulatorControl** (facebook/idb, MIT): the documented builder signature
  `(CGPoint*, CGPoint*, IndigoHIDTarget, NSEventType, NSSize, IndigoHIDEdge)` and the meaning of the edge
  argument.
Everything was checked against Xcode 27 on this machine (ObjC runtime dump via `sim_probe`, `nm -gU`,
`otool -tV`) before use.

## Risks

- Private API drift every Xcode release (SimulatorKit already moved in 27; ROCKit block-signature checks are
  new). `sim_probe` makes a re-check quick; the backend fails with explicit errors and the app can fall back
  to simctl frames.
- `IndigoHIDMessageForMouseNSEvent` is a mouse-to-touch translation; multi-touch and edge gestures need the
  edge argument or the IOHIDEvent path (baguette). Not needed for taps.
- Distribution: dlopen of Xcode-internal frameworks from a signed/notarised app (hardened runtime, library
  validation) is untested (PLAN open question 6).
- Frame copy: 1206×2622×4 ≈ 12.6 MB per frame, copied once from the IOSurface and once into the
  `RenderImage` (~1.5 GB/s at 60 fps). Fine for a spike; production should try zero-copy
  (GPUI's macOS `paint_surface(CVPixelBuffer)` exists but needs a CoreVideo binding, i.e. a new dependency).
- simctl fallback is 2 fps with ~0.5 s latency and has no input on Xcode 27: it is a "view only" fallback.

## Recommendation

- Use the private backend as the primary path, in-process Rust via objc2 / dlopen. **No Swift/ObjC helper is
  needed** for frames or taps on Xcode 27.
- Keep simctl as a view-only fallback (frames + explicit `Unsupported`/`Permission` errors for input). Do not
  invest in CGEvent clicking: Xcode 27 removed Simulator.app and the approach hijacks the user's mouse.
- Next steps for M5: zero-copy frames (decide on a CoreVideo dependency), drag/multi-touch, hardware buttons,
  keyboard, rotation, and listing the `Unsupported` methods in PLAN.md.
