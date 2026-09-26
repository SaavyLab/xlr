# xlr-dante

**Programmatic control for Dante audio networks.**

`xlr-dante` lets you see and change Dante audio routing from Rust: find the
devices on your network, list their transmitter and receiver channels, read
what every receiver channel is subscribed to, and route or clear
subscriptions. No Dante Controller in the loop.

```toml
[dependencies]
xlr-dante = "0.1"
```

## What it does

| Operation | Transport | API |
|---|---|---|
| Find devices | mDNS `_netaudio-arc._udp` | `DeviceBrowser::browse` |
| Device name and channel counts | ARC over UDP | `ArcClient::device_name`, `ArcClient::channel_counts` |
| List transmitter channels | ARC over UDP | `ArcClient::transmitter_channels` |
| Read receiver subscriptions | ARC over UDP | `ArcClient::receiver_subscriptions`, `ArcClient::query_subscription` |
| Set or clear a subscription | ARC over UDP | `ArcClient::apply_subscription`, `ArcClient::apply_paged_subscription` |
| Resolve one transmitter channel (`channel@device`) | mDNS `_netaudio-chan._udp` | `ChannelServiceClient::query` |

Every ARC operation also exists as a pure codec (`xlr_dante::arc`) that turns
domain values into bytes and back without touching a socket, so you can bring
your own transport or async runtime.

## Example

Find every device, list its channels, and print its routing:

```rust,no_run
use std::time::Duration;
use xlr_dante::model::SubscriptionState;
use xlr_dante::{ArcClient, DeviceBrowser};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Browse from the interface on your Dante network.
    let browser = DeviceBrowser::new("192.168.1.10".parse()?, Duration::from_secs(2))?;
    for device in browser.browse()? {
        let mut client = ArcClient::connect(device.arc_address(), Duration::from_millis(500))?;
        println!("{} ({})", device.name(), device.product().unwrap_or("?"));
        for tx in client.transmitter_channels()? {
            println!("  tx {} {}", tx.number(), tx.name());
        }
        for rx in client.receiver_subscriptions()? {
            let source = match rx.state() {
                SubscriptionState::Subscribed(tx) => {
                    format!("{}@{}", tx.channel_name(), tx.device_name())
                }
                SubscriptionState::Unsubscribed => "-".to_owned(),
            };
            println!("  rx {} <- {source}", rx.receiver_channel().value());
        }
    }
    Ok(())
}
```

Routing a receiver channel, then confirming it:

```rust,no_run
use std::time::Duration;
use xlr_dante::ArcClient;
use xlr_dante::model::{ReceiverChannel, TransmitterSelector};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = ArcClient::connect("192.168.1.40:4440".parse()?, Duration::from_millis(500))?;
    let rx = ReceiverChannel::new(1)?;

    // Subscribe rx 1 to "Mic 3" on the device named "stage-box".
    let source = TransmitterSelector::new("stage-box".into(), "Mic 3".into())?;
    client.apply_subscription(rx, Some(&source))?; // `None` clears it

    // Acceptance is not convergence: read it back.
    println!("{:?}", client.query_subscription(rx)?.state());
    Ok(())
}
```

Resolving one transmitter channel over mDNS on a specific interface:

```rust,no_run
use std::time::Duration;
use xlr_dante::{ChannelServiceClient, ChannelServiceName};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = ChannelServiceClient::new("192.168.1.10".parse()?, Duration::from_secs(1))?;
    let name = ChannelServiceName::new("stage-box".into(), "Mic 3".into())?;
    let service = client.query(&name)?;
    println!("{} answered from {}", name.instance_fqdn(), service.response_source());
    Ok(())
}
```

The runnable [`examples/`](examples/) include `discover` and a read-only
`inventory` of every device on an interface:

```bash
cargo run --example inventory -- 192.168.1.10
```

## Design

- **Codec, client, and policy are separate layers.** The codec is pure bytes
  ↔ domain values. The client is synchronous, bound to one fixed peer, and
  sends each request exactly once. Retries, async wrappers, and higher-level
  control are left to you.
- **Strict correlation.** Responses must match the request's sequence number
  and come from the configured peer. Each socket uses each 16-bit sequence at
  most once, then fails closed.
- **Writes never retry.** A write succeeds only on an exact, correlated
  acceptance reply. If it fails after the request left the socket (for
  example a `Receive`-stage timeout), the device may or may not have applied
  it; the crate reports that error and never resends, so you can re-query and
  decide.
- **Explicit variants only.** The codec implements the specific message forms
  listed above and rejects anything else with a typed error.

## Compatibility

Dante's control protocol is proprietary and undocumented. This crate
implements message forms observed from real devices on a small test network
(Dante Via on macOS and a Dante AVIO USB-C adapter) and verified against
synthetic fixtures. Other devices and firmware versions may use variants
that this crate will reject instead of misreading.

- Discovery, device info, channel listings, subscription reads, and
  subscription writes all work on both observed devices.
- There are two write forms. `ArcClient::apply_subscription` sends the Dante
  Via form (ARC protocol 2.8.15); `ArcClient::apply_paged_subscription` sends
  the paged form Dante hardware uses (verified on ARC protocol 2.8.9). Pick by
  `DiscoveredDevice::arc_protocol`; other versions are refused.
- Dante Via reports `.` as the device name for a subscription to one of its
  own channels.

Reports and captures of new variants are welcome.

## Acknowledgements

[NetAudio](https://github.com/chris-ritsen/network-audio-controller)
(public domain) was a reference for the paged subscription write and for
reading the ARC protocol version from `arcp_vers`; both were then verified
against the devices above.

## Trademarks

Dante is a trademark of Audinate Pty Ltd. This crate is an independent project
and is not affiliated with, endorsed by, or supported by Audinate.

## License

MIT
