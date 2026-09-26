//! Exact Via ARC `0x3010 / 0x1401` subscription write codec.
//!
//! This module intentionally implements only the bounded set/clear request and
//! its ten-byte correlated acceptance reply. It does not infer other ARC
//! variants, statuses, or receiver state.

use crate::model::{ReceiverChannel, TransmitterSelector};
use std::{error::Error, fmt};

/// Via ARC magic shared by this write request and its reply.
pub const VIA_MAGIC: u16 = 0x280F;
/// Via ARC set/clear subscription command.
pub const COMMAND: u16 = 0x3010;
/// Fixed set/clear selector.
pub const SELECTOR: u16 = 0x1401;
/// Fixed size of a clear request and base size of a set request.
pub const BASE_LENGTH: usize = 0x019C;
/// The only status treated as an observed acceptance.
pub const ACCEPTED_STATUS: u16 = 0x0001;
const COMMON_HEADER_LENGTH: usize = 14;
const ACCEPTANCE_LENGTH: usize = 10;
const MAX_NAME_BYTES: usize = 255;

/// A set or clear request bound to one sequence and receiver channel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubscriptionWriteRequest {
    sequence: u16,
    receiver_channel: ReceiverChannel,
    selector: Option<TransmitterSelector>,
}

impl SubscriptionWriteRequest {
    /// Creates a set request when `selector` is `Some`, or a clear request
    /// when it is `None`.
    pub fn new(
        sequence: u16,
        receiver_channel: ReceiverChannel,
        selector: Option<&TransmitterSelector>,
    ) -> Result<Self, SubscriptionWriteCodecError> {
        if let Some(selector) = selector {
            validate_name("source-channel", selector.channel_name())?;
            validate_name("source-device", selector.device_name())?;
            Ok(Self {
                sequence,
                receiver_channel,
                selector: Some(selector.clone()),
            })
        } else {
            Ok(Self {
                sequence,
                receiver_channel,
                selector: None,
            })
        }
    }

    /// The request sequence echoed by the acceptance reply.
    pub const fn sequence(&self) -> u16 {
        self.sequence
    }

    /// The receiver channel being changed.
    pub const fn receiver_channel(&self) -> ReceiverChannel {
        self.receiver_channel
    }

    /// The selector for a set request, or `None` for a clear request.
    pub fn selector(&self) -> Option<&TransmitterSelector> {
        self.selector.as_ref()
    }

    /// Encodes the exact bounded Via write payload.
    pub fn encode(&self) -> Result<Vec<u8>, SubscriptionWriteCodecError> {
        if self.receiver_channel.value() == 0 {
            return Err(SubscriptionWriteCodecError::InvalidReceiverChannel { found: 0 });
        }
        let Some(selector) = &self.selector else {
            let mut frame = vec![0u8; BASE_LENGTH];
            write_common_header(
                &mut frame,
                BASE_LENGTH,
                self.sequence,
                self.receiver_channel,
            );
            return Ok(frame);
        };

        validate_name("source-channel", selector.channel_name())?;
        validate_name("source-device", selector.device_name())?;
        let source_channel = selector.channel_name().as_bytes();
        let source_device = selector.device_name().as_bytes();
        let source_device_offset = BASE_LENGTH
            .checked_add(source_channel.len())
            .and_then(|length| length.checked_add(1))
            .ok_or(SubscriptionWriteCodecError::LengthOverflow)?;
        let declared_length = source_device_offset
            .checked_add(source_device.len())
            .and_then(|length| length.checked_add(1))
            .ok_or(SubscriptionWriteCodecError::LengthOverflow)?;
        let declared_length_u16 = u16::try_from(declared_length)
            .map_err(|_| SubscriptionWriteCodecError::LengthOverflow)?;
        let source_device_offset_u16 = u16::try_from(source_device_offset)
            .map_err(|_| SubscriptionWriteCodecError::LengthOverflow)?;

        let mut frame = vec![0u8; declared_length];
        write_common_header(
            &mut frame,
            declared_length,
            self.sequence,
            self.receiver_channel,
        );
        frame[0x0E..0x10].copy_from_slice(&(BASE_LENGTH as u16).to_be_bytes());
        frame[0x10..0x12].copy_from_slice(&source_device_offset_u16.to_be_bytes());
        let source_channel_end = BASE_LENGTH + source_channel.len();
        frame[BASE_LENGTH..source_channel_end].copy_from_slice(source_channel);
        frame[source_channel_end] = 0;
        let source_device_end = source_device_offset + source_device.len();
        frame[source_device_offset..source_device_end].copy_from_slice(source_device);
        frame[source_device_end] = 0;
        debug_assert_eq!(declared_length_u16 as usize, frame.len());
        Ok(frame)
    }

