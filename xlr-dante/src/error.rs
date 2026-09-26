//! Typed failures raised while decoding ARC frames.
//!
//! Every variant names a distinct protocol violation so codec and transport
//! consumers can classify a datagram without string matching. This module
//! defines codec failures only; synchronous UDP I/O errors live in
//! [`crate::client::ArcClientError`].

use std::{error::Error, fmt};

/// Decode failure for an ARC response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArcCodecError {
    /// The magic word identifies a different ARC envelope family than the one
    /// this decode supports. The codec refuses rather than reinterpreting.
    UnsupportedVariant { found: u16 },
    /// The leading word is not any recognized ARC envelope at all; the
    /// datagram is almost certainly not Dante ARC traffic.
    InvalidMagic { found: u16 },
    /// The reply carries a command other than the one requested.
    UnexpectedCommand { found: u16, expected: u16 },
    /// The reply reports a status other than the observed success value.
    UnexpectedStatus { found: u16, expected: u16 },
    /// The reply sequence does not match the encoded request.
    SequenceMismatch { received: u16, expected: u16 },
    /// The frame ends before a minimum header or declared table read can be
    /// satisfied. Used only when actual bytes are insufficient; a frame that
    /// disagrees with its own declared length is a `LengthMismatch` instead.
    TruncatedFrame { required: usize, available: usize },
    /// The frame's declared length differs from the bytes actually received,
    /// in either direction.
    LengthMismatch { declared: usize, actual: usize },
    /// The entry table violates the supported record layout.
    InvalidRecordLayout(RecordLayoutError),
    /// A resolved string offset points outside its bounded region.
    InvalidStringOffset(StringOffsetError),
    /// A decoded field is not bounded NUL-terminated ASCII.
    InvalidStringEncoding(StringEncodingError),
    /// A record carries exactly one of the two selector fields present, so it
    /// is neither a complete subscription nor an evidenced unsubscribe state.
    PartialSelector {
        receiver_channel: u16,
        missing_field: &'static str,
    },
}

/// Structural problems with the fixed-width records of a paged listing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordLayoutError {
    /// The page-entry count is unsupported: above the maximum, or zero where
    /// an entry is required.
    EntryCount { found: u8, maximum: u8 },
    /// An entry's channel id is not the expected sequential value.
    EntryChannelId { found: u16, expected: u16 },
    /// A record end offset lies outside the frame's string region.
    RecordEndOffset {
        channel: u16,
        found: u16,
        limit: usize,
    },
    /// The page does not span the requested receiver channel.
    RequestedChannelOutsidePage {
        requested: u16,
        first_entry_channel: u16,
        entry_count: u8,
    },
}

/// Out-of-range absolute string offsets within a response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StringOffsetError {
    /// The offset is not inside the record-bounded string region.
    OutOfBounds { channel: u16, offset: u16 },
    /// No NUL terminator occurs between the offset and the record end.
    NotTerminated { channel: u16, field: &'static str },
}

