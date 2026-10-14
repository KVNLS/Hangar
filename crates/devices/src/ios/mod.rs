//! iOS Simulator backends (macOS only).

mod dispatch;
mod introspect;
mod private;
mod runtime;
mod sim_window;
mod simctl_backend;

pub use introspect::introspect;
pub use private::PrivateSimulator;
pub use sim_window::{SimWindow, on_screen_windows, simulator_windows};
pub use simctl_backend::SimctlSimulator;

use crate::{Device, DeviceInfo, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// CoreSimulator framebuffer + SimulatorKit Indigo HID (private APIs).
    Private,
    /// `simctl io screenshot` polling + CGEvent clicks into Simulator.app (public APIs).
    Simctl,
}

impl std::str::FromStr for Backend {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "private" => Ok(Self::Private),
            "simctl" => Ok(Self::Simctl),
            other => Err(format!(
                "unknown backend {other:?} (expected private|simctl)"
            )),
        }
    }
}

pub fn open(info: DeviceInfo, backend: Backend) -> Result<Box<dyn Device>> {
    Ok(match backend {
        Backend::Private => Box::new(PrivateSimulator::open(info)?),
        Backend::Simctl => Box::new(SimctlSimulator::open(info)?),
    })
}
