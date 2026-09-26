//! Bounded read-only mDNS query for one exact Dante transmitter-channel service.

use crate::mdns::{
    self, MDNS_ADDRESS, MDNS_PORT, RData, RECEIVE_CAPACITY, SocketStage, TYPE_ANY, canonical_name,
};
use std::{
    collections::{BTreeSet, HashMap},
    error::Error,
    fmt, io,
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket},
    time::{Duration, Instant},
};

/// Exact `_netaudio-chan._udp.local` instance for one transmitter channel.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ChannelServiceName {
    device_name: String,
    channel_name: String,
    instance_fqdn: String,
}

impl ChannelServiceName {
    /// Builds the exact channel-service name without treating dots inside the
    /// instance label as DNS label separators.
    pub fn new(device_name: String, channel_name: String) -> Result<Self, ChannelServiceError> {
        if device_name.is_empty() {
            return Err(ChannelServiceError::InvalidName {
                field: "device_name",
            });
        }
        if channel_name.is_empty() {
            return Err(ChannelServiceError::InvalidName {
                field: "channel_name",
            });
        }
        let instance = format!("{channel_name}@{device_name}");
        if instance.len() > 63 {
            return Err(ChannelServiceError::InstanceLabelTooLong {
                length: instance.len(),
            });
        }
        let instance_fqdn = format!("{instance}._netaudio-chan._udp.local");
        Ok(Self {
            device_name,
            channel_name,
            instance_fqdn,
        })
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    pub fn channel_name(&self) -> &str {
        &self.channel_name
    }

    pub fn instance_fqdn(&self) -> &str {
        &self.instance_fqdn
    }

    fn encode_question(&self) -> Vec<u8> {
        mdns::encode_question(
            &[
                format!("{}@{}", self.channel_name, self.device_name),
                "_netaudio-chan".to_owned(),
                "_udp".to_owned(),
                "local".to_owned(),
            ],
            TYPE_ANY,
        )
    }
}

/// Exact SRV data retained from the responding channel-service profile.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ChannelServiceSrv {
    priority: u16,
    weight: u16,
    port: u16,
    target: String,
}

impl ChannelServiceSrv {
    pub fn priority(&self) -> u16 {
        self.priority
    }
    pub fn weight(&self) -> u16 {
        self.weight
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn target(&self) -> &str {
        &self.target
    }
}

/// Complete positive evidence from one exact responding mDNS subsystem.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelServiceEvidence {
    name: ChannelServiceName,
    response_source: SocketAddr,
    srv: ChannelServiceSrv,
    txt_strings: Vec<Box<[u8]>>,
    addresses: Vec<IpAddr>,
    minimum_ttl_seconds: u32,
}

impl ChannelServiceEvidence {
    pub fn name(&self) -> &ChannelServiceName {
        &self.name
    }
    pub fn response_source(&self) -> SocketAddr {
        self.response_source
    }
    pub fn srv(&self) -> &ChannelServiceSrv {
        &self.srv
    }
    pub fn txt_strings(&self) -> &[Box<[u8]>] {
        &self.txt_strings
    }
    pub fn addresses(&self) -> &[IpAddr] {
        &self.addresses
    }
    pub fn minimum_ttl_seconds(&self) -> u32 {
        self.minimum_ttl_seconds
    }

    fn same_profile(&self, other: &Self) -> bool {
        self.name == other.name
            && self.response_source == other.response_source
            && self.srv == other.srv
            && self.txt_strings == other.txt_strings
            && self.addresses == other.addresses
    }

    /// Canonical, deterministic identity string for this responding service:
    /// name, response source, SRV record, TXT strings, and addresses. Useful
    /// for detecting when a transmitter's advertised profile changes. TTL is
    /// excluded because it qualifies freshness, not identity.
    pub fn identity_claim(&self) -> String {
        let mut value = String::new();
        push_component(&mut value, self.name.instance_fqdn());
        push_component(&mut value, &self.response_source.to_string());
        push_component(&mut value, &self.srv.priority.to_string());
        push_component(&mut value, &self.srv.weight.to_string());
        push_component(&mut value, &self.srv.port.to_string());
        push_component(&mut value, &self.srv.target);
        for txt in &self.txt_strings {
            let encoded = hex(txt);
            push_component(&mut value, &encoded);
        }
        for address in &self.addresses {
            push_component(&mut value, &address.to_string());
        }
        value
    }
}

