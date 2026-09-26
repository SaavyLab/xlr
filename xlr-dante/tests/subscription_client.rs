//! Contract tests for the synchronous subscription-query client over real
//! loopback UDP sockets with synthetic responses. No captures, no LAN.

use std::error::Error as _;
use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;
use xlr_dante::client::{ArcClient, ArcClientError, IoStage};
use xlr_dante::error::ArcCodecError;
use xlr_dante::model::{ReceiverChannel, SubscriptionState, TransmitterSelector};

const QUERY_MAGIC: u16 = 0x280F;
const QUERY_REPLY_MAGIC: u16 = 0x2801;
const QUERY_COMMAND: u16 = 0x3000;
const HEADER: usize = 12;
const RECORD: usize = 20;

fn channel(value: u16) -> ReceiverChannel {
    ReceiverChannel::new(value).expect("test channel in range")
}

fn timeout() -> Duration {
    Duration::from_secs(2)
}

/// Binds a synthetic device on the loopback that records every request
/// datagram it receives and can answer each one from a scripted queue.
struct SyntheticDevice {
    socket: UdpSocket,
}

impl SyntheticDevice {
    fn bind() -> Self {
        let socket = UdpSocket::bind(("127.0.0.1", 0)).expect("synthetic server binds");
        socket
            .set_read_timeout(Some(timeout()))
            .expect("timeout settable");
        Self { socket }
    }

    fn local_address(&self) -> SocketAddr {
        self.socket.local_addr().expect("local address")
    }

    /// Receives one request datagram carrying exactly `expected_sequence`
    /// and returns its bytes plus the sender.
    ///
    /// Anything else is skipped: strays on the wire (including look-alike
    /// queries emitted by other processes on this host) must never satisfy
    /// a contract capture.
    fn receive_request(&self, expected_sequence: u16) -> (Vec<u8>, SocketAddr) {
        let mut buffer = vec![0u8; u16::MAX as usize + 1];
        loop {
            let (received, sender) = self
                .socket
                .recv_from(&mut buffer)
                .expect("synthetic server receives request");
            let request = &buffer[..received];
            if request.len() == 16
                && raw_word(request, 0) == QUERY_MAGIC
                && raw_word(request, 2) == expected_sequence
            {
                return (request.to_vec(), sender);
            }
        }
    }

    fn assert_no_request(&self) {
        self.socket
            .set_read_timeout(Some(Duration::from_millis(100)))
            .expect("probe timeout settable");
        let mut buffer = [0u8; 64];
        let received = self.socket.recv_from(&mut buffer);
        assert!(matches!(
            &received,
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
        ));
    }
}

/// Binds a client on an ephemeral loopback port chosen by the OS, which
/// never races another process for a probe-revealed port.
fn client(peer_address: SocketAddr, sequence: u16) -> ArcClient {
    ArcClient::bind(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        peer_address,
        timeout(),
        sequence,
    )
    .expect("client binds")
}

// Shared response builder -----------------------------------------------

struct Entry {
    device: Option<&'static str>,
    source_channel: Option<&'static str>,
}

impl Entry {
    fn unsubscribed() -> Self {
        Self {
            device: None,
            source_channel: None,
        }
    }
    fn subscribed(device: &'static str, source_channel: &'static str) -> Self {
        Self {
            device: Some(device),
            source_channel: Some(source_channel),
        }
    }
}

