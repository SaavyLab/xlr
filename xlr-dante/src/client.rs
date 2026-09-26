//! Synchronous fixed-peer UDP client for ARC queries and subscription writes.
//!
//! [`ArcClient`] owns one UDP socket connected once, at construction, to one
//! device's ARC endpoint. Every operation encodes one request with the
//! [`crate::arc`] codecs, sends it exactly once, and collects its reply; no
//! response parsing happens outside the codecs. All methods take
//! `&mut self`, so one socket performs one correlated request at a time and
//! responses cannot be cross-delivered.
//!
//! Queries discard uncorrelated same-peer datagrams until one correlated
//! response arrives or the original absolute response deadline expires.
//! Writes do not loop: their single received datagram must be from the fixed
//! peer and must be a correlated ten-byte acceptance reply. Nothing retries,
//! probes, or applies fallback policy; that remains the caller's choice. The
//! listing helpers ([`ArcClient::transmitter_channels`],
//! [`ArcClient::receiver_subscriptions`]) issue one query per page and stop
//! at the first failure.

use crate::{
    arc::{
        correlates,
        device::{ChannelCountQuery, DeviceNameQuery},
        subscription::{ReceiverPageQuery, SubscriptionQuery},
        subscription_write::{
            SubscriptionAcceptance, SubscriptionAcceptanceError, SubscriptionWriteCodecError,
            SubscriptionWriteRequest,
        },
        transmitters::TransmitterPageQuery,
    },
    error::ArcCodecError,
    model::{
        ChannelCounts, ReceiverChannel, ReceiverSubscription, ReceiverSubscriptionPage,
        TransmitterChannel, TransmitterSelector,
    },
};
use std::{
    error::Error as StdError,
    fmt,
    io::{self, ErrorKind},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket},
    num::NonZeroU16,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Capacity of the client's reusable receive buffer, in bytes.
///
/// The buffer exceeds the largest payload representable by the standard
/// 16-bit UDP length field: 65,527 bytes after its 8-byte header. The IPv4
/// maximum is lower. Every standard UDP datagram therefore fits in this
/// buffer and is received whole — Rust exposes no portable truncation flag,
/// and nonstandard IP jumbogram behavior is outside this client's scope.
/// Any received frame that disagrees with its own declared ARC length is
/// still rejected by the codec's length validation.
const RECEIVE_BUFFER_CAPACITY: usize = u16::MAX as usize + 1;

/// Number of distinct values in the 16-bit wire sequence space.
const SEQUENCE_SPACE_SIZE: u32 = u16::MAX as u32 + 1;

/// Allocates each wire sequence exactly once, then refuses further allocation.
#[derive(Debug)]
struct SequenceAllocator {
    next: u16,
    allocated: u32,
}

impl SequenceAllocator {
    fn new(initial: u16) -> Self {
        Self {
            next: initial,
            allocated: 0,
        }
    }

    fn allocate(&mut self) -> Option<u16> {
        if self.allocated == SEQUENCE_SPACE_SIZE {
            return None;
        }
        let sequence = self.next;
        self.next = sequence.wrapping_add(1);
        self.allocated += 1;
        Some(sequence)
    }
}

/// A synchronous ARC client for one fixed device, performing exactly one
/// correlated request at a time over one owned UDP socket.
///
/// The socket connects to the peer once during construction and is never
/// reconnected, so queued datagrams cannot be reattributed to another
/// device.
pub struct ArcClient {
    socket: UdpSocket,
    peer_address: SocketAddr,
    response_timeout: Duration,
    /// Allocates each 16-bit wire sequence at most once.
    sequence_allocator: SequenceAllocator,

    /// Single receive buffer reused by every request; never reallocated.
    receive_buffer: Box<[u8]>,
}

