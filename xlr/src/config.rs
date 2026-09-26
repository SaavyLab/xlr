//! This host's configuration: `$XLR_CONFIG`, or `~/.config/xlr/xlr.toml`.
//!
//! ```toml
//! # Short names for devices, used in addresses.
//! [devices]
//! scarlett = "focusrite/P9H9Q9D240E9AA"
//!
//! # Your names for the things you use.
//! [names]
//! guitar = "focusrite/scarlett/input/1"
//! desktop-left = "dante/AVIOUSBC-0563aa/rx/Left"
//! ```

use crate::address::Address;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashMap},
    env, fs, io,
    path::PathBuf,
};

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    devices: BTreeMap<String, String>,
    #[serde(default)]
    names: BTreeMap<String, String>,
}

/// A validated configuration.
#[derive(Default)]
pub struct Config {
    pub path: Option<PathBuf>,
    /// `(backend, alias)` → device identifier.
    devices: HashMap<(String, String), String>,
    /// Name → address, in name order.
    pub names: BTreeMap<String, Address>,
}

impl Config {
    /// Loads the configuration; a missing file is an empty configuration.
    pub fn load() -> Result<Self, String> {
        let Some(path) = path() else {
            return Ok(Self::default());
        };
        match fs::read_to_string(&path) {
            Ok(text) => {
                let mut config =
                    Self::parse(&text).map_err(|error| format!("{}: {error}", path.display()))?;
                config.path = Some(path);
                Ok(config)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("{}: {error}", path.display())),
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let file: File = toml::from_str(text).map_err(|error| error.to_string())?;
        let mut devices = HashMap::new();
        for (alias, target) in &file.devices {
            let (backend, device) = target
                .split_once('/')
                .filter(|(backend, device)| {
                    matches!(*backend, "dante" | "focusrite") && !device.is_empty() && !device.contains('/')
                })
                .ok_or_else(|| {
                    format!("device `{alias}` = `{target}` must be `dante/<device>` or `focusrite/<device>`")
                })?;
            devices.insert((backend.to_owned(), alias.clone()), device.to_owned());
        }
        let mut config = Self {
            path: None,
            devices,
            names: BTreeMap::new(),
        };
        for (name, text) in &file.names {
            if name.contains('/') || name.contains('@') {
                return Err(format!("name `{name}` must not contain `/` or `@`"));
            }
            let address = config
                .parse_address(text)
                .map_err(|error| format!("name `{name}`: {error}"))?;
            config.names.insert(name.clone(), address);
        }
        Ok(config)
    }

    /// Parses an address, resolving device aliases.
    pub fn parse_address(&self, text: &str) -> Result<Address, String> {
        Address::parse(text, &|backend, alias| {
            self.devices
                .get(&(backend.to_owned(), alias.to_owned()))
                .cloned()
        })
    }

    /// Every name for `address`, in name order.
    pub fn names_for(&self, address: &Address) -> Vec<String> {
        self.names
            .iter()
            .filter(|(_, target)| *target == address)
            .map(|(name, _)| name.clone())
            .collect()
    }
}

fn path() -> Option<PathBuf> {
    if let Some(path) = env::var_os("XLR_CONFIG") {
        return Some(PathBuf::from(path));
    }
    Some(home()?.join("xlr.toml"))
}

/// This machine's xlr directory: `$XLR_HOME`, or `~/.config/xlr`. It holds
/// `xlr.toml`, the identity, known hosts, and approved peers.
pub fn home() -> Option<PathBuf> {
    if let Some(home) = env::var_os("XLR_HOME") {
        return Some(PathBuf::from(home));
    }
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("xlr"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
        [devices]
        scarlett = "focusrite/P9H9"

        [names]
        guitar = "focusrite/scarlett/input/1"
        desktop-left = "dante/AVIOUSBC-0563aa/rx/Left"
        also-left = "dante/AVIOUSBC-0563aa/rx/Left"
    "#;

    #[test]
    fn names_resolve_through_device_aliases() {
        let config = Config::parse(SAMPLE).unwrap();
        assert_eq!(
            config.names["guitar"],
            Address::FocusriteInput {
                device: "P9H9".to_owned(),
                input: 1
            }
        );
        let left = config.names["desktop-left"].clone();
        assert_eq!(config.names_for(&left), ["also-left", "desktop-left"]);
    }

    #[test]
    fn invalid_configuration_is_rejected_with_context() {
        for text in [
            "[names]\nguitar = \"nowhere\"",
            "[names]\n\"a/b\" = \"focusrite/x/monitor\"",
            "[devices]\nx = \"pipewire/y\"",
            "[typo]\nx = 1",
        ] {
            assert!(Config::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn empty_configuration_is_valid() {
        assert!(Config::parse("").unwrap().names.is_empty());
    }
}
