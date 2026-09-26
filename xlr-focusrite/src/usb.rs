//! Finding Scarlett interfaces and talking to their vendor control
//! interface.
//!
//! Only the vendor-specific control interface is ever claimed. The audio
//! interfaces stay with the operating system's audio driver: this module
//! never detaches a driver, changes the USB configuration, or resets the
//! device, so audio keeps flowing while it runs. Only one program can hold
//! the control interface; on macOS, quit Focusrite Control first.

use crate::{
    gen3,
    protocol::{self, Opcode, ProtocolError},
};
use nusb::{
    DeviceInfo, Interface, MaybeFuture,
    transfer::{ControlIn, ControlOut, ControlType, Recipient, TransferError},
};
use std::{error::Error as StdError, fmt, thread, time::Duration};

/// Focusrite's USB vendor ID.
pub const VENDOR_ID: u16 = 0x1235;
/// USB interface class of the vendor-specific control interface.
const VENDOR_CLASS: u8 = 0xff;
/// How many times a response is fetched while waiting for it to be ready.
const RESPONSE_ATTEMPTS: u32 = 20;
const RESPONSE_RETRY_DELAY: Duration = Duration::from_millis(5);

/// A model this crate knows how to read.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Model {
    Scarlett18i20Gen3,
}

impl Model {
    fn from_product_id(product_id: u16) -> Option<Self> {
        match product_id {
            0x8215 => Some(Self::Scarlett18i20Gen3),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Scarlett18i20Gen3 => "Scarlett 18i20 3rd Gen",
        }
    }
}

/// A Focusrite USB device, as enumerated without opening it.
#[derive(Clone, Debug)]
pub struct FoundDevice {
    info: DeviceInfo,
}

impl FoundDevice {
    pub fn product_id(&self) -> u16 {
        self.info.product_id()
    }

    /// The supported model, or `None` for Focusrite devices this crate does
    /// not support.
    pub fn model(&self) -> Option<Model> {
        Model::from_product_id(self.product_id())
    }

    pub fn product(&self) -> Option<&str> {
        self.info.product_string()
    }

    pub fn serial_number(&self) -> Option<&str> {
        self.info.serial_number()
    }

    /// `bcdDevice`, which Focusrite uses as a firmware-family marker.
    pub fn device_version(&self) -> u16 {
        self.info.device_version()
    }

    /// The vendor-specific control interface number, if the device has one.
    pub fn control_interface(&self) -> Option<u8> {
        self.info
            .interfaces()
            .find(|interface| interface.class() == VENDOR_CLASS)
            .map(|interface| interface.interface_number())
    }
}

/// Lists connected Focusrite devices without opening any of them.
pub fn find_devices() -> Result<Vec<FoundDevice>, UsbError> {
    Ok(nusb::list_devices()
        .wait()
        .map_err(UsbError::Enumerate)?
        .filter(|info| info.vendor_id() == VENDOR_ID)
        .map(|info| FoundDevice { info })
        .collect())
}

/// An open, initialized control session with one supported device.
pub struct Session {
    interface: Interface,
    interface_number: u8,
    timeout: Duration,
    sequence: u16,
    firmware: Option<u32>,
    model: Model,
}

impl Session {
    /// Claims the device's control interface and runs the initialization
    /// handshake. Unsupported models are refused before the device is
    /// opened.
    pub fn open(device: &FoundDevice, timeout: Duration) -> Result<Self, UsbError> {
        let model = device.model().ok_or(UsbError::UnsupportedModel {
            product_id: device.product_id(),
        })?;
        let interface_number = device
            .control_interface()
            .ok_or(UsbError::NoControlInterface)?;
        let opened = device.info.open().wait().map_err(UsbError::Open)?;
        let interface = opened
            .claim_interface(interface_number)
            .wait()
            .map_err(UsbError::Claim)?;
        let mut session = Self {
            interface,
            interface_number,
            timeout,
            sequence: 0,
            firmware: None,
            model,
        };
        session.initialize()?;
        Ok(session)
    }

    pub fn model(&self) -> Model {
        self.model
    }

    /// Firmware build number reported during initialization.
    pub fn firmware(&self) -> Option<u32> {
        self.firmware
    }

    /// Reads and decodes the 18i20 3rd Gen switch settings.
    pub fn read_gen3_settings(&mut self) -> Result<gen3::Settings, UsbError> {
        let results = gen3::READS
            .iter()
            .map(|read| self.read_data(read.offset, read.size))
            .collect::<Result<Vec<_>, _>>()?;
        gen3::Settings::decode(&results).map_err(UsbError::Settings)
    }

