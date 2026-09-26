//! The whole studio: this host plus every known remote host, merged.
//!
//! Dante is one network, so its devices appear once (names from every host
//! are combined). Focusrite and PipeWire are local to each host and appear
//! under the host that reported them.

use crate::{
    dante::{DanteStatus, Source},
    focusrite::FocusriteStatus,
    identity::{Fingerprint, Identity},
    pipewire::PipewireStatus,
    remote::{self, Request},
    setup::Setup,
    trust::Hosts,
};
use serde::Serialize;
use std::{fmt::Write as _, path::Path};

#[derive(Serialize)]
pub struct Studio {
    pub dante: DanteStatus,
    pub hosts: Vec<HostStatus>,
}

#[derive(Serialize)]
pub struct HostStatus {
    pub name: String,
    /// Whether this is the machine running the command.
    pub local: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focusrite: Option<FocusriteStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipewire: Option<PipewireStatus>,
    /// Why the host was not read, when that is expected (awaiting pairing).
    pub unavailable: Option<String>,
    pub error: Option<String>,
}

/// Reads this host and every known host, in parallel.
pub fn read(local: impl FnOnce() -> Setup + Send, home: Option<&Path>) -> Studio {
    let hosts = home.map(Hosts::load).transpose();
    let identity = home.map(Identity::load_or_create).transpose();
    let (local_setup, remote) = std::thread::scope(|scope| {
        let remote_reads: Vec<_> = match (&hosts, &identity) {
            (Ok(Some(hosts)), Ok(Some(identity))) => hosts
                .hosts
                .iter()
                .map(|(name, host)| {
                    let (name, host) = (name.clone(), host.clone());
                    scope.spawn(move || {
                        (name, host.fingerprint.clone(), read_remote(identity, &host))
                    })
                })
                .collect(),
            _ => Vec::new(),
        };
        let local_setup = local();
        let remote: Vec<_> = remote_reads
            .into_iter()
            .map(|handle| handle.join().expect("remote read does not panic"))
            .collect();
        (local_setup, remote)
    });

    let local_name = remote::hostname();
    let mut owners: Vec<(String, String)> = local_setup
        .owned_dante
        .iter()
        .map(|device| (device.clone(), local_name.clone()))
        .collect();
    let mut studio = Studio {
        dante: local_setup.dante,
        hosts: vec![HostStatus {
            name: local_name,
            local: true,
            fingerprint: identity
                .as_ref()
                .ok()
                .and_then(Option::as_ref)
                .map(|identity| identity.fingerprint.to_string()),
            focusrite: Some(local_setup.focusrite),
            pipewire: Some(local_setup.pipewire),
            unavailable: None,
            error: None,
        }],
    };
    if let Err(error) = hosts.and(identity) {
        studio.hosts[0].error = Some(error);
    }
    for (name, fingerprint, result) in remote {
        let mut host = HostStatus {
            name,
            local: false,
            fingerprint: Some(fingerprint),
            focusrite: None,
            pipewire: None,
            unavailable: None,
            error: None,
        };
        match result {
            Ok(setup) => {
                owners.extend(
                    setup
                        .owned_dante
                        .iter()
                        .map(|device| (device.clone(), host.name.clone())),
                );
                merge_dante(&mut studio.dante, setup.dante);
                host.focusrite = Some(setup.focusrite);
                host.pipewire = Some(setup.pipewire);
            }
            Err(Unread::Pending(message)) => host.unavailable = Some(message),
            Err(Unread::Failed(message)) => host.error = Some(message),
        }
        studio.hosts.push(host);
    }
    for device in &mut studio.dante.devices {
        device.host = owners
            .iter()
            .find(|(name, _)| *name == device.name)
            .map(|(_, host)| host.clone());
    }
    studio
}

enum Unread {
    Pending(String),
    Failed(String),
}

fn read_remote(identity: &Identity, host: &crate::trust::Host) -> Result<Setup, Unread> {
    let pinned = Fingerprint::parse(&host.fingerprint).map_err(Unread::Failed)?;
    let (response, _) = remote::call(identity, &host.address, Some(pinned), &Request::Status)
        .map_err(Unread::Failed)?;
    if let Some(error) = response.error {
        return Err(if error.kind == "pairing-required" {
            Unread::Pending(format!(
                "awaiting approval on that host (code {})",
                error.code.unwrap_or_default()
            ))
        } else {
            Unread::Failed(error.message)
        });
    }
    serde_json::from_value(response.result.unwrap_or_default())
        .map_err(|error| Unread::Failed(format!("unexpected status shape: {error}")))
}

