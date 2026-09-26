//! The Scarlett USB control protocol framing, without any I/O.
//!
//! Commands travel as class control transfers addressed to the vendor
//! interface: `bRequest` 0 fetches the initial handshake block, 2 sends a
//! command, and 3 fetches its response. Commands and responses share a
//! 16-byte little-endian header:
//!
//! ```text
//! u32 opcode | u16 payload size | u16 sequence | u32 error | u32 padding
//! ```
//!
//! [`Opcode`] is a closed allowlist: only the handshake and data reads exist,
//! so nothing in this crate can encode a write, save, flash, or reboot.

use std::{error::Error, fmt};

/// `bRequest` that fetches the initial handshake block.
pub const REQUEST_HANDSHAKE: u8 = 0;
/// `bRequest` that sends one command packet.
pub const REQUEST_TRANSMIT: u8 = 2;
/// `bRequest` that fetches one response packet.
pub const REQUEST_RECEIVE: u8 = 3;
/// Length of the handshake block.
pub const HANDSHAKE_LENGTH: usize = 24;
/// Length of the command/response header.
pub const HEADER_LENGTH: usize = 16;
/// Largest data block this crate reads in one command.
pub const MAX_READ: u32 = 1024;
/// Length of the `INIT_2` response payload.
pub const INIT_2_RESPONSE_LENGTH: usize = 84;

/// The only commands this crate can send.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Opcode {
    /// First initialization step; no payload either way.
    Init1,
    /// Second initialization step; the response carries the firmware version.
    Init2,
    /// Reads `size` bytes of device configuration at `offset`.
    GetData { offset: u32, size: u32 },
}

impl Opcode {
    /// The wire opcode.
    pub fn code(self) -> u32 {
        match self {
            Self::Init1 => 0x0000_0000,
            Self::Init2 => 0x0000_0002,
            Self::GetData { .. } => 0x0080_0000,
        }
    }

    /// The exact response payload size this command must produce.
    pub fn response_length(self) -> usize {
        match self {
            Self::Init1 => 0,
            Self::Init2 => INIT_2_RESPONSE_LENGTH,
            Self::GetData { size, .. } => size as usize,
        }
    }

    fn payload(self) -> Vec<u8> {
        match self {
            Self::Init1 | Self::Init2 => Vec::new(),
            Self::GetData { offset, size } => {
                let mut payload = offset.to_le_bytes().to_vec();
                payload.extend_from_slice(&size.to_le_bytes());
                payload
            }
        }
    }

    fn is_init(self) -> bool {
        matches!(self, Self::Init1 | Self::Init2)
    }
}

/// Encodes one command packet.
///
/// # Errors
/// Rejects reads of zero bytes or more than [`MAX_READ`].
pub fn encode(opcode: Opcode, sequence: u16) -> Result<Vec<u8>, ProtocolError> {
    if let Opcode::GetData { size, .. } = opcode
        && (size == 0 || size > MAX_READ)
    {
        return Err(ProtocolError::ReadSize { size });
    }
    let payload = opcode.payload();
    let mut packet = Vec::with_capacity(HEADER_LENGTH + payload.len());
    packet.extend_from_slice(&opcode.code().to_le_bytes());
    packet.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    packet.extend_from_slice(&sequence.to_le_bytes());
    packet.extend_from_slice(&[0; 8]);
    packet.extend_from_slice(&payload);
    Ok(packet)
}

/// Validates a response to `opcode` sent with `sequence` and returns its
/// payload.
///
/// The initialization steps are also accepted with a zero sequence, which
/// devices use while the handshake is in progress.
pub fn decode(opcode: Opcode, sequence: u16, response: &[u8]) -> Result<&[u8], ProtocolError> {
    if response.len() < HEADER_LENGTH {
        return Err(ProtocolError::Truncated {
            length: response.len(),
        });
    }
    let u16_at = |at: usize| u16::from_le_bytes([response[at], response[at + 1]]);
    let u32_at = |at: usize| {
        u32::from_le_bytes([
            response[at],
            response[at + 1],
            response[at + 2],
            response[at + 3],
        ])
    };
    let found = u32_at(0);
    if found != opcode.code() {
        return Err(ProtocolError::Opcode {
            found,
            expected: opcode.code(),
        });
    }
    let received = u16_at(6);
    if received != sequence && !(opcode.is_init() && received == 0) {
        return Err(ProtocolError::Sequence {
            received,
            expected: sequence,
        });
    }
    let error = u32_at(8);
    if error != 0 {
        return Err(ProtocolError::Device { code: error });
    }
    let size = usize::from(u16_at(4));
    let expected = opcode.response_length();
    if size != expected || response.len() != HEADER_LENGTH + size {
        return Err(ProtocolError::Size {
            declared: size,
            received: response.len() - HEADER_LENGTH,
            expected,
        });
    }
    Ok(&response[HEADER_LENGTH..])
}

