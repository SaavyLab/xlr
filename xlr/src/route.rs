//! `xlr route`: change one receiver's subscription, safely.

use crate::{
    address::Address,
    config::Config,
    dante::{self, Options, Source},
};
use serde::Serialize;
use std::{thread, time::Duration};
use xlr_dante::{
    ArcClient, DeviceBrowser, DiscoveredDevice,
    model::{ReceiverChannel, TransmitterSelector},
};

/// Dante Via (ARC protocol 2.8.15): the `0x3010` write.
const VIA_PROTOCOL: u16 = 0x280F;
/// Dante hardware (ARC protocol 2.8.9): the paged `0x3410` write.
const PAGED_PROTOCOL: u16 = 0x2809;
const READ_BACK_ATTEMPTS: u32 = 5;
const READ_BACK_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Serialize)]
pub struct Outcome {
    pub device: String,
    pub receiver: ReceiverRef,
    pub before: Option<Source>,
    pub requested: Option<Source>,
    pub after: Option<Source>,
    /// Whether a write was sent.
    pub wrote: bool,
    pub dry_run: bool,
    /// Why the write reported failure, if it did. The device may still have
    /// applied it; `after` is what it reports now.
    pub write_error: Option<String>,
    /// Whether the receiver now matches the request (or, for a dry run,
    /// whether the plan is valid).
    pub ok: bool,
}

#[derive(Serialize)]
pub struct ReceiverRef {
    pub channel: u16,
    pub name: Option<String>,
}

pub fn run(
    options: &Options,
    config: &Config,
    receiver: &str,
    source: Option<&str>,
    dry_run: bool,
) -> Result<Outcome, Box<dyn std::error::Error>> {
    let (device_name, receiver_name) = resolve(config, receiver, Kind::Receiver)?;
    let requested_source = source
        .map(|source| resolve(config, source, Kind::Transmitter))
        .transpose()?;

    let devices = DeviceBrowser::new(options.interface, options.discovery)?.browse()?;
    let device = find_device(&devices, &device_name)?;
    let mut client = ArcClient::connect(device.arc_address(), options.timeout)?;
    let receivers = client.receiver_subscriptions()?;
    let entry = receivers
        .iter()
        .find(|entry| entry.name() == Some(receiver_name.as_str()))
        .or_else(|| {
            let number: u16 = receiver_name.parse().ok()?;
            receivers
                .iter()
                .find(|entry| entry.receiver_channel().value() == number)
        })
        .ok_or_else(|| {
            format!(
                "{} has no receiver named or numbered `{receiver_name}`",
                device.name()
            )
        })?;
    let channel = entry.receiver_channel();

    let requested = match requested_source {
        None => None,
        Some((tx_device, tx_channel)) => {
            Some(resolve_source(&devices, &tx_channel, &tx_device, options)?)
        }
    };
    let before = dante::source(entry.state(), device.name());
    let mut outcome = Outcome {
        device: device.name().to_owned(),
        receiver: ReceiverRef {
            channel: channel.value(),
            name: entry.name().map(str::to_owned),
        },
        before: before.clone(),
        requested: requested.clone(),
        after: before.clone(),
        wrote: false,
        dry_run,
        write_error: None,
        ok: true,
    };
    if before == requested {
        name_sources(&mut outcome, config);
        return Ok(outcome);
    }
    let protocol = device.arc_protocol();
    if !matches!(protocol, Some(VIA_PROTOCOL | PAGED_PROTOCOL)) {
        return Err(format!(
            "{} speaks ARC protocol {}, which xlr cannot write to yet; please open an issue at \
             https://github.com/saavylab/xlr/issues with `xlr status --json` output",
            device.name(),
            device.txt_value("arcp_vers").unwrap_or("(unknown)"),
        )
        .into());
    }
    if dry_run {
        name_sources(&mut outcome, config);
        return Ok(outcome);
    }

    let selector = requested
        .as_ref()
        .map(|source| TransmitterSelector::new(source.device.clone(), source.channel.clone()))
        .transpose()?;
    let written = match protocol {
        Some(VIA_PROTOCOL) => client.apply_subscription(channel, selector.as_ref()),
        _ => {
            let capacity = u8::try_from(receivers.len().clamp(1, 32)).expect("clamped");
            client.apply_paged_subscription(PAGED_PROTOCOL, capacity, channel, selector.as_ref())
        }
    };
    outcome.wrote = true;
    outcome.write_error = written.err().map(|error| error.to_string());

    outcome.after = read_back(&mut client, channel, device.name(), &requested)?;
    outcome.ok = outcome.after == requested;
    name_sources(&mut outcome, config);
    Ok(outcome)
}

/// Re-reads the receiver until it matches `requested` or attempts run out.
fn read_back(
    client: &mut ArcClient,
    channel: ReceiverChannel,
    own_name: &str,
    requested: &Option<Source>,
) -> Result<Option<Source>, xlr_dante::ArcClientError> {
    let mut current = None;
    for attempt in 0..READ_BACK_ATTEMPTS {
        if attempt > 0 {
            thread::sleep(READ_BACK_INTERVAL);
        }
        current = dante::source(client.query_subscription(channel)?.state(), own_name);
        if &current == requested {
            break;
        }
    }
    Ok(current)
}

