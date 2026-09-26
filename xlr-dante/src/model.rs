//! Normalized Dante-domain values.
//!
//! These types carry only domain meaning — receiver channels, transmitter
//! names, subscription state. They never contain packet offsets or raw wire
//! words; the [`crate::arc`] codec owns all byte layout.

use std::{error::Error, fmt};

/// A Dante receiver channel number: any nonzero 16-bit value.
///
/// Receiver channels are numbered from 1 on every device.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReceiverChannel {
    value: u16,
}

impl ReceiverChannel {
    /// Builds a receiver channel if `value` is a legal Dante channel number
    /// (nonzero).
    pub fn new(value: u16) -> Result<Self, ReceiverChannelRangeError> {
        Self::try_from(value)
    }

    /// The channel number itself.
    pub fn value(self) -> u16 {
        self.value
    }
}

impl TryFrom<u16> for ReceiverChannel {
    type Error = ReceiverChannelRangeError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        if value == 0 {
            Err(ReceiverChannelRangeError { value })
        } else {
            Ok(Self { value })
        }
    }
}

impl From<ReceiverChannel> for u16 {
    fn from(value: ReceiverChannel) -> Self {
        value.value()
    }
}

/// The receiver channel number was zero, which no Dante channel uses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReceiverChannelRangeError {
    pub value: u16,
}

impl fmt::Display for ReceiverChannelRangeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "receiver channel {} must not be zero",
            self.value
        )
    }
}

impl Error for ReceiverChannelRangeError {}

/// The transmitter side of an audio route, as named by the transmitter
/// device itself.
///
/// Construction takes owned strings because these are long-lived model
/// values, not borrowed wire views, and rejects empty names so no selector
/// can carry a missing field as empty text. Accessors return `&str`; there
/// is no inference from partial input.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TransmitterSelector {
    device_name: String,
    channel_name: String,
}

impl TransmitterSelector {
    /// Builds a transmitter selector from the device and channel names the
    /// transmitter reports for itself.
    ///
    /// # Errors
    /// Returns [`EmptySelectorField`] when either name is empty.
    pub fn new(device_name: String, channel_name: String) -> Result<Self, EmptySelectorField> {
        if device_name.is_empty() {
            return Err(EmptySelectorField {
                field: "device_name",
            });
        }
        if channel_name.is_empty() {
            return Err(EmptySelectorField {
                field: "channel_name",
            });
        }
        Ok(Self {
            device_name,
            channel_name,
        })
    }

    /// Builds a selector from names already proven non-empty by a caller
    /// inside this crate (for example, the codec after decoding validation).
    pub(crate) fn from_decoded(device_name: String, channel_name: String) -> Self {
        debug_assert!(!device_name.is_empty() && !channel_name.is_empty());
        Self {
            device_name,
            channel_name,
        }
    }

    /// The transmitter device name as reported by the device.
    ///
    /// Dante Via reports `.` for a channel on the receiving device itself.
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// The transmitter channel name as reported by the device.
    pub fn channel_name(&self) -> &str {
        &self.channel_name
    }
}

/// A selector field was empty where the domain requires an actual name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmptySelectorField {
    pub field: &'static str,
}

impl fmt::Display for EmptySelectorField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "transmitter selector field `{}` must not be empty",
            self.field
        )
    }
}

impl Error for EmptySelectorField {}

/// Normalized subscription state of one receiver channel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubscriptionState {
    /// No subscription is present. Names were never invented for this state.
    Unsubscribed,
    /// The receiver follows the named transmitter.
    Subscribed(TransmitterSelector),
}

/// The decoded subscription state of exactly one receiver channel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReceiverSubscription {
    receiver_channel: ReceiverChannel,
    name: Option<String>,
    state: SubscriptionState,
}

impl ReceiverSubscription {
    pub(crate) fn new(
        receiver_channel: ReceiverChannel,
        name: Option<String>,
        state: SubscriptionState,
    ) -> Self {
        Self {
            receiver_channel,
            name,
            state,
        }
    }

    /// The receiver channel whose state this entry describes.
    pub fn receiver_channel(&self) -> ReceiverChannel {
        self.receiver_channel
    }

    /// The receiver channel's own name, as the device reports it, when
    /// the device reports one.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Whether and to what this channel subscribes.
    pub fn state(&self) -> &SubscriptionState {
        &self.state
    }
}

/// One decoded page of receiver subscriptions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReceiverSubscriptionPage {
    subscriptions: Vec<ReceiverSubscription>,
}

impl ReceiverSubscriptionPage {
    pub(crate) fn new(subscriptions: Vec<ReceiverSubscription>) -> Self {
        Self { subscriptions }
    }

    /// The entries of this page, in ascending receiver-channel order.
    pub fn subscriptions(&self) -> &[ReceiverSubscription] {
        &self.subscriptions
    }

    /// Consumes the page and yields its entries in ascending
    /// receiver-channel order.
    pub fn into_subscriptions(self) -> Vec<ReceiverSubscription> {
        self.subscriptions
    }
}

/// One transmitter channel a device offers: its number and its name.
///
/// The name is what a receiver subscribes to, together with the device
/// name (see [`TransmitterSelector`]).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TransmitterChannel {
    number: u16,
    name: String,
}

impl TransmitterChannel {
    pub(crate) fn new(number: u16, name: String) -> Self {
        Self { number, name }
    }

    /// The transmitter channel number, starting at 1.
    pub fn number(&self) -> u16 {
        self.number
    }

    /// The channel name as the device reports it.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// How many transmitter and receiver channels a device reports.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ChannelCounts {
    pub transmitters: u16,
    pub receivers: u16,
}
