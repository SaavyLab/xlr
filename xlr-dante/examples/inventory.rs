//! Discovers every Dante device on one interface and prints its channels
//! and receiver subscriptions. Read-only.
//!
//! ```text
//! cargo run --example inventory -- 192.168.1.10
//! ```

use std::time::Duration;
use xlr_dante::model::SubscriptionState;
use xlr_dante::{ArcClient, DeviceBrowser};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let interface = std::env::args()
        .nth(1)
        .ok_or("usage: inventory <interface-ipv4>")?
        .parse()?;
    let browser = DeviceBrowser::new(interface, Duration::from_secs(2))?;
    for device in browser.browse()? {
        let mut client = ArcClient::connect(device.arc_address(), Duration::from_millis(500))?;
        let counts = client.channel_counts()?;
        println!(
            "{} ({}) at {}: {} tx, {} rx",
            client.device_name()?,
            device.product().unwrap_or("unknown product"),
            device.arc_address(),
            counts.transmitters,
            counts.receivers,
        );
        for tx in client.transmitter_channels()? {
            println!("  tx {:>3}  {}", tx.number(), tx.name());
        }
        for rx in client.receiver_subscriptions()? {
            let source = match rx.state() {
                SubscriptionState::Subscribed(tx) => {
                    format!("{}@{}", tx.channel_name(), tx.device_name())
                }
                SubscriptionState::Unsubscribed => "-".to_owned(),
            };
            println!(
                "  rx {:>3}  {:<28} <- {source}",
                rx.receiver_channel().value(),
                rx.name().unwrap_or(""),
            );
        }
    }
    Ok(())
}