fn resolve_source(
    devices: &[DiscoveredDevice],
    tx_channel: &str,
    tx_device: &str,
    options: &Options,
) -> Result<Source, Box<dyn std::error::Error>> {
    let device = find_device(devices, tx_device)?;
    let mut client = ArcClient::connect(device.arc_address(), options.timeout)?;
    let channels = client.transmitter_channels()?;
    if !channels.iter().any(|channel| channel.name() == tx_channel) {
        return Err(format!(
            "{} has no transmitter channel `{tx_channel}`; see `xlr status`",
            device.name()
        )
        .into());
    }
    Ok(Source {
        device: device.name().to_owned(),
        channel: tx_channel.to_owned(),
        names: Vec::new(),
    })
}

fn find_device<'a>(
    devices: &'a [DiscoveredDevice],
    name: &str,
) -> Result<&'a DiscoveredDevice, String> {
    devices
        .iter()
        .find(|device| device.name() == name)
        .or_else(|| {
            devices
                .iter()
                .find(|device| device.name().eq_ignore_ascii_case(name))
        })
        .ok_or_else(|| {
            let known: Vec<&str> = devices.iter().map(DiscoveredDevice::name).collect();
            format!(
                "no Dante device named `{name}` found (found: {})",
                if known.is_empty() {
                    "none".to_owned()
                } else {
                    known.join(", ")
                }
            )
        })
}

/// Splits `channel@device` at its last `@`.
fn split_address(address: &str) -> Result<(&str, &str), String> {
    match address.rsplit_once('@') {
        Some((channel, device)) if !channel.is_empty() && !device.is_empty() => {
            Ok((channel, device))
        }
        _ => Err(format!("`{address}` is not of the form channel@device")),
    }
}

impl Outcome {
    pub fn render(&self) -> String {
        let show = |source: &Option<Source>| match source {
            Some(source) => format!(
                "{}@{}{}",
                source.channel,
                source.device,
                dante::tags(&source.names)
            ),
            None => "(nothing)".to_owned(),
        };
        let receiver = match &self.receiver.name {
            Some(name) => format!("{name}@{} (rx {})", self.device, self.receiver.channel),
            None => format!("rx {}@{}", self.receiver.channel, self.device),
        };
        if self.before == self.requested {
            return format!("{receiver} already <- {}", show(&self.requested));
        }
        if self.dry_run {
            return format!(
                "would route {receiver}: {} -> {}",
                show(&self.before),
                show(&self.requested)
            );
        }
        let mut line = if self.ok {
            format!("routed {receiver} <- {}", show(&self.after))
        } else {
            format!(
                "route NOT confirmed for {receiver}: requested {}, device reports {}",
                show(&self.requested),
                show(&self.after)
            )
        };
        if let Some(error) = &self.write_error {
            line.push_str(&format!("\n  write reported: {error}"));
        }
        line
    }
}

#[cfg(test)]
mod tests {
    use super::split_address;

    #[test]
    fn addresses_split_at_the_last_at_sign() {
        assert_eq!(
            split_address("Scarlett 18i20:Channel 9@mac-mini"),
            Ok(("Scarlett 18i20:Channel 9", "mac-mini"))
        );
        assert_eq!(split_address("a@b@c"), Ok(("a@b", "c")));
        for invalid in ["Left", "@dev", "Left@", ""] {
            assert!(split_address(invalid).is_err(), "{invalid}");
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Receiver,
    Transmitter,
}

/// Resolves a name, a `dante/…` address, or `channel@device` to
/// `(device, channel)`.
fn resolve(config: &Config, text: &str, kind: Kind) -> Result<(String, String), String> {
    let address = match config.names.get(text) {
        Some(address) => Some(address.clone()),
        None if text.starts_with("dante/") => Some(config.parse_address(text)?),
        None => None,
    };
    match (address, kind) {
        (Some(Address::DanteRx { device, channel }), Kind::Receiver)
        | (Some(Address::DanteTx { device, channel }), Kind::Transmitter) => Ok((device, channel)),
        (Some(address), _) => Err(format!(
            "`{text}` is {address}, which is not a Dante {}",
            if kind == Kind::Receiver {
                "receiver"
            } else {
                "transmitter channel"
            }
        )),
        (None, _) => {
            let (channel, device) = split_address(text)?;
            Ok((device.to_owned(), channel.to_owned()))
        }
    }
}

/// Attaches your names to the sources in an outcome.
fn name_sources(outcome: &mut Outcome, config: &Config) {
    for source in [
        &mut outcome.before,
        &mut outcome.requested,
        &mut outcome.after,
    ]
    .into_iter()
    .flatten()
    {
        source.names = config.names_for(&Address::DanteTx {
            device: source.device.clone(),
            channel: source.channel.clone(),
        });
    }
}
