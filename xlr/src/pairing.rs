//! `xlr id`, `xlr hosts`, and `xlr peers`: identity and pairing.

use crate::{
    identity::{Fingerprint, Identity, pairing_code},
    remote::{self, Request},
    trust::{Host, Hosts, Peers, Role},
};
use std::path::Path;

pub fn id(home: &Path) -> Result<String, String> {
    let identity = Identity::load_or_create(home)?;
    Ok(format!(
        "{}  ({})\nfingerprint {}",
        remote::hostname(),
        identity.fingerprint.short(),
        identity.fingerprint
    ))
}

pub fn list_hosts(home: &Path) -> Result<String, String> {
    let hosts = Hosts::load(home)?;
    if hosts.hosts.is_empty() {
        return Ok("No hosts. Add one with `xlr hosts add <name> <address>`.".to_owned());
    }
    let identity = Identity::load_or_create(home)?;
    let mut lines = Vec::new();
    for (name, host) in &hosts.hosts {
        let pinned = Fingerprint::parse(&host.fingerprint)?;
        let state = match remote::call(
            &identity,
            &host.address,
            Some(pinned.clone()),
            &Request::Hello {
                name: remote::hostname(),
            },
        ) {
            Ok((response, _)) => match (response.result, response.error) {
                (Some(result), _) => format!("paired ({})", result["role"].as_str().unwrap_or("?")),
                (_, Some(error)) if error.kind == "pairing-required" => format!(
                    "awaiting approval: on {name}, run `xlr peers approve {}`",
                    error.code.unwrap_or_default().replace(' ', "")
                ),
                (_, Some(error)) => format!("error: {}", error.message),
                _ => "error: empty response".to_owned(),
            },
            Err(error) => format!("unreachable: {error}"),
        };
        lines.push(format!(
            "{name}  {}  {}  {state}",
            host.address,
            pinned.short()
        ));
    }
    Ok(lines.join("\n"))
}

pub fn add_host(
    home: &Path,
    name: &str,
    address: &str,
    fingerprint: Option<&str>,
) -> Result<String, String> {
    let mut hosts = Hosts::load(home)?;
    if hosts.hosts.contains_key(name) {
        return Err(format!(
            "host `{name}` already exists; remove it first with `xlr hosts remove {name}`"
        ));
    }
    let identity = Identity::load_or_create(home)?;
    let pinned = fingerprint.map(Fingerprint::parse).transpose()?;
    let address = remote::with_default_port(address);
    let (response, seen) = remote::call(
        &identity,
        &address,
        pinned,
        &Request::Hello {
            name: remote::hostname(),
        },
    )?;
    hosts.hosts.insert(
        name.to_owned(),
        Host {
            address: address.clone(),
            fingerprint: seen.to_string(),
        },
    );
    hosts.save(home)?;

    let mut message = format!("Pinned {name} at {address} as {}.", seen.short());
    match (response.result, response.error) {
        (Some(result), _) => {
            message.push_str(&format!(
                " Already paired ({}).",
                result["role"].as_str().unwrap_or("?")
            ));
        }
        (_, Some(error)) if error.kind == "pairing-required" => {
            let expected = pairing_code(&seen, &identity.fingerprint);
            let code = error.code.unwrap_or_default();
            if code != expected {
                return Err(format!(
                    "{name} reported pairing code {code} but this machine computed {expected}; \
                     something may be intercepting the connection. Not trusting it."
                ));
            }
            message.push_str(&format!(
                "\nTo finish pairing, on {name} run:\n\n  xlr peers approve {}\n\n\
                 and check that it shows code {code} for {} ({}).",
                code.replace(' ', ""),
                remote::hostname(),
                identity.fingerprint.short()
            ));
        }
        (_, Some(error)) => return Err(error.message),
        _ => return Err("empty response".to_owned()),
    }
    Ok(message)
}

pub fn remove_host(home: &Path, name: &str) -> Result<String, String> {
    let mut hosts = Hosts::load(home)?;
    hosts
        .hosts
        .remove(name)
        .ok_or_else(|| format!("no host named `{name}`"))?;
    hosts.save(home)?;
    Ok(format!("Removed {name}."))
}

pub fn list_peers(home: &Path) -> Result<String, String> {
    let peers = Peers::load(home)?;
    let mut lines = Vec::new();
    if peers.approved.is_empty() && peers.pending.is_empty() {
        return Ok("No peers. Another machine can request access with `xlr hosts add`.".to_owned());
    }
    for (fingerprint, peer) in &peers.approved {
        let short =
            Fingerprint::parse(fingerprint).map_or_else(|_| fingerprint.clone(), |f| f.short());
        lines.push(format!("{}  {short}  {:?}", peer.name, peer.role).to_lowercase());
    }
    for (fingerprint, pending) in &peers.pending {
        let short =
            Fingerprint::parse(fingerprint).map_or_else(|_| fingerprint.clone(), |f| f.short());
        lines.push(format!(
            "{}  {short}  pending, code {}: approve with `xlr peers approve {}`",
            pending.name,
            pending.code,
            pending.code.replace(' ', "")
        ));
    }
    Ok(lines.join("\n"))
}

pub fn approve(home: &Path, selector: &str, role: Role) -> Result<String, String> {
    let mut peers = Peers::load(home)?;
    let (fingerprint, name) = peers.approve(selector, role)?;
    peers.save(home)?;
    let short = Fingerprint::parse(&fingerprint).map_or(fingerprint, |f| f.short());
    Ok(format!(
        "Approved {name} ({short}) as {}.",
        format!("{role:?}").to_lowercase()
    ))
}

pub fn remove_peer(home: &Path, selector: &str) -> Result<String, String> {
    let mut peers = Peers::load(home)?;
    let wanted = selector.replace('-', "").to_ascii_lowercase();
    let matches: Vec<String> = peers
        .approved
        .iter()
        .filter(|(fingerprint, peer)| {
            peer.name == selector || (wanted.len() >= 8 && fingerprint.starts_with(&wanted))
        })
        .map(|(fingerprint, _)| fingerprint.clone())
        .collect();
    match matches.as_slice() {
        [one] => {
            let peer = peers.approved.remove(one).expect("matched");
            peers.save(home)?;
            Ok(format!("Removed {}.", peer.name))
        }
        [] => Err(format!("no approved peer matches `{selector}`")),
        _ => Err(format!(
            "`{selector}` matches several peers; use a fingerprint prefix"
        )),
    }
}
