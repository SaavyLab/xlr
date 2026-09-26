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
xlr status                                   # the whole setup: Dante + Focusrite
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

Built by [SaavyLab](https://github.com/saavylab).
