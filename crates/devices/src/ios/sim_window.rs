//! Clicking into the Simulator.app window that shows a device (public APIs only).

use crate::{DeviceInfo, Error, Input, Result};

pub(crate) fn click(_info: &DeviceInfo, _input: Input) -> Result<()> {
    Err(Error::Unsupported("simctl backend input"))
}
