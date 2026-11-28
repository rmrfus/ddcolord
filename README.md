# ddcolord

Keeps colord's display profile in step with the monitor's OSD color mode.

Wide-gamut monitors switch between sRGB, Adobe RGB, Display P3 and other
modes from their own menu, and nothing on the computer notices. ddcolord
reads the active mode over DDC/CI every few seconds and makes the matching ICC
profile the default for that display in colord. Applications that use the
"system display profile" from colord, such as darktable, follow the monitor.

It is written for compositors that do not register displays in colord
themselves (sway and other wlroots compositors). GNOME and KDE do register
them, and running ddcolord there would add a second device for the same
output.

## How it works

- Connected outputs come from `/sys/class/drm`; each output's EDID is matched
  against the `[[monitor]]` entries of the config.
- The I2C bus is the connector's `ddc` link where the driver has one. NVIDIA's
  proprietary driver has none, so ddcolord reads the EDID at 0x50 on each I2C
  adapter of the same GPU and keeps the one that matches.
- DDC/CI Get VCP Feature over `/dev/i2c-N`, with the same `flock` ddcutil
  takes, so the two can share a bus.
- colord gets a display device per output with `XRANDR_name=<connector>`
  (what colord clients look a display up by) and the mode's profile as its
  default. Device and profiles are temporary: they vanish when ddcolord stops.

No libddcutil: DDC/CI is spoken directly, which keeps the project MIT.

## Requirements

- Read/write access to `/dev/i2c-*` (on most distributions: group `i2c`, and
  the `i2c-dev` module loaded).
- colord running on the system bus, and libdbus.
- A monitor with DDC/CI enabled in its OSD.

## Setup

1. Start it once (`ddcolord`, or the service below). With no config at
   `~/.config/ddcolord/config.toml` it writes one for the monitors connected
   right now, copies the bundled profiles into `icc/` beside it, and keeps
   running with it. It never overwrites either. With no connected monitor
   whose EDID names it (none yet, or an EDID without a monitor name), it
   waits for one; `ddcolord --probe` shows which.

   - A monitor ddcolord ships a table for (the BenQ PD3220U, see
     [`examples/`](examples/)) gets the complete table and works at once.
   - Any other monitor gets a skeleton entry with no modes. The daemon then
     logs each OSD mode switch as `color mode unlisted (sl=0x.. sh=0x..)`;
     add a `[[monitor.mode]]` for each mode you want a profile for, as
     ddcolord.toml(5) describes, and restart.

   `ddcolord --probe` shows the same identity and raw value on demand:

   ```
   card1-DP-2: vendor = "BNQ"  model = "BenQ PD3220U"  serial = "..."
     bus:  /dev/i2c-3
     0xdc: mh=0x04 ml=0x24 sh=0x00 sl=0x0a
     no [[monitor]] entry matches
   ```

   Only trust a feature the monitor declares in `ddcutil capabilities`; see
   ddcolord.toml(5), NOTES. `ddcolord --check` validates an edited config.

2. Run it as a user service:

   ```zsh
   make && sudo make install   # or: nix profile install 'github:rmrfus/ddcolord'
   systemctl --user daemon-reload
   systemctl --user enable --now ddcolord
   ```

   The unit is `WantedBy=graphical-session.target`. Upstream sway does not
   start that target; without something that does (uwsm, home-manager's sway
   module, a `systemctl --user start graphical-session.target` in the sway
   config, or similar), `--now` starts the service once and it does not come
   back at the next login.

3. Check what colord hands out:

   ```zsh
   colormgr get-devices
   journalctl --user -u ddcolord
   ```

## ICC profiles

[`examples/icc/`](examples/icc/) holds generic CC0 profiles for sRGB, Rec.709,
Adobe RGB (1998), Display P3 and DCI-P3 from
[Compact ICC Profiles](https://github.com/saucecontrol/Compact-ICC-Profiles).
They are built into the binary for the starter config, and installed to
`<prefix>/share/ddcolord/examples/icc/` beside the example. Their README says
what each assumes: the Rec.709 and DCI-P3 ones may
not match the monitor modes of the same name. A profile measured for your own
monitor is always the better choice.

## darktable

Set the display profile to "system display profile". darktable asks colord at
start, and after a mode switch picks up the new profile on its next window
event (a move, a resize, a view switch), not instantly.

## License

MIT. The ICC profiles in `examples/icc/` are CC0-1.0.
