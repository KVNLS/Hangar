//! Emulator gRPC client (generated from the vendored `emulator_controller.proto`).

use std::time::Duration;

use tonic::service::Interceptor;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint};

use crate::{Error, Result};

#[allow(clippy::all, clippy::pedantic, unreachable_pub, missing_docs)]
pub(crate) mod proto {
    include!(concat!(env!("OUT_DIR"), "/android.emulation.control.rs"));
}

use proto::emulator_controller_client::EmulatorControllerClient;

/// Adds `authorization: Bearer <token>` (emulator started with `-grpc-use-token`).
#[derive(Clone)]
pub(crate) struct Auth {
    header: Option<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>,
}

impl Interceptor for Auth {
    fn call(&mut self, mut req: tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> {
        if let Some(h) = &self.header {
            req.metadata_mut().insert("authorization", h.clone());
        }
        Ok(req)
    }
}

pub(crate) type Client = EmulatorControllerClient<InterceptedService<Channel, Auth>>;

/// Frames are ~14 MB (1280x2856 RGBA); tonic's default limit is 4 MB.
const MAX_MESSAGE: usize = 64 * 1024 * 1024;

/// Connects to `127.0.0.1:<port>`. Must run inside a tokio runtime.
pub(crate) async fn connect(port: u16, token: Option<&str>) -> Result<Client> {
    let header = token
        .map(|t| {
            format!("Bearer {t}")
                .parse()
                .map_err(|e| Error::Parse(format!("gRPC token: {e}")))
        })
        .transpose()?;
    let channel = Endpoint::from_shared(format!("http://127.0.0.1:{port}"))
        .map_err(|e| Error::Parse(format!("gRPC endpoint: {e}")))?
        .connect_timeout(Duration::from_secs(3))
        .tcp_nodelay(true)
        .connect()
        .await
        .map_err(|e| Error::Command(format!("gRPC connect 127.0.0.1:{port}: {e}")))?;
    Ok(
        EmulatorControllerClient::with_interceptor(channel, Auth { header })
            .max_decoding_message_size(MAX_MESSAGE)
            .max_encoding_message_size(MAX_MESSAGE),
    )
}

pub(crate) fn status_err(what: &str, s: &tonic::Status) -> Error {
    match s.code() {
        tonic::Code::Unauthenticated | tonic::Code::PermissionDenied => {
            Error::Permission(format!("{what}: {}", s.message()))
        }
        _ => Error::Command(format!("{what}: {} ({:?})", s.message(), s.code())),
    }
}

/// Converts an emulator RGBA8888 image (rows top to bottom, measured on emulator 36.5) to packed
/// BGRA. Returns `None` if the buffer is too short.
pub fn rgba_to_bgra(mut rgba: Vec<u8>, width: u32, height: u32) -> Option<Vec<u8>> {
    let len = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    if rgba.len() < len {
        return None;
    }
    rgba.truncate(len);
    rgba_to_bgra_in_place(&mut rgba);
    Some(rgba)
}

/// Swaps R and B and makes every pixel opaque. Works on two pixels per step: ~8x faster than a
/// per-byte loop in unoptimised builds (13 ms vs 100 ms for a 1280x2856 frame).
pub fn rgba_to_bgra_in_place(buf: &mut [u8]) {
    const G_A: u64 = 0xff00_ff00_ff00_ff00;
    const R_B: u64 = 0x0000_00ff_0000_00ff;
    const OPAQUE: u64 = 0xff00_0000_ff00_0000;
    let (pairs, rest) = buf.as_chunks_mut::<8>();
    for w in pairs {
        let v = u64::from_le_bytes(*w);
        *w = ((v & G_A) | ((v >> 16) & R_B) | ((v & R_B) << 16) | OPAQUE).to_le_bytes();
    }
    if let Some(px) = rest.first_chunk_mut::<4>() {
        px.swap(0, 2);
        px[3] = 0xff;
    }
}

/// W3C `KeyboardEvent.key` values the emulator maps to Android buttons.
pub mod keys {
    pub const HOME: &str = "GoHome";
    pub const BACK: &str = "GoBack";
    pub const POWER: &str = "Power";
    pub const VOLUME_UP: &str = "AudioVolumeUp";
    pub const VOLUME_DOWN: &str = "AudioVolumeDown";
    pub const APP_SWITCH: &str = "AppSwitch";

    /// W3C `key` name of a keyboard key.
    pub fn named(key: crate::Key) -> &'static str {
        use crate::Key;
        match key {
            Key::Backspace => "Backspace",
            Key::Enter => "Enter",
            Key::Tab => "Tab",
            Key::Escape => "Escape",
            Key::ArrowUp => "ArrowUp",
            Key::ArrowDown => "ArrowDown",
            Key::ArrowLeft => "ArrowLeft",
            Key::ArrowRight => "ArrowRight",
        }
    }
}

pub(crate) fn key_press(key: &str) -> proto::KeyboardEvent {
    proto::KeyboardEvent {
        event_type: proto::keyboard_event::KeyEventType::Keypress as i32,
        key: key.to_owned(),
        ..Default::default()
    }
}

pub(crate) fn text_event(text: &str) -> proto::KeyboardEvent {
    proto::KeyboardEvent {
        event_type: proto::keyboard_event::KeyEventType::Keypress as i32,
        text: text.to_owned(),
        ..Default::default()
    }
}

/// Pressure and contact size the emulator itself uses when it turns a mouse click into a touch
/// (seen with `getevent`), so injected touches look like ordinary finger contacts.
const PRESSURE: i32 = 0x400;
const CONTACT: i32 = 0x500;

/// One finger (identifier 0) at device pixels; `down == false` lifts it (pressure 0).
pub(crate) fn touch(x: i32, y: i32, down: bool) -> proto::TouchEvent {
    proto::TouchEvent {
        touches: vec![proto::Touch {
            x,
            y,
            identifier: 0,
            pressure: if down { PRESSURE } else { 0 },
            touch_major: if down { CONTACT } else { 0 },
            touch_minor: if down { CONTACT } else { 0 },
            expiration: proto::touch::EventExpiration::NeverExpire as i32,
            ..Default::default()
        }],
        display: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_rgba_to_bgra() {
        let rgba = vec![1, 2, 3, 0, 10, 20, 30, 40, 99];
        let bgra = rgba_to_bgra(rgba.clone(), 2, 1).unwrap();
        assert_eq!(bgra, vec![3, 2, 1, 255, 30, 20, 10, 255]);
        assert!(rgba_to_bgra(rgba, 3, 1).is_none());
        // Three pixels: one two-pixel step plus a single trailing pixel.
        let rgba: Vec<u8> = (0..12).collect();
        assert_eq!(
            rgba_to_bgra(rgba, 3, 1).unwrap(),
            vec![2, 1, 0, 255, 6, 5, 4, 255, 10, 9, 8, 255]
        );
    }

    #[test]
    fn builds_events() {
        let t = touch(10, 20, true);
        assert_eq!(t.touches[0].pressure, PRESSURE);
        assert_eq!(touch(10, 20, false).touches[0].pressure, 0);
        assert_eq!(key_press(keys::BACK).key, "GoBack");
        assert_eq!(
            key_press(keys::named(crate::Key::Backspace)).key,
            "Backspace"
        );
        assert_eq!(text_event("hi").text, "hi");
    }
}
