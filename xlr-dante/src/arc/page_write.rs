//! Paged ARC subscription write (`0x3410`), as used by Dante hardware.
//!
//! Supported envelope (one record per request):
//!
//! - Request: ARC protocol magic `0x2809`, declared length, sequence,
//!   command `0x3410`, eight zero bytes, word `0x0800`, the page capacity
//!   byte (`min(32, receiver count)` on the device), the record count byte
//!   (always 1 here), then one 8-byte record: receiver channel, media type
//!   `3` (audio), and absolute offsets of the source channel and device
//!   names. Records are padded to `capacity` slots; the NUL-terminated
//!   ASCII names follow at offset `20 + 8 × capacity`. A clear request
//!   carries zero offsets and no names.
//! - Reply: exactly 20 bytes with the same magic, the echoed sequence and
//!   command, and status `0x0001` for acceptance. The trailing ten bytes are
//!   not interpreted.
//!
//! Observed from a Dante AVIO adapter (ARC protocol 2.8.9). Other protocol
//! versions are refused rather than guessed.

use super::subscription_write::{
    ACCEPTED_STATUS, SubscriptionAcceptance, SubscriptionAcceptanceError,
    SubscriptionWriteCodecError, validate_name,
};
use crate::model::{ReceiverChannel, TransmitterSelector};

/// Wire command of the paged subscription write.
pub const COMMAND: u16 = 0x3410;
/// The only ARC protocol version this form has been observed with.
pub const SUPPORTED_PROTOCOL: u16 = 0x2809;
/// Largest page capacity a device uses.
pub const MAX_PAGE_CAPACITY: u8 = 32;
const PAGE_MARKER: u16 = 0x0800;
const AUDIO_MEDIA_TYPE: u16 = 3;
const HEADER_LENGTH: usize = 20;
const RECORD_LENGTH: usize = 8;
const REPLY_LENGTH: usize = 20;

/// One set or clear request in the paged form, bound to one sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageSubscriptionWrite {
    protocol: u16,
    page_capacity: u8,
    sequence: u16,
    receiver_channel: ReceiverChannel,
    selector: Option<TransmitterSelector>,
}

impl PageSubscriptionWrite {
    /// Builds a set request (`selector` is `Some`) or a clear request.
    ///
    /// `protocol` is the device's ARC protocol version (see
    /// [`crate::DiscoveredDevice::arc_protocol`]); `page_capacity` must be
    /// `min(32, receiver channel count)` for the device.
    pub fn new(
        protocol: u16,
        page_capacity: u8,
        sequence: u16,
        receiver_channel: ReceiverChannel,
        selector: Option<&TransmitterSelector>,
    ) -> Result<Self, SubscriptionWriteCodecError> {
        if protocol != SUPPORTED_PROTOCOL {
            return Err(SubscriptionWriteCodecError::InvalidMagic { found: protocol });
        }
        if page_capacity == 0 || page_capacity > MAX_PAGE_CAPACITY {
            return Err(SubscriptionWriteCodecError::InvalidPageCapacity {
                found: page_capacity,
            });
        }
        if let Some(selector) = selector {
            validate_name("source-channel", selector.channel_name())?;
            validate_name("source-device", selector.device_name())?;
            // Hardware addresses sources by real device name only.
            if selector.device_name() == "." {
                return Err(SubscriptionWriteCodecError::InvalidName {
                    field: "source-device",
                    reason: super::subscription_write::NameErrorReason::Unsupported,
                });
            }
        }
        Ok(Self {
            protocol,
            page_capacity,
            sequence,
            receiver_channel,
            selector: selector.cloned(),
        })
    }

    pub const fn sequence(&self) -> u16 {
        self.sequence
    }

    pub const fn receiver_channel(&self) -> ReceiverChannel {
        self.receiver_channel
    }

    pub fn selector(&self) -> Option<&TransmitterSelector> {
        self.selector.as_ref()
    }

    /// Encodes the request.
    pub fn encode(&self) -> Result<Vec<u8>, SubscriptionWriteCodecError> {
        let strings_start = HEADER_LENGTH + usize::from(self.page_capacity) * RECORD_LENGTH;
        let (channel_offset, device_offset, strings) = match &self.selector {
            None => (0, 0, Vec::new()),
            Some(selector) => {
                let mut strings = Vec::new();
                strings.extend_from_slice(selector.channel_name().as_bytes());
                strings.push(0);
                let device_offset = strings_start + strings.len();
                strings.extend_from_slice(selector.device_name().as_bytes());
                strings.push(0);
                (strings_start, device_offset, strings)
            }
        };
        let length = strings_start + strings.len();
        let to_word = |value: usize| {
            u16::try_from(value).map_err(|_| SubscriptionWriteCodecError::LengthOverflow)
        };

        let mut frame = Vec::with_capacity(length);
        for word in [self.protocol, to_word(length)?, self.sequence, COMMAND] {
            frame.extend_from_slice(&word.to_be_bytes());
        }
        frame.extend_from_slice(&[0; 8]);
        frame.extend_from_slice(&PAGE_MARKER.to_be_bytes());
        frame.push(self.page_capacity);
        frame.push(1);
        for word in [
            self.receiver_channel.value(),
            AUDIO_MEDIA_TYPE,
            to_word(channel_offset)?,
            to_word(device_offset)?,
        ] {
            frame.extend_from_slice(&word.to_be_bytes());
        }
        frame.resize(strings_start, 0);
        frame.extend_from_slice(&strings);
        Ok(frame)
    }

    /// Parses the exactly 20-byte reply and returns only status-one
    /// correlated acceptance.
    pub fn parse_acceptance(
        &self,
        frame: &[u8],
    ) -> Result<SubscriptionAcceptance, SubscriptionAcceptanceError> {
        if frame.len() != REPLY_LENGTH {
            return Err(SubscriptionAcceptanceError::InvalidLength {
                actual: frame.len(),
            });
        }
        let word = |offset: usize| u16::from_be_bytes([frame[offset], frame[offset + 1]]);
        if word(0) != self.protocol {
            return Err(SubscriptionAcceptanceError::InvalidMagic { found: word(0) });
        }
        if usize::from(word(2)) != REPLY_LENGTH {
            return Err(SubscriptionAcceptanceError::InvalidDeclaredLength { found: word(2) });
        }
        if word(4) != self.sequence {
            return Err(SubscriptionAcceptanceError::SequenceMismatch {
                received: word(4),
                expected: self.sequence,
            });
        }
        if word(6) != COMMAND {
            return Err(SubscriptionAcceptanceError::UnexpectedCommand { found: word(6) });
        }
        if word(8) != ACCEPTED_STATUS {
            return Err(SubscriptionAcceptanceError::UnacceptedStatus { status: word(8) });
        }
        Ok(SubscriptionAcceptance::accepted(self.sequence))
    }
}
