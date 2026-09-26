//! Programmatic control for Focusrite Scarlett interfaces.
//!
//! `xlr-focusrite` talks to a Scarlett's vendor-specific USB control
//! interface directly, without Focusrite Control running. It currently
//! reads the Scarlett 18i20 3rd Gen's front-panel switches: phantom power,
//! pad, air, line/instrument, and monitor mute/dim.
//!
//! Safety properties:
//!
//! - **Read-only.** [`protocol::Opcode`] is a closed allowlist of the
//!   initialization handshake and configuration reads; nothing here can
//!   encode a write, save, flash, or reboot command.
//! - **Audio is never interrupted.** Only the vendor control interface is
//!   claimed; the audio interfaces, the USB configuration, and any kernel
//!   driver are left alone.
//! - **Fail closed.** Unsupported models are refused before the device is
//!   opened, and configuration bytes that do not look like switch values are
//!   refused rather than guessed at.
//!
//! ```no_run
//! use std::time::Duration;
//! use xlr_focusrite::usb::{Session, find_devices};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! for device in find_devices()? {
//!     if device.model().is_none() {
//!         continue;
//!     }
//!     let mut session = Session::open(&device, Duration::from_millis(500))?;
//!     let settings = session.read_gen3_settings()?;
//!     println!("48V on inputs 1-4: {}", settings.phantom_groups[0]);
//! }
//! # Ok(())
//! # }
//! ```

pub mod gen3;
pub mod protocol;
pub mod usb;