    /// Decodes and validates a complete set or clear request.
    pub fn decode(frame: &[u8]) -> Result<Self, SubscriptionWriteCodecError> {
        if frame.len() < COMMON_HEADER_LENGTH {
            return Err(SubscriptionWriteCodecError::TruncatedFrame {
                required: COMMON_HEADER_LENGTH,
                available: frame.len(),
            });
        }
        let word = |offset: usize| u16::from_be_bytes([frame[offset], frame[offset + 1]]);
        validate_common_header(
            word(0),
            word(2),
            word(6),
            word(8),
            word(10),
            word(12),
            frame.len(),
        )?;
        let sequence = word(4);
        let receiver_channel = ReceiverChannel::new(word(12))
            .map_err(|_| SubscriptionWriteCodecError::InvalidReceiverChannel { found: 0 })?;

        if frame.len() == BASE_LENGTH {
            if frame[0x0E..BASE_LENGTH].iter().any(|byte| *byte != 0) {
                return Err(SubscriptionWriteCodecError::NonzeroClearTail);
            }
            return Self::new(sequence, receiver_channel, None);
        }

        if frame.len() < BASE_LENGTH {
            return Err(SubscriptionWriteCodecError::InvalidLength {
                declared: BASE_LENGTH,
                actual: frame.len(),
            });
        }
        if frame[0x12..BASE_LENGTH].iter().any(|byte| *byte != 0) {
            return Err(SubscriptionWriteCodecError::NonzeroPadding);
        }
        let source_channel_offset = usize::from(word(0x0E));
        let source_device_offset = usize::from(word(0x10));
        if source_channel_offset != BASE_LENGTH {
            return Err(SubscriptionWriteCodecError::InvalidOffset {
                field: "source-channel",
                found: source_channel_offset,
                expected: BASE_LENGTH,
            });
        }
        let source_channel = decode_name(
            "source-channel",
            frame,
            source_channel_offset,
            source_device_offset,
        )?;
        let source_device = decode_name("source-device", frame, source_device_offset, frame.len())?;
        let selector = TransmitterSelector::new(source_device, source_channel)
            .expect("validated non-empty source names");
        Self::new(sequence, receiver_channel, Some(&selector))
    }

    /// Parses the exactly ten-byte reply and returns only status-one
    /// correlated acceptance.
    pub fn parse_acceptance(
        &self,
        frame: &[u8],
    ) -> Result<SubscriptionAcceptance, SubscriptionAcceptanceError> {
        SubscriptionAcceptance::parse(frame, self.sequence)
    }
}

fn write_common_header(
    frame: &mut [u8],
    length: usize,
    sequence: u16,
    receiver_channel: ReceiverChannel,
) {
    frame[0..2].copy_from_slice(&VIA_MAGIC.to_be_bytes());
    frame[2..4].copy_from_slice(&(length as u16).to_be_bytes());
    frame[4..6].copy_from_slice(&sequence.to_be_bytes());
    frame[6..8].copy_from_slice(&COMMAND.to_be_bytes());
    frame[8..10].copy_from_slice(&0u16.to_be_bytes());
    frame[10..12].copy_from_slice(&SELECTOR.to_be_bytes());
    frame[12..14].copy_from_slice(&receiver_channel.value().to_be_bytes());
}

fn validate_common_header(
    magic: u16,
    declared_length: u16,
    command: u16,
    flags: u16,
    selector: u16,
    receiver_channel: u16,
    actual_length: usize,
) -> Result<(), SubscriptionWriteCodecError> {
    if magic != VIA_MAGIC {
        return Err(SubscriptionWriteCodecError::InvalidMagic { found: magic });
    }
    if usize::from(declared_length) != actual_length {
        return Err(SubscriptionWriteCodecError::LengthMismatch {
            declared: usize::from(declared_length),
            actual: actual_length,
        });
    }
    if command != COMMAND {
        return Err(SubscriptionWriteCodecError::UnexpectedCommand { found: command });
    }
    if flags != 0 {
        return Err(SubscriptionWriteCodecError::InvalidFlags { found: flags });
    }
    if selector != SELECTOR {
        return Err(SubscriptionWriteCodecError::InvalidSelector { found: selector });
    }
    if receiver_channel == 0 {
        return Err(SubscriptionWriteCodecError::InvalidReceiverChannel {
            found: receiver_channel,
        });
    }
    Ok(())
}

