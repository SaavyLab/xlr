//! Bounded mDNS discovery of Dante devices by their ARC control service.
//!
//! Every Dante device that accepts ARC requests advertises a
//! `<device>._netaudio-arc._udp.local` DNS-SD instance. [`DeviceBrowser`]
//! sends one PTR browse on a chosen IPv4 interface, follows up once per
//! instance whose SRV or address records were not included in the browse
//! answer, and returns every device resolved before one absolute deadline.
//!
//! Discovery establishes only that a device advertised its control service
//! during the browse window. Devices that do not answer in time are simply
//! absent from the result; absence is not proof that a device is offline.

use crate::mdns::{
    self, MDNS_ADDRESS, MDNS_PORT, RData, RECEIVE_CAPACITY, SocketStage, SrvData, TYPE_ANY,
    TYPE_PTR, canonical_name,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt, io,
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket},
    time::{Duration, Instant},
};

const ARC_SERVICE: &str = "_netaudio-arc._udp.local";

/// One Dante device resolved from its `_netaudio-arc._udp.local` service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredDevice {
    name: String,
    host: String,
    arc_port: u16,
    addresses: Vec<IpAddr>,
    txt: Vec<(String, String)>,
}

impl DiscoveredDevice {
    /// The Dante device name, as advertised in the service instance label.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The mDNS host name the service's SRV record points at.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The UDP port the device accepts ARC requests on.
    pub fn arc_port(&self) -> u16 {
        self.arc_port
    }

    /// Every address advertised for [`Self::host`], IPv4 first.
    pub fn addresses(&self) -> &[IpAddr] {
        &self.addresses
    }

    /// The ARC endpoint to hand to [`crate::ArcClient`]: the first advertised
    /// address (IPv4 preferred) with [`Self::arc_port`].
    pub fn arc_address(&self) -> SocketAddr {
        SocketAddr::new(self.addresses[0], self.arc_port)
    }

    /// Every `key=value` TXT entry from the ARC service, in advertised order.
    /// Entries without `=` appear with an empty value.
    pub fn txt(&self) -> &[(String, String)] {
        &self.txt
    }

    /// Looks up one TXT value by key.
    pub fn txt_value(&self, key: &str) -> Option<&str> {
        self.txt
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.as_str())
    }

    /// The advertised model identifier (TXT `model`), e.g. `ViaMac`.
    pub fn model(&self) -> Option<&str> {
        self.txt_value("model")
    }

    /// The advertised manufacturer (TXT `mf`).
    pub fn manufacturer(&self) -> Option<&str> {
        self.txt_value("mf")
    }

    /// The ARC protocol version from TXT `arcp_vers`, encoded as the wire
    /// magic: `2.8.9` is `0x2809`, `2.8.15` is `0x280F`.
    pub fn arc_protocol(&self) -> Option<u16> {
        let mut parts = self.txt_value("arcp_vers")?.split('.');
        let mut part = |bits: u32| -> Option<u16> {
            let value: u16 = parts.next()?.parse().ok()?;
            (u32::from(value) < (1 << bits)).then_some(value)
        };
        let (major, minor, patch) = (part(4)?, part(4)?, part(8)?);
        parts
            .next()
            .is_none()
            .then_some((major << 12) | (minor << 8) | patch)
    }

    /// The advertised product description (TXT `router_info`), e.g.
    /// `Dante Via`.
    pub fn product(&self) -> Option<&str> {
        self.txt_value("router_info")
    }
}

/// Synchronous, bounded browser for Dante devices on one IPv4 interface.
pub struct DeviceBrowser {
    interface_address: Ipv4Addr,
    timeout: Duration,
    destination: SocketAddrV4,
    bind_port: u16,
}

