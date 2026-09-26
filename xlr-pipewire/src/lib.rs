//! Read the PipeWire audio graph.
//!
//! [`read`] runs `pw-dump` once and [`Graph::parse`] turns its JSON into a
//! small audio-focused model: endpoints (sinks and sources), application
//! streams, which endpoints each stream is linked to, and the session
//! manager's default sink and source. Everything else in the dump (MIDI,
//! video, modules, clients) is ignored.
//!
//! This crate only reads; it never changes the graph.

use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt, io,
    process::Command,
};

/// Whether an endpoint plays audio out or captures audio in.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Direction {
    /// An `Audio/Sink`: speakers, headphones, an audio interface's outputs.
    Sink,
    /// An `Audio/Source`: microphones, an audio interface's inputs.
    Source,
}

/// A hardware or virtual sink or source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Endpoint {
    pub id: u32,
    pub direction: Direction,
    /// The stable node name, e.g. `alsa_output.usb-…analog-stereo`.
    pub name: String,
    /// The human-readable description.
    pub description: Option<String>,
    /// The description of the device this endpoint belongs to.
    pub device: Option<String>,
    /// Node state as reported by PipeWire: `running`, `idle`, `suspended`, …
    pub state: Option<String>,
    pub is_default: bool,
}

/// An application's playback or capture stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Stream {
    pub id: u32,
    /// `Sink` for playback streams (they feed sinks), `Source` for capture
    /// streams (they read from sources or sink monitors).
    pub direction: Direction,
    pub application: Option<String>,
    pub node_name: Option<String>,
    pub media_name: Option<String>,
    pub state: Option<String>,
    /// Names of the endpoints this stream is linked to.
    pub endpoints: Vec<String>,
}

/// The audio part of a PipeWire graph.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Graph {
    pub endpoints: Vec<Endpoint>,
    pub streams: Vec<Stream>,
}

impl Graph {
    /// Parses `pw-dump` output.
    pub fn parse(json: &str) -> Result<Self, PipewireError> {
        let objects: Vec<Value> =
            serde_json::from_str(json).map_err(|error| PipewireError::Parse(error.to_string()))?;

        let mut devices = BTreeMap::new();
        let mut nodes = Vec::new();
        let mut links = Vec::new();
        let mut defaults = BTreeMap::new();
        for object in &objects {
            let Some(id) = object["id"].as_u64().and_then(|id| u32::try_from(id).ok()) else {
                continue;
            };
            let props = &object["info"]["props"];
            match object["type"].as_str() {
                Some("PipeWire:Interface:Device") => {
                    if let Some(description) = text(props, "device.description") {
                        devices.insert(id, description);
                    }
                }
                Some("PipeWire:Interface:Node") => nodes.push((id, object)),
                Some("PipeWire:Interface:Link") => {
                    let info = &object["info"];
                    if let (Some(output), Some(input)) = (
                        info["output-node-id"].as_u64(),
                        info["input-node-id"].as_u64(),
                    ) {
                        links.push((output, input));
                    }
                }
                Some("PipeWire:Interface:Metadata")
                    if object["props"]["metadata.name"].as_str() == Some("default") =>
                {
                    for entry in object["metadata"].as_array().into_iter().flatten() {
                        if let (Some(key), Some(name)) =
                            (entry["key"].as_str(), entry["value"]["name"].as_str())
                        {
                            defaults.insert(key.to_owned(), name.to_owned());
                        }
                    }
                }
                _ => {}
            }
        }

        let default_sink = defaults.get("default.audio.sink");
        let default_source = defaults.get("default.audio.source");
        let mut graph = Self::default();
        let mut endpoint_names = BTreeMap::new();
        for (id, node) in &nodes {
            let props = &node["info"]["props"];
            let direction = match props["media.class"].as_str() {
                Some("Audio/Sink") => Direction::Sink,
                Some("Audio/Source") => Direction::Source,
                _ => continue,
            };
            let Some(name) = text(props, "node.name") else {
                continue;
            };
            let is_default = match direction {
                Direction::Sink => default_sink == Some(&name),
                Direction::Source => default_source == Some(&name),
            };
            endpoint_names.insert(u64::from(*id), name.clone());
            graph.endpoints.push(Endpoint {
                id: *id,
                direction,
                name,
                description: text(props, "node.description"),
                device: props["device.id"]
                    .as_u64()
                    .and_then(|device| u32::try_from(device).ok())
                    .and_then(|device| devices.get(&device).cloned()),
                state: node["info"]["state"].as_str().map(str::to_owned),
                is_default,
            });
        }
        for (id, node) in &nodes {
            let props = &node["info"]["props"];
            let direction = match props["media.class"].as_str() {
                Some("Stream/Output/Audio") => Direction::Sink,
                Some("Stream/Input/Audio") => Direction::Source,
                _ => continue,
            };
            let id64 = u64::from(*id);
            let linked: BTreeSet<&String> = links
                .iter()
                .filter_map(|&(output, input)| match (output == id64, input == id64) {
                    (true, _) => endpoint_names.get(&input),
                    (_, true) => endpoint_names.get(&output),
                    _ => None,
                })
                .collect();
            graph.streams.push(Stream {
                id: *id,
                direction,
                application: text(props, "application.name"),
                node_name: text(props, "node.name"),
                media_name: text(props, "media.name"),
                state: node["info"]["state"].as_str().map(str::to_owned),
                endpoints: linked.into_iter().cloned().collect(),
            });
        }
        graph
            .endpoints
            .sort_by(|a, b| (a.direction, &a.name).cmp(&(b.direction, &b.name)));
        graph.streams.sort_by_key(|stream| stream.id);
        Ok(graph)
    }
}