/// Encoding violations for names carried by a response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StringEncodingError {
    /// The name exceeds the supported bound of 255 bytes.
    TooLong {
        channel: u16,
        field: &'static str,
        length: usize,
    },
    /// The name contains non-ASCII bytes.
    NonAscii { channel: u16, field: &'static str },
    /// A nonzero offset addresses a NUL byte directly — an empty encoded name
    /// rather than an absent field.
    EmptyName { channel: u16, field: &'static str },
}

impl fmt::Display for ArcCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVariant { found } => write!(
                formatter,
                "reply belongs to a different ARC envelope family (magic \
                 0x{found:04x}); this decoder does not \
                 accept for this command"
            ),
            Self::InvalidMagic { found } => {
                write!(formatter, "unrecognized reply magic 0x{found:04x}")
            }
            Self::UnexpectedCommand { found, expected } => write!(
                formatter,
                "unexpected reply command 0x{found:04x}; expected 0x{expected:04x}"
            ),
            Self::UnexpectedStatus { found, expected } => write!(
                formatter,
                "reply status 0x{found:04x} is not the expected 0x{expected:04x}"
            ),
            Self::SequenceMismatch { received, expected } => write!(
                formatter,
                "reply sequence {received} does not match request sequence {expected}"
            ),
            Self::TruncatedFrame {
                required,
                available,
            } => write!(
                formatter,
                "frame ends after {available} bytes but requires {required}"
            ),
            Self::LengthMismatch { declared, actual } => write!(
                formatter,
                "frame declares {declared} bytes but carries {actual}"
            ),
            Self::InvalidRecordLayout(reason) => reason.fmt(formatter),
            Self::InvalidStringOffset(reason) => reason.fmt(formatter),
            Self::InvalidStringEncoding(reason) => reason.fmt(formatter),
            Self::PartialSelector {
                receiver_channel,
                missing_field,
            } => write!(
                formatter,
                "receiver channel {receiver_channel} carries one selector \
                 field but is missing `{missing_field}`; the entry cannot be \
                 classified as subscribed or unsubscribed"
            ),
        }
    }
}

impl fmt::Display for RecordLayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EntryCount { found, maximum } => {
                write!(
                    formatter,
                    "entry count {found} is outside the supported range (maximum {maximum})"
                )
            }
            Self::EntryChannelId { found, expected } => write!(
                formatter,
                "table channel id {found} does not match expected {expected}"
            ),
            Self::RecordEndOffset {
                channel,
                found,
                limit,
            } => write!(
                formatter,
                "channel {channel} record end offset \
                 0x{found:04x} must be zero (unbounded) or within [table_end, \
                 0x{limit:04X}]"
            ),
            Self::RequestedChannelOutsidePage {
                requested,
                first_entry_channel,
                entry_count,
            } => write!(
                formatter,
                "page starting at channel {first_entry_channel} holds \
                 {entry_count} entries and does not contain requested channel \
                 {requested}"
            ),
        }
    }
}

impl fmt::Display for StringOffsetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfBounds { channel, offset } => write!(
                formatter,
                "channel {channel} string offset \
                 0x{offset:04x} is outside its bounded string region"
            ),
            Self::NotTerminated { channel, field } => write!(
                formatter,
                "channel {channel} {field} is not \
                 NUL-terminated before its record end"
            ),
        }
    }
}

impl fmt::Display for StringEncodingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong {
                channel,
                field,
                length,
            } => write!(
                formatter,
                "channel {channel} {field} is {length} \
                 bytes and exceeds the 255-byte name bound"
            ),
            Self::NonAscii { channel, field } => {
                write!(formatter, "channel {channel} {field} is not ASCII")
            }
            Self::EmptyName { channel, field } => write!(
                formatter,
                "channel {channel} {field} decodes to an \
                 empty name from a nonzero offset; only offset zero means absent"
            ),
        }
    }
}

impl Error for ArcCodecError {
    /// The nested structural or encoding reason for the three wrapped
    /// variants; every other variant is its own complete diagnosis and
    /// reports no source.
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidRecordLayout(reason) => Some(reason),
            Self::InvalidStringOffset(reason) => Some(reason),
            Self::InvalidStringEncoding(reason) => Some(reason),
            _ => None,
        }
    }
}

impl Error for RecordLayoutError {}
impl Error for StringOffsetError {}
impl Error for StringEncodingError {}

impl From<RecordLayoutError> for ArcCodecError {
    fn from(value: RecordLayoutError) -> Self {
        Self::InvalidRecordLayout(value)
    }
}

impl From<StringOffsetError> for ArcCodecError {
    fn from(value: StringOffsetError) -> Self {
        Self::InvalidStringOffset(value)
    }
}

impl From<StringEncodingError> for ArcCodecError {
    fn from(value: StringEncodingError) -> Self {
        Self::InvalidStringEncoding(value)
    }
}