impl ArcClient {
    /// Connects to the device's ARC endpoint (as advertised by
    /// [`crate::DiscoveredDevice::arc_address`]) from an ephemeral local
    /// port, with a clock-derived initial sequence.
    ///
    /// Use [`Self::bind`] for full control over the local address and
    /// sequence.
    pub fn connect(peer_address: SocketAddr, timeout: Duration) -> Result<Self, ArcClientError> {
        let local_address = match peer_address.ip() {
            IpAddr::V4(_) => SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
            IpAddr::V6(_) => SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
        };
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.subsec_nanos());
        Self::bind(
            local_address,
            peer_address,
            timeout,
            (nanos ^ (nanos >> 16)) as u16,
        )
    }

    /// Binds a client to `local_address`, connects it permanently to
    /// `peer_address`, and configures send/receive timeouts.
    ///
    /// The receive timeout is an absolute budget for correlated-response
    /// collection. Discarding uncorrelated datagrams never resets it.
    /// Send and receive each have their own budget, so one request cannot
    /// outlive roughly twice `timeout` even against saturated buffers.
    ///
    /// # Errors
    /// Returns [`IoStage::Bind`] failures from the socket constructor,
    /// [`IoStage::Connect`] failures from the one fixed-peer connection, and
    /// [`IoStage::ConfigureTimeout`] failures when a timeout cannot be applied
    /// (the standard library rejects a zero duration).
    pub fn bind(
        local_address: SocketAddr,
        peer_address: SocketAddr,
        timeout: Duration,
        initial_sequence: u16,
    ) -> Result<Self, ArcClientError> {
        let socket = UdpSocket::bind(local_address).map_err(|source| ArcClientError::Io {
            stage: IoStage::Bind,
            source,
        })?;
        socket
            .connect(peer_address)
            .map_err(|source| ArcClientError::Io {
                stage: IoStage::Connect,
                source,
            })?;
        let configure = |configured: io::Result<()>| {
            configured.map_err(|source| ArcClientError::Io {
                stage: IoStage::ConfigureTimeout,
                source,
            })
        };
        configure(socket.set_read_timeout(Some(timeout)))?;
        configure(socket.set_write_timeout(Some(timeout)))?;
        Ok(Self {
            socket,
            peer_address,
            response_timeout: timeout,
            sequence_allocator: SequenceAllocator::new(initial_sequence),
            receive_buffer: vec![0; RECEIVE_BUFFER_CAPACITY].into_boxed_slice(),
        })
    }

    /// The one peer this client talks to for its entire lifetime.
    pub const fn peer_address(&self) -> SocketAddr {
        self.peer_address
    }

    /// The device's Dante name.
    pub fn device_name(&mut self) -> Result<String, ArcClientError> {
        self.request(|sequence| {
            let query = DeviceNameQuery::new(sequence);
            (query.encode(), move |frame: &[u8]| {
                query.decode_response(frame)
            })
        })
    }

    /// How many transmitter and receiver channels the device reports.
    pub fn channel_counts(&mut self) -> Result<ChannelCounts, ArcClientError> {
        self.request(|sequence| {
            let query = ChannelCountQuery::new(sequence);
            (query.encode(), move |frame: &[u8]| {
                query.decode_response(frame)
            })
        })
    }

    /// One page of transmitter channels starting at `first_channel`. An
    /// empty page means the device has no channels from there on.
    pub fn transmitter_page(
        &mut self,
        first_channel: NonZeroU16,
    ) -> Result<Vec<TransmitterChannel>, ArcClientError> {
        self.request(|sequence| {
            let query = TransmitterPageQuery::new(sequence, first_channel);
            (query.encode(), move |frame: &[u8]| {
                query.decode_response(frame)
            })
        })
    }

    /// Every transmitter channel on the device, in ascending order.
    ///
    /// Requests pages until the device returns an empty one.
    pub fn transmitter_channels(&mut self) -> Result<Vec<TransmitterChannel>, ArcClientError> {
        let mut channels: Vec<TransmitterChannel> = Vec::new();
        let mut next = NonZeroU16::MIN;
        loop {
            let page = self.transmitter_page(next)?;
            let Some(last) = page.last().map(TransmitterChannel::number) else {
                break;
            };
            channels.extend(page);
            match last.checked_add(1).and_then(NonZeroU16::new) {
                Some(following) => next = following,
                None => break,
            }
        }
        Ok(channels)
    }

    /// The 16-channel page of receiver subscriptions containing `channel`.
    pub fn receiver_page(
        &mut self,
        channel: ReceiverChannel,
    ) -> Result<ReceiverSubscriptionPage, ArcClientError> {
        self.request(|sequence| {
            let query = ReceiverPageQuery::containing(sequence, channel);
            (query.encode().to_vec(), move |frame: &[u8]| {
                query.decode_response(frame)
            })
        })
    }

    /// Every receiver channel's subscription, in ascending channel order.
    ///
    /// Requests 16-channel pages until one comes back short.
    pub fn receiver_subscriptions(&mut self) -> Result<Vec<ReceiverSubscription>, ArcClientError> {
        let mut subscriptions = Vec::new();
        let mut next = Some(ReceiverChannel::new(1).expect("one is nonzero"));
        while let Some(channel) = next {
            let page = self.receiver_page(channel)?.into_subscriptions();
            let full = page.len() == 16;
            subscriptions.extend(page);
            next = full
                .then(|| ReceiverPageQuery::containing(0, channel).next_first_channel())
                .flatten();
        }
        Ok(subscriptions)
    }

    /// Returns the current subscription of one receiver channel.
    ///
    /// # Errors
    /// See [`ArcClientError`]. A page that does not contain the channel
    /// (because the device has fewer channels) is a codec error.
    pub fn query_subscription(
        &mut self,
        receiver_channel: ReceiverChannel,
    ) -> Result<ReceiverSubscription, ArcClientError> {
        let page = self.request(|sequence| {
            let query = SubscriptionQuery::new(sequence, receiver_channel);
            (query.encode().to_vec(), move |frame: &[u8]| {
                query.decode_response(frame)
            })
        })?;
        page.into_subscriptions()
            .into_iter()
            .find(|entry| entry.receiver_channel() == receiver_channel)
            .ok_or(ArcClientError::RequestedSubscriptionMissing { receiver_channel })
    }

    /// Subscribes `receiver_channel` to `selector`, or clears its
    /// subscription when `selector` is `None`.
    ///
    /// Emits the exact Via `0x3010 / 0x1401` set/clear request, sends it
    /// once, and receives exactly one datagram from the immutable peer.
    /// Status `0x0001` is the only result represented as
    /// [`SubscriptionAcceptance`]; it confirms acceptance of the request,
    /// not eventual receiver state. Query the channel afterwards to confirm.
    pub fn apply_subscription(
        &mut self,
        receiver_channel: ReceiverChannel,
        selector: Option<&TransmitterSelector>,
    ) -> Result<SubscriptionAcceptance, ArcClientError> {
        let sequence = self.allocate_sequence()?;
        let request = SubscriptionWriteRequest::new(sequence, receiver_channel, selector)
            .map_err(ArcClientError::WriteCodec)?;
        let encoded = request.encode().map_err(ArcClientError::WriteCodec)?;
        self.send(&encoded)?;

        let receive_started = Instant::now();
        let (received, source) = self.receive_one_from(receive_started)?;
        if source != self.peer_address {
            return Err(ArcClientError::SourceMismatch {
                expected: self.peer_address,
                received: source,
            });
        }
        request
            .parse_acceptance(&self.receive_buffer[..received])
            .map_err(ArcClientError::Acceptance)
    }

    /// Allocates a sequence, lets `build` encode the request and produce its
    /// decoder, sends once, and decodes the first correlated reply.
    fn request<T, D>(
        &mut self,
        build: impl FnOnce(u16) -> (Vec<u8>, D),
    ) -> Result<T, ArcClientError>
    where
        D: Fn(&[u8]) -> Result<T, ArcCodecError>,
    {
        let sequence = self.allocate_sequence()?;
        let (request, decode) = build(sequence);
        self.send(&request)?;

        let receive_started = Instant::now();
        loop {
            let received = self.receive_one(receive_started)?;
            let frame = &self.receive_buffer[..received];
            if correlates(frame, sequence) {
                return decode(frame).map_err(ArcClientError::Codec);
            }
        }
    }

    fn allocate_sequence(&mut self) -> Result<u16, ArcClientError> {
        self.sequence_allocator
            .allocate()
            .ok_or(ArcClientError::SequenceExhausted)
    }

    fn send(&self, request: &[u8]) -> Result<(), ArcClientError> {
        let sent = self
            .socket
            .send(request)
            .map_err(|source| ArcClientError::Io {
                stage: IoStage::Send,
                source,
            })?;
        if sent != request.len() {
            return Err(ArcClientError::ShortSend {
                expected: request.len(),
                actual: sent,
            });
        }
        Ok(())
    }

    /// Reduces the socket timeout to what remains of the absolute budget.
    fn arm_receive(&self, receive_started: Instant) -> Result<(), ArcClientError> {
        let remaining = self
            .response_timeout
            .checked_sub(receive_started.elapsed())
            .filter(|remaining| !remaining.is_zero())
            .ok_or(ArcClientError::Timeout {
                stage: IoStage::Receive,
            })?;
        self.socket
            .set_read_timeout(Some(remaining))
            .map_err(|source| ArcClientError::Io {
                stage: IoStage::ConfigureTimeout,
                source,
            })
    }

    /// Receives one datagram and retains its kernel-reported source address.
    fn receive_one_from(
        &mut self,
        receive_started: Instant,
    ) -> Result<(usize, SocketAddr), ArcClientError> {
        self.arm_receive(receive_started)?;
        self.socket
            .recv_from(&mut self.receive_buffer)
            .map_err(receive_error)
    }

    /// Receives one datagram within the original request's response budget.
    ///
    /// Before every receive, the socket timeout is reduced to the remaining
    /// absolute budget. A stream of stale datagrams therefore cannot extend the
    /// request indefinitely.
    fn receive_one(&mut self, receive_started: Instant) -> Result<usize, ArcClientError> {
        self.arm_receive(receive_started)?;
        self.socket
            .recv(&mut self.receive_buffer)
            .map_err(receive_error)
    }
}

