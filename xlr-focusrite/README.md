# xlr-focusrite

**Programmatic control for Focusrite Scarlett interfaces.**

`xlr-focusrite` talks to a Scarlett's vendor-specific USB control interface
directly, without Focusrite Control running. It currently reads the
Scarlett 18i20 3rd Gen's switches: phantom power, pad, air,
line/instrument, and monitor mute/dim.

```toml
[dependencies]
xlr-focusrite = "0.1"
```

## Safety

- **Read-only.** The command set is a closed allowlist of the initialization
  handshake and configuration reads; write, save, flash, and reboot commands
  cannot be expressed.
- **Audio keeps flowing.** Only the vendor control interface is claimed. The
  audio interfaces, the USB configuration, and the OS audio driver are never
  touched.
- **Fail closed.** Unsupported models are refused before the device is
  opened, and configuration bytes that do not look like switch values are
  refused rather than guessed at.

Only one program can hold the control interface, so quit Focusrite Control
(or Focusrite Control 2) first.

## Compatibility

Verified on a Scarlett 18i20 3rd Gen (USB product `0x8215`, firmware 1644)
on macOS. Other models are refused until their layouts are verified.

## Acknowledgements

Configuration offsets follow the documentation of the Linux kernel's
`scarlett2` mixer driver by Geoffrey D. Bennett. The USB framing was
cross-checked against
[focusrite-control-2-api](https://github.com/alexanderdalesio/focusrite-control-2-api)
(MIT).

## Trademarks

Focusrite and Scarlett are trademarks of Focusrite Audio Engineering Ltd.
This crate is an independent project and is not affiliated with, endorsed
by, or supported by Focusrite.

## License

MIT
