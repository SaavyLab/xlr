//! Read-only ARC transmitter-channel listing (`0x2000`).
//!
//! Supported envelope:
//!
//! - Request: magic `0x280F`, declared length 16, command `0x2000`, two
//!   fixed words (`0x0000`, `0x0001`), the first transmitter channel to
//!   list, trailing zero word.
//! - Response: magic `0x2801`, the echoed sequence and command, status
//!   `0x0001`, and a count word whose low byte is the number of entries in
//!   this page (its high byte is not interpreted). Then one 8-byte record
//!   per entry: channel number, an uninterpreted word, an uninterpreted
//!   offset, and the absolute offset of the channel's NUL-terminated ASCII
//!   name.
//! - Channel numbers run sequentially from the requested first channel. A
//!   page past the device's last channel carries zero entries.

use super::{
    COMMON_HEADER_LENGTH, LISTING_REPLY_MAGIC, REQUEST_MAGIC, check_header, check_status,
    encode_words, resolve_string, word_at,
};
use crate::{
    error::{ArcCodecError, RecordLayoutError, StringEncodingError},
    model::TransmitterChannel,
};
use std::num::NonZeroU16;

const COMMAND: u16 = 0x2000;
const REQUEST_LENGTH: u16 = 16;
const RESPONSE_HEADER_LENGTH: usize = COMMON_HEADER_LENGTH + 4;
const RECORD_LENGTH: usize = 8;
const NAME_OFFSET_BYTE: usize = 6;
const NAME_FIELD: &str = "transmitter-name";

/// A read-only query for the transmitter channels starting at one channel
/// number, bound to one wire sequence.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TransmitterPageQuery {
    sequence: u16,
    first_channel: NonZeroU16,
}

impl TransmitterPageQuery {
    pub fn new(sequence: u16, first_channel: NonZeroU16) -> Self {
        Self {
            sequence,
            first_channel,
        }
    }

    pub fn sequence(&self) -> u16 {
        self.sequence
    }

    pub fn first_channel(&self) -> NonZeroU16 {
        self.first_channel
    }

    pub fn encode(&self) -> Vec<u8> {
        encode_words(&[
            REQUEST_MAGIC,
            REQUEST_LENGTH,
            self.sequence,
            COMMAND,
            0x0000,
            0x0001,
            self.first_channel.get(),
            0x0000,
        ])
    }

    /// Decodes one page of transmitter channels, in ascending order.
    pub fn decode_response(&self, frame: &[u8]) -> Result<Vec<TransmitterChannel>, ArcCodecError> {
        check_header(
            frame,
            &[LISTING_REPLY_MAGIC],
            COMMAND,
            self.sequence,
            RESPONSE_HEADER_LENGTH,
        )?;
        check_status(frame)?;
        let entry_count = usize::from(frame[11]);
        let entries_end = RESPONSE_HEADER_LENGTH + entry_count * RECORD_LENGTH;
        if entries_end > frame.len() {
            return Err(ArcCodecError::TruncatedFrame {
                required: entries_end,
                available: frame.len(),
            });
        }

        let first = self.first_channel.get();
        let mut channels = Vec::with_capacity(entry_count);
        for index in 0..entry_count {
            let base = RESPONSE_HEADER_LENGTH + index * RECORD_LENGTH;
            let number = word_at(frame, base);
            let expected = u16::try_from(index)
                .ok()
                .and_then(|index| first.checked_add(index))
                .ok_or(RecordLayoutError::EntryChannelId {
                    found: number,
                    expected: u16::MAX,
                })?;
            if number != expected {
                return Err(RecordLayoutError::EntryChannelId {
                    found: number,
                    expected,
                }
                .into());
            }
            let name = resolve_string(
                frame,
                word_at(frame, base + NAME_OFFSET_BYTE),
                entries_end..frame.len(),
                NAME_FIELD,
                number,
            )?;
            if name.is_empty() {
                return Err(StringEncodingError::EmptyName {
                    channel: number,
                    field: NAME_FIELD,
                }
                .into());
            }
            channels.push(TransmitterChannel::new(number, name));
        }
        Ok(channels)
    }
}
