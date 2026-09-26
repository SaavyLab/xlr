//! `xlr id`, `xlr hosts`, and `xlr peers`: identity and pairing.

use crate::{
    identity::{Fingerprint, Identity, pairing_code},
    remote::{self, Request},
    trust::{Host, Hosts, Peer, Peers, Role},
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
                serve_port: None,
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

/// One-way: this machine reads the host.
pub fn add_host(
    home: &Path,
    name: &str,
    address: &str,
    fingerprint: Option<&str>,
) -> Result<String, String> {
    connect(home, name, address, fingerprint, None)
}

/// Both ways: this machine reads the host, and the host (once it approves)
/// reads this machine, with the host granted `grant` here.
pub fn pair(
    home: &Path,
    name: &str,
    address: &str,
    fingerprint: Option<&str>,
    port: u16,
    grant: Role,
) -> Result<String, String> {
    connect(home, name, address, fingerprint, Some((port, grant)))
}

fn connect(
    home: &Path,
    name: &str,
    address: &str,
    fingerprint: Option<&str>,
    mutual: Option<(u16, Role)>,
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
            serve_port: mutual.map(|(port, _)| port),
        },
    )?;

    // Verify before trusting anything: nothing is saved until the host's
    // pairing code matches the one computed here.
    let next_step = match (response.result, response.error) {
        (Some(result), _) => {
            format!(
                " Already paired ({}).",
                result["role"].as_str().unwrap_or("?")
            )
        }
        (_, Some(error)) if error.kind == "pairing-required" => {
            let expected = pairing_code(&seen, &identity.fingerprint);
            let code = error.code.unwrap_or_default();
            if code != expected {
                return Err(format!(
                    "{name} reported pairing code {code} but this machine computed {expected}; \
                     something may be intercepting the connection. Nothing was saved."
                ));
            }
            format!(
                "\nTo finish pairing, on {name} run:\n\n  xlr peers approve {}\n\n\
                 and check that it shows code {code} for {} ({}).",
                code.replace(' ', ""),
                remote::hostname(),
                identity.fingerprint.short()
            )
        }
        (_, Some(error)) => return Err(error.message),
        _ => return Err("empty response".to_owned()),
    };

    hosts.hosts.insert(
        name.to_owned(),
        Host {
            address: address.clone(),
            fingerprint: seen.to_string(),
        },
    );
    hosts.save(home)?;
    let mut message = format!("Pinned {name} at {address} as {}.", seen.short());
    if let Some((_, grant)) = mutual {
        let mut peers = Peers::load(home)?;
        peers.approved.insert(
            seen.to_string(),
            Peer {
                name: name.to_owned(),
                role: grant,
            },
        );
        peers.pending.remove(seen.as_str());
        peers.save(home)?;
        message.push_str(&format!(
            " It may read this machine ({}).",
            format!("{grant:?}").to_lowercase()
        ));
    }
    message.push_str(&next_step);
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
    let (fingerprint, pending) = peers.approve(selector, role)?;
    peers.save(home)?;
    let short =
        Fingerprint::parse(&fingerprint).map_or_else(|_| fingerprint.clone(), |f| f.short());
    let name = &pending.name;
    let mut message = format!(
        "Approved {name} ({short}) as {}.",
        format!("{role:?}").to_lowercase()
    );
    // A mutual request (from `xlr pair`): read the requester back.
    if let Some(address) = pending.address {
        let mut hosts = Hosts::load(home)?;
        match hosts.hosts.get(name) {
            Some(host) if host.fingerprint != fingerprint => message.push_str(&format!(
                " A different host is already named {name} here, so it was not added back; \
                 remove it and run `xlr hosts add {name} {address}` to read it."
            )),
            _ => {
                hosts.hosts.insert(
                    name.clone(),
                    Host {
                        address: address.clone(),
                        fingerprint,
                    },
                );
                hosts.save(home)?;
                message.push_str(&format!(
                    " Added {name} at {address} as a host: {} ⇄ {name} paired.",
                    remote::hostname()
                ));
            }
        }
    }
    Ok(message)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, thread, time::Duration};

    #[test]
    fn one_approval_pairs_both_directions() {
        let base = std::env::temp_dir().join(format!("xlr-pair-test-{}", std::process::id()));
        let (host_home, client_home) = (base.join("host"), base.join("client"));
        let host_identity = Identity::load_or_create(&host_home).unwrap();
        let host_fingerprint = host_identity.fingerprint.clone();
        let client_fingerprint = Identity::load_or_create(&client_home).unwrap().fingerprint;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        drop(listener);
        let serve_home = host_home.clone();
        let serve_address = address.parse().unwrap();
        thread::spawn(move || {
            remote::serve(
                serve_address,
                remote::Server {
                    home: serve_home,
                    identity: host_identity,
                    status: Box::new(|| Ok(serde_json::json!({}))),
                },
            )
        });

        let message = (0..50)
            .find_map(
                |_| match pair(&client_home, "studio", &address, None, 9999, Role::Read) {
                    Err(error) if error.contains("refused") => {
                        thread::sleep(Duration::from_millis(20));
                        None
                    }
                    other => Some(other),
                },
            )
            .expect("server started")
            .unwrap();
        assert!(message.contains("xlr peers approve"), "{message}");

        // Before approval: the client already trusts the host, not vice versa.
        let client_peers = Peers::load(&client_home).unwrap();
        assert_eq!(client_peers.role(&host_fingerprint), Some(Role::Read));
        assert_eq!(
            Peers::load(&host_home).unwrap().role(&client_fingerprint),
            None
        );

        let code = pairing_code(&host_fingerprint, &client_fingerprint);
        let approved = approve(&host_home, &code, Role::Read).unwrap();
        assert!(approved.contains("paired"), "{approved}");

        assert_eq!(
            Peers::load(&host_home).unwrap().role(&client_fingerprint),
            Some(Role::Read)
        );
        let host_hosts = Hosts::load(&host_home).unwrap();
        let (_, back) = host_hosts.hosts.iter().next().expect("client added back");
        assert_eq!(back.address, "127.0.0.1:9999");
        assert_eq!(back.fingerprint, client_fingerprint.to_string());
        std::fs::remove_dir_all(base).unwrap();
    }
}
