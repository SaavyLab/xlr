# xlr

**An opinionated CLI for controlling a pro audio setup — built for people and
AI agents alike.**

`xlr` gives one consistent vocabulary over the gear in an audio setup: which
devices exist, what is routed where, and how to change it safely. Backends
cover specific ecosystems, starting with Dante
([`xlr-dante`](https://crates.io/crates/xlr-dante)) with Focusrite next.

```bash
cargo install xlr
```

## Usage

```bash
xlr status                                   # the whole setup: Dante, Focusrite, PipeWire
xlr status --json                            # the same, as stable JSON
xlr route "Left@stage-box" "Mic 3@foh-rack"   # route a receiver from a source
xlr route "Left@stage-box" --clear            # unsubscribe it
xlr route "Left@stage-box" "Mic 3@foh-rack" --dry-run
```

`xlr route` checks that the source exists, skips the write when the route is
already in place, picks the device's write form from its advertised ARC
protocol, and reads the receiver back to confirm. It exits nonzero unless the
device reports the requested route.

```text
stage-box  (DIOUSB, 192.168.1.40:4440)
  tx   1  Left
  tx   2  Right
  rx   1  Left   <- Mic 3@foh-rack
  rx   2  Right  <- -
```

Sources are always written `channel@device`, with real device names (Dante
Via's `.` shorthand for itself is resolved). The local interface is detected
automatically; override it with `--interface` or `XLR_INTERFACE`. When any
device cannot be read, `xlr` still prints everything it could, marks the
failure in that device's `error` field, and exits nonzero.

## Multiple machines

Run `xlr serve` on each machine with audio hardware, then pair the machines
you control from:

```bash
# on the machine with the hardware (e.g. mac-mini)
xlr serve                      # listens on 0.0.0.0:7373
xlr id                         # shows its fingerprint

# on the machine you control from
xlr hosts add mac-mini 192.168.1.20 --fingerprint <fingerprint from xlr id>

# back on mac-mini: check the code matches, then approve
xlr peers                      # shows the pending request and its code
xlr peers approve 954466
```

After that, `xlr status` shows the whole studio: Dante once, and each host's
Focusrite and PipeWire state under that host, each labelled with the names
from that host's own config. `xlr status --local` reads only this machine.

Each machine has its own key pair (in `~/.config/xlr/identity/`, or
`$XLR_HOME`). Connections use TLS 1.3 with both sides pinning the other's
certificate fingerprint; there is no certificate authority, and addresses
never grant anything. Approved peers and their roles live in
`~/.config/xlr/peers.toml`; revoke one with `xlr peers remove`.

## Names

Give things your own names in `~/.config/xlr/xlr.toml` (or `$XLR_CONFIG`):

```toml
# Short names for devices, used inside addresses.
[devices]
scarlett = "focusrite/<serial>"
stage-box = "dante/<dante device name>"

# Your names for the things you use.
[names]
guitar = "focusrite/scarlett/input/1"
desktop-left = "dante/stage-box/rx/Left"
mic-3 = "dante/foh-rack/tx/Mic 3"
```

Every command accepts names, `xlr status` shows them next to the hardware
they refer to, and `xlr names` checks each one against what is actually
connected (`found`, `missing`, or `unverified` when its device can't be
read), exiting nonzero if any are missing.

A host can also declare Dante devices attached to it (Dante Via on the same
machine is recognised automatically), so status shows which host each Dante
device lives on:

```toml
[host]
owns = ["dante/avio"]
```

Addresses: `dante/<device>/rx/<channel or number>`,
`dante/<device>/tx/<channel>`, `focusrite/<device>/input/<n>`,
`focusrite/<device>/monitor`, `pipewire/sink/<node>`, and
`pipewire/source/<node>`. A Focusrite device is identified by its serial
number (shown in `xlr status --json` as `id`).

Built by [SaavyLab](https://github.com/saavylab).