/// Synchronous one-send query client bound to one selected IPv4 interface.
pub struct ChannelServiceClient {
    interface_address: Ipv4Addr,
    timeout: Duration,
    destination: SocketAddrV4,
    bind_port: u16,
}

impl ChannelServiceClient {
    pub fn new(
        interface_address: Ipv4Addr,
        timeout: Duration,
    ) -> Result<Self, ChannelServiceError> {
        if timeout.is_zero() {
            return Err(ChannelServiceError::ZeroTimeout);
        }
        Ok(Self {
            interface_address,
            timeout,
            destination: SocketAddrV4::new(MDNS_ADDRESS, MDNS_PORT),
            bind_port: MDNS_PORT,
        })
    }

    #[cfg(test)]
    fn for_test(interface_address: Ipv4Addr, timeout: Duration, destination: SocketAddrV4) -> Self {
        Self {
            interface_address,
            timeout,
            destination,
            bind_port: 0,
        }
    }

    /// Sends one exact DNS `ANY` question and collects a complete positive
    /// response within one absolute monotonic deadline.
    pub fn query(
        &self,
        name: &ChannelServiceName,
    ) -> Result<ChannelServiceEvidence, ChannelServiceError> {
        let socket = self.bind_socket()?;
        let query = name.encode_question();
        socket
            .send_to(&query, self.destination)
            .map_err(|source| ChannelServiceError::Io {
                stage: ChannelServiceIoStage::Send,
                source,
            })?;

        let started = Instant::now();
        let mut buffer = vec![0; RECEIVE_CAPACITY];
        let mut evidence_by_source: HashMap<SocketAddr, ChannelServiceEvidence> = HashMap::new();
        while let Some(remaining) = self.timeout.checked_sub(started.elapsed()) {
            if remaining.is_zero() {
                break;
            }
            socket
                .set_read_timeout(Some(remaining))
                .map_err(|source| ChannelServiceError::Io {
                    stage: ChannelServiceIoStage::Configure,
                    source,
                })?;
            let (received, source) = match socket.recv_from(&mut buffer) {
                Ok(result) => result,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    break;
                }
                Err(source) => {
                    return Err(ChannelServiceError::Io {
                        stage: ChannelServiceIoStage::Receive,
                        source,
                    });
                }
            };
            if source.port() != self.destination.port() {
                continue;
            }
            let Ok(Some(evidence)) = parse_evidence(&buffer[..received], source, name) else {
                continue;
            };
            match evidence_by_source.get_mut(&source) {
                Some(previous) if !previous.same_profile(&evidence) => {
                    return Err(ChannelServiceError::ConflictingResponse { source });
                }
                Some(previous) => {
                    previous.minimum_ttl_seconds = previous
                        .minimum_ttl_seconds
                        .min(evidence.minimum_ttl_seconds);
                }
                None => {
                    evidence_by_source.insert(source, evidence);
                }
            }
        }