fn text(props: &Value, key: &str) -> Option<String> {
    props[key].as_str().map(str::to_owned)
}

/// Reads the current graph by running `pw-dump` once.
pub fn read() -> Result<Graph, PipewireError> {
    let output = Command::new("pw-dump").output().map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            PipewireError::NotInstalled
        } else {
            PipewireError::Run(error.to_string())
        }
    })?;
    if !output.status.success() {
        return Err(PipewireError::Run(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Graph::parse(&String::from_utf8_lossy(&output.stdout))
}

/// Why the graph could not be read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PipewireError {
    /// `pw-dump` is not on `PATH`: PipeWire is not installed here.
    NotInstalled,
    /// `pw-dump` failed, usually because no PipeWire daemon is running.
    Run(String),
    /// The output was not the expected JSON.
    Parse(String),
}

impl fmt::Display for PipewireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInstalled => formatter.write_str("PipeWire is not installed (no pw-dump)"),
            Self::Run(reason) => write!(formatter, "pw-dump failed: {reason}"),
            Self::Parse(reason) => write!(formatter, "could not parse pw-dump output: {reason}"),
        }
    }
}

impl Error for PipewireError {}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = r#"[
      {"id": 0, "type": "PipeWire:Interface:Core", "info": {}},
      {"id": 80, "type": "PipeWire:Interface:Device",
       "info": {"props": {"device.description": "USB Interface"}}},
      {"id": 58, "type": "PipeWire:Interface:Node",
       "info": {"state": "running", "props": {"media.class": "Audio/Sink",
        "node.name": "alsa_output.usb-iface.analog-stereo",
        "node.description": "USB Interface Analog Stereo", "device.id": 80}}},
      {"id": 59, "type": "PipeWire:Interface:Node",
       "info": {"state": "suspended", "props": {"media.class": "Audio/Sink",
        "node.name": "alsa_output.hdmi", "node.description": "HDMI"}}},
      {"id": 111, "type": "PipeWire:Interface:Node",
       "info": {"state": "suspended", "props": {"media.class": "Audio/Source",
        "node.name": "alsa_input.usb-iface.analog-stereo", "device.id": 80}}},
      {"id": 51, "type": "PipeWire:Interface:Node",
       "info": {"props": {"media.class": "Midi/Bridge", "node.name": "Midi-Bridge"}}},
      {"id": 55, "type": "PipeWire:Interface:Node",
       "info": {"state": "running", "props": {"media.class": "Stream/Output/Audio",
        "node.name": "player", "application.name": "Player", "media.name": "Song"}}},
      {"id": 73, "type": "PipeWire:Interface:Node",
       "info": {"state": "idle", "props": {"media.class": "Stream/Input/Audio",
        "application.name": "Browser"}}},
      {"id": 200, "type": "PipeWire:Interface:Link",
       "info": {"output-node-id": 55, "input-node-id": 58, "state": "active"}},
      {"id": 201, "type": "PipeWire:Interface:Link",
       "info": {"output-node-id": 55, "input-node-id": 58, "state": "active"}},
      {"id": 202, "type": "PipeWire:Interface:Link",
       "info": {"output-node-id": 111, "input-node-id": 73, "state": "active"}},
      {"id": 40, "type": "PipeWire:Interface:Metadata",
       "props": {"metadata.name": "default"},
       "metadata": [
         {"subject": 0, "key": "default.audio.sink",
          "value": {"name": "alsa_output.usb-iface.analog-stereo"}},
         {"subject": 0, "key": "default.audio.source",
          "value": {"name": "alsa_input.usb-iface.analog-stereo"}}
       ]}
    ]"#;

    #[test]
    fn parses_endpoints_with_devices_and_defaults() {
        let graph = Graph::parse(DUMP).unwrap();
        let summary: Vec<_> = graph
            .endpoints
            .iter()
            .map(|e| {
                (
                    e.direction,
                    e.name.as_str(),
                    e.is_default,
                    e.device.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (Direction::Sink, "alsa_output.hdmi", false, None),
                (
                    Direction::Sink,
                    "alsa_output.usb-iface.analog-stereo",
                    true,
                    Some("USB Interface")
                ),
                (
                    Direction::Source,
                    "alsa_input.usb-iface.analog-stereo",
                    true,
                    Some("USB Interface")
                ),
            ]
        );
    }

    #[test]
    fn streams_list_their_linked_endpoints_once() {
        let graph = Graph::parse(DUMP).unwrap();
        assert_eq!(graph.streams.len(), 2);
        let player = &graph.streams[0];
        assert_eq!(player.direction, Direction::Sink);
        assert_eq!(player.application.as_deref(), Some("Player"));
        assert_eq!(player.endpoints, ["alsa_output.usb-iface.analog-stereo"]);
        let browser = &graph.streams[1];
        assert_eq!(browser.direction, Direction::Source);
        assert_eq!(browser.endpoints, ["alsa_input.usb-iface.analog-stereo"]);
    }

    #[test]
    fn non_json_is_a_parse_error() {
        assert!(matches!(Graph::parse("nope"), Err(PipewireError::Parse(_))));
        assert_eq!(Graph::parse("[]").unwrap(), Graph::default());
    }
}