/// Builds a conforming synthetic response page (same wire layout as the
/// codec tests use).
fn synthetic_page(
    sequence: u16,
    total_channels: u8,
    entries: &[Entry],
    start_channel: u16,
) -> Vec<u8> {
    assert!(!entries.is_empty() && entries.len() <= 16);
    let mut frame = Vec::new();
    let count_word = ((total_channels as u16) << 8) | entries.len() as u16;
    frame.extend(u16::to_be_bytes(QUERY_REPLY_MAGIC));
    frame.extend(0u16.to_be_bytes()); // declared length, patched below
    frame.extend(sequence.to_be_bytes());
    frame.extend(QUERY_COMMAND.to_be_bytes());
    frame.extend(0u16.to_be_bytes());
    frame.extend(count_word.to_be_bytes());
    for index in 0..entries.len() {
        frame.extend((start_channel + index as u16).to_be_bytes());
        frame.extend([0u8; RECORD - 2]);
    }
    for (index, entry) in entries.iter().enumerate() {
        let base = HEADER + index * RECORD;
        if let Some(device) = entry.device {
            let offset = frame.len();
            frame.extend_from_slice(device.as_bytes());
            frame.push(0);
            frame[base + 8..base + 10].copy_from_slice(&(offset as u16).to_be_bytes());
        }
        if let Some(source_channel) = entry.source_channel {
            let offset = frame.len();
            frame.extend_from_slice(source_channel.as_bytes());
            frame.push(0);
            frame[base + 6..base + 8].copy_from_slice(&(offset as u16).to_be_bytes());
        }
    }
    for index in 0..entries.len() {
        let base = HEADER + index * RECORD;
        let end_offset = frame.len() as u16;
        frame[base + 18..base + 20].copy_from_slice(&end_offset.to_be_bytes());
    }
    let declared = frame.len() as u16;
    frame[2..4].copy_from_slice(&declared.to_be_bytes());
    frame
}

fn raw_word(frame: &[u8], word_index: usize) -> u16 {
    u16::from_be_bytes([frame[word_index * 2], frame[word_index * 2 + 1]])
}

// Request reaches the configured peer with expected sequence/page start.

#[test]
fn request_reaches_peer_with_expected_sequence_and_page_start() {
    let device = SyntheticDevice::bind();
    let address = device.local_address();
    let mut client = client(address, 0xAB12);

    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            client
                .query_subscription(channel(3))
                .expect_err("correlated truncated reply fails to decode")
        });
        // The query arrived at the configured synthetic device from a real
        // loopback source; the source port is whatever the OS assigned.
        let (request, sender) = device.receive_request(0xAB12);
        assert!(sender.ip().is_loopback());
        assert_ne!(sender.port(), 0);
        assert_eq!(request.len(), 16);
        assert_eq!(raw_word(&request, 0), QUERY_MAGIC);
        assert_eq!(raw_word(&request, 2), 0xAB12);
        assert_eq!(raw_word(&request, 6), 1); // page start for channel 3
        // Six bytes establish sequence correlation but remain shorter than the
        // complete ARC response header.
        let truncated = [0x28, 0x01, 0, 6, 0xAB, 0x12];
        device
            .socket
            .send_to(&truncated, sender)
            .expect("reply sent");
        let error = worker.join().expect("worker finishes");
        assert!(matches!(
            &error,
            ArcClientError::Codec(ArcCodecError::TruncatedFrame {
                required: HEADER,
                available: 6,
            })
        ));
    });
}

// A multi-entry page returns the requested channel, not the first.

#[test]
fn multi_entry_page_returns_the_requested_channel_not_the_first() {
    let device = SyntheticDevice::bind();
    let address = device.local_address();
    let mut client = client(address, 42);
    let reply = synthetic_page(
        42,
        64,
        &[
            Entry::subscribed("SYNTH-DEV-C", "LEFT"),
            Entry::unsubscribed(),
            Entry::subscribed("SYNTH-DEV-D", "RIGHT"),
            Entry::unsubscribed(),
        ],
        1,
    );

    std::thread::scope(|scope| {
        let worker = scope.spawn(|| client.query_subscription(channel(3)).expect("entry"));
        let (_, sender) = device.receive_request(42);
        device.socket.send_to(&reply, sender).expect("reply sent");
        let entry = worker.join().expect("worker finishes");
        assert_eq!(entry.receiver_channel(), channel(3));
        assert_eq!(
            entry.state(),
            &SubscriptionState::Subscribed(
                TransmitterSelector::new("SYNTH-DEV-D".to_owned(), "RIGHT".to_owned())
                    .expect("valid test names")
            )
        );
    });
}