        let mut evidence = evidence_by_source.into_values();
        let Some(first) = evidence.next() else {
            return Err(ChannelServiceError::Unavailable {
                instance_fqdn: name.instance_fqdn.clone(),
            });
        };
        if let Some(second) = evidence.next() {
            return Err(ChannelServiceError::AmbiguousResponders {
                first: first.response_source,
                second: second.response_source,
            });
        }
        Ok(first)
    }

    fn bind_socket(&self) -> Result<UdpSocket, ChannelServiceError> {
        mdns::open_socket(self.interface_address, self.bind_port, self.destination).map_err(
            |(stage, source)| ChannelServiceError::Io {
                stage: match stage {
                    SocketStage::Bind => ChannelServiceIoStage::Bind,
                    SocketStage::Configure => ChannelServiceIoStage::Configure,
                },
                source,
            },
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelServiceIoStage {
    Bind,
    Configure,
    Send,
    Receive,
}

#[derive(Debug)]
pub enum ChannelServiceError {
    InvalidName {
        field: &'static str,
    },
    InstanceLabelTooLong {
        length: usize,
    },
    ZeroTimeout,
    Io {
        stage: ChannelServiceIoStage,
        source: io::Error,
    },
    Unavailable {
        instance_fqdn: String,
    },
    AmbiguousResponders {
        first: SocketAddr,
        second: SocketAddr,
    },
    ConflictingResponse {
        source: SocketAddr,
    },
}

impl fmt::Display for ChannelServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName { field } => {
                write!(formatter, "channel-service {field} must not be empty")
            }
            Self::InstanceLabelTooLong { length } => write!(
                formatter,
                "channel-service instance label is {length} bytes; maximum is 63"
            ),
            Self::ZeroTimeout => {
                formatter.write_str("channel-service timeout must be greater than zero")
            }
            Self::Io { stage, source } => {
                write!(formatter, "channel-service {stage:?} failed: {source}")
            }
            Self::Unavailable { instance_fqdn } => write!(
                formatter,
                "no complete response advertised {instance_fqdn} before the deadline"
            ),
            Self::AmbiguousResponders { first, second } => write!(
                formatter,
                "multiple responders advertised the exact channel service: {first} and {second}"
            ),
            Self::ConflictingResponse { source } => write!(
                formatter,
                "responder {source} advertised conflicting channel-service profiles"
            ),
        }
    }
}

impl Error for ChannelServiceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn parse_evidence(
    packet: &[u8],
    source: SocketAddr,
    name: &ChannelServiceName,
) -> Result<Option<ChannelServiceEvidence>, mdns::ParseError> {
    let Some(records) = mdns::parse_response(packet)? else {
        return Ok(None);
    };

    let wanted = canonical_name(name.instance_fqdn());
    let mut srvs = BTreeSet::new();
    let mut txt = BTreeSet::new();
    let mut required_ttls = Vec::new();
    for record in &records {
        if !record.is_live_internet() || canonical_name(&record.owner) != wanted {
            continue;
        }
        match &record.data {
            RData::Srv(srv) => {
                srvs.insert((
                    srv.priority,
                    srv.weight,
                    srv.port,
                    canonical_name(&srv.target),
                    srv.target.clone(),
                ));
                required_ttls.push(record.ttl);
            }
            RData::Txt(values) if !values.is_empty() => {
                txt.insert(values.clone());
                required_ttls.push(record.ttl);
            }
            _ => {}
        }
    }
    if srvs.len() != 1 || txt.len() != 1 {
        return Ok(None);
    }
    let (priority, weight, port, target_key, target_name) =
        srvs.into_iter().next().ok_or(mdns::ParseError)?;
    let mut addresses = BTreeSet::new();
    for record in &records {
        if !record.is_live_internet() || canonical_name(&record.owner) != target_key {
            continue;
        }
        if let RData::Address(address) = record.data {
            addresses.insert(address);
            required_ttls.push(record.ttl);
        }
    }
    if !addresses.contains(&source.ip()) {
        return Ok(None);
    }
    let minimum_ttl_seconds = required_ttls.into_iter().min().ok_or(mdns::ParseError)?;
    Ok(Some(ChannelServiceEvidence {
        name: name.clone(),
        response_source: source,
        srv: ChannelServiceSrv {
            priority,
            weight,
            port,
            target: target_name,
        },
        txt_strings: txt.into_iter().next().ok_or(mdns::ParseError)?,
        addresses: addresses.into_iter().collect(),
        minimum_ttl_seconds,
    }))
}

