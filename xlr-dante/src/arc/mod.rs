//! Dante ARC (Audio Routing Control) message codec.
//!
//! The modules here implement the byte layout of individual ARC message
//! families: request encoding, response validation, correlation, and
//! normalization into the domain model.
//!
//! Only explicitly implemented message forms are accepted or produced; see
//! each module's documentation for its supported envelope. Unrecognized or
//! out-of-scope traffic is rejected with a typed error rather than guessed
//! at — never reinterpret one family's frame as another's.
//!
//! Every ARC frame starts with the same four big-endian words: magic,
//! declared length, sequence, and command. Replies echo the request's
//! sequence and command.

pub mod device;
pub mod subscription;
pub mod subscription_write;
pub mod transmitters;

use crate::error::{ArcCodecError, StringEncodingError, StringOffsetError};
use std::ops::Range;

/// Magic of every request this crate sends.
pub(crate) const REQUEST_MAGIC: u16 = 0x280F;
/// Reply magic observed for the paged channel listings (`0x2000`, `0x3000`).
pub(crate) const LISTING_REPLY_MAGIC: u16 = 0x2801;
/// Reply magic some devices use for other commands (observed on a
/// physical Dante adapter for `0x1000` and `0x1002`).
pub(crate) const ALTERNATE_REPLY_MAGIC: u16 = 0x2809;
/// The status word value observed on every successful reply that carries
/// one.
pub(crate) const SUCCESS_STATUS: u16 = 0x0001;
/// Bytes of the common header: magic, length, sequence, command.
pub(crate) const COMMON_HEADER_LENGTH: usize = 8;
/// Observed upper bound for a transmitted name, excluding the terminator.
pub(crate) const MAX_NAME_BYTES: usize = 255;

/// Whether `frame` carries `sequence` in its correlation word. Everything
/// else about the frame is left to the command's decoder.
pub(crate) fn correlates(frame: &[u8], sequence: u16) -> bool {
    frame.len() >= 6 && u16::from_be_bytes([frame[4], frame[5]]) == sequence
}

/// Encodes big-endian words into a request frame.
pub(crate) fn encode_words(words: &[u16]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_be_bytes()).collect()
}

/// Reads the big-endian word at byte `offset`. Callers bound `offset`.
pub(crate) fn word_at(frame: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([frame[offset], frame[offset + 1]])
}

/// Validates the common header of a reply.
///
/// Checks, in order: minimum length, magic (one of `accepted`; other known
/// ARC magics are [`ArcCodecError::UnsupportedVariant`]), command, sequence,
/// and declared length against the received length.
pub(crate) fn check_header(
    frame: &[u8],
    accepted: &[u16],
    command: u16,
    sequence: u16,
    minimum_length: usize,
) -> Result<(), ArcCodecError> {
    if frame.len() < minimum_length {
        return Err(ArcCodecError::TruncatedFrame {
            required: minimum_length,
            available: frame.len(),
        });
    }
    match word_at(frame, 0) {
        found if accepted.contains(&found) => {}
        found @ (REQUEST_MAGIC | LISTING_REPLY_MAGIC | ALTERNATE_REPLY_MAGIC) => {
            return Err(ArcCodecError::UnsupportedVariant { found });
        }
        found => return Err(ArcCodecError::InvalidMagic { found }),
    }
    let found = word_at(frame, 6);
    if found != command {
        return Err(ArcCodecError::UnexpectedCommand {
            found,
            expected: command,
        });
    }
    let received = word_at(frame, 4);
    if received != sequence {
        return Err(ArcCodecError::SequenceMismatch {
            received,
            expected: sequence,
        });
    }
    let declared = usize::from(word_at(frame, 2));
    if declared != frame.len() {
        return Err(ArcCodecError::LengthMismatch {
            declared,
            actual: frame.len(),
        });
    }
    Ok(())
}

/// Requires the status word at byte 8 to be [`SUCCESS_STATUS`].
pub(crate) fn check_status(frame: &[u8]) -> Result<(), ArcCodecError> {
    match word_at(frame, 8) {
        SUCCESS_STATUS => Ok(()),
        found => Err(ArcCodecError::UnexpectedStatus {
            found,
            expected: SUCCESS_STATUS,
        }),
    }
}

/// Resolves one bounded, absolute, NUL-terminated ASCII field.
///
/// An offset of zero means "absent" per the observed representation and
/// yields an empty string. Any nonzero offset must land inside `range`,
/// terminate before the range end, and decode to a non-empty ASCII name; a
/// nonzero offset aimed straight at a NUL byte encodes an empty name and is
/// rejected rather than treated as an absent field. `channel` only labels
/// errors.
pub(crate) fn resolve_string(
    frame: &[u8],
    offset: u16,
    range: Range<usize>,
    field: &'static str,
    channel: u16,
) -> Result<String, ArcCodecError> {
    if offset == 0 {
        return Ok(String::new());
    }
    // Offsets are absolute byte positions; the bound only limits how far
    // the field may run before terminating.
    let start = usize::from(offset);
    if !range.contains(&start) {
        return Err(StringOffsetError::OutOfBounds { channel, offset }.into());
    }
    let Some(field_length) = frame[start..range.end].iter().position(|&byte| byte == 0) else {
        return Err(StringOffsetError::NotTerminated { channel, field }.into());
    };
    if field_length == 0 {
        return Err(StringEncodingError::EmptyName { channel, field }.into());
    }
    let encoded = &frame[start..start + field_length];
    if encoded.len() > MAX_NAME_BYTES {
        return Err(StringEncodingError::TooLong {
            channel,
            field,
            length: encoded.len(),
        }
        .into());
    }
    match std::str::from_utf8(encoded) {
        Ok(decoded) if decoded.is_ascii() => Ok(decoded.to_owned()),
        _ => Err(StringEncodingError::NonAscii { channel, field }.into()),
    }
}
