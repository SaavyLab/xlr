# xlr

**Let people and AI agents control a pro audio setup.**

xlr is an opinionated CLI plus a set of Rust crates for controlling audio
networks and devices programmatically, without the vendor's GUI in the loop.
Each backend crate targets one ecosystem, keeps its I/O explicit, and refuses
traffic it doesn't understand instead of guessing.

## Crates

### [xlr](./xlr/)

The CLI: one vocabulary for your whole setup, with `--json` output for scripts
and AI agents.

```bash
cargo install xlr
xlr status
```

### [xlr-focusrite](./xlr-focusrite/)

Direct USB control of Focusrite Scarlett interfaces, no Focusrite Control
required. Currently reads the Scarlett 18i20 3rd Gen's switches.

```toml
[dependencies]
xlr-focusrite = "0.1"
```

### [xlr-dante](./xlr-dante/)

Programmatic control for Dante audio networks: discover devices, list their
channels, and read and change receiver subscriptions.

```toml
[dependencies]
xlr-dante = "0.1"
```

## Direction

See [docs/vision.md](docs/vision.md) for where xlr is headed: per-host daemons
with key-pinned trust, your own names for everything, scenes with undo, and
end-to-end signal tracing.

## Development

The toolchain is pinned through Nix. With direnv, `cd` into the repository and
it activates automatically; otherwise use `nix develop`.

```bash
cargo test --workspace
nix run .#ci    # fmt, clippy, tests, and package verification
```

## Trademarks

Dante is a trademark of Audinate Pty Ltd. Focusrite and Scarlett are
trademarks of Focusrite Audio Engineering Ltd. xlr is an independent project
and is not affiliated with, endorsed by, or supported by Audinate, Focusrite,
or any other audio vendor.

---

Built by [SaavyLab](https://github.com/saavylab).
