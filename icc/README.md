# ICC profiles

Generic profiles for the shipped example config, from
[saucecontrol/Compact-ICC-Profiles](https://github.com/saucecontrol/Compact-ICC-Profiles)
at commit `bdd84663061bc4ae95ca70decff54f581e27f702`, unmodified. They are
CC0-1.0; see [LICENSE](LICENSE). A profile measured for your own monitor will
always be closer than any of these.

| File                 | Primaries        | White point          | Transfer curve      | sha256                                                             |
| -------------------- | ---------------- | -------------------- | ------------------- | ------------------------------------------------------------------ |
| `sRGB-v4.icc`        | sRGB             | D65                  | sRGB                | `c56e1685d888f5edb92fe07f2750f387f8fe8e91b32ff8fb0b56bfbbb9458353` |
| `Rec709-v4.icc`      | Rec. 709         | D65                  | Rec. 709 (camera)   | `cf2a17aedc5fab20b207ee7bc42cea1d0bbf6f9f089a93973e370b2d45f7e700` |
| `AdobeCompat-v4.icc` | Adobe RGB (1998) | D65                  | gamma 2.2           | `1e35b53d118eba6835a7bac06137ea87cd5ad6eee97b20a88b29ab6356b00e43` |
| `DisplayP3-v4.icc`   | DCI-P3           | D65                  | sRGB                | `cb51de38e482ee974c0c76b9689e16aad04bad16e226fed2f30c842d15ff3a3d` |
| `DCI-P3-v4.icc`      | DCI-P3           | DCI (0.314, 0.351)   | gamma 2.6           | `58ac463ea778e9b70692971dd4ba9071363a03aba77043894162b1ad6c051da7` |

Two of them may not match what a monitor mode of the same name does:

- `Rec709-v4.icc` carries the Rec. 709 camera curve. Displays in a Rec. 709
  mode usually follow BT.1886 (close to gamma 2.4) instead.
- `DCI-P3-v4.icc` is theatrical DCI-P3: DCI white and gamma 2.6. A monitor's
  "DCI-P3" mode may use D65 white instead.

All five are ICC v4 with parametric curves; lcms2 (darktable, colord) reads
them.
