//! Programmatic control for Dante audio networks.
//!
//! `xlr-dante` finds Dante devices, lists their channels, reads what every
//! receiver channel is subscribed to, and changes those subscriptions. It
//! implements observed message forms rather than the Dante control protocol
//! generally: it encodes and decodes exactly the messages listed below and
//! refuses anything else rather than guessing.
//!
//! | Operation | Transport | Entry point |
//! |---|---|---|
//! | Find devices | mDNS `_netaudio-arc._udp` | [`DeviceBrowser::browse`] |
//! | Device name, channel counts | ARC `0x1002`, `0x1000` | [`ArcClient::device_name`], [`ArcClient::channel_counts`] |
//! | List transmitter channels | ARC `0x2000` | [`ArcClient::transmitter_channels`] |
//! | Read receiver subscriptions | ARC `0x3000` | [`ArcClient::receiver_subscriptions`], [`ArcClient::query_subscription`] |
//! | Set or clear a subscription | ARC `0x3010` (Dante Via form) | [`ArcClient::apply_subscription`] |
//! | Resolve one transmitter channel | mDNS `_netaudio-chan._udp` | [`ChannelServiceClient::query`] |
//!
//! # Layering
//!
//! The crate separates three concerns; each builds only on the ones above
//! it:
//!
//! ```text
//! codec layer  → pure bytes ↔ domain values (no I/O at all)
//! client layer → synchronous fixed-peer, one-send UDP correlation
//! consumer     → chooses runtime, retries, policy, control surface
//! ```
//!
//! The [`arc`] codecs never touch a socket. The [`client`] connects one
//! socket to one immutable peer and sends each request once. Queries discard
//! uncorrelated same-peer datagrams until a sequence-correlated datagram
//! arrives or the original absolute deadline expires. Writes receive exactly
//! one datagram, which must come from the immutable peer and pass the exact
//! ten-byte acceptance parser. A socket consumes every 16-bit sequence at
//! most once and then fails closed. Async wrappers, retransmission policy,
//! and command surfaces belong to the consumer.
//!
//! # Example
//!
//! Find devices, then print every receiver channel's subscription:
//!
//! ```no_run
//! use std::time::Duration;
//! use xlr_dante::model::SubscriptionState;
//! use xlr_dante::{ArcClient, DeviceBrowser};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let browser = DeviceBrowser::new("192.168.1.10".parse()?, Duration::from_secs(2))?;
//! for device in browser.browse()? {
//!     let mut client = ArcClient::connect(device.arc_address(), Duration::from_millis(500))?;
//!     for rx in client.receiver_subscriptions()? {
//!         let source = match rx.state() {
//!             SubscriptionState::Subscribed(tx) => {
//!                 format!("{}@{}", tx.channel_name(), tx.device_name())
//!             }
//!             SubscriptionState::Unsubscribed => "-".to_owned(),
//!         };
//!         println!("{} rx {}: {source}", device.name(), rx.receiver_channel().value());
//!     }
//! }
//! # Ok(())
//! # }
//! ```

pub mod error;
mod mdns;

pub mod arc;
pub mod channel_service;
pub mod client;
pub mod discovery;
pub mod model;

pub use arc::subscription_write::{
    NameErrorReason, SubscriptionAcceptance, SubscriptionAcceptanceError,
    SubscriptionWriteCodecError, SubscriptionWriteRequest,
};
pub use channel_service::{
    ChannelServiceClient, ChannelServiceError, ChannelServiceEvidence, ChannelServiceIoStage,
    ChannelServiceName, ChannelServiceSrv,
};
pub use client::{ArcClient, ArcClientError, IoStage};
pub use discovery::{DeviceBrowser, DiscoveredDevice, DiscoveryError, DiscoveryIoStage};
pub use error::{ArcCodecError, RecordLayoutError, StringEncodingError, StringOffsetError};

#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
