//! The TOML config: which monitors to watch, and the ICC profile for each of
//! their color modes. See ddcolord.toml(5).

use std::collections::HashSet;
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

/// colord fails to parse a broken profile but keeps a half-built one under
/// that filename, which FindProfileByFilename returns on the next poll, so
/// a bad file would turn into a dead default. Opening the file proves
/// nothing: a directory opens, and so does an HTML page saved from a GitHub
/// "blob" URL.
fn check_icc(path: &Path) -> Result<()> {
    // Before opening: open(2) on a FIFO waits for a writer, and reading a
    // device such as /dev/zero never ends.
    let meta = std::fs::metadata(path).context("reading its metadata")?;
    ensure!(meta.is_file(), "not a regular file");
    let data = std::fs::read(path).context("reading it")?;
    check_icc_data(&data)
}

/// The structure of an ICC profile (ICC.1:2022, section 7): the `acsp`
/// signature at byte 36, the profile size at byte 0, and after the 128-byte
/// header a tag table whose tags all lie within that size. It catches
/// truncation, the way a profile goes bad in an interrupted copy or
/// download, which leaves the header intact.
///
/// Stricter than lcms2, colord's parser, on purpose: lcms2 clamps a size
/// past the end of the file and skips tags beyond it, which is how a
/// truncated profile comes out half-built. Bytes past the declared size are
/// ignored, as lcms2 ignores them. What lcms2 refuses that no accident
/// produces (more than 100 tags, a tag listed twice, version above 5) is
/// not checked.
fn check_icc_data(b: &[u8]) -> Result<()> {
    ensure!(
        b.get(36..40) == Some(b"acsp".as_slice()),
        "not an ICC profile (no 'acsp' signature)"
    );
    let (Some(size), Some(count)) = (be32(b, 0), be32(b, 128)) else {
        bail!(
            "truncated: {} bytes, shorter than the header and tag count",
            b.len()
        );
    };
    let len = b.len() as u64;
    ensure!(
        size <= len,
        "truncated: the header says {size} bytes, the file has {len}"
    );
    // Each entry: signature, offset and size, four bytes each.
    let end = 132 + 12 * count;
    ensure!(
        end <= size,
        "its tag table of {count} entries runs past the profile's {size} bytes"
    );
    for e in b[132..usize::try_from(end)?].as_chunks::<12>().0 {
        let offset = u64::from(u32::from_be_bytes([e[4], e[5], e[6], e[7]]));
        let tag_size = u64::from(u32::from_be_bytes([e[8], e[9], e[10], e[11]]));
        ensure!(
            offset + tag_size <= size,
            "tag '{}' lies past the profile's {size} bytes",
            String::from_utf8_lossy(&e[..4])
        );
    }
    Ok(())
}

fn be32(b: &[u8], at: usize) -> Option<u64> {
    let word: [u8; 4] = b.get(at..at + 4)?.try_into().ok()?;
    Some(u64::from(u32::from_be_bytes(word)))
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

    /// The smallest file that passes check_icc: a header with size 132 and
    /// the `acsp` signature, and an empty tag table.
    const ICC: &str = "minimal.icc";

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
        assert!(icc.ends_with("testdata/minimal.icc"), "{}", icc.display());
    }

    #[test]
    fn a_file_that_is_not_an_icc_profile_fails_at_load() {
        assert!(check_icc(&testdata().join(ICC)).is_ok());
        assert!(check_icc(&testdata().join("relative.toml")).is_err());
        assert!(check_icc(&testdata()).is_err());
    }

    const SRGB: &[u8] = include_bytes!("../examples/icc/sRGB-v4.icc");

    #[test]
    fn a_truncated_profile_with_an_intact_header_fails_at_load() {
        assert!(check_icc_data(SRGB).is_ok());
        let err = check_icc_data(&SRGB[..SRGB.len() - 1]).unwrap_err();
        assert!(err.to_string().contains("truncated"), "{err}");
        assert!(check_icc_data(&SRGB[..130]).is_err());
    }

    #[test]
    fn a_tag_past_the_declared_size_fails_at_load() {
        let mut b = SRGB.to_vec();
        // The first tag's size field.
        b[140..144].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(check_icc_data(&b).is_err());

        let mut b = SRGB.to_vec();
        // A tag count whose table cannot fit.
        b[128..132].copy_from_slice(&1000u32.to_be_bytes());
        assert!(check_icc_data(&b).is_err());
    }

    #[test]
    fn bytes_past_the_declared_size_are_ignored() {
        let mut b = SRGB.to_vec();
        b.extend_from_slice(&[0; 16]);
        assert!(check_icc_data(&b).is_ok());
    }

    /// open(2) on a FIFO blocks until a writer appears, so the check has to
    /// refuse it before opening; on a deadline, since failing means hanging.
    #[test]
    fn a_fifo_is_refused_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;
        let path = std::env::temp_dir().join(format!("ddcolord-fifo-{}.icc", std::process::id()));
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c` is a NUL-terminated path that outlives the call.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        let p = path.clone();
        std::thread::spawn(move || {
            let _ = tx.send(check_icc(&p).is_err());
        });
        let refused = rx.recv_timeout(std::time::Duration::from_secs(5));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            refused,
            Ok(true),
            "check_icc blocked on a FIFO or accepted it"
        );
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
