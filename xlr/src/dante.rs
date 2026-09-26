//! Dante backend: discovery plus a full read of every device.

use serde::Serialize;
use std::{fmt::Write as _, net::Ipv4Addr, time::Duration};
use xlr_dante::{
    ArcClient, DeviceBrowser, DiscoveredDevice,
    model::{ReceiverSubscription, SubscriptionState},
};

pub struct Options {
    pub interface: Ipv4Addr,
    pub discovery: Duration,
    pub timeout: Duration,
}

#[derive(Serialize)]
pub struct DanteStatus {
    pub interface: Ipv4Addr,
    pub devices: Vec<Device>,
    /// Why discovery itself failed, if it did.
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct Device {
    pub name: String,
    pub product: Option<String>,
    pub model: Option<String>,
    pub manufacturer: Option<String>,
    pub address: String,
    pub transmitters: Vec<Transmitter>,
    pub receivers: Vec<Receiver>,
    /// Why the device could not be read completely, if it could not.
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct Transmitter {
    pub channel: u16,
    pub name: String,
    /// Your names for this channel.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

#[derive(Serialize)]
pub struct Receiver {
    pub channel: u16,
    pub name: Option<String>,
    /// The transmitter this receiver is subscribed to, or `null`.
    pub source: Option<Source>,
    /// Your names for this receiver.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Source {
    pub device: String,
    pub channel: String,
    /// Your names for the source channel.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

pub fn status(options: &Options) -> DanteStatus {
    let found = DeviceBrowser::new(options.interface, options.discovery)
        .and_then(|browser| browser.browse());
    let (devices, error) = match found {
        Ok(found) => (
            found
                .iter()
                .map(|device| read_device(device, options.timeout))
                .collect(),
            None,
        ),
        Err(error) => (Vec::new(), Some(error.to_string())),
    };
    DanteStatus {
        interface: options.interface,
        devices,
        error,
    }
}

fn read_device(found: &DiscoveredDevice, timeout: Duration) -> Device {
    let mut device = Device {
        name: found.name().to_owned(),
        product: found.product().map(str::to_owned),
        model: found.model().map(str::to_owned),
        manufacturer: found.manufacturer().map(str::to_owned),
        address: found.arc_address().to_string(),
        transmitters: Vec::new(),
        receivers: Vec::new(),
        error: None,
    };
    let result = (|| -> Result<(), xlr_dante::ArcClientError> {
        let mut client = ArcClient::connect(found.arc_address(), timeout)?;
        device.transmitters = client
            .transmitter_channels()?
            .into_iter()
            .map(|channel| Transmitter {
                channel: channel.number(),
                name: channel.name().to_owned(),
                names: Vec::new(),
            })
            .collect();
        device.receivers = client
            .receiver_subscriptions()?
            .iter()
            .map(|entry| receiver(entry, found.name()))
            .collect();
        Ok(())
    })();
    if let Err(error) = result {
        device.error = Some(error.to_string());
    }
    device
}

/// Normalizes one receiver entry. Dante Via names its own device `.`; the
/// output always uses real device names so every source is addressable.
fn receiver(entry: &ReceiverSubscription, own_name: &str) -> Receiver {
    Receiver {
        channel: entry.receiver_channel().value(),
        name: entry.name().map(str::to_owned),
        source: source(entry.state(), own_name),
        names: Vec::new(),
    }
}

/// The normalized source of a subscription state, resolving Via's `.`.
pub fn source(state: &SubscriptionState, own_name: &str) -> Option<Source> {
    match state {
        SubscriptionState::Subscribed(tx) => Some(Source {
            device: match tx.device_name() {
                "." => own_name.to_owned(),
                name => name.to_owned(),
            },
            channel: tx.channel_name().to_owned(),
            names: Vec::new(),
        }),
        SubscriptionState::Unsubscribed => None,
    }
}

impl DanteStatus {
    pub fn has_errors(&self) -> bool {
        self.error.is_some() || self.devices.iter().any(|device| device.error.is_some())
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        let devices = &self.devices;
        if let Some(error) = &self.error {
            let _ = writeln!(out, "Dante discovery failed: {error}");
        } else if devices.is_empty() {
            let _ = writeln!(out, "No Dante devices found via {}.", self.interface);
        }
        for device in devices {
            let product = device.product.as_deref().unwrap_or("unknown product");
            let _ = writeln!(out, "{}  ({product}, {})", device.name, device.address);
            if let Some(error) = &device.error {
                let _ = writeln!(out, "  error: {error}");
            }
            for tx in &device.transmitters {
                let _ = writeln!(
                    out,
                    "  tx {:>3}  {}{}",
                    tx.channel,
                    tx.name,
                    tags(&tx.names)
                );
            }
            let label =
                |rx: &Receiver| format!("{}{}", rx.name.as_deref().unwrap_or(""), tags(&rx.names));
            let width = device
                .receivers
                .iter()
                .map(|rx| label(rx).len())
                .max()
                .unwrap_or(0);
            for rx in &device.receivers {
                let name = label(rx);
                let source = match &rx.source {
                    Some(source) => {
                        format!(
                            "{}@{}{}",
                            source.channel,
                            source.device,
                            tags(&source.names)
                        )
                    }
                    None => "-".to_owned(),
                };
                let _ = writeln!(out, "  rx {:>3}  {name:<width$}  <- {source}", rx.channel);
            }
            out.push('\n');
        }
        out
    }
}

/// Renders names as `  [a, b]`, or nothing.
pub fn tags(names: &[String]) -> String {
    if names.is_empty() {
        String::new()
    } else {
        format!("  [{}]", names.join(", "))
    }
}
