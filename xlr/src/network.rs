//! Local network helpers.

use std::{
    io,
    net::{IpAddr, Ipv4Addr, UdpSocket},
};

/// The local IPv4 address the OS routes mDNS multicast through.
///
/// Connecting a UDP socket sends nothing; it only asks the routing table
/// which source address it would use.
pub fn default_interface() -> io::Result<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.connect((Ipv4Addr::new(224, 0, 0, 251), 5353))?;
    match socket.local_addr()?.ip() {
        IpAddr::V4(address) if !address.is_unspecified() => Ok(address),
        _ => Err(io::Error::other(
            "could not determine the local interface; pass --interface",
        )),
    }
}
