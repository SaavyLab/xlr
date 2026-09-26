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

### [xlr-dante](./xlr-dante/)

Programmatic control for Dante audio networks: discover devices, list their
channels, and read and change receiver subscriptions.

```toml
[dependencies]
xlr-dante = "0.1"
```

## Development

The toolchain is pinned through Nix. With direnv, `cd` into the repository and
it activates automatically; otherwise use `nix develop`.

```bash
cargo test --workspace
nix run .#ci    # fmt, clippy, tests, and package verification
```

## Trademarks

Dante is a trademark of Audinate Pty Ltd. xlr is an independent project and is
not affiliated with, endorsed by, or supported by Audinate or any other audio
vendor.

---

Built by [SaavyLab](https://github.com/saavylab).
