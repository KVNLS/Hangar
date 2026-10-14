//! Build and run per stack (Xcode, Gradle, React Native, Flutter).
//!
//! Headless: detects what a worktree holds, allocates dev-server ports and turns a stack plus a
//! target device into plain command lines. The app runs those commands in terminal tabs.

mod detect;
mod error;
mod port;

pub use detect::{Stack, detect};
pub use error::{Error, Result};
pub use port::{PORT_RANGE, allocate_port, allocate_port_with};
