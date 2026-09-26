//! Loopback tests for one-send fixed-peer subscription writes.

use std::net::{SocketAddr, UdpSocket};
use std::thread;
use std::time::Duration;
use xlr_dante::model::{ReceiverChannel, TransmitterSelector};
use xlr_dante::{ArcClient, ArcClientError, SubscriptionAcceptanceError, SubscriptionWriteRequest};

fn channel(value: u16) -> ReceiverChannel {
    ReceiverChannel::new(value).expect("nonzero test channel")
}

fn acceptance(sequence: u16, status: u16) -> [u8; 10] {
    [
        0x28,
        0x0F,
        0,
        10,
        (sequence >> 8) as u8,
        sequence as u8,
        0x30,
        0x10,
        (status >> 8) as u8,
        status as u8,
    ]
}

#[test]
fn apply_subscription_sends_one_exact_set_and_returns_acceptance() {
    let device = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
    let device_address = device.local_addr().unwrap();
    let worker = thread::spawn(move || {
        let mut request_bytes = vec![0u8; 2048];
        let (received, client_address) = device.recv_from(&mut request_bytes).unwrap();
        request_bytes.truncate(received);
        let request = SubscriptionWriteRequest::decode(&request_bytes).unwrap();
        assert_eq!(request.sequence(), 0x4000);
        assert_eq!(request.receiver_channel().value(), 19);
        let selector = request.selector().unwrap();
        assert_eq!(selector.device_name(), "device");
        assert_eq!(selector.channel_name(), "channel");
        device
            .send_to(&acceptance(request.sequence(), 1), client_address)
            .unwrap();
    });

    let mut client = ArcClient::bind(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        device_address,
        Duration::from_secs(1),
        0x4000,
    )
    .unwrap();
    let selector = TransmitterSelector::new("device".into(), "channel".into()).unwrap();
    let result = client.apply_subscription(channel(19), Some(&selector));
    let acceptance = result.unwrap();
    assert_eq!(acceptance.sequence(), 0x4000);
    assert_eq!(acceptance.status(), 1);
    worker.join().unwrap();
}

#[test]
fn apply_subscription_sends_zero_filled_clear_and_preserves_unknown_status() {
    let device = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
    let device_address = device.local_addr().unwrap();
    let worker = thread::spawn(move || {
        let mut request_bytes = vec![0u8; 1024];
        let (received, client_address) = device.recv_from(&mut request_bytes).unwrap();
        assert_eq!(received, 0x019C);
        assert!(request_bytes[0x0E..received].iter().all(|byte| *byte == 0));
        device
            .send_to(&acceptance(0x4001, 2), client_address)
            .unwrap();
    });

    let mut client = ArcClient::bind(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        device_address,
        Duration::from_secs(1),
        0x4001,
    )
    .unwrap();
    let error = client
        .apply_subscription(channel(u16::MAX), None)
        .unwrap_err();
    assert!(matches!(
        error,
        ArcClientError::Acceptance(SubscriptionAcceptanceError::UnacceptedStatus { status: 2 })
    ));
    worker.join().unwrap();
}
