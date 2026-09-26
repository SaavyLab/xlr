//! Synthetic tests for the device-info and transmitter-listing codecs, and
//! for the client's paging over loopback. Frames mirror the observed layouts;
//! every byte and name is constructed here.

use std::net::{SocketAddr, UdpSocket};
use std::num::NonZeroU16;
use std::time::Duration;
use xlr_dante::arc::device::{ChannelCountQuery, DeviceNameQuery};
use xlr_dante::arc::transmitters::TransmitterPageQuery;
use xlr_dante::error::{ArcCodecError, RecordLayoutError, StringEncodingError};
use xlr_dante::model::{ChannelCounts, SubscriptionState};
use xlr_dante::{ArcClient, ArcClientError};

fn words(values: &[u16]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_be_bytes())
        .collect()
}

fn word(frame: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([frame[offset], frame[offset + 1]])
}

fn patch_length(mut frame: Vec<u8>) -> Vec<u8> {
    let length = frame.len() as u16;
    frame[2..4].copy_from_slice(&length.to_be_bytes());
    frame
}

fn name_reply(magic: u16, sequence: u16, name: &[u8]) -> Vec<u8> {
    let mut frame = words(&[magic, 0, sequence, 0x1002, 0x0001]);
    frame.extend_from_slice(name);
    frame.push(0);
    patch_length(frame)
}

fn counts_reply(magic: u16, sequence: u16, transmitters: u16, receivers: u16) -> Vec<u8> {
    let mut frame = words(&[magic, 0, sequence, 0x1000, 0x0001, 0x1df9]);
    frame.extend(words(&[transmitters, receivers, 0, 2, 0x0008, 0x0002]));
    frame.extend([0u8; 20]);
    patch_length(frame)
}

/// A transmitter page: 8-byte records, then a format block, then names.
fn transmitter_reply(sequence: u16, first: u16, names: &[&str]) -> Vec<u8> {
    let count = names.len() as u16;
    let mut frame = words(&[0x2801, 0, sequence, 0x2000, 0x0001, (count << 8) | count]);
    let table_end = frame.len() + names.len() * 8;
    let format_offset = table_end as u16;
    let mut pool = words(&[0x0000, 0xbb80, 0x0101, 0x0018]);
    let mut name_offsets = Vec::new();
    for name in names {
        name_offsets.push((table_end + pool.len()) as u16);
        pool.extend_from_slice(name.as_bytes());
        pool.push(0);
    }
    for (index, offset) in name_offsets.iter().enumerate() {
        frame.extend(words(&[
            first + index as u16,
            0x0007,
            format_offset,
            *offset,
        ]));
    }
    frame.extend(pool);
    patch_length(frame)
}

#[test]
fn device_name_request_matches_observed_shape() {
    let encoded = DeviceNameQuery::new(0x710c).encode();
    assert_eq!(encoded, words(&[0x280f, 10, 0x710c, 0x1002, 0]));
}

#[test]
fn device_name_decodes_under_either_observed_reply_magic() {
    let query = DeviceNameQuery::new(7);
    for magic in [0x280f, 0x2809] {
        assert_eq!(
            query.decode_response(&name_reply(magic, 7, b"stage-box")),
            Ok("stage-box".to_owned())
        );
    }
    assert_eq!(
        query.decode_response(&name_reply(0x2801, 7, b"stage-box")),
        Err(ArcCodecError::UnsupportedVariant { found: 0x2801 })
    );
}

#[test]
fn device_name_rejects_bad_status_padding_and_encoding() {
    let query = DeviceNameQuery::new(7);
    let mut failed = name_reply(0x280f, 7, b"stage-box");
    failed[8..10].copy_from_slice(&0x0002u16.to_be_bytes());
    assert_eq!(
        query.decode_response(&failed),
        Err(ArcCodecError::UnexpectedStatus {
            found: 2,
            expected: 1
        })
    );

    let mut padded = name_reply(0x280f, 7, b"stage-box");
    padded.push(0);
    let padded = patch_length(padded);
    assert!(matches!(
        query.decode_response(&padded),
        Err(ArcCodecError::LengthMismatch { .. })
    ));

    assert!(matches!(
        query.decode_response(&name_reply(0x280f, 7, b"")),
        Err(ArcCodecError::InvalidStringEncoding(
            StringEncodingError::EmptyName { .. }
        ))
    ));
    assert!(matches!(
        query.decode_response(&name_reply(0x280f, 7, "caf\u{e9}".as_bytes())),
        Err(ArcCodecError::InvalidStringEncoding(
            StringEncodingError::NonAscii { .. }
        ))
    ));
}

#[test]
fn channel_counts_decode_transmitters_then_receivers() {
    let query = ChannelCountQuery::new(3);
    assert_eq!(query.encode(), words(&[0x280f, 10, 3, 0x1000, 0]));
    assert_eq!(
        query.decode_response(&counts_reply(0x2809, 3, 2, 8)),
        Ok(ChannelCounts {
            transmitters: 2,
            receivers: 8
        })
    );
    let short = patch_length(counts_reply(0x2809, 3, 2, 8)[..14].to_vec());
    assert!(matches!(
        query.decode_response(&short),
        Err(ArcCodecError::TruncatedFrame { required: 16, .. })
    ));
}

#[test]
fn transmitter_page_request_carries_the_first_channel() {
    let encoded = TransmitterPageQuery::new(0x4347, NonZeroU16::new(17).unwrap()).encode();
    assert_eq!(encoded, words(&[0x280f, 16, 0x4347, 0x2000, 0, 1, 17, 0]));
}

