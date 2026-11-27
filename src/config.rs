//! The TOML config: which monitors to watch, and the ICC profile for each of
//! their color modes. See ddcolord.toml(5).

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

use crate::edid::Edid;

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Seconds between polls.
    pub interval: u64,
    pub monitor: Vec<Monitor>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            interval: d_interval(),
            monitor: Vec::new(),
        }
    }
}

fn d_interval() -> u64 {
    3
}

// Monitor and Mode have required fields, so no container-level default: a
// missing `vendor` must be a load error, not an empty string matching nothing.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Monitor {
    /// EDID PNP manufacturer ID, e.g. "BNQ".
    pub vendor: String,
    /// EDID monitor name, e.g. "BenQ PD3220U".
    pub model: String,
    /// EDID serial string; only needed to tell identical monitors apart.
    #[serde(default)]
    pub serial: Option<String>,
    /// VCP feature that reports the color mode.
    #[serde(default = "d_feature")]
    pub feature: u8,
    /// Profile for modes that have no `icc` of their own, or are not listed.
    /// Unset: such a mode leaves the current profile in place.
    #[serde(default)]
    pub unmapped: Option<PathBuf>,
    #[serde(default)]
    pub mode: Vec<Mode>,
}

fn d_feature() -> u8 {
    0xdc
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mode {
    /// Low byte (SL) of the feature's value.
    pub code: u8,
    /// High byte (SH); unset matches any.
    #[serde(default)]
    pub sub: Option<u8>,
    pub name: String,
    #[serde(default)]
    pub icc: Option<PathBuf>,
}

/// What a mode asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice<'a> {
    Profile(&'a Path),
    Keep,
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        cfg.validate()?;
        // Absolute, or `-c config.toml` leaves the ICC paths relative too, and
        // colord matches a relative filename by basename alone: `sRGB.icc`
        // would bind colord's own sRGB.icc instead of the user's file.
        let abs =
            std::path::absolute(path).with_context(|| format!("resolving {}", path.display()))?;
        cfg.resolve(abs.parent().unwrap_or(Path::new("/")))?;
        Ok(cfg)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(self.interval >= 1, "interval must be at least 1 second");
        for m in &self.monitor {
            ensure!(
                !m.vendor.is_empty() && !m.model.is_empty(),
                "a [[monitor]] has an empty vendor or model, which no EDID matches"
            );
            let label = format!("monitor {} {}", m.vendor, m.model);
            ensure!(
                m.serial.as_deref() != Some(""),
                "{label}: serial = \"\" matches no EDID; remove the key to match any serial"
            );
            let mut seen = HashSet::new();
            for mode in &m.mode {
                ensure!(
                    seen.insert((mode.code, mode.sub)),
                    "{label}: mode {:#04x} sub {:?} is listed twice",
                    mode.code,
                    mode.sub
                );
            }
        }
        Ok(())
    }

    /// Relative ICC paths are relative to the config file's directory; every
    /// profile must check out now, not at the first mode switch an hour later.
    fn resolve(&mut self, base: &Path) -> Result<()> {
        for m in &mut self.monitor {
            let label = format!("monitor {} {}", m.vendor, m.model);
            let icc = m
                .mode
                .iter_mut()
                .filter_map(|mode| mode.icc.as_mut())
                .chain(m.unmapped.as_mut());
            for p in icc {
                if p.is_relative() {
                    *p = base.join(&*p);
                }
                check_icc(p).with_context(|| format!("{label}: ICC profile {}", p.display()))?;
            }
        }
        Ok(())
    }

    pub fn monitor_for(&self, edid: &Edid) -> Option<&Monitor> {
        self.monitor.iter().find(|m| {
            m.vendor == edid.vendor
                && Some(&m.model) == edid.name.as_ref()
                && (m.serial.is_none() || m.serial == edid.serial)
        })
    }
}

impl Monitor {
    /// The mode entry for a feature value: an exact sub-mode match first, then
    /// an entry without `sub`.
    pub fn mode(&self, sl: u8, sh: u8) -> Option<&Mode> {
        let at = |sub| self.mode.iter().find(|m| m.code == sl && m.sub == sub);
        at(Some(sh)).or_else(|| at(None))
    }

    pub fn choice(&self, sl: u8, sh: u8) -> Choice<'_> {
        let own = self.mode(sl, sh).and_then(|m| m.icc.as_deref());
        match own.or(self.unmapped.as_deref()) {
            Some(p) => Choice::Profile(p),
            None => Choice::Keep,
        }
    }
}

/// The ICC header carries the signature `acsp` at byte 36 (ICC.1:2022, 7.2.9).
/// Opening the file is not enough: a directory opens, and so does an HTML
/// page saved from a GitHub "blob" URL. colord then fails to parse it, but
/// keeps a half-built profile under that filename which FindProfileByFilename
/// returns on the next poll, so the failure would turn into a dead default.
fn check_icc(path: &Path) -> Result<()> {
    let mut header = [0u8; 40];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut header))
        .context("reading the header")?;
    ensure!(
        &header[36..] == b"acsp",
        "not an ICC profile (no 'acsp' signature)"
    );
    Ok(())
}

