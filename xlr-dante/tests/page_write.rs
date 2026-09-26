//! The paged `0x3410` subscription write, checked word-for-word against the
//! layout observed from a Dante AVIO adapter. Names are synthetic.

use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;
use xlr_dante::arc::page_write::PageSubscriptionWrite;
use xlr_dante::model::{ReceiverChannel, TransmitterSelector};
use xlr_dante::{
    ArcClient, ArcClientError, NameErrorReason, SubscriptionAcceptanceError,
    SubscriptionWriteCodecError,
};

fn words(values: &[u16]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_be_bytes())
        .collect()
}

fn channel(value: u16) -> ReceiverChannel {
    ReceiverChannel::new(value).unwrap()
}

fn selector(device: &str, channel: &str) -> TransmitterSelector {
    TransmitterSelector::new(device.to_owned(), channel.to_owned()).unwrap()
}

#[test]
fn clear_matches_the_observed_two_channel_layout() {
    let request = PageSubscriptionWrite::new(0x2809, 2, 0x1234, channel(1), None).unwrap();
    assert_eq!(
        request.encode().unwrap(),
        words(&[
            0x2809, 0x0024, 0x1234, 0x3410, 0, 0, 0, 0, 0x0800, 0x0201, 1, 3, 0, 0, 0, 0, 0, 0,
        ])
    );
}

#[test]
fn set_places_names_after_the_padded_record_table() {
    let source = selector("mixer", "Out 9");
    let request = PageSubscriptionWrite::new(0x2809, 2, 7, channel(2), Some(&source)).unwrap();
    let mut expected = words(&[
        0x2809, 0x0030, 7, 0x3410, 0, 0, 0, 0, 0x0800, 0x0201, 2, 3, 0x0024, 0x002a, 0, 0, 0, 0,
    ]);
    expected.extend_from_slice(b"Out 9\0mixer\0");
    assert_eq!(request.encode().unwrap(), expected);
}

#[test]
fn capacity_pads_the_record_table() {
    let request = PageSubscriptionWrite::new(0x2809, 32, 7, channel(5), None).unwrap();
    let encoded = request.encode().unwrap();
    assert_eq!(encoded.len(), 20 + 32 * 8);
    assert_eq!(&encoded[18..20], &[32, 1]);
}

#[test]
fn unobserved_protocols_capacities_and_self_references_are_refused() {
    assert_eq!(
        PageSubscriptionWrite::new(0x280F, 2, 1, channel(1), None),
        Err(SubscriptionWriteCodecError::InvalidMagic { found: 0x280F })
    );
    for capacity in [0, 33] {
        assert_eq!(
            PageSubscriptionWrite::new(0x2809, capacity, 1, channel(1), None),
            Err(SubscriptionWriteCodecError::InvalidPageCapacity { found: capacity })
        );
    }
    assert_eq!(
        PageSubscriptionWrite::new(0x2809, 2, 1, channel(1), Some(&selector(".", "Left"))),
        Err(SubscriptionWriteCodecError::InvalidName {
            field: "source-device",
            reason: NameErrorReason::Unsupported,
        })
    );
}

fn reply(magic: u16, sequence: u16, command: u16, status: u16) -> Vec<u8> {
    let mut frame = words(&[magic, 20, sequence, command, status]);
    frame.extend_from_slice(&[0xAA; 10]);
    frame
}

#[test]
fn acceptance_requires_exact_correlated_status_one_reply() {
    let request = PageSubscriptionWrite::new(0x2809, 2, 9, channel(1), None).unwrap();
    let accepted = request
        .parse_acceptance(&reply(0x2809, 9, 0x3410, 1))
        .unwrap();
    assert_eq!(accepted.sequence(), 9);

    let cases = [
        (
            reply(0x280F, 9, 0x3410, 1),
            SubscriptionAcceptanceError::InvalidMagic { found: 0x280F },
        ),
        (
            reply(0x2809, 8, 0x3410, 1),
            SubscriptionAcceptanceError::SequenceMismatch {
                received: 8,
                expected: 9,
            },
        ),
        (
            reply(0x2809, 9, 0x3010, 1),
            SubscriptionAcceptanceError::UnexpectedCommand { found: 0x3010 },
        ),
        (
            reply(0x2809, 9, 0x3410, 2),
            SubscriptionAcceptanceError::UnacceptedStatus { status: 2 },
        ),
    ];
    for (frame, error) in cases {
        assert_eq!(request.parse_acceptance(&frame), Err(error));
    }
    assert_eq!(
        request.parse_acceptance(&reply(0x2809, 9, 0x3410, 1)[..10]),
        Err(SubscriptionAcceptanceError::InvalidLength { actual: 10 })
    );
}

#[test]
fn client_sends_once_and_parses_the_paged_reply() {
    let device = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
    device
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut client = ArcClient::bind(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        device.local_addr().unwrap(),
        Duration::from_secs(2),
        0x0400,
    )
    .unwrap();
    let source = selector("mixer", "Out 9");
    std::thread::scope(|scope| {
        let worker =
            scope.spawn(|| client.apply_paged_subscription(0x2809, 2, channel(2), Some(&source)));
        let mut buffer = [0u8; 512];
        let (length, sender) = device.recv_from(&mut buffer).unwrap();
        assert_eq!(&buffer[6..8], &0x3410u16.to_be_bytes());
        assert_eq!(length, 0x30);
        device
            .send_to(&reply(0x2809, 0x0400, 0x3410, 1), sender)
            .unwrap();
        let accepted = worker.join().unwrap().expect("accepted");
        assert_eq!(accepted.sequence(), 0x0400);
    });
    assert!(matches!(
        client.apply_paged_subscription(0x280F, 2, channel(1), None),
        Err(ArcClientError::WriteCodec(_))
    ));
}
