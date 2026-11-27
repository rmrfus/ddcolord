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

1. Find the monitor's identity and the raw value of its current mode:

   ```zsh
   ddcolord --probe
   ```

   ```
   DP-2: vendor = "BNQ"  model = "BenQ PD3220U"  serial = "..."
     bus:  /dev/i2c-3
     0xdc: mh=0x04 ml=0x24 sh=0x00 sl=0x0a
     no [[monitor]] entry matches
   ```

   Switch modes in the OSD and probe again to map each mode's `sl` (and `sh`
   for sub-modes).

2. Write `~/.config/ddcolord/config.toml`; ddcolord.toml(5) describes every
   key. [`examples/benq-pd3220u.toml`](examples/benq-pd3220u.toml) is a complete
   table for the BenQ PD3220U. Check it with `ddcolord --check`.

3. Run it as a user service:

   ```zsh
   make && sudo make install   # or: nix profile install 'github:rmrfus/ddcolord'
   systemctl --user daemon-reload
   systemctl --user enable --now ddcolord
   ```

4. Check what colord hands out:

   ```zsh
   colormgr get-devices
   journalctl --user -u ddcolord
   ```

## ICC profiles

colord ships sRGB, Adobe RGB (1998) and Rec.709 among others, under
`share/color/icc/colord/`. It ships no Display P3 or DCI-P3 profile; the
[Compact ICC Profiles](https://github.com/saucecontrol/Compact-ICC-Profiles)
(CC0) have both, as `DisplayP3-v4.icc` and `DCI-P3-v4.icc`.

## darktable

Set the display profile to "system display profile". darktable asks colord at
start, and after a mode switch picks up the new profile on its next window
event (a move, a resize, a view switch), not instantly.

## License

MIT