fn receive_error(source: io::Error) -> ArcClientError {
    if matches!(source.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) {
        ArcClientError::Timeout {
            stage: IoStage::Receive,
        }
    } else {
        ArcClientError::Io {
            stage: IoStage::Receive,
            source,
        }
    }
}

/// Stage of the one-request/one-response exchange at which a failure
/// occurred.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum IoStage {
    /// Binding the local socket failed.
    Bind,
    /// Applying the receive timeout failed.
    ConfigureTimeout,
    /// Connecting the socket to the target address failed.
    Connect,
    /// Sending the encoded request failed.
    Send,
    /// Receiving the correlated response failed or timed out.
    Receive,
}

/// Every way an [`ArcClient`] operation can fail.
///
/// Failures at [`IoStage::Receive`] (including [`ArcClientError::Timeout`])
/// happen after the request was sent: for a write, the device may or may
/// not have applied it.
#[derive(Debug)]
pub enum ArcClientError {
    /// Every 16-bit sequence value has already been consumed by this socket;
    /// no further datagram is sent.
    SequenceExhausted,
    /// An I/O operation failed at the named stage. Every non-timeout I/O
    /// error is preserved verbatim — never matched on strings or rewritten.
    Io { stage: IoStage, source: io::Error },
    /// The absolute response deadline expired before a correlated datagram
    /// arrived. Uncorrelated same-peer datagrams may have been discarded first.
    /// Also covers `WouldBlock`, which nonblocking sockets produce in place of
    /// `TimedOut`.
    Timeout { stage: IoStage },
    /// A sequence-correlated reply failed codec validation.
    Codec(ArcCodecError),
    /// The write request could not be constructed or encoded.
    WriteCodec(SubscriptionWriteCodecError),
    /// The write reply failed its exact ten-byte acceptance validation.
    Acceptance(SubscriptionAcceptanceError),
    /// The decoded page did not contain the exact requested receiver channel.
    RequestedSubscriptionMissing { receiver_channel: ReceiverChannel },
    /// The socket sent fewer bytes than the encoded request.
    ShortSend { expected: usize, actual: usize },
    /// The received write reply was reported by an address other than the
    /// client's immutable peer.
    SourceMismatch {
        expected: SocketAddr,
        received: SocketAddr,
    },
}

