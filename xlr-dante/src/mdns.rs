//! Minimal multicast DNS primitives shared by the discovery and
//! channel-service queries.
//!
//! This is deliberately not a general DNS library: it encodes single
//! questions, parses the record sections of a response with bounded name
//! decompression, and opens an interface-scoped multicast socket.

use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::{
    collections::BTreeSet,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddrV4, UdpSocket},
};

pub(crate) const MDNS_ADDRESS: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
pub(crate) const MDNS_PORT: u16 = 5353;
pub(crate) const TYPE_A: u16 = 1;
pub(crate) const TYPE_PTR: u16 = 12;
pub(crate) const TYPE_TXT: u16 = 16;
pub(crate) const TYPE_AAAA: u16 = 28;
pub(crate) const TYPE_SRV: u16 = 33;
pub(crate) const TYPE_ANY: u16 = 255;
pub(crate) const CLASS_IN: u16 = 1;
/// Large enough for any standard UDP datagram.
pub(crate) const RECEIVE_CAPACITY: usize = u16::MAX as usize + 1;
const DNS_HEADER_LENGTH: usize = 12;
const MAX_POINTERS: usize = 128;

/// SRV record data.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct SrvData {
    pub(crate) priority: u16,
    pub(crate) weight: u16,
    pub(crate) port: u16,
    pub(crate) target: String,
}

#[derive(Debug)]
pub(crate) enum RData {
    Ptr(String),
    Srv(SrvData),
    Txt(Vec<Box<[u8]>>),
    Address(IpAddr),
    Other,
}

#[derive(Debug)]
pub(crate) struct Record {
    pub(crate) owner: String,
    pub(crate) ttl: u32,
    pub(crate) class: u16,
    pub(crate) data: RData,
}

impl Record {
    /// Whether this is a live (nonzero TTL) Internet-class record, ignoring
    /// the mDNS cache-flush bit.
    pub(crate) fn is_live_internet(&self) -> bool {
        self.class & 0x7fff == CLASS_IN && self.ttl != 0
    }
}

#[derive(Debug)]
pub(crate) struct ParseError;

/// Encodes a one-question query message with a zero transaction id.
pub(crate) fn encode_question(labels: &[String], record_type: u16) -> Vec<u8> {
    let mut query = vec![0; DNS_HEADER_LENGTH];
    query[4..6].copy_from_slice(&1u16.to_be_bytes());
    encode_labels(&mut query, labels);
    query.extend_from_slice(&record_type.to_be_bytes());
    query.extend_from_slice(&CLASS_IN.to_be_bytes());
    query
}

/// Parses every answer, authority, and additional record of a response.
///
/// Returns `Ok(None)` for a well-formed query (non-response) message. Any
/// structural violation fails the whole datagram.
pub(crate) fn parse_response(packet: &[u8]) -> Result<Option<Vec<Record>>, ParseError> {
    if packet.len() < DNS_HEADER_LENGTH {
        return Err(ParseError);
    }
    let flags = word(packet, 2)?;
    if flags & 0x8000 == 0 {
        return Ok(None);
    }
    let counts = [
        word(packet, 4)?,
        word(packet, 6)?,
        word(packet, 8)?,
        word(packet, 10)?,
    ];
    let mut offset = DNS_HEADER_LENGTH;
    for _ in 0..counts[0] {
        let (_, next) = decode_name(packet, offset, packet.len())?;
        offset = next.checked_add(4).ok_or(ParseError)?;
        if offset > packet.len() {
            return Err(ParseError);
        }
    }
    let mut records = Vec::new();
    for count in &counts[1..] {
        for _ in 0..*count {
            let (owner, next) = decode_name(packet, offset, packet.len())?;
            if next + 10 > packet.len() {
                return Err(ParseError);
            }
            let rtype = word(packet, next)?;
            let class = word(packet, next + 2)?;
            let ttl = dword(packet, next + 4)?;
            let length = usize::from(word(packet, next + 8)?);
            let start = next + 10;
            let end = start.checked_add(length).ok_or(ParseError)?;
            if end > packet.len() {
                return Err(ParseError);
            }
            let data = match rtype {
                TYPE_PTR if length >= 1 => {
                    let (target, consumed) = decode_name(packet, start, end)?;
                    if consumed != end {
                        return Err(ParseError);
                    }
                    RData::Ptr(target)
                }
                TYPE_SRV if length >= 7 => {
                    let priority = word(packet, start)?;
                    let weight = word(packet, start + 2)?;
                    let port = word(packet, start + 4)?;
                    let (target, consumed) = decode_name(packet, start + 6, end)?;
                    if consumed != end || port == 0 {
                        return Err(ParseError);
                    }
                    RData::Srv(SrvData {
                        priority,
                        weight,
                        port,
                        target,
                    })
                }
                TYPE_TXT => {
                    let mut values = Vec::new();
                    let mut cursor = start;
                    while cursor < end {
                        let length = usize::from(packet[cursor]);
                        cursor += 1;
                        if cursor + length > end {
                            return Err(ParseError);
                        }
                        values.push(packet[cursor..cursor + length].to_vec().into_boxed_slice());
                        cursor += length;
                    }
                    RData::Txt(values)
                }
                TYPE_A if length == 4 => RData::Address(IpAddr::V4(Ipv4Addr::new(
                    packet[start],
                    packet[start + 1],
                    packet[start + 2],
                    packet[start + 3],
                ))),
                TYPE_AAAA if length == 16 => {
                    let octets: [u8; 16] = packet[start..end].try_into().map_err(|_| ParseError)?;
                    RData::Address(IpAddr::V6(octets.into()))
                }
                _ => RData::Other,
            };
            records.push(Record {
                owner,
                ttl,
                class,
                data,
            });
            offset = end;
        }
    }
    Ok(Some(records))
}