/// $XDG_CONFIG_HOME/ddcolord/config.toml, with the XDG fallback to ~/.config.
pub fn default_path() -> Result<PathBuf> {
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    let base = match xdg {
        Some(p) => p,
        None => match std::env::var_os("HOME").filter(|h| !h.is_empty()) {
            Some(h) => PathBuf::from(h).join(".config"),
            None => bail!("neither XDG_CONFIG_HOME nor HOME is set"),
        },
    };
    Ok(base.join("ddcolord").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn testdata() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
    }

    /// Parse, validate and resolve as `load` does, against testdata/.
    fn cfg(text: &str) -> Result<Config> {
        let mut c: Config = toml::from_str(text)?;
        c.validate()?;
        c.resolve(&testdata())?;
        Ok(c)
    }

    /// 128 bytes, zero but for the `acsp` signature.
    const ICC: &str = "header-only.icc";

    #[test]
    fn exact_sub_mode_wins_over_the_catch_all() {
        let c = cfg(r#"
            [[monitor]]
            vendor = "BNQ"
            model = "M"
            [[monitor.mode]]
            code = 0x13
            name = "LBL"
            [[monitor.mode]]
            code = 0x13
            sub = 4
            name = "LBL Reading"
        "#)
        .unwrap();
        let m = &c.monitor[0];
        assert_eq!(m.mode(0x13, 4).unwrap().name, "LBL Reading");
        assert_eq!(m.mode(0x13, 1).unwrap().name, "LBL");
        assert!(m.mode(0x14, 0).is_none());
    }

    #[test]
    fn a_mode_without_icc_falls_back_to_unmapped_and_otherwise_keeps() {
        let text = format!(
            r#"
            [[monitor]]
            vendor = "BNQ"
            model = "M"
            unmapped = "{ICC}"
            [[monitor.mode]]
            code = 0x20
            name = "Darkroom"
        "#
        );
        let c = cfg(&text).unwrap();
        let icc = testdata().join(ICC);
        assert_eq!(c.monitor[0].choice(0x20, 0), Choice::Profile(&icc));
        assert_eq!(c.monitor[0].choice(0x99, 0), Choice::Profile(&icc));

        let c = cfg(r#"
            [[monitor]]
            vendor = "BNQ"
            model = "M"
        "#)
        .unwrap();
        assert_eq!(c.monitor[0].choice(0x20, 0), Choice::Keep);
    }

    #[test]
    fn a_typoed_key_is_a_load_error_not_a_silent_default() {
        assert!(cfg("intervall = 5").is_err());
        assert!(cfg("[[monitor]]\nvendor = \"X\"\nmodel = \"Y\"\nunmaped = \"/a\"").is_err());
    }

    #[test]
    fn a_monitor_without_a_model_is_rejected() {
        assert!(cfg("[[monitor]]\nvendor = \"X\"").is_err());
    }

    #[test]
    fn a_missing_icc_file_fails_at_load() {
        let err = cfg(r#"
            [[monitor]]
            vendor = "X"
            model = "Y"
            [[monitor.mode]]
            code = 1
            name = "a"
            icc = "/nonexistent/ddcolord-test.icc"
        "#)
        .unwrap_err();
        assert!(format!("{err:#}").contains("ddcolord-test.icc"), "{err:#}");
    }

    #[test]
    fn a_duplicate_mode_is_rejected() {
        assert!(
            cfg(r#"
            [[monitor]]
            vendor = "X"
            model = "Y"
            [[monitor.mode]]
            code = 1
            name = "a"
            [[monitor.mode]]
            code = 1
            name = "b"
        "#)
            .is_err()
        );
    }

    #[test]
    fn serial_narrows_the_match_only_when_given() {
        let c = cfg(r#"
            [[monitor]]
            vendor = "BNQ"
            model = "M"
            serial = "B"
            [[monitor]]
            vendor = "BNQ"
            model = "M"
        "#)
        .unwrap();
        let e = |s: &str| Edid {
            vendor: "BNQ".into(),
            name: Some("M".into()),
            serial: Some(s.into()),
        };
        assert_eq!(c.monitor_for(&e("B")).unwrap().serial.as_deref(), Some("B"));
        assert!(c.monitor_for(&e("A")).unwrap().serial.is_none());
    }

    /// Loads it the way the daemon does, so every bundled profile in icc/ it
    /// points at must exist and carry the ICC signature.
    #[test]
    fn the_shipped_example_loads_with_its_bundled_profiles() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/benq-pd3220u.toml");
        let c = Config::load(&path).unwrap();
        let m = &c.monitor[0];
        let Choice::Profile(p3) = m.choice(0x22, 0) else {
            panic!("Display P3 should map to a profile");
        };
        assert!(p3.ends_with("icc/DisplayP3-v4.icc"), "{}", p3.display());
        // Darkroom has no profile of its own and falls back to unmapped.
        assert_eq!(m.choice(0x20, 0), m.choice(0x0a, 0));
    }

    #[test]
    fn a_relative_config_path_still_yields_absolute_icc_paths() {
        // cargo runs unit tests from the package root, so this path is
        // relative exactly the way `ddcolord -c config.toml` is.
        let c = Config::load(Path::new("testdata/relative.toml")).unwrap();
        let icc = c.monitor[0].unmapped.as_deref().unwrap();
        assert!(icc.is_absolute(), "{}", icc.display());
        assert!(
            icc.ends_with("testdata/header-only.icc"),
            "{}",
            icc.display()
        );
    }

    #[test]
    fn a_file_that_is_not_an_icc_profile_fails_at_load() {
        assert!(check_icc(&testdata().join(ICC)).is_ok());
        assert!(check_icc(&testdata().join("relative.toml")).is_err());
        assert!(check_icc(&testdata()).is_err());
    }

    #[test]
    fn an_empty_serial_is_rejected_because_it_matches_nothing() {
        let err = cfg("[[monitor]]\nvendor = \"X\"\nmodel = \"Y\"\nserial = \"\"").unwrap_err();
        assert!(err.to_string().contains("serial"), "{err}");
    }

    #[test]
    fn an_empty_model_is_rejected() {
        assert!(cfg("[[monitor]]\nvendor = \"X\"\nmodel = \"\"").is_err());
    }
}