pub(crate) fn validate_name(
    field: &'static str,
    name: &str,
) -> Result<(), SubscriptionWriteCodecError> {
    let bytes = name.as_bytes();
    if bytes.is_empty() {
        return Err(SubscriptionWriteCodecError::InvalidName {
            field,
            reason: NameErrorReason::Empty,
        });
    }
    if bytes.len() > MAX_NAME_BYTES {
        return Err(SubscriptionWriteCodecError::InvalidName {
            field,
            reason: NameErrorReason::TooLong {
                length: bytes.len(),
            },
        });
    }
    if bytes.contains(&0) {
        return Err(SubscriptionWriteCodecError::InvalidName {
            field,
            reason: NameErrorReason::ContainsNul,
        });
    }
    if bytes.iter().any(|byte| !byte.is_ascii()) {
        return Err(SubscriptionWriteCodecError::InvalidName {
            field,
            reason: NameErrorReason::NonAscii,
        });
    }
    Ok(())
}

fn decode_name(
    field: &'static str,
    frame: &[u8],
    start: usize,
    end: usize,
) -> Result<String, SubscriptionWriteCodecError> {
    if start >= end || end > frame.len() {
        return Err(SubscriptionWriteCodecError::InvalidOffset {
            field,
            found: start,
            expected: end,
        });
    }
    let bytes = &frame[start..end];
    if bytes.last() != Some(&0) {
        return Err(SubscriptionWriteCodecError::MissingTerminator { field });
    }
    let name = &bytes[..bytes.len() - 1];
    if name.contains(&0) {
        return Err(SubscriptionWriteCodecError::InvalidName {
            field,
            reason: NameErrorReason::ContainsNul,
        });
    }
    validate_name(field, std::str::from_utf8(name).unwrap_or("\u{FFFD}"))?;
    Ok(String::from_utf8(name.to_vec()).expect("ASCII name is UTF-8"))
}

/// A successful status-one acceptance correlated to one request sequence.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SubscriptionAcceptance {
    sequence: u16,
}

impl SubscriptionAcceptance {
    /// Parses and validates a correlated ten-byte status-one reply.
    pub fn parse(
        frame: &[u8],
        expected_sequence: u16,
    ) -> Result<Self, SubscriptionAcceptanceError> {
        if frame.len() != ACCEPTANCE_LENGTH {
            return Err(SubscriptionAcceptanceError::InvalidLength {
                actual: frame.len(),
            });
        }
        let word = |offset: usize| u16::from_be_bytes([frame[offset], frame[offset + 1]]);
        let magic = word(0);
        if magic != VIA_MAGIC {
            return Err(SubscriptionAcceptanceError::InvalidMagic { found: magic });
        }
        let declared_length = word(2);
        if declared_length != ACCEPTANCE_LENGTH as u16 {
            return Err(SubscriptionAcceptanceError::InvalidDeclaredLength {
                found: declared_length,
            });
        }
        let sequence = word(4);
        if sequence != expected_sequence {
            return Err(SubscriptionAcceptanceError::SequenceMismatch {
                received: sequence,
                expected: expected_sequence,
            });
        }
        let command = word(6);
        if command != COMMAND {
            return Err(SubscriptionAcceptanceError::UnexpectedCommand { found: command });
        }
        let status = word(8);
        if status != ACCEPTED_STATUS {
            return Err(SubscriptionAcceptanceError::UnacceptedStatus { status });
        }
        Ok(Self { sequence })
    }

    /// The request sequence that this acceptance confirms.
    pub(crate) const fn accepted(sequence: u16) -> Self {
        Self { sequence }
    }

    pub const fn sequence(self) -> u16 {
        self.sequence
    }

    /// The only status represented by this type: `0x0001`.
    pub const fn status(self) -> u16 {
        ACCEPTED_STATUS
    }
}

