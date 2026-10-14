use std::net::{Ipv4Addr, TcpListener};
use std::ops::RangeInclusive;

use crate::{Error, Result};

/// Dev-server ports handed to workspaces. Metro's default 8081 stays free for runs outside Hangar.
pub const PORT_RANGE: RangeInclusive<u16> = 8100..=8199;

/// First port of [`PORT_RANGE`] that no workspace holds and that binds on localhost right now.
pub fn allocate_port(taken: &[u16]) -> Result<u16> {
    allocate_port_with(taken, |port| {
        TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok()
    })
}

/// [`allocate_port`] with the "is it free" check supplied by the caller.
pub fn allocate_port_with(taken: &[u16], mut free: impl FnMut(u16) -> bool) -> Result<u16> {
    PORT_RANGE
        .filter(|port| !taken.contains(port))
        .find(|&port| free(port))
        .ok_or(Error::NoFreePort {
            first: *PORT_RANGE.start(),
            last: *PORT_RANGE.end(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_taken_and_busy_ports() {
        let port = allocate_port_with(&[8100, 8101], |p| p != 8102).unwrap();
        assert_eq!(port, 8103);
    }

    #[test]
    fn fails_when_the_range_is_exhausted() {
        let taken: Vec<u16> = PORT_RANGE.clone().collect();
        assert!(matches!(
            allocate_port_with(&taken, |_| true),
            Err(Error::NoFreePort { .. })
        ));
    }

    #[test]
    fn a_bound_port_is_not_handed_out() {
        let Some(held) = PORT_RANGE
            .clone()
            .find_map(|p| TcpListener::bind((Ipv4Addr::LOCALHOST, p)).ok())
        else {
            return;
        };
        let held_port = held.local_addr().unwrap().port();
        let taken: Vec<u16> = PORT_RANGE.clone().filter(|&p| p < held_port).collect();
        let port = allocate_port(&taken).unwrap();
        assert_ne!(port, held_port);
    }
}