impl fmt::Display for ArcClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { stage, source } => {
                write!(formatter, "{stage} failed: {source}")
            }
            Self::Timeout { stage } => {
                write!(
                    formatter,
                    "no correlated response arrived before the \
                                   configured timeout expired ({stage})"
                )
            }
            Self::SequenceExhausted => write!(
                formatter,
                "all 65,536 wire sequence values have been consumed; refusing to send"
            ),
            Self::Codec(reason) => write!(formatter, "codec rejected the response: {reason}"),
            Self::WriteCodec(reason) => {
                write!(formatter, "write codec rejected the request: {reason}")
            }
            Self::Acceptance(reason) => write!(formatter, "write acceptance rejected: {reason}"),
            Self::RequestedSubscriptionMissing { receiver_channel } => write!(
                formatter,
                "decoded page does not contain requested receiver channel {}",
                receiver_channel.value()
            ),
            Self::ShortSend { expected, actual } => write!(
                formatter,
                "socket sent {actual} of the {expected} request bytes"
            ),
            Self::SourceMismatch { expected, received } => write!(
                formatter,
                "write reply source {received} does not match configured peer {expected}"
            ),
        }
    }
}

impl StdError for ArcClientError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Codec(reason) => Some(reason),
            Self::WriteCodec(reason) => Some(reason),
            Self::Acceptance(reason) => Some(reason),
            Self::SequenceExhausted
            | Self::Timeout { .. }
            | Self::RequestedSubscriptionMissing { .. }
            | Self::ShortSend { .. }
            | Self::SourceMismatch { .. } => None,
        }
    }
}

impl fmt::Display for IoStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Bind => "bind",
            Self::ConfigureTimeout => "configure-timeout",
            Self::Connect => "connect",
            Self::Send => "send",
            Self::Receive => "receive",
        };
        formatter.write_str(name)
    }
}

#[cfg(test)]
mod tests {
    use super::SequenceAllocator;
    use std::collections::HashSet;

    #[test]
    fn sequence_allocator_consumes_the_full_wrapping_space_once() {
        let initial = 0xFFFE;
        let mut allocator = SequenceAllocator::new(initial);
        let mut seen = HashSet::with_capacity(usize::from(u16::MAX) + 1);

        for _ in 0..=u16::MAX {
            let sequence = allocator.allocate().expect("space remains");
            assert!(seen.insert(sequence), "sequence {sequence} was reused");
        }

        assert_eq!(seen.len(), usize::from(u16::MAX) + 1);
        assert_eq!(allocator.allocate(), None);
    }
}
