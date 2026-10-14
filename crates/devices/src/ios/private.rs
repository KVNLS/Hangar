//! Private backend: frames from the simulator's framebuffer IOSurface (CoreSimulator IO ports) and
//! touches through SimulatorKit's Indigo HID client. Every ObjC object lives on the thread that
//! created it; the rest of the crate talks to those threads through channels.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use block2::{ManualBlockEncoding, RcBlock};
use objc2::rc::{Allocated, Retained, autoreleasepool};
use objc2::runtime::{AnyObject, Sel};
use objc2::{msg_send, sel};
use objc2_core_foundation::{CGPoint, CGSize};
use objc2_foundation::{NSArray, NSError, NSUUID};
use objc2_io_surface::{IOSurfaceLockOptions, IOSurfaceRef};

use super::dispatch;
use super::runtime::{self, raw, responds};
use crate::{
    Device, DeviceInfo, Error, Frame, FrameReceiver, FrameSender, Input, Result, geometry,
};

const FRAMEBUFFER_PORT: &str = "com.apple.framebuffer.display";
const HID_CLIENT: &std::ffi::CStr = c"_TtC12SimulatorKit24SimDeviceLegacyHIDClient";
/// Polling floor in case a damage callback is missed; cheap because it only compares the seed.
const IDLE_POLL: Duration = Duration::from_millis(100);
const PIXEL_FORMAT_BGRA: u32 = u32::from_be_bytes(*b"BGRA");
const TAP_HOLD: Duration = Duration::from_millis(60);
const HID_SEND_TIMEOUT: Duration = Duration::from_secs(2);

/// `IndigoHIDTarget` of the main touch digitizer (same constant idb, SimView and baguette use).
const TOUCH_TARGET: u32 = 0x32;
/// NSEventType values the message builder understands.
const NS_LEFT_MOUSE_DOWN: usize = 1;
const NS_LEFT_MOUSE_UP: usize = 2;
const NS_LEFT_MOUSE_DRAGGED: usize = 6;
const EDGE_NONE: usize = 0;

/// `IndigoHIDMessageForMouseNSEvent(CGPoint *p1, CGPoint *p2, IndigoHIDTarget, NSEventType,
/// NSSize, IndigoHIDEdge)`. Verified by disassembly on Xcode 27: x0/x1 points, w2 target,
/// x3 event type, d0/d1 size (point is divided by it), x4 edge (0..=4).
type MouseEventFn =
    unsafe extern "C" fn(*const CGPoint, *const CGPoint, u32, usize, CGSize, usize) -> *mut c_void;

/// `IndigoHIDMessageForButton(source, direction, target)`; three 32-bit values stored verbatim.
type ButtonFn = unsafe extern "C" fn(u32, u32, u32) -> *mut c_void;
const BUTTON_HOME: u32 = 0;
const BUTTON_DOWN: u32 = 1;
const BUTTON_UP: u32 = 2;
const BUTTON_TARGET: u32 = 0x33;

type SendMessageFn = unsafe extern "C-unwind" fn(
    *const AnyObject,
    Sel,
    *mut c_void,
    bool,
    *const AnyObject,
    *const AnyObject,
);

/// ROCKit serialises blocks passed to remote proxies and asserts they carry a signature.
struct VoidBlock;
// SAFETY: `void (^)(void)` on 64-bit.
unsafe impl ManualBlockEncoding for VoidBlock {
    type Arguments = ();
    type Return = ();
    const ENCODING_CSTR: &'static std::ffi::CStr = c"v8@?0";
}

struct ErrorBlock;
// SAFETY: `void (^)(NSError *)` on 64-bit.
unsafe impl ManualBlockEncoding for ErrorBlock {
    type Arguments = (*mut NSError,);
    type Return = ();
    const ENCODING_CSTR: &'static std::ffi::CStr = c"v16@?0@\"NSError\"8";
}

fn void_block(f: impl Fn() + 'static) -> RcBlock<dyn Fn()> {
    RcBlock::with_encoding::<_, _, _, VoidBlock>(f)
}

pub struct PrivateSimulator {
    info: DeviceInfo,
    frames: FrameReceiver,
    capture_cmd: Sender<CaptureEvent>,
    hid_cmd: Sender<HidCommand>,
    threads: Vec<JoinHandle<()>>,
}