    /// Reads `size` bytes of configuration at `offset`.
    pub fn read_data(&mut self, offset: u32, size: u32) -> Result<Vec<u8>, UsbError> {
        self.command(Opcode::GetData { offset, size })
    }

    fn initialize(&mut self) -> Result<(), UsbError> {
        let handshake = self.receive(protocol::REQUEST_HANDSHAKE, protocol::HANDSHAKE_LENGTH)?;
        if handshake.len() != protocol::HANDSHAKE_LENGTH {
            return Err(UsbError::Handshake {
                length: handshake.len(),
            });
        }
        self.sequence = 0;
        self.command(Opcode::Init1)?;
        let init_2 = self.command(Opcode::Init2)?;
        self.firmware = protocol::firmware_version(&init_2);
        Ok(())
    }

    fn command(&mut self, opcode: Opcode) -> Result<Vec<u8>, UsbError> {
        let sequence = self.sequence;
        self.sequence = sequence.wrapping_add(1);
        let packet = protocol::encode(opcode, sequence).map_err(UsbError::Protocol)?;
        self.interface
            .control_out(
                ControlOut {
                    control_type: ControlType::Class,
                    recipient: Recipient::Interface,
                    request: protocol::REQUEST_TRANSMIT,
                    value: 0,
                    index: u16::from(self.interface_number),
                    data: &packet,
                },
                self.timeout,
            )
            .wait()
            .map_err(UsbError::Transfer)?;

        let length = protocol::HEADER_LENGTH + opcode.response_length();
        let mut last = None;
        for attempt in 0..RESPONSE_ATTEMPTS {
            if attempt > 0 {
                thread::sleep(RESPONSE_RETRY_DELAY);
            }
            let response = self.receive(protocol::REQUEST_RECEIVE, length)?;
            match protocol::decode(opcode, sequence, &response) {
                Ok(payload) => return Ok(payload.to_vec()),
                // Not ready yet: the previous response is still latched.
                Err(error @ (ProtocolError::Opcode { .. } | ProtocolError::Sequence { .. })) => {
                    last = Some(error);
                }
                Err(error) => return Err(UsbError::Protocol(error)),
            }
        }
        Err(UsbError::Protocol(last.expect("at least one attempt")))
    }

    fn receive(&self, request: u8, length: usize) -> Result<Vec<u8>, UsbError> {
        self.interface
            .control_in(
                ControlIn {
                    control_type: ControlType::Class,
                    recipient: Recipient::Interface,
                    request,
                    value: 0,
                    index: u16::from(self.interface_number),
                    length: u16::try_from(length).expect("bounded response length"),
                },
                self.timeout,
            )
            .wait()
            .map_err(UsbError::Transfer)
    }
}

/// Why talking to a device failed.
#[derive(Debug)]
pub enum UsbError {
    Enumerate(nusb::Error),
    UnsupportedModel {
        product_id: u16,
    },
    NoControlInterface,
    Open(nusb::Error),
    /// The control interface could not be claimed, most often because
    /// Focusrite Control holds it.
    Claim(nusb::Error),
    Transfer(TransferError),
    Handshake {
        length: usize,
    },
    Protocol(ProtocolError),
    Settings(gen3::SettingsError),
}

impl fmt::Display for UsbError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Enumerate(error) => write!(formatter, "could not list USB devices: {error}"),
            Self::UnsupportedModel { product_id } => write!(
                formatter,
                "Focusrite product 0x{product_id:04x} is not supported yet"
            ),
            Self::NoControlInterface => {
                formatter.write_str("device has no vendor control interface")
            }
            Self::Open(error) => write!(formatter, "could not open the device: {error}"),
            Self::Claim(error) => write!(
                formatter,
                "could not claim the control interface ({error}); quit Focusrite Control and retry"
            ),
            Self::Transfer(error) => write!(formatter, "USB transfer failed: {error}"),
            Self::Handshake { length } => {
                write!(formatter, "handshake returned {length} bytes, expected 24")
            }
            Self::Protocol(error) => write!(formatter, "{error}"),
            Self::Settings(error) => write!(formatter, "{error}"),
        }
    }
}

impl StdError for UsbError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Enumerate(error) | Self::Open(error) | Self::Claim(error) => Some(error),
            Self::Transfer(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::Settings(error) => Some(error),
            Self::UnsupportedModel { .. } | Self::NoControlInterface | Self::Handshake { .. } => {
                None
            }
        }
    }
}
