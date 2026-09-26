# xlr-pipewire

**Read the PipeWire audio graph.**

`xlr-pipewire` runs `pw-dump` once and turns its output into a small,
audio-focused model: sinks and sources (with their devices and the session
manager's defaults) and application streams (with the endpoints they are
linked to). It only reads; it never changes the graph.

```rust,no_run
let graph = xlr_pipewire::read()?;
for endpoint in &graph.endpoints {
    let mark = if endpoint.is_default { "*" } else { " " };
    println!("{mark} {:?} {}", endpoint.direction, endpoint.name);
}
# Ok::<(), xlr_pipewire::PipewireError>(())
```

Requires PipeWire's `pw-dump` on `PATH`.

## License

MIT