enum CaptureEvent {
    Damage,
    SurfacesChanged,
    Stop,
}

struct HidCommand {
    input: Input,
    reply: Sender<Result<()>>,
}

impl PrivateSimulator {
    pub fn open(info: DeviceInfo) -> Result<Self> {
        runtime::frameworks()?;
        let (frame_tx, frames) = tokio::sync::watch::channel(None);

        let (capture_cmd, capture_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let udid = info.udid.clone();
        let damage_tx = capture_cmd.clone();
        let capture = std::thread::Builder::new()
            .name("sim-capture".into())
            .spawn(move || capture_thread(&udid, frame_tx, damage_tx, capture_rx, ready_tx))?;
        recv_ready(&ready_rx)?;

        let screen = info
            .screen
            .ok_or_else(|| Error::PrivateApi("screen size unknown; cannot map touches".into()))?;
        let (hid_cmd, hid_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let udid = info.udid.clone();
        let hid = std::thread::Builder::new()
            .name("sim-hid".into())
            .spawn(move || {
                hid_thread(
                    &udid,
                    (screen.width_pt(), screen.height_pt()),
                    hid_rx,
                    ready_tx,
                )
            })?;
        recv_ready(&ready_rx)?;

        Ok(Self {
            info,
            frames,
            capture_cmd,
            hid_cmd,
            threads: vec![capture, hid],
        })
    }
}

fn recv_ready(rx: &Receiver<Result<()>>) -> Result<()> {
    rx.recv_timeout(Duration::from_secs(10))
        .map_err(|_| Error::PrivateApi("backend thread did not start".into()))?
}

impl Device for PrivateSimulator {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn backend(&self) -> &'static str {
        "private"
    }

    fn frames(&self) -> FrameReceiver {
        self.frames.clone()
    }

    /// Blocks until the HID messages were delivered to the simulator (a tap holds ~60 ms).
    fn send_input(&self, input: Input) -> Result<()> {
        let (reply, rx) = mpsc::channel();
        self.hid_cmd
            .send(HidCommand { input, reply })
            .map_err(|_| Error::PrivateApi("HID thread stopped".into()))?;
        rx.recv()
            .map_err(|_| Error::PrivateApi("HID thread stopped".into()))?
    }
}

impl Drop for PrivateSimulator {
    fn drop(&mut self) {
        let _ = self.capture_cmd.send(CaptureEvent::Stop);
        // Closing the HID channel ends its loop.
        let (dead, _) = mpsc::channel();
        self.hid_cmd = dead;
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Frames

struct Framebuffer {
    descriptors: Vec<Retained<AnyObject>>,
    registrations: Vec<(Retained<AnyObject>, Retained<NSUUID>)>,
    // Blocks and queue must outlive the registrations.
    _blocks: Vec<RcBlock<dyn Fn()>>,
    _queue: Retained<AnyObject>,
    surface: Option<Retained<AnyObject>>,
    last_seed: Option<u32>,
    seq: u64,
    warned_format: bool,
}

fn capture_thread(
    udid: &str,
    frames: FrameSender,
    events_tx: Sender<CaptureEvent>,
    events: Receiver<CaptureEvent>,
    ready: Sender<Result<()>>,
) {
    let mut fb = match autoreleasepool(|_| setup_framebuffer(udid, &events_tx)) {
        Ok(fb) => {
            let _ = ready.send(Ok(()));
            fb
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    drop(events_tx);
    // An idle screen produces no damage callbacks, so publish the current content right away.
    autoreleasepool(|_| {
        fb.refresh_surface();
        if let Some(frame) = fb.capture() {
            frames.send_replace(Some(frame));
        }
    });
    loop {
        let event = events.recv_timeout(IDLE_POLL);
        let mut refresh = matches!(event, Ok(CaptureEvent::SurfacesChanged));
        match event {
            Ok(CaptureEvent::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            _ => {}
        }
        // Coalesce queued callbacks into one capture.
        let mut stop = false;
        while let Ok(e) = events.try_recv() {
            match e {
                CaptureEvent::SurfacesChanged => refresh = true,
                CaptureEvent::Stop => stop = true,
                CaptureEvent::Damage => {}
            }
        }
        if stop {
            break;
        }
        autoreleasepool(|_| {
            if refresh || fb.surface.is_none() {
                fb.refresh_surface();
            }
            if let Some(frame) = fb.capture() {
                frames.send_replace(Some(frame));
            }
        });
    }
    autoreleasepool(|_| fb.unregister());
}

fn setup_framebuffer(udid: &str, events: &Sender<CaptureEvent>) -> Result<Framebuffer> {
    let device = runtime::sim_device(udid)?;
    runtime::require(&device, sel!(io))?;
    // SAFETY: `-[SimDevice io]` takes no arguments and returns the IO client.
    let io = unsafe { raw::id0(&device, sel!(io)) }
        .ok_or_else(|| Error::PrivateApi("device has no io client (not booted?)".into()))?;
    let descriptors = framebuffer_descriptors(&io)?;
    if descriptors.is_empty() {
        return Err(Error::PrivateApi(format!(
            "no {FRAMEBUFFER_PORT} port with a framebufferSurface"
        )));
    }
    let queue = dispatch::serial_queue(c"dev.hangar.sim.framebuffer")
        .ok_or_else(|| Error::PrivateApi("dispatch_queue_create failed".into()))?;
    let register = sel!(registerScreenCallbacksWithUUID:callbackQueue:frameCallback:surfacesChangedCallback:propertiesChangedCallback:);
    let mut blocks = Vec::new();
    let mut registrations = Vec::new();
    for desc in &descriptors {
        if !responds(desc, register) {
            tracing::warn!("framebuffer descriptor cannot register callbacks; polling only");
            continue;
        }
        let damage_tx = events.clone();
        let frame_cb = void_block(move || {
            let _ = damage_tx.send(CaptureEvent::Damage);
        });
        let surfaces_tx = events.clone();
        let surfaces_cb = void_block(move || {
            let _ = surfaces_tx.send(CaptureEvent::SurfacesChanged);
        });
        let props_cb = void_block(|| {});
        let uuid = NSUUID::new();
        // SAFETY: selector verified on Xcode 27 via respondsToSelector:; arguments are an NSUUID,
        // a dispatch queue and three `void (^)(void)` blocks, all kept alive in `Framebuffer`.
        unsafe {
            raw::void5(
                desc,
                register,
                [
                    Retained::as_ptr(&uuid).cast(),
                    Retained::as_ptr(&queue),
                    RcBlock::as_ptr(&frame_cb).cast_const().cast(),
                    RcBlock::as_ptr(&surfaces_cb).cast_const().cast(),
                    RcBlock::as_ptr(&props_cb).cast_const().cast(),
                ],
            );
        }
        blocks.extend([frame_cb, surfaces_cb, props_cb]);
        registrations.push((desc.clone(), uuid));
    }
    Ok(Framebuffer {
        descriptors,
        registrations,
        _blocks: blocks,
        _queue: queue,
        surface: None,
        last_seed: None,
        seq: 0,
        warned_format: false,
    })
}

fn framebuffer_descriptors(io: &AnyObject) -> Result<Vec<Retained<AnyObject>>> {
    runtime::require(io, sel!(ioPorts))?;
    // SAFETY: `-[SimDeviceIO ioPorts]` returns an NSArray of port proxies.
    let ports = unsafe { raw::id0(io, sel!(ioPorts)) }
        .ok_or_else(|| Error::PrivateApi("ioPorts returned nil".into()))?;
    // SAFETY: the object is an NSArray.
    let ports: Retained<NSArray<AnyObject>> = unsafe { Retained::cast_unchecked(ports) };
    let mut out = Vec::new();
    for port in ports.iter() {
        if runtime::string_property(&port, sel!(portIdentifier)).as_deref()
            != Some(FRAMEBUFFER_PORT)
        {
            continue;
        }
        if !responds(&port, sel!(descriptor)) {
            continue;
        }
        // SAFETY: `descriptor` takes no arguments and returns the descriptor proxy.
        let Some(desc) = (unsafe { raw::id0(&port, sel!(descriptor)) }) else {
            continue;
        };
        if responds(&desc, sel!(framebufferSurface)) {
            out.push(desc);
        }
    }
    Ok(out)
}

impl Framebuffer {
    /// Picks the largest display surface (the main screen; others are auxiliary planes).
    fn refresh_surface(&mut self) {
        let mut best: Option<(usize, Retained<AnyObject>)> = None;
        for desc in &self.descriptors {
            // SAFETY: `framebufferSurface` takes no arguments and returns an IOSurface (or nil).
            let Some(surface) = (unsafe { raw::id0(desc, sel!(framebufferSurface)) }) else {
                continue;
            };
            let s = as_iosurface(&surface);
            let area = s.width() * s.height();
            if area > 0 && best.as_ref().is_none_or(|(a, _)| area > *a) {
                best = Some((area, surface));
            }
        }
        self.surface = best.map(|(_, s)| s);
        self.last_seed = None;
    }

    fn capture(&mut self) -> Option<Frame> {
        let surface = self.surface.as_ref()?;
        let s = as_iosurface(surface);
        if self.last_seed == Some(s.seed()) {
            return None;
        }
        if s.pixel_format() != PIXEL_FORMAT_BGRA {
            if !self.warned_format {
                tracing::warn!(
                    "unexpected framebuffer pixel format {:#x}",
                    s.pixel_format()
                );
                self.warned_format = true;
            }
            return None;
        }
        let mut seed = 0u32;
        // SAFETY: read-only lock of a live IOSurface; `seed` is a valid out-pointer.
        if unsafe { s.lock(IOSurfaceLockOptions::ReadOnly, &mut seed) } != 0 {
            return None;
        }
        let captured_at = Instant::now();
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
        self.last_seed = Some(seed);
        self.seq += 1;
        Some(Frame {
            width: u32::try_from(width).ok()?,
            height: u32::try_from(height).ok()?,
            stride: row,
            bgra: Arc::from(bgra),
            captured_at,
            seq: self.seq,
        })
    }

    fn unregister(&mut self) {
        for (desc, uuid) in self.registrations.drain(..) {
            if responds(&desc, sel!(unregisterScreenCallbacksWithUUID:)) {
                // SAFETY: takes the NSUUID used at registration; returns void.
                unsafe {
                    raw::void1(
                        &desc,
                        sel!(unregisterScreenCallbacksWithUUID:),
                        Retained::as_ptr(&uuid).cast(),
                    );
                }
            }
        }
    }
}

fn as_iosurface(obj: &AnyObject) -> &IOSurfaceRef {
    // SAFETY: `framebufferSurface` returns an IOSurface, toll-free bridged with IOSurfaceRef.
    unsafe { &*(obj as *const AnyObject).cast::<IOSurfaceRef>() }
}

// ---------------------------------------------------------------------------------------------
// Touch input

struct Hid {
    client: Retained<AnyObject>,
    queue: Retained<AnyObject>,
    mouse_event: MouseEventFn,
    button: ButtonFn,
    screen_pt: (f64, f64),
}

fn hid_thread(
    udid: &str,
    screen_pt: (f64, f64),
    commands: Receiver<HidCommand>,
    ready: Sender<Result<()>>,
) {
    let hid = match autoreleasepool(|_| Hid::new(udid, screen_pt)) {
        Ok(h) => {
            let _ = ready.send(Ok(()));
            h
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    while let Ok(cmd) = commands.recv() {
        let result = autoreleasepool(|_| hid.handle(cmd.input));
        let _ = cmd.reply.send(result);
    }
}

impl Hid {
    fn new(udid: &str, screen_pt: (f64, f64)) -> Result<Self> {
        let fw = runtime::frameworks()?;
        let device = runtime::sim_device(udid)?;
        let cls = runtime::class(HID_CLIENT)?;
        // SAFETY: `+alloc` on an NSObject subclass.
        let allocated: Allocated<AnyObject> = unsafe { msg_send![cls, alloc] };
        // SAFETY: `-initWithDevice:error:` is ObjC-exposed by SimDeviceLegacyHIDClient
        // (type encoding `@@:@^@` on Xcode 27).
        let client: Retained<AnyObject> =
            unsafe { msg_send![allocated, initWithDevice: &*device, error: _] }
                .map_err(|e: Retained<NSError>| runtime::ns_error(&e))?;
        runtime::require(
            &client,
            sel!(sendWithMessage:freeWhenDone:completionQueue:completion:),
        )?;
        let sym = runtime::symbol(Some(fw.simulator_kit), "IndigoHIDMessageForMouseNSEvent")?;
        // SAFETY: signature documented on `MouseEventFn`.
        let mouse_event: MouseEventFn = unsafe { std::mem::transmute(sym) };
        let sym = runtime::symbol(Some(fw.simulator_kit), "IndigoHIDMessageForButton")?;
        // SAFETY: signature documented on `ButtonFn`.
        let button: ButtonFn = unsafe { std::mem::transmute(sym) };
        let queue = dispatch::serial_queue(c"dev.hangar.sim.hid")
            .ok_or_else(|| Error::PrivateApi("dispatch_queue_create failed".into()))?;
        Ok(Self {
            client,
            queue,
            mouse_event,
            button,
            screen_pt,
        })
    }

    fn handle(&self, input: Input) -> Result<()> {
        match input {
            Input::Tap { x, y } => {
                self.touch(x, y, NS_LEFT_MOUSE_DOWN)?;
                std::thread::sleep(TAP_HOLD);
                self.touch(x, y, NS_LEFT_MOUSE_UP)
            }
            Input::TouchDown { x, y } => self.touch(x, y, NS_LEFT_MOUSE_DOWN),
            Input::TouchMove { x, y } => self.touch(x, y, NS_LEFT_MOUSE_DRAGGED),
            Input::TouchUp { x, y } => self.touch(x, y, NS_LEFT_MOUSE_UP),
            Input::Home => {
                self.press(BUTTON_HOME, BUTTON_DOWN)?;
                std::thread::sleep(TAP_HOLD);
                self.press(BUTTON_HOME, BUTTON_UP)
            }
        }
    }

    fn press(&self, source: u32, direction: u32) -> Result<()> {
        // SAFETY: returns a malloc'd message.
        let msg = unsafe { (self.button)(source, direction, BUTTON_TARGET) };
        if msg.is_null() {
            return Err(Error::PrivateApi(
                "IndigoHIDMessageForButton returned NULL".into(),
            ));
        }
        self.send(msg)
    }

    fn touch(&self, x: f64, y: f64, event_type: usize) -> Result<()> {
        let (ux, uy) = geometry::device_to_unit(x, y, self.screen_pt.0, self.screen_pt.1);
        let point = CGPoint { x: ux, y: uy };
        let unit = CGSize {
            width: 1.0,
            height: 1.0,
        };
        // SAFETY: SimulatorKit returns a malloc'd message or NULL (drags are rate-limited to
        // ~60 Hz and return NULL in between).
        let msg = unsafe {
            (self.mouse_event)(
                &point,
                std::ptr::null(),
                TOUCH_TARGET,
                event_type,
                unit,
                EDGE_NONE,
            )
        };
        if msg.is_null() {
            if event_type == NS_LEFT_MOUSE_DRAGGED {
                return Ok(());
            }
            return Err(Error::PrivateApi(
                "IndigoHIDMessageForMouseNSEvent returned NULL".into(),
            ));
        }
        self.send(msg)
    }

    fn send(&self, msg: *mut c_void) -> Result<()> {
        let (done_tx, done_rx) = mpsc::channel::<Option<String>>();
        let completion: RcBlock<dyn Fn(*mut NSError)> =
            RcBlock::with_encoding::<_, _, _, ErrorBlock>(move |err: *mut NSError| {
                // SAFETY: SimulatorKit passes a valid NSError or nil.
                let msg = unsafe { err.as_ref() }.map(|e| e.localizedDescription().to_string());
                let _ = done_tx.send(msg);
            });
        let sel = sel!(sendWithMessage:freeWhenDone:completionQueue:completion:);
        // SAFETY: method type `v@:^{IndigoHIDMessageStruct=…}B@@?` on Xcode 27. With
        // freeWhenDone=YES SimulatorKit takes ownership of the malloc'd message.
        unsafe {
            let f: SendMessageFn =
                std::mem::transmute(objc2::ffi::objc_msgSend as unsafe extern "C-unwind" fn());
            f(
                &*self.client,
                sel,
                msg,
                true,
                Retained::as_ptr(&self.queue),
                RcBlock::as_ptr(&completion).cast_const().cast(),
            );
        }
        match done_rx.recv_timeout(HID_SEND_TIMEOUT) {
            Ok(None) => Ok(()),
            Ok(Some(e)) => Err(Error::PrivateApi(format!("HID send failed: {e}"))),
            Err(_) => Err(Error::PrivateApi("HID send timed out".into())),
        }
    }
}
