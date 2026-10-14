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
pub fn rgba_to_bgra(rgba: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let len = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    let src = rgba.get(..len)?;
    let mut out = src.to_vec();
    for px in out.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
        px[3] = 0xff;
    }
    Some(out)
}

/// Linux evdev key codes understood by `sendKey` with `KeyCodeType::Evdev`.
pub mod evdev {
    /// `KEY_HOMEPAGE`, mapped to Android's HOME by the emulator's key layout.
    pub const HOME: i32 = 172;
    /// `KEY_BACK`.
    pub const BACK: i32 = 158;
}

pub(crate) fn key_press(code: i32) -> proto::KeyboardEvent {
    proto::KeyboardEvent {
        code_type: proto::keyboard_event::KeyCodeType::Evdev as i32,
        event_type: proto::keyboard_event::KeyEventType::Keypress as i32,
        key_code: code,
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

/// One finger (identifier 0) at device pixels; `down == false` lifts it (pressure 0).
pub(crate) fn touch(x: i32, y: i32, down: bool) -> proto::TouchEvent {
    proto::TouchEvent {
        touches: vec![proto::Touch {
            x,
            y,
            identifier: 0,
            pressure: i32::from(down),
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
        let rgba = [1, 2, 3, 0, 10, 20, 30, 40, 99];
        let bgra = rgba_to_bgra(&rgba, 2, 1).unwrap();
        assert_eq!(bgra, vec![3, 2, 1, 255, 30, 20, 10, 255]);
        assert!(rgba_to_bgra(&rgba, 3, 1).is_none());
    }

    #[test]
    fn builds_events() {
        let t = touch(10, 20, true);
        assert_eq!(t.touches[0].pressure, 1);
        assert_eq!(touch(10, 20, false).touches[0].pressure, 0);
        assert_eq!(key_press(evdev::BACK).key_code, 158);
        assert_eq!(text_event("hi").text, "hi");
    }
}
