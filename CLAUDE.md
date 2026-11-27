# ddcolord — conventions

A user daemon that reads a monitor's OSD color mode over DDC/CI and keeps
colord's default profile for that display in sync, for compositors that do not
register displays in colord (sway). First deployment: iridium (NixOS, sway,
BenQ PD3220U on DP-2), wired in by the nix.config repo as a home-manager user
service.

- `nix develop --command cargo build --locked`
- `nix develop --command cargo test --locked`
- `nix develop --command cargo clippy --all-targets --locked -- -D warnings`
- `git config core.hooksPath hooks` once per clone
- Live check: `cargo run -- --probe`, then `colormgr get-devices`

## Layout

- `main.rs` — CLI, poll loop, `--probe`
- `config.rs` — TOML config, mode → profile lookup
- `drm.rs` — connected outputs from sysfs, connector → I2C bus
- `edid.rs` — vendor / name / serial from the EDID base block
- `i2c.rs` — `/dev/i2c-N`: flock + `I2C_RDWR`
- `ddc.rs` — DDC/CI Get VCP request and reply
- `colord.rs` — colord D-Bus calls
- `examples/benq-pd3220u.toml` — the shipped monitor table; a test loads it
- Hardware notes for the PD3220U: `/home/kuno/nixos/docs/benq-pd3220u-ddc.md`

## Non-negotiables

- **No libddcutil.** It is GPL-2.0-or-later and the project is MIT. DDC/CI is
  spoken directly over i2c-dev.
- **colord objects use scope "temp".** colord drops them when our bus
  connection closes, so a crash or stop leaves no stale profile that
  applications keep trusting. "normal" survives us until colord restarts.
  Consequence: nothing about colord is cached between polls; every poll
  re-finds the device and recreates what is missing (colord restarts drop
  temp objects too). Losing the bus connection exits the process so systemd
  restarts it onto a fresh one.
- **Lock the bus with flock, non-blocking, like ddcutil.** A held lock means
  skip this poll; `ddcutil getvcp SCAN` holds it for tens of seconds, and a
  blocking wait would stall every monitor behind one bus.
- **Check the feature code in the reply.** Monitors answer an unimplemented
  VCP code with a copy of their previous reply; the code byte is the only
  thing that exposes the copy.
- **Probe EDID only on the GPU's own I2C adapters.** The SMBus controllers
  carry RAM SPD EEPROMs at the same 0x50.
- **`XRANDR_name` is the connector name** (`DP-2`), not the model: colord-gtk
  looks displays up by it.
- **`rust-version` and the MSRV CI job:** the job reads the floor from
  Cargo.toml, so raising it needs no second edit — but re-run clippy after a
  raise, it gates suggestions on the floor.
