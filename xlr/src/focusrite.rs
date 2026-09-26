//! Focusrite backend: USB identification and read-only settings.

use crate::dante::tags;
use serde::Serialize;
use std::{fmt::Write as _, time::Duration};
use xlr_focusrite::usb::{FoundDevice, Session, UsbError, find_devices};

#[derive(Serialize)]
pub struct Identity {
    /// The device segment of this device's addresses: its serial number.
    pub id: String,
    pub product: Option<String>,
    pub model: Option<&'static str>,
    pub product_id: String,
    pub serial: Option<String>,
    pub supported: bool,
}

#[derive(Serialize)]
pub struct Status {
    #[serde(flatten)]
    pub identity: Identity,
    pub firmware: Option<u32>,
    pub monitor: Option<Monitor>,
    pub phantom_groups: Vec<PhantomGroup>,
    pub inputs: Vec<Input>,
    /// Why settings were not read, when that is expected (for example,
    /// Focusrite Control holds the device). Not a failure.
    pub unavailable: Option<String>,
    pub error: Option<String>,
}

/// Every Focusrite device on this machine.
#[derive(Serialize)]
pub struct FocusriteStatus {
    pub devices: Vec<Status>,
    /// Why USB enumeration itself failed, if it did.
    pub error: Option<String>,
}

impl FocusriteStatus {
    pub fn has_errors(&self) -> bool {
        self.error.is_some() || self.devices.iter().any(|device| device.error.is_some())
    }

    pub fn render(&self) -> String {
        match &self.error {
            Some(error) => format!("Focusrite USB enumeration failed: {error}\n"),
            None => render_statuses(&self.devices),
        }
    }
}

#[derive(Serialize)]
pub struct Monitor {
    pub mute: bool,
    pub dim: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

#[derive(Serialize)]
pub struct PhantomGroup {
    pub inputs: String,
    pub on: bool,
}

#[derive(Serialize)]
pub struct Input {
    pub input: u8,
    pub phantom: bool,
    pub pad: bool,
    pub air: bool,
    /// `true` for instrument, `false` for line; absent on inputs without a
    /// software switch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instrument: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
}

fn identity(device: &FoundDevice) -> Identity {
    Identity {
        id: device_id(device),
        product: device.product().map(str::to_owned),
        model: device.model().map(|model| model.name()),
        product_id: format!("0x{:04x}", device.product_id()),
        serial: device.serial_number().map(str::to_owned),
        supported: device.model().is_some(),
    }
}

pub fn identify() -> Result<Vec<Identity>, Box<dyn std::error::Error>> {
    Ok(find_devices()?.iter().map(identity).collect())
}

pub fn status(timeout: Duration) -> FocusriteStatus {
    match find_devices() {
        Ok(devices) => FocusriteStatus {
            devices: devices.iter().map(|device| read(device, timeout)).collect(),
            error: None,
        },
        Err(error) => FocusriteStatus {
            devices: Vec::new(),
            error: Some(error.to_string()),
        },
    }
}

fn read(device: &FoundDevice, timeout: Duration) -> Status {
    let mut status = Status {
        identity: identity(device),
        firmware: None,
        monitor: None,
        phantom_groups: Vec::new(),
        inputs: Vec::new(),
        unavailable: None,
        error: None,
    };
    if device.model().is_none() {
        status.unavailable = Some("model not supported yet".to_owned());
        return status;
    }
    let result = Session::open(device, timeout).and_then(|mut session| {
        status.firmware = session.firmware();
        session.read_gen3_settings()
    });
    match result {
        Ok(settings) => {
            status.monitor = Some(Monitor {
                mute: settings.mute,
                dim: settings.dim,
                names: Vec::new(),
            });
            status.phantom_groups = settings
                .phantom_groups
                .iter()
                .enumerate()
                .map(|(index, &on)| PhantomGroup {
                    inputs: format!("{}-{}", index * 4 + 1, index * 4 + 4),
                    on,
                })
                .collect();
            status.inputs = settings
                .inputs
                .iter()
                .map(|input| Input {
                    input: input.number,
                    phantom: input.phantom,
                    pad: input.pad,
                    air: input.air,
                    instrument: input.instrument,
                    names: Vec::new(),
                })
                .collect();
        }
        Err(UsbError::Claim(_)) => {
            status.unavailable =
                Some("Focusrite Control is using the device; quit it to read settings".to_owned());
        }
        Err(error) => status.error = Some(error.to_string()),
    }
    status
}

pub fn render_identities(identities: &[Identity]) -> String {
    let mut out = String::new();
    if identities.is_empty() {
        out.push_str("No Focusrite USB devices found.\n");
    }
    for device in identities {
        let _ = writeln!(
            out,
            "{}  ({}, {}, serial {}){}",
            device.product.as_deref().unwrap_or("Focusrite device"),
            device.model.unwrap_or("unsupported model"),
            device.product_id,
            device.serial.as_deref().unwrap_or("?"),
            if device.supported {
                ""
            } else {
                "  [unsupported]"
            },
        );
    }
    out
}

pub fn render_statuses(statuses: &[Status]) -> String {
    let mut out = String::new();
    if statuses.is_empty() {
        out.push_str("No Focusrite USB devices found.\n");
    }
    let on = |value: bool| if value { "on" } else { "off" };
    for device in statuses {
        let _ = writeln!(
            out,
            "{}  ({}, firmware {})",
            device
                .identity
                .product
                .as_deref()
                .unwrap_or("Focusrite device"),
            device.identity.model.unwrap_or("unsupported model"),
            device
                .firmware
                .map_or_else(|| "?".to_owned(), |firmware| firmware.to_string()),
        );
        if let Some(reason) = &device.unavailable {
            let _ = writeln!(out, "  unavailable: {reason}");
            continue;
        }
        if let Some(error) = &device.error {
            let _ = writeln!(out, "  error: {error}");
            continue;
        }
        if let Some(monitor) = &device.monitor {
            let _ = writeln!(
                out,
                "  monitor  mute {}  dim {}",
                on(monitor.mute),
                on(monitor.dim)
            );
        }
        for group in &device.phantom_groups {
            let _ = writeln!(out, "  48V {:<5}  {}", group.inputs, on(group.on));
        }
        for input in &device.inputs {
            let mode = match input.instrument {
                Some(true) => "  inst",
                Some(false) => "  line",
                None => "",
            };
            let _ = writeln!(
                out,
                "  in {}  pad {:<3}  air {:<3}{mode}{}",
                input.input,
                on(input.pad),
                on(input.air),
                tags(&input.names),
            );
        }
    }
    out
}

/// The address device segment for a Focusrite device: its serial number,
/// or its product ID when it reports none.
fn device_id(device: &FoundDevice) -> String {
    device
        .serial_number()
        .map_or_else(|| format!("{:04x}", device.product_id()), str::to_owned)
}
