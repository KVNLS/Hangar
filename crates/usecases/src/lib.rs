//! Application services: Hangar's use cases, written against the ports of `hangar_core` only.
//! Headless and blocking: callers run them off the UI thread. Services may stat paths; anything
//! that changes the disk, a repository or the database goes through a port.

pub mod agents;
#[cfg(test)]
mod fakes;
pub mod review;
pub mod sessions;
pub mod settings;
pub mod workspaces;