fn word(packet: &[u8], offset: usize) -> Result<u16, ParseError> {
    let bytes: [u8; 2] = packet
        .get(offset..offset + 2)
        .ok_or(ParseError)?
        .try_into()
        .map_err(|_| ParseError)?;
    Ok(u16::from_be_bytes(bytes))
}

fn dword(packet: &[u8], offset: usize) -> Result<u32, ParseError> {
    let bytes: [u8; 4] = packet
        .get(offset..offset + 4)
        .ok_or(ParseError)?
        .try_into()
        .map_err(|_| ParseError)?;
    Ok(u32::from_be_bytes(bytes))
}

/// Decodes one possibly-compressed name, returning its dotted form and the
/// offset just past its in-place encoding.
fn decode_name(packet: &[u8], offset: usize, limit: usize) -> Result<(String, usize), ParseError> {
    if offset >= packet.len() || limit > packet.len() || offset >= limit {
        return Err(ParseError);
    }
    let mut labels = Vec::new();
    let mut cursor = offset;
    let mut next = None;
    let mut pointers = 0;
    let mut expanded = 1;
    let mut visited = BTreeSet::new();
    loop {
        if cursor >= packet.len() || !visited.insert(cursor) {
            return Err(ParseError);
        }
        let length = packet[cursor];
        if length & 0xc0 == 0xc0 {
            if cursor + 2 > packet.len() || (next.is_none() && cursor + 2 > limit) {
                return Err(ParseError);
            }
            let pointer = ((usize::from(length & 0x3f)) << 8) | usize::from(packet[cursor + 1]);
            if pointer >= packet.len() {
                return Err(ParseError);
            }
            pointers += 1;
            if pointers > MAX_POINTERS {
                return Err(ParseError);
            }
            next.get_or_insert(cursor + 2);
            cursor = pointer;
            continue;
        }
        if length & 0xc0 != 0 {
            return Err(ParseError);
        }
        if length == 0 {
            let consumed = next.unwrap_or(cursor + 1);
            if consumed > limit {
                return Err(ParseError);
            }
            return Ok((labels.join("."), consumed));
        }
        let end = cursor + 1 + usize::from(length);
        if end > packet.len() || (next.is_none() && end > limit) {
            return Err(ParseError);
        }
        expanded += usize::from(length) + 1;
        if expanded > 255 {
            return Err(ParseError);
        }
        let label = std::str::from_utf8(&packet[cursor + 1..end]).map_err(|_| ParseError)?;
        labels.push(label.to_owned());
        cursor = end;
    }
}

pub(crate) fn encode_labels(buffer: &mut Vec<u8>, labels: &[String]) {
    for label in labels {
        buffer.push(u8::try_from(label.len()).expect("validated DNS label length"));
        buffer.extend_from_slice(label.as_bytes());
    }
    buffer.push(0);
}

/// Case-insensitive, trailing-dot-insensitive comparison key for DNS names.
pub(crate) fn canonical_name(name: &str) -> String {
    name.trim_end_matches('.').to_ascii_lowercase()
}

/// Which socket setup step failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SocketStage {
    Bind,
    Configure,
}

/// Opens a reusable UDP socket bound to `bind_port`. When `destination` is
/// multicast, joins the mDNS group and sends through `interface` only.
pub(crate) fn open_socket(
    interface: Ipv4Addr,
    bind_port: u16,
    destination: SocketAddrV4,
) -> Result<UdpSocket, (SocketStage, io::Error)> {
    let bind = |source| (SocketStage::Bind, source);
    let configure = |source| (SocketStage::Configure, source);
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).map_err(bind)?;
    socket.set_reuse_address(true).map_err(configure)?;
    #[cfg(unix)]
    socket.set_reuse_port(true).map_err(configure)?;
    socket
        .bind(&SockAddr::from(SocketAddrV4::new(
            Ipv4Addr::UNSPECIFIED,
            bind_port,
        )))
        .map_err(bind)?;
    if destination.ip().is_multicast() {
        socket
            .join_multicast_v4(&MDNS_ADDRESS, &interface)
            .map_err(configure)?;
        socket.set_multicast_if_v4(&interface).map_err(configure)?;
        socket.set_multicast_ttl_v4(255).map_err(configure)?;
    }
    Ok(socket.into())
}
