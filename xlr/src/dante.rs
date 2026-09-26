//! Dante backend: discovery plus a full read of every device.

use serde::{Deserialize, Serialize};
use std::{fmt::Write as _, net::Ipv4Addr, time::Duration};
use xlr_dante::{
    ArcClient, DeviceBrowser, DiscoveredDevice,
    model::{ReceiverSubscription, SubscriptionState},
};

#[derive(Clone, Copy)]
pub struct Options {
    pub interface: Ipv4Addr,
    pub discovery: Duration,
    pub timeout: Duration,
}

#[derive(Deserialize, Serialize)]
pub struct DanteStatus {
    pub interface: Ipv4Addr,
    pub devices: Vec<Device>,
    /// Why discovery itself failed, if it did.
    pub error: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct Device {
    pub name: String,
    /// The host this device is attached to, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    pub product: Option<String>,
    pub model: Option<String>,
    pub manufacturer: Option<String>,
    pub address: String,
    pub transmitters: Vec<Transmitter>,
    pub receivers: Vec<Receiver>,
    /// Why the device could not be read completely, if it could not.
    pub error: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct Transmitter {
    pub channel: u16,
    pub name: String,
    /// Your names for this channel.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

#[derive(Deserialize, Serialize)]
pub struct Receiver {
    pub channel: u16,
    pub name: Option<String>,
    /// The transmitter this receiver is subscribed to, or `null`.
    pub source: Option<Source>,
    /// Your names for this receiver.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Source {
    pub device: String,
    pub channel: String,
    /// A source on the receiving device itself rather than on the network
    /// (Dante Via routing a local application), so it cannot be routed
    /// elsewhere.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub local: bool,
    /// Your names for the source channel.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
        host: None,
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
            local: tx.device_name() == ".",
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
        // One set of columns for every device, so labels and arrows line up.
        let channel_names = devices.iter().flat_map(|device| {
            let tx = device.transmitters.iter().map(|tx| tx.name.len());
            let rx = device
                .receivers
                .iter()
                .map(|rx| rx.name.as_deref().map_or(0, str::len));
            tx.chain(rx)
        });
        let name_width = channel_names.max().unwrap_or(0);
        let tag_width = devices
            .iter()
            .flat_map(|device| device.receivers.iter().map(|rx| tags(&rx.names).len()))
            .max()
            .unwrap_or(0);
        for device in devices {
            let product = device.product.as_deref().unwrap_or("unknown product");
            let host = device
                .host
                .as_ref()
                .map_or_else(String::new, |host| format!(", on {host}"));
            let _ = writeln!(
                out,
                "{}  ({product}, {}{host})",
                device.name, device.address
            );
            if let Some(error) = &device.error {
                let _ = writeln!(out, "  error: {error}");
            }
            let transmitters = &device.transmitters;
            let mut index = 0;
            while index < transmitters.len() {
                // Collapse runs like "Channel 1" … "Channel 8" on tx 1–8.
                let mut run = 0;
                if let Some((prefix, _)) = numbered(&transmitters[index]) {
                    while let Some(tx) = transmitters.get(index + run) {
                        let follows =
                            run == 0 || tx.channel == transmitters[index + run - 1].channel + 1;
                        if !(follows
                            && tx.names.is_empty()
                            && numbered(tx).is_some_and(|(p, _)| p == prefix))
                        {
                            break;
                        }
                        run += 1;
                    }
                    if run >= 3 {
                        let (first, last) = (
                            transmitters[index].channel,
                            transmitters[index + run - 1].channel,
                        );
                        let _ = writeln!(out, "  tx {first}–{last}  {prefix}{first}–{last}");
                        index += run;
                        continue;
                    }
                }
                let tx = &transmitters[index];
                let line = format!(
                    "  tx {:>3}  {:<name_width$}{}",
                    tx.channel,
                    tx.name,
                    tags(&tx.names)
                );
                let _ = writeln!(out, "{}", line.trim_end());
                index += 1;
            }
            let receivers = &device.receivers;
            let mut index = 0;
            while index < receivers.len() {
                // Collapse runs of unnamed, unsubscribed receivers.
                let idle = |rx: &Receiver| rx.source.is_none() && rx.names.is_empty();
                let run = receivers[index..].iter().take_while(|rx| idle(rx)).count();
                if run >= 2 {
                    let (first, last) = (&receivers[index], &receivers[index + run - 1]);
                    let _ = writeln!(out, "  rx {}–{}  unsubscribed", first.channel, last.channel);
                    index += run;
                    continue;
                }
                let rx = &receivers[index];
                let source = match &rx.source {
                    Some(source) if source.local => {
                        format!("{} (local to {})", source.channel, source.device)
                    }
                    Some(source) => format!(
                        "{}@{}{}",
                        source.channel,
                        source.device,
                        tags(&source.names)
                    ),
                    None => "-".to_owned(),
                };
                let _ = writeln!(
                    out,
                    "  rx {:>3}  {:<name_width$}{:<tag_width$}  <- {source}",
                    rx.channel,
                    rx.name.as_deref().unwrap_or(""),
                    tags(&rx.names),
                );
                index += 1;
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

/// Splits a channel name like `Channel 9` into (`Channel `, 9) when its
/// trailing number equals its channel number, so runs can be collapsed.
fn numbered(tx: &Transmitter) -> Option<(&str, u16)> {
    let digits = tx.name.len() - tx.name.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let (prefix, number) = tx.name.split_at(tx.name.len() - digits);
    let number: u16 = number.parse().ok()?;
    (!prefix.is_empty() && number == tx.channel).then_some((prefix, number))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx(channel: u16, name: &str, names: &[&str]) -> Transmitter {
        Transmitter {
            channel,
            name: name.to_owned(),
            names: names.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    fn rx(channel: u16, source: Option<&str>) -> Receiver {
        Receiver {
            channel,
            name: Some(format!("In {channel}")),
            source: source.map(|channel| Source {
                device: "mixer".to_owned(),
                channel: channel.to_owned(),
                local: false,
                names: Vec::new(),
            }),
            names: Vec::new(),
        }
    }

    fn render(transmitters: Vec<Transmitter>, receivers: Vec<Receiver>) -> String {
        DanteStatus {
            interface: "10.0.0.1".parse().unwrap(),
            devices: vec![Device {
                name: "box".to_owned(),
                host: Some("desk".to_owned()),
                product: None,
                model: None,
                manufacturer: None,
                address: "10.0.0.2:4440".to_owned(),
                transmitters,
                receivers,
                error: None,
            }],
            error: None,
        }
        .render()
    }

    #[test]
    fn numbered_transmitter_runs_collapse_around_named_ones() {
        let out = render(
            (1..=6)
                .map(|n| tx(n, &format!("Out {n}"), if n == 4 { &["mine"] } else { &[] }))
                .collect(),
            Vec::new(),
        );
        assert!(
            out.contains("(unknown product, 10.0.0.2:4440, on desk)"),
            "{out}"
        );
        assert!(out.contains("tx 1–3  Out 1–3"), "{out}");
        assert!(out.contains("Out 4  [mine]"), "{out}");
        assert!(
            !out.contains("tx 5–6"),
            "runs shorter than three stay expanded: {out}"
        );
        assert!(
            out.lines().all(|line| line == line.trim_end()),
            "no trailing spaces"
        );
    }

    #[test]
    fn idle_receiver_runs_collapse() {
        let out = render(
            Vec::new(),
            vec![
                rx(1, Some("A")),
                rx(2, None),
                rx(3, None),
                rx(4, None),
                rx(5, Some("B")),
            ],
        );
        assert!(out.contains("rx 2–4  unsubscribed"), "{out}");
        assert!(
            out.contains("<- A@mixer") && out.contains("<- B@mixer"),
            "{out}"
        );
    }
}
