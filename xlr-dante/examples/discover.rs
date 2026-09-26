//! Lists Dante devices advertising ARC on one interface.
//!
//! ```text
//! cargo run --example discover -- 192.168.1.10
//! ```

use std::time::Duration;
use xlr_dante::DeviceBrowser;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let interface = std::env::args()
        .nth(1)
        .ok_or("usage: discover <interface-ipv4>")?
        .parse()?;
    let browser = DeviceBrowser::new(interface, Duration::from_secs(2))?;
    for device in browser.browse()? {
        println!(
            "{:<24} {:<22} {} {}",
            device.name(),
            device.arc_address(),
            device.manufacturer().unwrap_or("?"),
            device.product().unwrap_or("?"),
        );
    }
    Ok(())
}
