//! Build and run per stack (Xcode, Gradle, React Native, Flutter).
//!
//! Headless: detects what a worktree holds, allocates dev-server ports and turns a stack plus a
//! target device into plain command lines. The app runs those commands in terminal tabs.

mod artifact;
mod detect;
mod error;
mod plan;
mod port;

pub use artifact::{Built, pick_scheme, resolve_artifact, xcode_schemes};
pub use detect::{Stack, detect};
pub use error::{Error, Result};
pub use plan::{Artifact, Command, PlanInput, RunPlan, Step, Target, plan, shell_quote};
pub use port::{PORT_RANGE, allocate_port, allocate_port_with};