// Two consecutive queries use consecutive sequences.

#[test]
fn consecutive_queries_use_consecutive_sequences() {
    let device = SyntheticDevice::bind();
    let address = device.local_address();
    let mut client = client(address, 7);

    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            for _ in 0..2 {
                client
                    .query_subscription(channel(1))
                    .expect("both queries succeed");
            }
        });
        for expected_sequence in [7u16, 8u16] {
            let (request, sender) = device.receive_request(expected_sequence);
            assert_eq!(raw_word(&request, 2), expected_sequence);
            let reply = synthetic_page(expected_sequence, 16, &[Entry::unsubscribed()], 1);
            device.socket.send_to(&reply, sender).expect("reply sent");
        }
        worker.join().expect("worker finishes");
    });
}

#[test]
fn stale_response_from_prior_query_is_discarded() {
    let device = SyntheticDevice::bind();
    let address = device.local_address();
    let mut client = client(address, 7);

    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let first = client
                .query_subscription(channel(1))
                .expect("first query succeeds");
            let second = client
                .query_subscription(channel(1))
                .expect("second query ignores stale response");
            (first, second)
        });

        let (_, sender) = device.receive_request(7);
        let stale = synthetic_page(7, 16, &[Entry::unsubscribed()], 1);
        device.socket.send_to(&stale, sender).expect("reply sent");
        // Queue a duplicate after the correlated response. It belongs to the
        // completed first query and must not poison the next one.
        device
            .socket
            .send_to(&stale, sender)
            .expect("stale duplicate sent");

        let (second_request, second_sender) = device.receive_request(8);
        assert_eq!(raw_word(&second_request, 2), 8);
        device
            .socket
            .send_to(
                &synthetic_page(8, 16, &[Entry::unsubscribed()], 1),
                second_sender,
            )
            .expect("second reply sent");

        let (first, second) = worker.join().expect("worker finishes");
        assert_eq!(first.receiver_channel(), channel(1));
        assert_eq!(second.receiver_channel(), channel(1));
    });
}

#[test]
fn wrong_sequence_with_invalid_magic_is_discarded_before_decode() {
    let device = SyntheticDevice::bind();
    let address = device.local_address();
    let mut client = client(address, 100);

    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            client
                .query_subscription(channel(1))
                .expect("matching response follows malformed stale noise")
        });

        let (_, sender) = device.receive_request(100);
        let mut stale = synthetic_page(99, 16, &[Entry::unsubscribed()], 1);
        stale[0..2].copy_from_slice(&0x9999u16.to_be_bytes());
        device
            .socket
            .send_to(&stale, sender)
            .expect("malformed stale response sent");
        device
            .socket
            .send_to(
                &synthetic_page(100, 16, &[Entry::unsubscribed()], 1),
                sender,
            )
            .expect("matching response sent");

        worker.join().expect("worker finishes");
        device.assert_no_request();
    });
}

#[test]
fn too_short_noise_is_discarded_before_full_decode() {
    let device = SyntheticDevice::bind();
    let address = device.local_address();
    let mut client = client(address, 101);

    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            client
                .query_subscription(channel(1))
                .expect("matching response follows short noise")
        });

        let (_, sender) = device.receive_request(101);
        device
            .socket
            .send_to(&[0x28], sender)
            .expect("too-short noise sent");
        device
            .socket
            .send_to(
                &synthetic_page(101, 16, &[Entry::unsubscribed()], 1),
                sender,
            )
            .expect("matching response sent");

        worker.join().expect("worker finishes");
        device.assert_no_request();
    });
}

