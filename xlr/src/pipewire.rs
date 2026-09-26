//! PipeWire backend: the desktop audio graph, read-only.

use crate::dante::tags;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use xlr_pipewire::{Direction, PipewireError};

#[derive(Deserialize, Serialize)]
pub struct PipewireStatus {
    pub outputs: Vec<Endpoint>,
    pub inputs: Vec<Endpoint>,
    /// Streams that are not linked to any output or input.
    pub unlinked_streams: Vec<String>,
    /// Why the graph was not read, when that is expected (no PipeWire here).
    pub unavailable: Option<String>,
    pub error: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct Endpoint {
    /// The stable node name, used in `pipewire/sink/<node>` addresses.
    pub node: String,
    pub description: Option<String>,
    pub device: Option<String>,
    pub state: Option<String>,
    pub default: bool,
    /// Applications playing to (outputs) or recording from (inputs) it.
    pub streams: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

pub fn status() -> PipewireStatus {
    let mut status = PipewireStatus {
        outputs: Vec::new(),
        inputs: Vec::new(),
        unlinked_streams: Vec::new(),
        unavailable: None,
        error: None,
    };
    let graph = match xlr_pipewire::read() {
        Ok(graph) => graph,
        Err(PipewireError::NotInstalled) => {
            status.unavailable = Some("PipeWire is not installed on this host".to_owned());
            return status;
        }
        Err(error) => {
            status.error = Some(error.to_string());
            return status;
        }
    };
    let label = |stream: &xlr_pipewire::Stream| {
        stream
            .application
            .clone()
            .or_else(|| stream.node_name.clone())
            .unwrap_or_else(|| format!("stream {}", stream.id))
    };
    for endpoint in &graph.endpoints {
        let view = Endpoint {
            node: endpoint.name.clone(),
            description: endpoint.description.clone(),
            device: endpoint.device.clone(),
            state: endpoint.state.clone(),
            default: endpoint.is_default,
            streams: graph
                .streams
                .iter()
                .filter(|stream| stream.endpoints.contains(&endpoint.name))
                .map(label)
                .collect(),
            names: Vec::new(),
        };
        match endpoint.direction {
            Direction::Sink => status.outputs.push(view),
            Direction::Source => status.inputs.push(view),
        }
    }
    status.unlinked_streams = graph
        .streams
        .iter()
        .filter(|stream| stream.endpoints.is_empty())
        .map(label)
        .collect();
    status
}

impl PipewireStatus {
    pub fn has_errors(&self) -> bool {
        self.error.is_some()
    }

    pub fn render(&self) -> String {
        if let Some(reason) = &self.unavailable {
            return format!("unavailable: {reason}\n");
        }
        if let Some(error) = &self.error {
            return format!("error: {error}\n");
        }
        let mut out = String::new();
        for (heading, endpoints, arrow) in [
            ("outputs", &self.outputs, "<-"),
            ("inputs", &self.inputs, "->"),
        ] {
            let _ = writeln!(out, "{heading}");
            if endpoints.is_empty() {
                let _ = writeln!(out, "  (none)");
            }
            for endpoint in endpoints {
                let _ = writeln!(
                    out,
                    "  {} {}  ({}){}",
                    if endpoint.default { "*" } else { " " },
                    endpoint.description.as_deref().unwrap_or(&endpoint.node),
                    endpoint.state.as_deref().unwrap_or("?"),
                    tags(&endpoint.names),
                );
                if !endpoint.streams.is_empty() {
                    let _ = writeln!(out, "      {arrow} {}", endpoint.streams.join(", "));
                }
            }
        }
        if !self.unlinked_streams.is_empty() {
            let _ = writeln!(
                out,
                "unlinked streams: {}",
                self.unlinked_streams.join(", ")
            );
        }
        out
    }
}
