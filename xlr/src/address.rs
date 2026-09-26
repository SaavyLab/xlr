//! Canonical addresses for everything xlr can see or change.
//!
//! ```text
//! dante/<device>/rx/<channel>      a Dante receiver, by name or number
//! dante/<device>/tx/<channel>      a Dante transmitter channel
//! focusrite/<device>/input/<n>     a Focusrite input, 1-based
//! focusrite/<device>/monitor       a Focusrite monitor section
//! pipewire/sink/<node>             a PipeWire output (sink), by node name
//! pipewire/source/<node>           a PipeWire input (source), by node name
//! ```
//!
//! The last segment of a Dante address is the rest of the string, so
//! channel names may contain `/`.

use std::fmt;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Address {
    DanteRx { device: String, channel: String },
    DanteTx { device: String, channel: String },
    FocusriteInput { device: String, input: u8 },
    FocusriteMonitor { device: String },
    PipewireSink { node: String },
    PipewireSource { node: String },
}

impl Address {
    /// Parses an address, resolving the device segment through `aliases`
    /// (a device alias maps to the backend's own device identifier).
    pub fn parse(
        text: &str,
        aliases: &dyn Fn(&str, &str) -> Option<String>,
    ) -> Result<Self, String> {
        let invalid = || format!("`{text}` is not a valid address; see `xlr names --help`");
        if let Some(rest) = text.strip_prefix("pipewire/") {
            return match rest.split_once('/') {
                Some(("sink", node)) if !node.is_empty() => Ok(Self::PipewireSink {
                    node: node.to_owned(),
                }),
                Some(("source", node)) if !node.is_empty() => Ok(Self::PipewireSource {
                    node: node.to_owned(),
                }),
                _ => Err(invalid()),
            };
        }
        let (backend, rest) = text.split_once('/').ok_or_else(invalid)?;
        let (device, rest) = rest.split_once('/').ok_or_else(invalid)?;
        if device.is_empty() {
            return Err(invalid());
        }
        let device = aliases(backend, device).unwrap_or_else(|| device.to_owned());
        match backend {
            "dante" => {
                let (kind, channel) = rest.split_once('/').ok_or_else(invalid)?;
                if channel.is_empty() {
                    return Err(invalid());
                }
                let channel = channel.to_owned();
                match kind {
                    "rx" => Ok(Self::DanteRx { device, channel }),
                    "tx" => Ok(Self::DanteTx { device, channel }),
                    _ => Err(invalid()),
                }
            }
            "focusrite" => match rest.split_once('/') {
                None if rest == "monitor" => Ok(Self::FocusriteMonitor { device }),
                Some(("input", number)) => {
                    let input = number
                        .parse()
                        .ok()
                        .filter(|&input| input > 0)
                        .ok_or_else(invalid)?;
                    Ok(Self::FocusriteInput { device, input })
                }
                _ => Err(invalid()),
            },
            _ => Err(format!(
                "`{text}`: unknown backend `{backend}` (expected dante, focusrite, or pipewire)"
            )),
        }
    }
}

impl fmt::Display for Address {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DanteRx { device, channel } => write!(formatter, "dante/{device}/rx/{channel}"),
            Self::DanteTx { device, channel } => write!(formatter, "dante/{device}/tx/{channel}"),
            Self::FocusriteInput { device, input } => {
                write!(formatter, "focusrite/{device}/input/{input}")
            }
            Self::FocusriteMonitor { device } => write!(formatter, "focusrite/{device}/monitor"),
            Self::PipewireSink { node } => write!(formatter, "pipewire/sink/{node}"),
            Self::PipewireSource { node } => write!(formatter, "pipewire/source/{node}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Address;

    fn parse(text: &str) -> Result<Address, String> {
        Address::parse(text, &|backend, alias| {
            (backend == "focusrite" && alias == "scarlett").then(|| "P9H9".to_owned())
        })
    }

    #[test]
    fn parses_every_kind_and_round_trips() {
        for text in [
            "dante/AVIOUSBC-0563aa/rx/Left",
            "dante/mac/tx/Scarlett 18i20:Channel 9",
            "dante/dev/rx/Bus A/B",
            "focusrite/P9H9/input/1",
            "focusrite/P9H9/monitor",
            "pipewire/sink/alsa_output.usb-x.analog-stereo",
            "pipewire/source/alsa_input.pci",
        ] {
            assert_eq!(parse(text).unwrap().to_string(), text);
        }
    }

    #[test]
    fn device_aliases_resolve() {
        assert_eq!(
            parse("focusrite/scarlett/input/2").unwrap(),
            Address::FocusriteInput {
                device: "P9H9".to_owned(),
                input: 2
            }
        );
    }

    #[test]
    fn malformed_addresses_are_rejected() {
        for text in [
            "",
            "dante",
            "dante/dev",
            "dante//rx/Left",
            "dante/dev/rx/",
            "dante/dev/mix/1",
            "focusrite/dev/input/0",
            "focusrite/dev/input/x",
            "focusrite/dev/output/1",
            "pipewire/node/x",
            "pipewire/sink/",
        ] {
            assert!(parse(text).is_err(), "{text}");
        }
    }
}
