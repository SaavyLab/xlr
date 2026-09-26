# xlr vision

## Goal

Let people and AI agents control a real audio setup (routing, input
switches, scenes) safely, from the command line, without driving vendor GUIs.

The test for every feature: can an agent take a vague request ("my desktop
mic sounds dead", "set up for a guitar session") and act on it correctly
without breaking anything?

## Principles

- **The user's vocabulary, not the vendor's.** Things have names you chose
  ("guitar", "desktop-in"). Vendor addresses stay available underneath.
- **Acceptance is not verification.** A device saying "OK" proves nothing.
  Every change is read back, and `xlr` reports what the hardware says now.
- **One change per step, restore on failure.** A write touches one thing.
  If it does not verify, the previous state is put back.
- **Fail closed.** Unknown models, protocol variants, and values are refused
  with a clear next step, never guessed at.
- **Dry-run and idempotence everywhere.** Every write can be previewed.
  Repeating a command that is already satisfied changes nothing.
- **No control plane.** No machine is in charge. Each host is authoritative
  for its own hardware.

## Architecture

### One binary

`xlr` is both the CLI and the daemon (`xlr serve`). With no daemons
configured, `xlr` talks to local backends in-process, as it does today.

### Hosts own facts

Each host runs `xlr serve` and is authoritative for the hardware attached to
it:

- **devices**: its backends (USB interfaces, the local audio server, the Dante
  endpoints that live on it);
- **names**: your aliases for its inputs, outputs, and channels;
- **guardrails**: what may change, and what needs explicit confirmation;
- **journal**: a record of every change and the prior state, for `undo`.

Dante devices are network-visible from anywhere, but each is still owned by
one host. Dante Via belongs to the machine running it, and a Dante adapter
belongs to the machine it is plugged into. Standalone Dante hardware is
assigned to a host in its configuration.

### Clients hold intent

A client knows only which hosts exist and their pinned keys, like
`~/.ssh/known_hosts`. It asks every known host, merges the answers into one
view, and sends each host only the changes for its own hardware.

Scenes (below) are plain files that live with whoever applies them, and can
be shared through git. There is no scene server and nothing central to lose.

### Trust

Tailscale and other overlays are welcome but never required.

- Each daemon generates a key pair on first run. Its fingerprint is the
  host's identity; there is no certificate authority.
- Remote connections use TLS with both sides pinning the peer's key
  fingerprint. Names and network addresses never grant anything.
- Pairing: a new client requests access, the host shows its fingerprint and
  a short code, and the operator approves it on the host with a role.
- Roles: `read` (status, trace, meters) and `control` (routes, switches,
  scenes). Guardrails apply on top of roles.
- Local callers use a Unix socket restricted to the same user.
- Optional integration: a host may also trust tailnet peers that hold a
  specific Tailscale app capability, verified through the local `tailscaled`.
  The underlying peer check is the same either way.

## Backends

| Backend | Reach | Status |
|---|---|---|
| Dante | network (ARC over UDP, mDNS) | discovery, channels, subscriptions read and write (Via and hardware forms) |
| Focusrite | USB vendor control interface | Scarlett 18i20 3rd Gen switches, read-only |
| PipeWire | local audio server | planned: graph read, then links and defaults |

Each backend crate (`xlr-dante`, `xlr-focusrite`, …) is a standalone library
with no knowledge of the CLI, daemons, or other backends.

## Addresses and names

Every controllable thing has a canonical address:

```text
dante/<device>/rx/<channel>      dante/AVIOUSBC-0563aa/rx/Left
dante/<device>/tx/<channel>      dante/saavys-Mac-mini/tx/Scarlett 18i20:Channel 9
focusrite/<device>/input/<n>     focusrite/scarlett/input/1
focusrite/<device>/monitor       focusrite/scarlett/monitor
pipewire/<node>                  pipewire/alsa_input.usb-Audinate…
```

A host's configuration maps names to addresses. Names are what `xlr status`
shows first and what every command accepts.

## Scenes

A scene is a desired state spanning hosts:

```bash
xlr snapshot guitar-session          # capture the current state to a file
xlr apply guitar-session --dry-run   # show exactly what would change
xlr apply guitar-session             # change it, one step at a time, read back
xlr undo                             # revert the last change from the journals
```

An agent edits or chooses a desired state and presents the diff instead of
issuing a sequence of imperative commands.

## Diagnosis

- **Trace**: follow a signal end to end, for example physical input →
  Scarlett routing → Via → Dante → AVIO → PipeWire → application.
- **Meters**: show whether signal is actually arriving at each hop, so a dead
  mic, a wrong route, and a muted output look different.

## Agent interface

- Stable JSON (`--json`) for every command and meaningful exit codes.
- Unavailable (expected) is distinguished from error (unexpected).
- `xlr agent-guide` prints the vocabulary, the JSON shapes, and the safe
  workflow (status → plan → apply → verify) in one call.

## Build order

1. Names: per-host configuration mapping names to addresses; `status` and
   `route` speak names.
2. PipeWire read backend.
3. `xlr serve` with key-pinned trust and pairing; multi-host `status`,
   read-only.
4. Remote control role; snapshot, apply, and undo; host-enforced guardrails.
5. Trace, meters, and `xlr agent-guide`.

Scarlett writes can land once the read path is confirmed on hardware.

## Non-goals

- A broker, a central certificate authority, or any control-plane machine.
- Leases, plan digests, and receipts (lessons kept from SaavyLab Fabric: key
  identity, scoped roles, verify-after-write, and restore-on-failure).
- Carrying audio. `xlr` controls audio systems; it never transports audio.

## Open questions

- Pairing code mechanics: a short confirmation code displayed on both sides,
  or a PAKE.
- Journal retention, and how `undo` behaves when a host is offline.
- How hosts announce themselves (mDNS `_xlr._tcp` is likely).
- Whether scenes should record the host keys they were captured against.