/// Adds another host's view of the Dante network to `into`.
fn merge_dante(into: &mut DanteStatus, other: DanteStatus) {
    if into.error.is_some() && other.error.is_none() {
        let interface = into.interface;
        *into = other;
        into.interface = interface;
        return;
    }
    for device in other.devices {
        let Some(existing) = into
            .devices
            .iter_mut()
            .find(|known| known.name == device.name)
        else {
            into.devices.push(device);
            continue;
        };
        if existing.error.is_some() && device.error.is_none() {
            *existing = device;
            continue;
        }
        for tx in device.transmitters {
            if let Some(known) = existing
                .transmitters
                .iter_mut()
                .find(|known| known.channel == tx.channel)
            {
                union(&mut known.names, tx.names);
            }
        }
        for rx in device.receivers {
            if let Some(known) = existing
                .receivers
                .iter_mut()
                .find(|known| known.channel == rx.channel)
            {
                union(&mut known.names, rx.names);
                if let (Some(known), Some(source)) = (&mut known.source, rx.source) {
                    merge_source(known, source);
                }
            }
        }
    }
    into.devices.sort_by(|a, b| a.name.cmp(&b.name));
}

fn merge_source(into: &mut Source, other: Source) {
    if into.device == other.device && into.channel == other.channel {
        union(&mut into.names, other.names);
    }
}

fn union(into: &mut Vec<String>, other: Vec<String>) {
    for name in other {
        if !into.contains(&name) {
            into.push(name);
        }
    }
    into.sort();
}

impl Studio {
    pub fn has_errors(&self) -> bool {
        self.dante.has_errors()
            || self.hosts.iter().any(|host| {
                host.error.is_some()
                    || host
                        .focusrite
                        .as_ref()
                        .is_some_and(FocusriteStatus::has_errors)
                    || host
                        .pipewire
                        .as_ref()
                        .is_some_and(PipewireStatus::has_errors)
            })
    }

    pub fn render(&self) -> String {
        let mut out = format!("══ Dante network ══\n{}", self.dante.render());
        for host in &self.hosts {
            let _ = writeln!(
                out,
                "══ {}{} ══",
                host.name,
                if host.local { " (this host)" } else { "" }
            );
            if let Some(reason) = &host.unavailable {
                let _ = writeln!(out, "unavailable: {reason}\n");
                continue;
            }
            if let Some(error) = &host.error {
                let _ = writeln!(out, "error: {error}\n");
                continue;
            }
            if let Some(focusrite) = host
                .focusrite
                .as_ref()
                .filter(|status| status.error.is_some() || !status.devices.is_empty())
            {
                let _ = write!(out, "── Focusrite (USB) ──\n{}", focusrite.render());
            }
            if let Some(pipewire) = host
                .pipewire
                .as_ref()
                .filter(|status| status.unavailable.is_none())
            {
                let _ = write!(out, "── PipeWire ──\n{}", pipewire.render());
            }
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dante::{Device, Receiver, Transmitter};

    fn device(names: &[&str], error: Option<&str>) -> Device {
        Device {
            name: "avio".to_owned(),
            host: None,
            product: None,
            model: None,
            manufacturer: None,
            address: "10.0.0.5:4440".to_owned(),
            transmitters: vec![Transmitter {
                channel: 1,
                name: "Left".to_owned(),
                names: names.iter().map(|name| (*name).to_owned()).collect(),
            }],
            receivers: vec![Receiver {
                channel: 1,
                name: Some("Left".to_owned()),
                source: None,
                names: Vec::new(),
            }],
            error: error.map(str::to_owned),
        }
    }

    fn status(devices: Vec<Device>) -> DanteStatus {
        DanteStatus {
            interface: "10.0.0.1".parse().unwrap(),
            devices,
            error: None,
        }
    }

    #[test]
    fn devices_appear_once_with_names_from_every_host() {
        let mut merged = status(vec![device(&["a"], None)]);
        merge_dante(&mut merged, status(vec![device(&["b", "a"], None)]));
        assert_eq!(merged.devices.len(), 1);
        assert_eq!(merged.devices[0].transmitters[0].names, ["a", "b"]);
    }

    #[test]
    fn status_round_trips_through_json_without_names() {
        let original = status(vec![device(&[], None)]);
        let json = serde_json::to_value(&original).unwrap();
        assert!(json["devices"][0]["transmitters"][0].get("names").is_none());
        let back: DanteStatus = serde_json::from_value(json).unwrap();
        assert!(back.devices[0].transmitters[0].names.is_empty());
    }

    #[test]
    fn a_readable_copy_replaces_a_failed_one() {
        let mut merged = status(vec![device(&[], Some("timeout"))]);
        merge_dante(&mut merged, status(vec![device(&["x"], None)]));
        assert!(merged.devices[0].error.is_none());
    }
}
