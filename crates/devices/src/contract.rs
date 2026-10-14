//! The capabilities contract: a capability a device does not declare must fail with
//! [`Error::Unsupported`], so callers can trust the flags instead of trying. Only absent
//! capabilities are exercised (no device is booted, rotated or shut down), so it is safe to run
//! against a real device.

use crate::{Device, Error, Input, Key, LogFilter};

/// Each undeclared capability whose call did not fail with `Unsupported`.
pub fn violations(device: &dyn Device) -> Vec<&'static str> {
    let caps = device.capabilities();
    let mut found = Vec::new();
    let mut check = |declared: bool, name: &'static str, call: &dyn Fn() -> crate::Result<()>| {
        if !declared && !matches!(call(), Err(Error::Unsupported(_))) {
            found.push(name);
        }
    };
    check(caps.text, "text", &|| device.send_text(""));
    check(caps.keys, "keys", &|| {
        device.send_input(Input::Key(Key::Escape))
    });
    check(caps.side_buttons, "side_buttons", &|| {
        device.send_input(Input::VolumeDown)
    });
    check(caps.navigation_buttons, "navigation_buttons", &|| {
        device.send_input(Input::Back)
    });
    check(caps.rotate, "rotate", &|| device.orientation().map(drop));
    check(caps.boot, "boot", &|| device.boot());
    check(caps.shutdown, "shutdown", &|| device.shutdown());
    check(caps.install, "install", &|| {
        device.install(std::path::Path::new("/nonexistent/hangar-contract.app"))
    });
    check(caps.launch, "launch", &|| {
        device.launch("hangar.contract.missing")
    });
    check(caps.logs, "logs", &|| {
        device.logs(&LogFilter::default()).map(drop)
    });
    check(caps.log_history, "log_history", &|| {
        let filter = LogFilter {
            history: true,
            ..LogFilter::default()
        };
        device.logs(&filter).map(drop)
    });
    found
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::{Capabilities, DeviceInfo, FrameReceiver, Result};

    /// Implements only what `Device` requires; `accepts_keys` lies about one input.
    struct Bare {
        info: DeviceInfo,
        accepts_keys: bool,
        sent: Mutex<Vec<Input>>,
    }

    impl Device for Bare {
        fn info(&self) -> &DeviceInfo {
            &self.info
        }
        fn backend(&self) -> &'static str {
            "bare"
        }
        fn frames(&self) -> FrameReceiver {
            tokio::sync::watch::channel(None).1
        }
        fn send_input(&self, input: Input) -> Result<()> {
            if !self.accepts_keys || !matches!(input, Input::Key(_)) {
                return Err(Error::Unsupported("bare input"));
            }
            self.sent
                .lock()
                .map_err(|_| Error::Unsupported("poisoned"))?
                .push(input);
            Ok(())
        }
    }

    fn bare(accepts_keys: bool) -> Bare {
        Bare {
            info: DeviceInfo {
                udid: "bare".into(),
                name: "Bare".into(),
                runtime: String::new(),
                state: String::new(),
                device_type: None,
                screen: None,
            },
            accepts_keys,
            sent: Mutex::default(),
        }
    }

    #[test]
    fn the_trait_defaults_declare_nothing_and_support_nothing() {
        let device = bare(false);
        assert_eq!(device.capabilities(), Capabilities::default());
        assert!(violations(&device).is_empty());
    }

    #[test]
    fn an_undeclared_working_capability_is_reported() {
        assert_eq!(violations(&bare(true)), ["keys"]);
    }
}