impl DeviceBrowser {
    /// Creates a browser that sends from, and listens on, the interface
    /// holding `interface_address`. `timeout` bounds the whole browse.
    pub fn new(interface_address: Ipv4Addr, timeout: Duration) -> Result<Self, DiscoveryError> {
        if timeout.is_zero() {
            return Err(DiscoveryError::ZeroTimeout);
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

    /// Browses for Dante devices until the timeout elapses and returns every
    /// device whose SRV and address records were both observed, sorted by
    /// name.
    ///
    /// Malformed or unrelated datagrams are ignored. At most one follow-up
    /// query is sent per unresolved instance or host.
    pub fn browse(&self) -> Result<Vec<DiscoveredDevice>, DiscoveryError> {
        let socket = mdns::open_socket(self.interface_address, self.bind_port, self.destination)
            .map_err(|(stage, source)| DiscoveryError::Io {
                stage: match stage {
                    SocketStage::Bind => DiscoveryIoStage::Bind,
                    SocketStage::Configure => DiscoveryIoStage::Configure,
                },
                source,
            })?;
        self.send(&socket, &service_labels(), TYPE_PTR)?;

        let started = Instant::now();
        let mut state = BrowseState::default();
        let mut buffer = vec![0; RECEIVE_CAPACITY];
        while let Some(remaining) = self.timeout.checked_sub(started.elapsed()) {
            if remaining.is_zero() {
                break;
            }
            socket
                .set_read_timeout(Some(remaining))
                .map_err(|source| DiscoveryError::Io {
                    stage: DiscoveryIoStage::Configure,
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
                    return Err(DiscoveryError::Io {
                        stage: DiscoveryIoStage::Receive,
                        source,
                    });
                }
            };
            if source.port() != self.destination.port() {
                continue;
            }
            let Ok(Some(records)) = mdns::parse_response(&buffer[..received]) else {
                continue;
            };
            state.absorb(&records);
            for labels in state.follow_ups() {
                self.send(&socket, &labels, TYPE_ANY)?;
            }
        }
        Ok(state.into_devices())
    }

    fn send(
        &self,
        socket: &UdpSocket,
        labels: &[String],
        record_type: u16,
    ) -> Result<(), DiscoveryError> {
        socket
            .send_to(
                &mdns::encode_question(labels, record_type),
                self.destination,
            )
            .map(|_| ())
            .map_err(|source| DiscoveryError::Io {
                stage: DiscoveryIoStage::Send,
                source,
            })
    }
}

fn service_labels() -> Vec<String> {
    ARC_SERVICE.split('.').map(str::to_owned).collect()
}

/// Everything learned so far, keyed by canonical DNS name.
#[derive(Default)]
struct BrowseState {
    /// Canonical instance name → advertised instance name.
    instances: BTreeMap<String, String>,
    srv: BTreeMap<String, SrvData>,
    txt: BTreeMap<String, Vec<Box<[u8]>>>,
    addresses: BTreeMap<String, BTreeSet<IpAddr>>,
    /// Canonical names already followed up, so each is asked at most once.
    asked: BTreeSet<String>,
}

impl BrowseState {
    fn absorb(&mut self, records: &[mdns::Record]) {
        let service = canonical_name(ARC_SERVICE);
        for record in records.iter().filter(|record| record.is_live_internet()) {
            let owner = canonical_name(&record.owner);
            match &record.data {
                RData::Ptr(target) if owner == service => {
                    let key = canonical_name(target);
                    if key.ends_with(&format!(".{service}")) {
                        self.instances.entry(key).or_insert_with(|| target.clone());
                    }
                }
                RData::Srv(srv) => {
                    self.srv.insert(owner, srv.clone());
                }
                RData::Txt(values) => {
                    self.txt.insert(owner, values.clone());
                }
                RData::Address(address) => {
                    self.addresses.entry(owner).or_default().insert(*address);
                }
                _ => {}
            }
        }
    }

    /// Names that still lack records and have not been asked about yet.
    fn follow_ups(&mut self) -> Vec<Vec<String>> {
        let mut pending = Vec::new();
        for (key, instance) in &self.instances {
            match self.srv.get(key) {
                None => pending.push((key.clone(), instance.clone())),
                Some(srv) => {
                    let host = canonical_name(&srv.target);
                    if !self.addresses.contains_key(&host) {
                        pending.push((host, srv.target.clone()));
                    }
                }
            }
        }
        pending
            .into_iter()
            .filter(|(key, _)| self.asked.insert(key.clone()))
            .map(|(_, name)| {
                name.trim_end_matches('.')
                    .split('.')
                    .map(str::to_owned)
                    .collect()
            })
            .collect()
    }

    fn into_devices(self) -> Vec<DiscoveredDevice> {
        let suffix_length = ARC_SERVICE.len() + 1;
        let mut devices = Vec::new();
        for (key, instance) in &self.instances {
            let Some(srv) = self.srv.get(key) else {
                continue;
            };
            let Some(addresses) = self.addresses.get(&canonical_name(&srv.target)) else {
                continue;
            };
            let mut addresses: Vec<IpAddr> = addresses.iter().copied().collect();
            addresses.sort_by_key(|address| (address.is_ipv6(), *address));
            let txt = self
                .txt
                .get(key)
                .map(|values| values.iter().filter_map(|value| txt_pair(value)).collect())
                .unwrap_or_default();
            let instance = instance.trim_end_matches('.');
            devices.push(DiscoveredDevice {
                name: instance[..instance.len() - suffix_length].to_owned(),
                host: srv.target.clone(),
                arc_port: srv.port,
                addresses,
                txt,
            });
        }
        devices.sort_by(|left, right| left.name.cmp(&right.name));
        devices
    }
}

fn txt_pair(value: &[u8]) -> Option<(String, String)> {
    if value.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(value);
    Some(match text.split_once('=') {
        Some((key, value)) => (key.to_owned(), value.to_owned()),
        None => (text.into_owned(), String::new()),
    })
}

/// Which step of a browse failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryIoStage {
    Bind,
    Configure,
    Send,
    Receive,
}

/// Failure to run a browse at all. An empty result is not an error.
#[derive(Debug)]
pub enum DiscoveryError {
    ZeroTimeout,
    Io {
        stage: DiscoveryIoStage,
        source: io::Error,
    },
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroTimeout => formatter.write_str("discovery timeout must be greater than zero"),
            Self::Io { stage, source } => write!(formatter, "discovery {stage:?} failed: {source}"),
        }
    }
}