#[test]
fn transmitter_page_decodes_names_in_order() {
    let query = TransmitterPageQuery::new(9, NonZeroU16::new(5).unwrap());
    let channels = query
        .decode_response(&transmitter_reply(9, 5, &["Mic 5", "Mic 6"]))
        .expect("well-formed page");
    let summary: Vec<_> = channels
        .iter()
        .map(|channel| (channel.number(), channel.name()))
        .collect();
    assert_eq!(summary, [(5, "Mic 5"), (6, "Mic 6")]);
}

#[test]
fn transmitter_page_accepts_an_empty_page() {
    let query = TransmitterPageQuery::new(9, NonZeroU16::new(33).unwrap());
    assert_eq!(
        query.decode_response(&transmitter_reply(9, 33, &[])),
        Ok(vec![])
    );
}

#[test]
fn transmitter_page_rejects_out_of_sequence_channels_and_missing_names() {
    let query = TransmitterPageQuery::new(9, NonZeroU16::new(1).unwrap());
    let mut skipped = transmitter_reply(9, 1, &["A", "B"]);
    skipped[20..22].copy_from_slice(&3u16.to_be_bytes());
    assert_eq!(
        query.decode_response(&skipped),
        Err(ArcCodecError::InvalidRecordLayout(
            RecordLayoutError::EntryChannelId {
                found: 3,
                expected: 2
            }
        ))
    );

    let mut unnamed = transmitter_reply(9, 1, &["A"]);
    unnamed[18..20].copy_from_slice(&0u16.to_be_bytes());
    assert!(matches!(
        query.decode_response(&unnamed),
        Err(ArcCodecError::InvalidStringEncoding(
            StringEncodingError::EmptyName { channel: 1, .. }
        ))
    ));

    let mut truncated = transmitter_reply(9, 1, &["A", "B"]);
    truncated[11] = 40;
    assert!(matches!(
        query.decode_response(&truncated),
        Err(ArcCodecError::TruncatedFrame { .. })
    ));
}

/// Answers each request on a loopback socket with `reply(request)`.
fn serve(device: &UdpSocket, replies: usize, reply: impl Fn(&[u8]) -> Vec<u8>) {
    let mut buffer = [0u8; 2048];
    for _ in 0..replies {
        let (length, sender) = device.recv_from(&mut buffer).expect("request arrives");
        device
            .send_to(&reply(&buffer[..length]), sender)
            .expect("reply sent");
    }
}

fn loopback() -> (UdpSocket, ArcClient) {
    let device = UdpSocket::bind(("127.0.0.1", 0)).expect("device binds");
    device
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let client = ArcClient::bind(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        device.local_addr().unwrap(),
        Duration::from_secs(2),
        100,
    )
    .expect("client binds");
    (device, client)
}

#[test]
fn transmitter_channels_page_until_an_empty_page() {
    let (device, mut client) = loopback();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| client.transmitter_channels());
        serve(&device, 3, |request| {
            let sequence = word(request, 4);
            match word(request, 12) {
                1 => transmitter_reply(sequence, 1, &["A", "B"]),
                3 => transmitter_reply(sequence, 3, &["C"]),
                4 => transmitter_reply(sequence, 4, &[]),
                other => panic!("unexpected page start {other}"),
            }
        });
        let channels = worker.join().unwrap().expect("listing succeeds");
        let names: Vec<_> = channels.iter().map(|channel| channel.name()).collect();
        assert_eq!(names, ["A", "B", "C"]);
    });
}

/// A receiver page with `count` unsubscribed, unnamed entries.
fn receiver_reply(sequence: u16, first: u16, count: u16) -> Vec<u8> {
    let mut frame = words(&[0x2801, 0, sequence, 0x3000, 0x0001, (16 << 8) | count]);
    for index in 0..count {
        frame.extend(words(&[first + index, 0x0406, 0, 0, 0, 0, 0, 0, 0, 0]));
    }
    patch_length(frame)
}

#[test]
fn receiver_subscriptions_page_until_a_short_page() {
    let (device, mut client) = loopback();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| client.receiver_subscriptions());
        serve(&device, 2, |request| {
            let sequence = word(request, 4);
            match word(request, 12) {
                1 => receiver_reply(sequence, 1, 16),
                17 => receiver_reply(sequence, 17, 4),
                other => panic!("unexpected page start {other}"),
            }
        });
        let subscriptions = worker.join().unwrap().expect("listing succeeds");
        assert_eq!(subscriptions.len(), 20);
        assert_eq!(subscriptions[19].receiver_channel().value(), 20);
        assert!(
            subscriptions
                .iter()
                .all(|entry| *entry.state() == SubscriptionState::Unsubscribed)
        );
    });
}

#[test]
fn device_queries_round_trip_over_loopback() {
    let (device, mut client) = loopback();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| (client.device_name(), client.channel_counts()));
        serve(&device, 2, |request| {
            let sequence = word(request, 4);
            match word(request, 6) {
                0x1002 => name_reply(0x280f, sequence, b"stage-box"),
                0x1000 => counts_reply(0x280f, sequence, 16, 16),
                other => panic!("unexpected command {other:#x}"),
            }
        });
        let (name, counts) = worker.join().unwrap();
        assert_eq!(name.expect("name"), "stage-box");
        assert_eq!(counts.expect("counts").transmitters, 16);
    });
}

#[test]
fn codec_failure_surfaces_as_client_error() {
    let (device, mut client) = loopback();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| client.device_name());
        serve(&device, 1, |request| {
            name_reply(0x280f, word(request, 4), b"")
        });
        assert!(matches!(
            worker.join().unwrap(),
            Err(ArcClientError::Codec(ArcCodecError::InvalidStringEncoding(
                _
            )))
        ));
    });
}
