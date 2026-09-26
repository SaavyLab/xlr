//! `xlr route`: change one receiver's subscription, safely.

use crate::dante::{self, Options, Source};
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
    receiver: &str,
    source: Option<&str>,
    dry_run: bool,
) -> Result<Outcome, Box<dyn std::error::Error>> {
    let (receiver_name, device_name) = split_address(receiver)?;
    let requested_source = source.map(split_address).transpose()?;

    let devices = DeviceBrowser::new(options.interface, options.discovery)?.browse()?;
    let device = find_device(&devices, device_name)?;
    let mut client = ArcClient::connect(device.arc_address(), options.timeout)?;
    let receivers = client.receiver_subscriptions()?;
    let entry = receivers
        .iter()
        .find(|entry| entry.name() == Some(receiver_name))
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
        Some((tx_channel, tx_device)) => {
            Some(resolve_source(&devices, tx_channel, tx_device, options)?)
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
            Some(source) => format!("{}@{}", source.channel, source.device),
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
