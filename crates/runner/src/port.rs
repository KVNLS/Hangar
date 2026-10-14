use std::net::{Ipv4Addr, TcpListener};
use std::ops::RangeInclusive;

use crate::{Error, Result};

/// Dev-server ports handed to workspaces. Metro's default 8081 stays free for runs outside Hangar.
pub const PORT_RANGE: RangeInclusive<u16> = 8100..=8199;

/// First port of [`PORT_RANGE`] that no workspace holds and that nothing listens on right now.
pub fn allocate_port(taken: &[u16]) -> Result<u16> {
    allocate_port_with(taken, is_free)
}

/// `saved` when no workspace holds it and it is still free, else [`allocate_port`].
pub fn pick_port(saved: Option<u16>, taken: &[u16]) -> Result<u16> {
    pick_port_with(saved, taken, is_free)
}

fn pick_port_with(
    saved: Option<u16>,
    taken: &[u16],
    mut free: impl FnMut(u16) -> bool,
) -> Result<u16> {
    match saved.filter(|p| !taken.contains(p)) {
        Some(port) if free(port) => Ok(port),
        _ => allocate_port_with(taken, free),
    }
}

/// Nothing listens on `port`: it binds on every address a dev server may use. Metro and Expo
/// listen on `::` (or `0.0.0.0`), which a bind on 127.0.0.1 alone does not see.
pub fn is_free(port: u16) -> bool {
    use std::net::{IpAddr, Ipv6Addr};
    let addrs: [IpAddr; 4] = [
        Ipv4Addr::LOCALHOST.into(),
        Ipv4Addr::UNSPECIFIED.into(),
        Ipv6Addr::LOCALHOST.into(),
        Ipv6Addr::UNSPECIFIED.into(),
    ];
    addrs
        .into_iter()
        .all(|addr| match TcpListener::bind((addr, port)) {
            Ok(_) => true,
            // No IPv6 on this host: that address cannot be in use either.
            Err(e) => addr.is_ipv6() && e.kind() == std::io::ErrorKind::AddrNotAvailable,
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
    fn a_saved_port_is_kept_only_while_free() {
        assert_eq!(pick_port_with(Some(8150), &[], |_| true).unwrap(), 8150);
        assert_eq!(pick_port_with(Some(8150), &[8150], |_| true).unwrap(), 8100);
        assert_eq!(
            pick_port_with(Some(8150), &[], |p| p != 8150).unwrap(),
            8100
        );
        assert_eq!(pick_port_with(None, &[8100], |_| true).unwrap(), 8101);
    }

    #[test]
    fn a_port_held_on_any_address_is_busy() {
        use std::net::Ipv6Addr;
        for addr in [
            std::net::IpAddr::from(Ipv4Addr::UNSPECIFIED),
            Ipv6Addr::UNSPECIFIED.into(),
            Ipv6Addr::LOCALHOST.into(),
        ] {
            let Some(held) = PORT_RANGE
                .clone()
                .find_map(|p| TcpListener::bind((addr, p)).ok())
            else {
                continue;
            };
            let port = held.local_addr().unwrap().port();
            assert!(!is_free(port), "{addr}:{port} looked free");
        }
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
