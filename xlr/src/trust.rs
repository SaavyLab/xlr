//! Who this machine trusts, in two small files under the xlr home:
//!
//! - `hosts.toml` (client side): hosts this machine talks to, with the
//!   fingerprint pinned when they were added;
//! - `peers.toml` (host side): peers approved to talk to this machine and
//!   their roles, plus pairing requests awaiting approval.
//!
//! The files are the only shared state between `xlr serve` and the
//! `xlr peers` commands, so approving a peer needs no running daemon.

use crate::identity::{Fingerprint, write_private};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// At most this many pairing requests are kept; older ones are dropped.
const MAX_PENDING: usize = 16;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Status, names, and other reads.
    Read,
    /// Reads plus changes (routes, switches, scenes).
    Control,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Hosts {
    #[serde(default)]
    pub hosts: BTreeMap<String, Host>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Host {
    /// `address:port` of its `xlr serve`.
    pub address: String,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Peers {
    /// Approved peers, by fingerprint.
    #[serde(default)]
    pub approved: BTreeMap<String, Peer>,
    /// Pairing requests awaiting approval, by fingerprint.
    #[serde(default)]
    pub pending: BTreeMap<String, Pending>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Peer {
    pub name: String,
    pub role: Role,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Pending {
    pub name: String,
    pub code: String,
    /// Where the requester serves, for `xlr pair` requests: approving one
    /// also adds the requester as a host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// Seconds since the Unix epoch.
    pub requested_at: u64,
}

impl Hosts {
    pub fn load(home: &Path) -> Result<Self, String> {
        load(&home.join("hosts.toml"))
    }

    pub fn save(&self, home: &Path) -> Result<(), String> {
        save(&home.join("hosts.toml"), self)
    }
}

impl Peers {
    pub fn load(home: &Path) -> Result<Self, String> {
        load(&home.join("peers.toml"))
    }

    pub fn save(&self, home: &Path) -> Result<(), String> {
        save(&home.join("peers.toml"), self)
    }

    pub fn role(&self, fingerprint: &Fingerprint) -> Option<Role> {
        self.approved
            .get(fingerprint.as_str())
            .map(|peer| peer.role)
    }

    /// Records (or refreshes) a pairing request.
    pub fn request(
        &mut self,
        fingerprint: &Fingerprint,
        name: &str,
        code: String,
        address: Option<String>,
    ) {
        self.pending.insert(
            fingerprint.to_string(),
            Pending {
                name: name.to_owned(),
                code,
                address,
                requested_at: now(),
            },
        );
        while self.pending.len() > MAX_PENDING {
            let oldest = self
                .pending
                .iter()
                .min_by_key(|(_, pending)| pending.requested_at)
                .map(|(key, _)| key.clone())
                .expect("nonempty");
            self.pending.remove(&oldest);
        }
    }

    /// Approves the pending request whose code or fingerprint prefix
    /// matches `selector`, returning its fingerprint and the request.
    pub fn approve(&mut self, selector: &str, role: Role) -> Result<(String, Pending), String> {
        let wanted = selector.replace([' ', '-'], "").to_ascii_lowercase();
        let matches: Vec<String> = self
            .pending
            .iter()
            .filter(|(fingerprint, pending)| {
                pending.code.replace(' ', "") == wanted
                    || (wanted.len() >= 8 && fingerprint.starts_with(&wanted))
            })
            .map(|(fingerprint, _)| fingerprint.clone())
            .collect();
        let fingerprint = match matches.as_slice() {
            [one] => one.clone(),
            [] => {
                return Err(format!(
                    "no pending request matches `{selector}`; see `xlr peers`"
                ));
            }
            _ => {
                return Err(format!(
                    "`{selector}` matches several requests; use a fingerprint"
                ));
            }
        };
        let pending = self.pending.remove(&fingerprint).expect("matched");
        self.approved.insert(
            fingerprint.clone(),
            Peer {
                name: pending.name.clone(),
                role,
            },
        );
        Ok((fingerprint, pending))
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn load<T: Default + for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|error| format!("{}: {error}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// Writes atomically: a temporary file renamed over the target.
fn save<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let text = toml::to_string(value).map_err(|error| error.to_string())?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    }
    let temporary = PathBuf::from(format!("{}.tmp", path.display()));
    write_private(&temporary, text.as_bytes())?;
    fs::rename(&temporary, path).map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_approved_by_code_or_fingerprint_prefix() {
        let mut peers = Peers::default();
        let a = Fingerprint::of(b"a");
        let b = Fingerprint::of(b"b");
        peers.request(&a, "laptop", "123 456".to_owned(), None);
        peers.request(
            &b,
            "desktop",
            "654 321".to_owned(),
            Some("10.0.0.9:7373".to_owned()),
        );
        assert_eq!(peers.role(&a), None);
        let (approved, pending) = peers.approve("123456", Role::Read).unwrap();
        assert_eq!(
            (approved.as_str(), pending.name.as_str()),
            (a.as_str(), "laptop")
        );
        assert_eq!(pending.address, None);
        assert_eq!(peers.role(&a), Some(Role::Read));
        let (_, mutual) = peers.approve(&b.as_str()[..8], Role::Control).unwrap();
        assert_eq!(mutual.address.as_deref(), Some("10.0.0.9:7373"));
        assert_eq!(peers.role(&b), Some(Role::Control));
        assert!(peers.pending.is_empty());
        assert!(peers.approve("000000", Role::Read).is_err());
    }

    #[test]
    fn pending_requests_are_bounded() {
        let mut peers = Peers::default();
        for index in 0..(MAX_PENDING + 5) {
            peers.request(
                &Fingerprint::of(&index.to_be_bytes()),
                "x",
                "000 000".to_owned(),
                None,
            );
        }
        assert_eq!(peers.pending.len(), MAX_PENDING);
    }

    #[test]
    fn files_round_trip() {
        let home = std::env::temp_dir().join(format!("xlr-trust-test-{}", std::process::id()));
        let mut peers = Peers::default();
        peers.request(&Fingerprint::of(b"a"), "laptop", "123 456".to_owned(), None);
        peers.approve("123456", Role::Control).unwrap();
        peers.save(&home).unwrap();
        assert_eq!(Peers::load(&home).unwrap().approved.len(), 1);
        let mut hosts = Hosts::default();
        hosts.hosts.insert(
            "mac".to_owned(),
            Host {
                address: "10.0.0.2:7373".to_owned(),
                fingerprint: Fingerprint::of(b"m").to_string(),
            },
        );
        hosts.save(&home).unwrap();
        assert_eq!(
            Hosts::load(&home).unwrap().hosts["mac"].address,
            "10.0.0.2:7373"
        );
        fs::remove_dir_all(home).unwrap();
    }
}