#[test]
fn stale_datagram_stream_does_not_reset_the_absolute_response_deadline() {
    let device = SyntheticDevice::bind();
    let address = device.local_address();
    let mut client = ArcClient::bind(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        address,
        Duration::from_millis(200),
        100,
    )
    .expect("client binds");

    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let started = std::time::Instant::now();
            let error = client
                .query_subscription(channel(1))
                .expect_err("only stale responses must time out");
            (error, started.elapsed())
        });

        let (_, sender) = device.receive_request(100);
        let stale = synthetic_page(99, 16, &[Entry::unsubscribed()], 1);
        for _ in 0..6 {
            std::thread::sleep(Duration::from_millis(50));
            device
                .socket
                .send_to(&stale, sender)
                .expect("stale response sent");
        }

        let (error, elapsed) = worker.join().expect("worker finishes");
        assert!(matches!(
            error,
            ArcClientError::Timeout {
                stage: IoStage::Receive
            }
        ));
        // A per-datagram reset would run for roughly 500 ms. The original
        // 200 ms budget must expire despite the continuing stale stream.
        assert!(elapsed >= Duration::from_millis(100));
        assert!(elapsed < Duration::from_millis(400));
    });
}

// Sequence wraps from u16::MAX to 0.

#[test]
fn sequence_wraps_from_max_to_zero() {
    let device = SyntheticDevice::bind();
    let address = device.local_address();
    let mut client = client(address, u16::MAX);

    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            for _ in 0..2 {
                client
                    .query_subscription(channel(1))
                    .expect("both wrap queries succeed");
            }
        });
        for expected_sequence in [u16::MAX, 0u16] {
            let (request, sender) = device.receive_request(expected_sequence);
            assert_eq!(raw_word(&request, 2), expected_sequence);
            let reply = synthetic_page(expected_sequence, 16, &[Entry::unsubscribed()], 1);
            device.socket.send_to(&reply, sender).expect("reply sent");
        }
        worker.join().expect("worker finishes");
    });
}

// Malformed response preserves ArcCodecError.

#[test]
fn malformed_response_preserves_arc_codec_error() {
    let device = SyntheticDevice::bind();
    let address = device.local_address();
    let mut client = client(address, 99);
    // Correct sequence but invalid envelope magic: this is a correlated
    // malformed response, not a stale datagram.
    let mut reply = synthetic_page(99, 16, &[Entry::unsubscribed()], 1);
    reply[0..2].copy_from_slice(&0x9999u16.to_be_bytes());

    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            client
                .query_subscription(channel(1))
                .expect_err("invalid magic fails")
        });
        let (_, sender) = device.receive_request(99);
        device.socket.send_to(&reply, sender).expect("reply sent");

        let error = worker.join().expect("worker finishes");
        assert!(matches!(
            &error,
            ArcClientError::Codec(ArcCodecError::InvalidMagic { found: 0x9999 })
        ));
        assert_eq!(
            error.source().map(|source| source.to_string()),
            Some(String::from("unrecognized reply magic 0x9999"))
        );
    });
}

// Receive timeout returns the typed timeout error.

#[test]
fn receive_timeout_returns_typed_timeout_error() {
    // No server bound at all: nothing ever answers this port.
    let quiet = UdpSocket::bind(("127.0.0.1", 0)).expect("quiet socket binds");
    let address = quiet.local_addr().expect("quiet address");

    let mut client = ArcClient::bind(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        address,
        Duration::from_millis(50),
        1,
    )
    .expect("client binds");
    let started = std::time::Instant::now();
    let error = client
        .query_subscription(channel(1))
        .expect_err("nothing answers the query");
    assert!(matches!(
        error,
        ArcClientError::Timeout {
            stage: IoStage::Receive
        }
    ));
    // Bounded: well under any CI stall, and observably not immediate.
    assert!(started.elapsed() >= Duration::from_millis(25));
    assert!(started.elapsed() < timeout());
}

// Deterministic I/O-stage retention and typed cross-platform outcomes.