/// Construction and request-decoding failures for the exact write codec.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubscriptionWriteCodecError {
    TruncatedFrame {
        required: usize,
        available: usize,
    },
    InvalidMagic {
        found: u16,
    },
    LengthMismatch {
        declared: usize,
        actual: usize,
    },
    InvalidLength {
        declared: usize,
        actual: usize,
    },
    UnexpectedCommand {
        found: u16,
    },
    InvalidFlags {
        found: u16,
    },
    InvalidSelector {
        found: u16,
    },
    InvalidReceiverChannel {
        found: u16,
    },
    InvalidOffset {
        field: &'static str,
        found: usize,
        expected: usize,
    },
    InvalidName {
        field: &'static str,
        reason: NameErrorReason,
    },
    MissingTerminator {
        field: &'static str,
    },
    InvalidPageCapacity {
        found: u8,
    },
    NonzeroPadding,
    NonzeroClearTail,
    LengthOverflow,
}

/// Why a source name violates the bounded ASCII name contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NameErrorReason {
    Empty,
    NonAscii,
    ContainsNul,
    TooLong {
        length: usize,
    },
    /// The name is valid ASCII but not accepted by this write form.
    Unsupported,
}

/// Structural or semantic failure while parsing a write reply.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubscriptionAcceptanceError {
    InvalidLength { actual: usize },
    InvalidMagic { found: u16 },
    InvalidDeclaredLength { found: u16 },
    SequenceMismatch { received: u16, expected: u16 },
    UnexpectedCommand { found: u16 },
    UnacceptedStatus { status: u16 },
}

impl fmt::Display for SubscriptionWriteCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TruncatedFrame {
                required,
                available,
            } => {
                write!(
                    formatter,
                    "write frame needs {required} bytes, got {available}"
                )
            }
            Self::InvalidMagic { found } => write!(formatter, "invalid write magic 0x{found:04x}"),
            Self::LengthMismatch { declared, actual } => {
                write!(
                    formatter,
                    "write length declares {declared} bytes, got {actual}"
                )
            }
            Self::InvalidLength { declared, actual } => {
                write!(
                    formatter,
                    "write length {declared} is invalid for {actual}-byte frame"
                )
            }
            Self::UnexpectedCommand { found } => {
                write!(formatter, "unexpected write command 0x{found:04x}")
            }
            Self::InvalidFlags { found } => {
                write!(formatter, "write flags must be zero, got 0x{found:04x}")
            }
            Self::InvalidSelector { found } => write!(
                formatter,
                "write selector must be 0x1401, got 0x{found:04x}"
            ),
            Self::InvalidReceiverChannel { found } => {
                write!(formatter, "receiver channel {found} must not be zero")
            }
            Self::InvalidOffset {
                field,
                found,
                expected,
            } => write!(
                formatter,
                "{field} offset {found} is invalid; expected {expected}"
            ),
            Self::InvalidName { field, reason } => {
                write!(formatter, "invalid {field} name: {reason}")
            }
            Self::MissingTerminator { field } => {
                write!(formatter, "{field} name is not NUL terminated")
            }
            Self::InvalidPageCapacity { found } => {
                write!(formatter, "page capacity {found} is outside 1..=32")
            }
            Self::NonzeroPadding => write!(formatter, "set padding is not zero"),
            Self::NonzeroClearTail => write!(formatter, "clear request tail is not zero"),
            Self::LengthOverflow => write!(formatter, "write payload length exceeds u16"),
        }
    }
}

impl fmt::Display for NameErrorReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("name is empty"),
            Self::NonAscii => formatter.write_str("name is not ASCII"),
            Self::ContainsNul => formatter.write_str("name contains NUL"),
            Self::TooLong { length } => write!(
                formatter,
                "name is {length} bytes; maximum is {MAX_NAME_BYTES}"
            ),
            Self::Unsupported => formatter.write_str("name is not supported by this write form"),
        }
    }
}

impl fmt::Display for SubscriptionAcceptanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLength { actual } => write!(
                formatter,
                "acceptance must be exactly 10 bytes, got {actual}"
            ),
            Self::InvalidMagic { found } => {
                write!(formatter, "invalid acceptance magic 0x{found:04x}")
            }
            Self::InvalidDeclaredLength { found } => {
                write!(formatter, "acceptance declares {found} bytes instead of 10")
            }
            Self::SequenceMismatch { received, expected } => write!(
                formatter,
                "acceptance sequence {received} does not match {expected}"
            ),
            Self::UnexpectedCommand { found } => {
                write!(formatter, "unexpected acceptance command 0x{found:04x}")
            }
            Self::UnacceptedStatus { status } => {
                write!(formatter, "status 0x{status:04x} is not acceptance")
            }
        }
    }
}

impl Error for SubscriptionWriteCodecError {}
impl Error for NameErrorReason {}
impl Error for SubscriptionAcceptanceError {}
