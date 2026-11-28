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
- Live check: `cargo run -- --probe` reads EDID, bus and mode without
  touching colord; `colormgr get-devices` shows a `ddcolord-card1-DP-2`
  device only while the daemon runs

## Layout

- `main.rs` — CLI, poll loop, `--probe`
- `config.rs` — TOML config, mode → profile lookup
- `drm.rs` — connected outputs from sysfs, connector → I2C bus
- `edid.rs` — vendor / name / serial from the EDID base block
- `i2c.rs` — `/dev/i2c-N`: flock + `I2C_RDWR`
- `ddc.rs` — DDC/CI Get VCP request and reply
- `colord.rs` — colord D-Bus calls
- `starter.rs` — the first-start config: shipped table or skeleton, plus
  the bundled profiles, written without ever overwriting
- `examples/benq-pd3220u.toml` — the shipped monitor table, also embedded as
  a starter table; tests load it, so its `icc/` paths must resolve
- `examples/icc/` — vendored CC0 profiles, unmodified, pinned commit and
  sha256 in its README; embedded in the binary and installed under
  `share/ddcolord/examples/icc/`
- `testdata/` — config test fixtures; `minimal.icc` is the smallest file
  the ICC check accepts: a 128-byte header declaring 132 bytes, then a tag
  count of zero
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
- **Only declared VCP features can be trusted, and the code cannot tell.**
  Monitors may answer an unimplemented code with the values of their previous
  reply; the PD3220U writes the requested code into the copy, so the code
  check in ddc.rs passes it. A config `feature` must be one the monitor
  declares (`ddcutil capabilities`).
- **Probe EDID only on the GPU's own I2C adapters.** The SMBus controllers
  carry RAM SPD EEPROMs at the same 0x50.
- **Rule out twin EDIDs from sysfs before probing, not from probe results.**
  A failed 0x50 read must count as "no monitor" (that is what an empty port
  looks like), so one transient NAK on the twin's bus would leave a single
  match and bind the output to the other monitor for good.
- **The bus cache lives as long as the monitor on the output.** Re-looking-up
  after a failure finds the same bus and logs it every poll.
- **Per-output state belongs to one monitor (its EDID identity).** Another
  monitor on the same output — a laptop resumed at another desk — drops the
  state and the colord device first; otherwise it inherits the old device's
  metadata, and in a mode that keeps the current profile, the old monitor's
  profile.
- **`Monitor` and `Mode` carry `deny_unknown_fields` but no container-level
  `#[serde(default)]`,** against the rust skill's rule. Their identity fields
  are required: with a container default, a missing `vendor` or `model`
  becomes "" and silently matches no monitor, ever.
- **ICC files are checked at load: the `acsp` signature, a declared size the
  file reaches, and a tag table inside it.** colord keeps a half-built
  profile for a file it failed to parse, and FindProfileByFilename hands it
  back on the next poll, so a bad file would become a dead default that
  looks like success. The signature alone passes a truncated copy, whose
  header is intact. Tag contents are not checked: that is lcms2's job, and
  it would add a C dependency to catch files nobody produces by accident.
- **The starter config copies the bundled profiles; it never points into
  the install prefix.** On NixOS that is a store path, which an upgrade plus
  a GC deletes under a config still naming it, and the daemon then refuses
  to start. It also never overwrites a file, so a profile the user swapped
  into icc/ survives.
- **ICC paths are absolute before they reach colord.** colord matches a
  relative filename by basename, so `sRGB.icc` would bind colord's own copy.
- **`XRANDR_name` is the connector name** (`DP-2`), not the model: colord-gtk
  looks displays up by it. **Everything else per output is keyed by the
  sysfs name** (`card1-DP-2`): state, logs, the colord device ID. Two GPUs
  can each have a `DP-1`, and keyed by connector their monitors would share
  one bus and one device.
- **`rust-version` and the MSRV CI job:** the job reads the floor from
  Cargo.toml, so raising it needs no second edit — but re-run clippy after a
  raise, it gates suggestions on the floor.
