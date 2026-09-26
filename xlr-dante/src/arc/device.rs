//! Read-only ARC device queries: device name (`0x1002`) and channel counts
//! (`0x1000`).
//!
//! Supported envelope for both:
//!
//! - Request: magic `0x280F`, declared length 10, command, one zero word.
//! - Response: magic `0x280F` or `0x2809` (both observed, depending on the
//!   device), the echoed sequence and command, and status `0x0001` at byte
//!   8.
//! - Device name: a NUL-terminated ASCII name fills the rest of the frame.
//! - Channel counts: transmitter count at byte 12, receiver count at
//!   byte 14. The remaining words are not interpreted.

use super::{
    ALTERNATE_REPLY_MAGIC, COMMON_HEADER_LENGTH, MAX_NAME_BYTES, REQUEST_MAGIC, check_header,
    check_status, encode_words, word_at,
};
use crate::{
    error::{ArcCodecError, StringEncodingError, StringOffsetError},
    model::ChannelCounts,
};

const CHANNEL_COUNT_COMMAND: u16 = 0x1000;
const DEVICE_NAME_COMMAND: u16 = 0x1002;
const REQUEST_LENGTH: u16 = 10;
const STATUS_END: usize = COMMON_HEADER_LENGTH + 2;
const COUNTS_LENGTH: usize = 16;
const REPLY_MAGICS: [u16; 2] = [REQUEST_MAGIC, ALTERNATE_REPLY_MAGIC];
const DEVICE_NAME_FIELD: &str = "device-name";

fn encode(sequence: u16, command: u16) -> Vec<u8> {
    encode_words(&[REQUEST_MAGIC, REQUEST_LENGTH, sequence, command, 0x0000])
}

/// Asks a device for its Dante device name.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DeviceNameQuery {
    sequence: u16,
}

impl DeviceNameQuery {
    pub fn new(sequence: u16) -> Self {
        Self { sequence }
    }

    pub fn sequence(&self) -> u16 {
        self.sequence
    }

    pub fn encode(&self) -> Vec<u8> {
        encode(self.sequence, DEVICE_NAME_COMMAND)
    }

    /// Decodes the reply. The name must be non-empty ASCII and its NUL
    /// terminator must be the frame's last byte.
    pub fn decode_response(&self, frame: &[u8]) -> Result<String, ArcCodecError> {
        check_header(
            frame,
            &REPLY_MAGICS,
            DEVICE_NAME_COMMAND,
            self.sequence,
            STATUS_END + 1,
        )?;
        check_status(frame)?;
        let body = &frame[STATUS_END..];
        let length =
            body.iter()
                .position(|&byte| byte == 0)
                .ok_or(StringOffsetError::NotTerminated {
                    channel: 0,
                    field: DEVICE_NAME_FIELD,
                })?;
        if length + 1 != body.len() {
            return Err(ArcCodecError::LengthMismatch {
                declared: frame.len(),
                actual: STATUS_END + length + 1,
            });
        }
        let name = &body[..length];
        let error = |kind| Err(ArcCodecError::InvalidStringEncoding(kind));
        if name.is_empty() {
            return error(StringEncodingError::EmptyName {
                channel: 0,
                field: DEVICE_NAME_FIELD,
            });
        }
        if name.len() > MAX_NAME_BYTES {
            return error(StringEncodingError::TooLong {
                channel: 0,
                field: DEVICE_NAME_FIELD,
                length: name.len(),
            });
        }
        if !name.is_ascii() {
            return error(StringEncodingError::NonAscii {
                channel: 0,
                field: DEVICE_NAME_FIELD,
            });
        }
        Ok(String::from_utf8(name.to_vec()).expect("ASCII is UTF-8"))
    }
}

/// Asks a device how many transmitter and receiver channels it has.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ChannelCountQuery {
    sequence: u16,
}

impl ChannelCountQuery {
    pub fn new(sequence: u16) -> Self {
        Self { sequence }
    }

    pub fn sequence(&self) -> u16 {
        self.sequence
    }

    pub fn encode(&self) -> Vec<u8> {
        encode(self.sequence, CHANNEL_COUNT_COMMAND)
    }

    pub fn decode_response(&self, frame: &[u8]) -> Result<ChannelCounts, ArcCodecError> {
        check_header(
            frame,
            &REPLY_MAGICS,
            CHANNEL_COUNT_COMMAND,
            self.sequence,
            COUNTS_LENGTH,
        )?;
        check_status(frame)?;
        Ok(ChannelCounts {
            transmitters: word_at(frame, 12),
            receivers: word_at(frame, 14),
        })
    }
}