/// Firmware build number from the `INIT_2` response payload.
pub fn firmware_version(init_2: &[u8]) -> Option<u32> {
    let bytes = init_2.get(8..12)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

/// A response that does not answer the command that was sent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    Truncated {
        length: usize,
    },
    Opcode {
        found: u32,
        expected: u32,
    },
    Sequence {
        received: u16,
        expected: u16,
    },
    Device {
        code: u32,
    },
    Size {
        declared: usize,
        received: usize,
        expected: usize,
    },
    ReadSize {
        size: u32,
    },
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { length } => {
                write!(
                    formatter,
                    "response is {length} bytes, shorter than its header"
                )
            }
            Self::Opcode { found, expected } => write!(
                formatter,
                "response opcode 0x{found:08x} does not match request 0x{expected:08x}"
            ),
            Self::Sequence { received, expected } => write!(
                formatter,
                "response sequence {received} does not match request {expected}"
            ),
            Self::Device { code } => write!(formatter, "device reported error {code}"),
            Self::Size {
                declared,
                received,
                expected,
            } => write!(
                formatter,
                "response declares {declared} bytes and carries {received}; expected {expected}"
            ),
            Self::ReadSize { size } => {
                write!(formatter, "read of {size} bytes is outside 1..={MAX_READ}")
            }
        }
    }
}

impl Error for ProtocolError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(opcode: u32, sequence: u16, error: u32, payload: &[u8]) -> Vec<u8> {
        let mut frame = opcode.to_le_bytes().to_vec();
        frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        frame.extend_from_slice(&sequence.to_le_bytes());
        frame.extend_from_slice(&error.to_le_bytes());
        frame.extend_from_slice(&[0; 4]);
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn get_data_encodes_offset_and_size_little_endian() {
        let packet = encode(
            Opcode::GetData {
                offset: 0x9c,
                size: 2,
            },
            7,
        )
        .unwrap();
        assert_eq!(
            packet,
            [
                0x00, 0x00, 0x80, 0x00, 8, 0, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x9c, 0, 0, 0, 2, 0, 0,
                0
            ]
        );
    }

    #[test]
    fn init_encodes_an_empty_payload() {
        assert_eq!(encode(Opcode::Init1, 0).unwrap(), [0; 16]);
        assert_eq!(
            encode(Opcode::Init2, 1).unwrap()[..8],
            [2, 0, 0, 0, 0, 0, 1, 0]
        );
    }

    #[test]
    fn reads_outside_bounds_are_refused() {
        for size in [0, MAX_READ + 1] {
            assert_eq!(
                encode(Opcode::GetData { offset: 0, size }, 0),
                Err(ProtocolError::ReadSize { size })
            );
        }
    }

    #[test]
    fn decode_returns_the_payload_of_a_matching_response() {
        let read = Opcode::GetData {
            offset: 0x84,
            size: 3,
        };
        let frame = response(0x80_0000, 5, 0, &[1, 0, 1]);
        assert_eq!(decode(read, 5, &frame), Ok(&[1u8, 0, 1][..]));
    }

    #[test]
    fn decode_rejects_mismatches() {
        let read = Opcode::GetData {
            offset: 0x84,
            size: 1,
        };
        assert!(matches!(
            decode(read, 5, &response(0x2, 5, 0, &[0])),
            Err(ProtocolError::Opcode { .. })
        ));
        assert!(matches!(
            decode(read, 5, &response(0x80_0000, 4, 0, &[0])),
            Err(ProtocolError::Sequence { .. })
        ));
        assert!(matches!(
            decode(read, 5, &response(0x80_0000, 5, 9, &[0])),
            Err(ProtocolError::Device { code: 9 })
        ));
        assert!(matches!(
            decode(read, 5, &response(0x80_0000, 5, 0, &[0, 0])),
            Err(ProtocolError::Size { .. })
        ));
        assert!(matches!(
            decode(read, 5, &[0; 8]),
            Err(ProtocolError::Truncated { length: 8 })
        ));
    }

    #[test]
    fn init_accepts_a_zero_sequence_but_reads_do_not() {
        let init = response(0x2, 0, 0, &[0; INIT_2_RESPONSE_LENGTH]);
        assert!(decode(Opcode::Init2, 1, &init).is_ok());
        let read = Opcode::GetData { offset: 0, size: 1 };
        assert!(decode(read, 1, &response(0x80_0000, 0, 0, &[0])).is_err());
    }

    #[test]
    fn firmware_version_is_read_from_the_init_2_payload() {
        let mut payload = [0u8; INIT_2_RESPONSE_LENGTH];
        payload[8..12].copy_from_slice(&1644u32.to_le_bytes());
        assert_eq!(firmware_version(&payload), Some(1644));
    }
}
