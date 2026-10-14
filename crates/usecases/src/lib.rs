//! Application services: Hangar's use cases, written against the ports of `hangar_core` only.
//! Headless and blocking: callers run them off the UI thread. Services may stat paths; anything
//! that changes the disk, a repository or the database goes through a port.

pub mod agents;
pub mod batch;
#[cfg(test)]
mod fakes;
pub mod history;
pub mod orchestration;
pub mod review;
pub mod sessions;
pub mod settings;
pub mod usage;
pub mod workspaces;
