//! Tests of the permanent public constructor contract on the normalized
//! domain types: what [`ReceiverChannel`] and [`TransmitterSelector`] accept
//! and reject, independent of any codec module.

use xlr_dante::model::{ReceiverChannel, TransmitterSelector};

#[test]
fn zero_receiver_channel_is_rejected() {
    let error = ReceiverChannel::new(0).expect_err("channel zero is not a Dante channel");
    assert_eq!(error.value, 0);
}

#[test]
fn receiver_channel_thirty_three_is_accepted() {
    let channel = ReceiverChannel::new(33).expect("33 is a valid Dante channel number");
    assert_eq!(channel.value(), 33);
}

#[test]
fn empty_transmitter_device_name_is_rejected() {
    let error = TransmitterSelector::new(String::new(), "Analog 1".to_owned())
        .expect_err("an empty device name cannot name a transmitter");
    assert_eq!(error.field, "device_name");
}

#[test]
fn empty_transmitter_channel_name_is_rejected() {
    let error = TransmitterSelector::new("Scarlett 18i20".to_owned(), String::new())
        .expect_err("an empty channel name cannot address a transmitter channel");
    assert_eq!(error.field, "channel_name");
}

#[test]
fn valid_selector_preserves_both_names() {
    let selector = TransmitterSelector::new("AVIOUSBC-0563aa".to_owned(), "Left".to_owned())
        .expect("both names are non-empty");
    assert_eq!(selector.device_name(), "AVIOUSBC-0563aa");
    assert_eq!(selector.channel_name(), "Left");
}