#[test]
fn bind_failure_retains_bind_stage() {
    // Binding twice to one port makes the second bind fail with AddrInUse,
    // deterministically on loopback.
    let held = UdpSocket::bind(("127.0.0.1", 0)).expect("held socket binds");
    let address = held.local_addr().expect("held address");
    let error = match ArcClient::bind(address, SocketAddr::from(([127, 0, 0, 1], 9)), timeout(), 1)
    {
        Ok(_) => panic!("second bind of a held port must fail"),
        Err(error) => error,
    };
    match &error {
        ArcClientError::Io {
            stage: IoStage::Bind,
            source,
        } => {
            assert_eq!(source.kind(), ErrorKind::AddrInUse);
        }
        other => panic!("expected bind-stage I/O error, got {other:?}"),
    }
}

#[test]
fn closed_peer_failure_remains_typed() {
    // Connect while the peer exists, then close it before the first write.
    let closed = UdpSocket::bind(("127.0.0.1", 0)).expect("closed-port holder binds");
    let closed_address = closed.local_addr().expect("closed address");
    let mut client = client(closed_address, 1);
    drop(closed); // now nobody is bound there

    let error = client
        .query_subscription(channel(1))
        .expect_err("closed peer port must eventually fail");
    match error {
        ArcClientError::Io {
            stage: IoStage::Send,
            ..
        } => {}
        ArcClientError::Io {
            stage: IoStage::Receive,
            source,
        } => {
            assert_eq!(source.kind(), ErrorKind::ConnectionRefused);
        }
        ArcClientError::Timeout {
            stage: IoStage::Receive,
        } => {
            // macOS/Windows may swallow ICMP-derived refusal into a timeout;
            // then the error is still non-crash and typed.
        }
        other => panic!("unexpected error shape for closed peer: {other:?}"),
    }
}

// A zero timeout fails at the configuration stage.

#[test]
fn zero_duration_timeout_fails_at_configure_stage() {
    let error = match ArcClient::bind(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        SocketAddr::from(([127, 0, 0, 1], 9)),
        Duration::ZERO,
        1,
    ) {
        Ok(_) => panic!("Duration::ZERO must be rejected as a socket timeout"),
        Err(error) => error,
    };
    match &error {
        ArcClientError::Io {
            stage: IoStage::ConfigureTimeout,
            source,
        } => assert_eq!(source.kind(), ErrorKind::InvalidInput),
        other => panic!("expected configure-timeout I/O error, got {other:?}"),
    }
}

// An attempted-but-failed request still consumes its sequence.

#[test]
fn failed_attempt_still_consumes_one_sequence() {
    let device = SyntheticDevice::bind();
    let address = device.local_address();
    let mut client = client(address, 7);

    // One worker performs both queries back to back; the test thread plays
    // the device and captures each request between replies.
    std::thread::scope(|scope| {
        let outcome = scope.spawn(move || {
            let error = client
                .query_subscription(channel(1))
                .expect_err("correlated malformed reply must fail");
            let entry = client
                .query_subscription(channel(1))
                .expect("second query succeeds");
            (error, entry)
        });

        // First query: serve a correlated response with invalid magic.
        let (_, sender) = device.receive_request(7);
        let mut malformed = synthetic_page(7, 16, &[Entry::unsubscribed()], 1);
        malformed[0..2].copy_from_slice(&0x9999u16.to_be_bytes());
        device
            .socket
            .send_to(&malformed, sender)
            .expect("reply sent");

        // Second query must carry sequence 8, not the consumed 7.
        let (request, sender) = device.receive_request(8);
        assert_eq!(raw_word(&request, 2), 8);
        device
            .socket
            .send_to(&synthetic_page(8, 16, &[Entry::unsubscribed()], 1), sender)
            .expect("reply sent");

        let (error, entry) = outcome.join().expect("worker finishes");
        assert!(matches!(
            &error,
            ArcClientError::Codec(ArcCodecError::InvalidMagic { found: 0x9999 })
        ));
        assert_eq!(entry.receiver_channel(), channel(1));
    });
}
