//! Behavioral tests for the exact Via ARC subscription write contract.

use xlr_dante::model::{ReceiverChannel, TransmitterSelector};
use xlr_dante::{
    SubscriptionAcceptanceError, SubscriptionWriteCodecError, SubscriptionWriteRequest,
};

const BASE: usize = 0x019C;
const MAGIC: u16 = 0x280F;
const COMMAND: u16 = 0x3010;
const SELECTOR: u16 = 0x1401;

fn channel(value: u16) -> ReceiverChannel {
    ReceiverChannel::new(value).expect("nonzero test channel")
}

fn word(frame: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([frame[offset], frame[offset + 1]])
}

#[test]
fn set_request_uses_absolute_offsets_and_one_terminator_each() {
    let selector = TransmitterSelector::new("TX device".into(), "Bus 1".into()).unwrap();
    let request = SubscriptionWriteRequest::new(0x1234, channel(7), Some(&selector)).unwrap();
    let frame = request.encode().unwrap();

    assert_eq!(frame.len(), BASE + 5 + 1 + 9 + 1);
    assert_eq!(word(&frame, 0), MAGIC);
    assert_eq!(word(&frame, 2), frame.len() as u16);
    assert_eq!(word(&frame, 4), 0x1234);
    assert_eq!(word(&frame, 6), COMMAND);
    assert_eq!(word(&frame, 8), 0);
    assert_eq!(word(&frame, 10), SELECTOR);
    assert_eq!(word(&frame, 12), 7);
    assert_eq!(word(&frame, 0x0E), BASE as u16);
    assert_eq!(word(&frame, 0x10), (BASE + 5 + 1) as u16);
    assert!(frame[0x12..BASE].iter().all(|byte| *byte == 0));
    assert_eq!(&frame[BASE..BASE + 6], b"Bus 1\0");
    assert_eq!(&frame[BASE + 6..], b"TX device\0");

    assert_eq!(SubscriptionWriteRequest::decode(&frame).unwrap(), request);
}

#[test]
fn clear_request_is_the_zero_filled_base_shape() {
    let request = SubscriptionWriteRequest::new(0, channel(u16::MAX), None).unwrap();
    let frame = request.encode().unwrap();

    assert_eq!(frame.len(), BASE);
    assert_eq!(word(&frame, 2), BASE as u16);
    assert_eq!(word(&frame, 4), 0);
    assert_eq!(word(&frame, 12), u16::MAX);
    assert!(frame[0x0E..].iter().all(|byte| *byte == 0));
    assert_eq!(SubscriptionWriteRequest::decode(&frame).unwrap(), request);
}

#[test]
fn request_codec_rejects_invalid_names_and_nonzero_clear_tail() {
    let non_ascii = TransmitterSelector::new("device".into(), "café".into()).unwrap();
    assert!(matches!(
        SubscriptionWriteRequest::new(1, channel(1), Some(&non_ascii)),
        Err(SubscriptionWriteCodecError::InvalidName {
            field: "source-channel",
            ..
        })
    ));

    let mut clear = SubscriptionWriteRequest::new(1, channel(1), None)
        .unwrap()
        .encode()
        .unwrap();
    clear[BASE - 1] = 1;
    assert_eq!(
        SubscriptionWriteRequest::decode(&clear),
        Err(SubscriptionWriteCodecError::NonzeroClearTail)
    );
}

#[test]
fn acceptance_requires_exact_correlated_status_one_reply() {
    let selector = TransmitterSelector::new("device".into(), "channel".into()).unwrap();
    let request = SubscriptionWriteRequest::new(0xBEEF, channel(1), Some(&selector)).unwrap();
    let reply = [0x28, 0x0F, 0x00, 0x0A, 0xBE, 0xEF, 0x30, 0x10, 0x00, 0x01];
    let acceptance = request.parse_acceptance(&reply).unwrap();
    assert_eq!(acceptance.sequence(), 0xBEEF);
    assert_eq!(acceptance.status(), 1);

    let mut wrong_magic = reply;
    wrong_magic[1] = 0x0E;
    assert_eq!(
        request.parse_acceptance(&wrong_magic),
        Err(SubscriptionAcceptanceError::InvalidMagic { found: 0x280E })
    );

    let mut wrong_declared_length = reply;
    wrong_declared_length[3] = 11;
    assert_eq!(
        request.parse_acceptance(&wrong_declared_length),
        Err(SubscriptionAcceptanceError::InvalidDeclaredLength { found: 11 })
    );

    let mut wrong_sequence = reply;
    wrong_sequence[4] = 0;
    wrong_sequence[5] = 1;
    assert_eq!(
        request.parse_acceptance(&wrong_sequence),
        Err(SubscriptionAcceptanceError::SequenceMismatch {
            received: 1,
            expected: 0xBEEF
        })
    );

    let mut wrong_command = reply;
    wrong_command[6] = 0x30;
    wrong_command[7] = 0;
    assert_eq!(
        request.parse_acceptance(&wrong_command),
        Err(SubscriptionAcceptanceError::UnexpectedCommand { found: 0x3000 })
    );

    let mut unknown = reply;
    unknown[9] = 2;
    assert_eq!(
        request.parse_acceptance(&unknown),
        Err(SubscriptionAcceptanceError::UnacceptedStatus { status: 2 })
    );

    let mut trailing = reply.to_vec();
    trailing.push(0);
    assert_eq!(
        request.parse_acceptance(&trailing),
        Err(SubscriptionAcceptanceError::InvalidLength { actual: 11 })
    );
}