fn push_component(output: &mut String, value: &str) {
    output.push_str(&value.len().to_string());
    output.push(':');
    output.push_str(value);
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdns::{TYPE_A, TYPE_SRV, TYPE_TXT, encode_labels};
    use std::thread;

    fn response(name: &ChannelServiceName, source: Ipv4Addr, sequence_port: u16) -> Vec<u8> {
        let instance_labels = [
            format!("{}@{}", name.channel_name(), name.device_name()),
            "_netaudio-chan".to_owned(),
            "_udp".to_owned(),
            "local".to_owned(),
        ];
        let target_labels = ["device-host".to_owned(), "local".to_owned()];
        let mut packet = vec![0u8; 12];
        packet[2..4].copy_from_slice(&0x8400u16.to_be_bytes());
        packet[6..8].copy_from_slice(&2u16.to_be_bytes());
        packet[10..12].copy_from_slice(&1u16.to_be_bytes());
        encode_labels(&mut packet, &instance_labels);
        packet.extend_from_slice(&TYPE_SRV.to_be_bytes());
        packet.extend_from_slice(&0x8001u16.to_be_bytes());
        packet.extend_from_slice(&120u32.to_be_bytes());
        let srv_length_offset = packet.len();
        packet.extend_from_slice(&0u16.to_be_bytes());
        let srv_start = packet.len();
        packet.extend_from_slice(&0u16.to_be_bytes());
        packet.extend_from_slice(&0u16.to_be_bytes());
        packet.extend_from_slice(&sequence_port.to_be_bytes());
        encode_labels(&mut packet, &target_labels);
        let srv_length = u16::try_from(packet.len() - srv_start).unwrap();
        packet[srv_length_offset..srv_length_offset + 2].copy_from_slice(&srv_length.to_be_bytes());
        encode_labels(&mut packet, &instance_labels);
        packet.extend_from_slice(&TYPE_TXT.to_be_bytes());
        packet.extend_from_slice(&0x8001u16.to_be_bytes());
        packet.extend_from_slice(&100u32.to_be_bytes());
        let txt = b"id=1";
        packet.extend_from_slice(&u16::try_from(txt.len() + 1).unwrap().to_be_bytes());
        packet.push(u8::try_from(txt.len()).unwrap());
        packet.extend_from_slice(txt);
        encode_labels(&mut packet, &target_labels);
        packet.extend_from_slice(&TYPE_A.to_be_bytes());
        packet.extend_from_slice(&0x8001u16.to_be_bytes());
        packet.extend_from_slice(&80u32.to_be_bytes());
        packet.extend_from_slice(&4u16.to_be_bytes());
        packet.extend_from_slice(&source.octets());
        packet
    }

    #[test]
    fn exact_complete_response_produces_profile_evidence() {
        let name =
            ChannelServiceName::new("AVIOUSBC-0563aa".to_owned(), "Left".to_owned()).unwrap();
        let packet = response(&name, Ipv4Addr::LOCALHOST, 4455);
        let evidence = parse_evidence(
            &packet,
            SocketAddr::from((Ipv4Addr::LOCALHOST, 5353)),
            &name,
        )
        .unwrap()
        .unwrap();
        assert_eq!(evidence.srv().target(), "device-host.local");
        assert_eq!(evidence.srv().port(), 4455);
        assert_eq!(evidence.minimum_ttl_seconds(), 80);
        assert_eq!(evidence.txt_strings(), &[Box::<[u8]>::from(&b"id=1"[..])]);
        assert!(evidence.identity_claim().contains("AVIOUSBC-0563aa"));
    }

    #[test]
    fn wrong_owner_or_missing_address_is_not_positive_evidence() {
        let wanted = ChannelServiceName::new("Device".to_owned(), "Left".to_owned()).unwrap();
        let other = ChannelServiceName::new("Other".to_owned(), "Left".to_owned()).unwrap();
        let packet = response(&other, Ipv4Addr::LOCALHOST, 4455);
        assert!(
            parse_evidence(
                &packet,
                SocketAddr::from((Ipv4Addr::LOCALHOST, 5353)),
                &wanted,
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn loopback_query_ignores_unrelated_then_accepts_exact_response() {
        let server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let destination = match server.local_addr().unwrap() {
            SocketAddr::V4(address) => address,
            SocketAddr::V6(_) => unreachable!(),
        };
        let client = ChannelServiceClient::for_test(
            Ipv4Addr::LOCALHOST,
            Duration::from_millis(100),
            destination,
        );
        let name = ChannelServiceName::new("Device".to_owned(), "Left".to_owned()).unwrap();
        let expected_name = name.clone();
        let worker = thread::spawn(move || client.query(&expected_name));
        let mut request = [0u8; 512];
        let (_, source) = server.recv_from(&mut request).unwrap();
        server.send_to(&[1, 2, 3], source).unwrap();
        server
            .send_to(&response(&name, Ipv4Addr::LOCALHOST, 4455), source)
            .unwrap();
        let evidence = worker.join().unwrap().unwrap();
        assert_eq!(evidence.name(), &name);
    }
}
