//! The whole setup: every backend's view in one report.

use crate::{
    address::Address,
    config::Config,
    dante::{self, DanteStatus},
    focusrite::{self, FocusriteStatus},
    pipewire::{self, PipewireStatus},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Deserialize, Serialize)]
pub struct Setup {
    /// Dante devices attached to this host: those declared in `[host] owns`,
    /// plus any (such as Dante Via) at this host's own address.
    #[serde(default)]
    pub owned_dante: Vec<String>,
    pub dante: DanteStatus,
    pub focusrite: FocusriteStatus,
    pub pipewire: PipewireStatus,
}

pub fn read(options: &dante::Options, config: &Config) -> Setup {
    let dante = dante::status(options);
    let mut owned_dante = config.owned_dante.clone();
    for device in &dante.devices {
        let at_this_host = device
            .address
            .parse::<std::net::SocketAddr>()
            .is_ok_and(|address| address.ip() == std::net::IpAddr::V4(options.interface));
        if at_this_host && !owned_dante.contains(&device.name) {
            owned_dante.push(device.name.clone());
        }
    }
    let mut setup = Setup {
        owned_dante,
        dante,
        focusrite: focusrite::status(options.timeout),
        pipewire: pipewire::status(),
    };
    setup.annotate(config);
    setup
}

impl Setup {
    /// Attaches your names to everything they refer to.
    fn annotate(&mut self, config: &Config) {
        for output in &mut self.pipewire.outputs {
            output.names = config.names_for(&Address::PipewireSink {
                node: output.node.clone(),
            });
        }
        for input in &mut self.pipewire.inputs {
            input.names = config.names_for(&Address::PipewireSource {
                node: input.node.clone(),
            });
        }
        for device in &mut self.dante.devices {
            for tx in &mut device.transmitters {
                tx.names = config.names_for(&Address::DanteTx {
                    device: device.name.clone(),
                    channel: tx.name.clone(),
                });
            }
            for rx in &mut device.receivers {
                rx.names = receiver_addresses(&device.name, rx.channel, rx.name.as_deref())
                    .iter()
                    .flat_map(|address| config.names_for(address))
                    .collect();
                if let Some(source) = &mut rx.source {
                    source.names = config.names_for(&Address::DanteTx {
                        device: source.device.clone(),
                        channel: source.channel.clone(),
                    });
                }
            }
        }
        for device in &mut self.focusrite.devices {
            let id = device.identity.id.clone();
            if let Some(monitor) = &mut device.monitor {
                monitor.names = config.names_for(&Address::FocusriteMonitor { device: id.clone() });
            }
            for input in &mut device.inputs {
                input.names = config.names_for(&Address::FocusriteInput {
                    device: id.clone(),
                    input: input.input,
                });
            }
        }
    }

    /// Every address that currently exists, and every device segment whose
    /// backend could not be read (so its addresses cannot be checked).
    pub fn addresses(&self) -> (BTreeSet<Address>, BTreeSet<String>) {
        let mut live = BTreeSet::new();
        let mut unverified = BTreeSet::new();
        for device in &self.dante.devices {
            if device.error.is_some() {
                unverified.insert(format!("dante/{}", device.name));
            }
            for tx in &device.transmitters {
                live.insert(Address::DanteTx {
                    device: device.name.clone(),
                    channel: tx.name.clone(),
                });
            }
            for rx in &device.receivers {
                live.extend(receiver_addresses(
                    &device.name,
                    rx.channel,
                    rx.name.as_deref(),
                ));
            }
        }
        for device in &self.focusrite.devices {
            let id = device.identity.id.clone();
            if device.error.is_some() || device.unavailable.is_some() {
                unverified.insert(format!("focusrite/{id}"));
            }
            if device.monitor.is_some() {
                live.insert(Address::FocusriteMonitor { device: id.clone() });
            }
            for input in &device.inputs {
                live.insert(Address::FocusriteInput {
                    device: id.clone(),
                    input: input.input,
                });
            }
        }
        if self.pipewire.unavailable.is_some() || self.pipewire.error.is_some() {
            unverified.insert("pipewire/sink".to_owned());
            unverified.insert("pipewire/source".to_owned());
        }
        live.extend(
            self.pipewire
                .outputs
                .iter()
                .map(|output| Address::PipewireSink {
                    node: output.node.clone(),
                }),
        );
        live.extend(
            self.pipewire
                .inputs
                .iter()
                .map(|input| Address::PipewireSource {
                    node: input.node.clone(),
                }),
        );
        (live, unverified)
    }
}

/// A receiver is addressable by its name and by its number.
fn receiver_addresses(device: &str, channel: u16, name: Option<&str>) -> Vec<Address> {
    let mut addresses = vec![Address::DanteRx {
        device: device.to_owned(),
        channel: channel.to_string(),
    }];
    if let Some(name) = name {
        addresses.push(Address::DanteRx {
            device: device.to_owned(),
            channel: name.to_owned(),
        });
    }
    addresses
}

/// One configured name and whether it matches live hardware.
#[derive(Serialize)]
pub struct NameCheck {
    pub name: String,
    pub address: String,
    /// `found`, `missing`, or `unverified` (its device could not be read).
    pub state: &'static str,
}

pub fn check_names(config: &Config, setup: &Setup) -> Vec<NameCheck> {
    let (live, unverified) = setup.addresses();
    config
        .names
        .iter()
        .map(|(name, address)| {
            let text = address.to_string();
            let device_prefix: String = text.splitn(3, '/').take(2).collect::<Vec<_>>().join("/");
            let state = if live.contains(address) {
                "found"
            } else if unverified.contains(&device_prefix) {
                "unverified"
            } else {
                "missing"
            };
            NameCheck {
                name: name.clone(),
                address: text,
                state,
            }
        })
        .collect()
}