impl Error for DiscoveryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::ZeroTimeout => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdns::{TYPE_A, TYPE_SRV, TYPE_TXT, encode_labels};
    use std::thread;

    fn labels(name: &str) -> Vec<String> {
        name.split('.').map(str::to_owned).collect()
    }

    /// A response builder for synthetic mDNS answers.
    struct Response {
        packet: Vec<u8>,
        answers: u16,
    }

    impl Response {
        fn new() -> Self {
            let mut packet = vec![0u8; 12];
            packet[2..4].copy_from_slice(&0x8400u16.to_be_bytes());
            Self { packet, answers: 0 }
        }

        fn record(mut self, owner: &str, rtype: u16, data: &[u8]) -> Self {
            encode_labels(&mut self.packet, &labels(owner));
            self.packet.extend_from_slice(&rtype.to_be_bytes());
            self.packet.extend_from_slice(&0x8001u16.to_be_bytes());
            self.packet.extend_from_slice(&120u32.to_be_bytes());
            self.packet
                .extend_from_slice(&u16::try_from(data.len()).unwrap().to_be_bytes());
            self.packet.extend_from_slice(data);
            self.answers += 1;
            self
        }

        fn ptr(self, owner: &str, target: &str) -> Self {
            let mut data = Vec::new();
            encode_labels(&mut data, &labels(target));
            self.record(owner, TYPE_PTR, &data)
        }

        fn srv(self, owner: &str, target: &str, port: u16) -> Self {
            let mut data = vec![0, 0, 0, 0];
            data.extend_from_slice(&port.to_be_bytes());
            encode_labels(&mut data, &labels(target));
            self.record(owner, TYPE_SRV, &data)
        }

        fn txt(self, owner: &str, values: &[&str]) -> Self {
            let mut data = Vec::new();
            for value in values {
                data.push(u8::try_from(value.len()).unwrap());
                data.extend_from_slice(value.as_bytes());
            }
            self.record(owner, TYPE_TXT, &data)
        }

        fn a(self, owner: &str, address: Ipv4Addr) -> Self {
            self.record(owner, TYPE_A, &address.octets())
        }

        fn build(mut self) -> Vec<u8> {
            self.packet[6..8].copy_from_slice(&self.answers.to_be_bytes());
            self.packet
        }
    }

    #[test]
    fn complete_browse_answer_resolves_a_device() {
        let mut state = BrowseState::default();
        let packet = Response::new()
            .ptr(ARC_SERVICE, "stage-box._netaudio-arc._udp.local")
            .srv(
                "stage-box._netaudio-arc._udp.local",
                "stage-box.local",
                4440,
            )
            .txt(
                "stage-box._netaudio-arc._udp.local",
                &["model=DIOUSBC", "mf=Audinate", "router_info=DIOUSB"],
            )
            .a("stage-box.local", Ipv4Addr::new(10, 0, 0, 5))
            .build();
        state.absorb(&mdns::parse_response(&packet).unwrap().unwrap());
        assert!(state.follow_ups().is_empty());
        let devices = state.into_devices();
        assert_eq!(devices.len(), 1);
        let device = &devices[0];
        assert_eq!(device.name(), "stage-box");
        assert_eq!(device.arc_address(), "10.0.0.5:4440".parse().unwrap());
        assert_eq!(device.model(), Some("DIOUSBC"));
        assert_eq!(device.manufacturer(), Some("Audinate"));
        assert_eq!(device.product(), Some("DIOUSB"));
    }

    #[test]
    fn arc_protocol_parses_advertised_versions() {
        let device = |version: &str| DiscoveredDevice {
            name: "d".to_owned(),
            host: "d.local".to_owned(),
            arc_port: 4440,
            addresses: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            txt: vec![("arcp_vers".to_owned(), version.to_owned())],
        };
        assert_eq!(device("2.8.9").arc_protocol(), Some(0x2809));
        assert_eq!(device("2.8.15").arc_protocol(), Some(0x280F));
        for invalid in ["2.8", "2.8.9.1", "16.0.0", "2.x.9", "2.8.256"] {
            assert_eq!(device(invalid).arc_protocol(), None, "{invalid}");
        }
    }

    #[test]
    fn bare_ptr_answer_asks_for_the_instance_once() {
        let mut state = BrowseState::default();
        let packet = Response::new()
            .ptr(ARC_SERVICE, "stage-box._netaudio-arc._udp.local")
            .build();
        state.absorb(&mdns::parse_response(&packet).unwrap().unwrap());
        assert_eq!(
            state.follow_ups(),
            vec![labels("stage-box._netaudio-arc._udp.local")]
        );
        assert!(state.follow_ups().is_empty(), "each name is asked once");
        assert!(state.into_devices().is_empty(), "unresolved is omitted");
    }

    #[test]
    fn srv_without_address_asks_for_the_host() {
        let mut state = BrowseState::default();
        let packet = Response::new()
            .ptr(ARC_SERVICE, "stage-box._netaudio-arc._udp.local")
            .srv(
                "stage-box._netaudio-arc._udp.local",
                "stage-box.local",
                4440,
            )
            .build();
        state.absorb(&mdns::parse_response(&packet).unwrap().unwrap());
        assert_eq!(state.follow_ups(), vec![labels("stage-box.local")]);
    }

    #[test]
    fn unrelated_ptr_targets_are_ignored() {
        let mut state = BrowseState::default();
        let packet = Response::new()
            .ptr(ARC_SERVICE, "printer._ipp._tcp.local")
            .ptr("_other._udp.local", "x._other._udp.local")
            .build();
        state.absorb(&mdns::parse_response(&packet).unwrap().unwrap());
        assert!(state.follow_ups().is_empty());
        assert!(state.into_devices().is_empty());
    }

    #[test]
    fn loopback_browse_follows_up_and_resolves() {
        let server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let destination = match server.local_addr().unwrap() {
            SocketAddr::V4(address) => address,
            SocketAddr::V6(_) => unreachable!(),
        };
        let browser =
            DeviceBrowser::for_test(Ipv4Addr::LOCALHOST, Duration::from_millis(300), destination);
        let worker = thread::spawn(move || browser.browse());

        let mut request = [0u8; 512];
        let (_, client) = server.recv_from(&mut request).unwrap();
        server.send_to(&[1, 2, 3], client).unwrap();
        let bare = Response::new()
            .ptr(ARC_SERVICE, "via-host._netaudio-arc._udp.local")
            .build();
        server.send_to(&bare, client).unwrap();

        let (length, _) = server.recv_from(&mut request).unwrap();
        let follow_up = &request[..length];
        let mut expected = Vec::new();
        encode_labels(&mut expected, &labels("via-host._netaudio-arc._udp.local"));
        assert!(
            follow_up.windows(expected.len()).any(|w| w == expected),
            "follow-up names the unresolved instance"
        );
        let full = Response::new()
            .srv("via-host._netaudio-arc._udp.local", "via-host.local", 24440)
            .a("via-host.local", Ipv4Addr::LOCALHOST)
            .build();
        server.send_to(&full, client).unwrap();

        let devices = worker.join().unwrap().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name(), "via-host");
        assert_eq!(devices[0].arc_port(), 24440);
    }
}
