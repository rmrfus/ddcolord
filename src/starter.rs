//! The starter config written on first start.
//!
//! Started without `-c` and with no config at the default path, the daemon
//! writes one for the monitors connected right now instead of failing: the
//! shipped table for a monitor it has one for, a skeleton otherwise. The
//! bundled ICC profiles are copied into icc/ beside it so its relative paths
//! resolve. Copied, not referenced where they are installed: on NixOS that is
//! a store path, which an upgrade plus a garbage collection deletes from
//! under a config that still names it.
//!
//! Nothing that exists is overwritten, the config included: a profile the
//! user swapped into icc/ survives every later start.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::edid::Edid;

/// Shipped monitor tables. Each names its profiles as `icc/<file>`, and a
/// test checks every such file is in FILES.
const TABLES: &[&str] = &[include_str!("../examples/benq-pd3220u.toml")];

/// What goes into icc/ beside the config.
const FILES: &[(&str, &[u8])] = &[
    ("sRGB-v4.icc", include_bytes!("../examples/icc/sRGB-v4.icc")),
    (
        "Rec709-v4.icc",
        include_bytes!("../examples/icc/Rec709-v4.icc"),
    ),
    (
        "AdobeCompat-v4.icc",
        include_bytes!("../examples/icc/AdobeCompat-v4.icc"),
    ),
    (
        "DisplayP3-v4.icc",
        include_bytes!("../examples/icc/DisplayP3-v4.icc"),
    ),
    (
        "DCI-P3-v4.icc",
        include_bytes!("../examples/icc/DCI-P3-v4.icc"),
    ),
    ("README.md", include_bytes!("../examples/icc/README.md")),
    ("LICENSE", include_bytes!("../examples/icc/LICENSE")),
];

const HEADER: &str = "\
# ddcolord config, written on first start for the monitors connected then.
# Edit freely: ddcolord never overwrites it. Keys: ddcolord.toml(5).
# The profiles in icc/ are generic CC0 ones (icc/README.md); a profile
# measured for your own monitor is better.
";

/// The starter text for these outputs (connector, EDID). `None` when no
/// entry could match any of them: an EDID without a monitor name.
///
/// Two monitors of one model would get two identical entries, and the
/// second would never apply (the first match wins). So a model connected
/// more than once gets `serial` in each entry, and outputs whose identity is
/// identical down to the serial share one entry: nothing can tell them apart.
pub fn render(outputs: &[(&str, &Edid)]) -> Option<String> {
    let mut body = String::new();
    // Entries with a serial first: one without matches any unit of its
    // model, so placed earlier it would take its twin's output too.
    let mut ordered = outputs.to_vec();
    ordered.sort_by_key(|(_, e)| e.serial.is_none());
    let mut done: Vec<(&str, &Edid)> = Vec::new();
    for (conn, e) in &ordered {
        let Some(model) = &e.name else { continue };
        if let Some((first, _)) = done.iter().find(|(_, d)| **d == **e) {
            let why = if e.serial.is_some() {
                "the same vendor, model and serial"
            } else {
                "the same vendor and model, and no serial to tell them apart"
            };
            body.push_str(&format!(
                "\n# {conn}: {why} as {first}, whose entry covers it too.\n"
            ));
            continue;
        }
        let twins = outputs
            .iter()
            .filter(|(_, o)| o.vendor == e.vendor && o.name == e.name)
            .count();
        let serial = match (&e.serial, twins > 1) {
            (Some(s), true) => format!("serial = {}\n", toml_string(s)),
            _ => String::new(),
        };
        body.push('\n');
        match table_for(e) {
            Some(table) => {
                body.push_str(&format!("# {conn}: the shipped table for this monitor.\n"));
                // Keys of a table may come in any order; right after the header
                // needs no knowledge of the table's own layout.
                body.push_str(&table.replacen(
                    "[[monitor]]\n",
                    &format!("[[monitor]]\n{serial}"),
                    1,
                ));
            }
            None => body.push_str(&skeleton(conn, &e.vendor, model, &serial)),
        }
        done.push((conn, e));
    }
    if body.is_empty() {
        return None;
    }
    Some(format!("{HEADER}\ninterval = 3\n{body}"))
}

/// The `[[monitor]]` part of the shipped table matching this EDID; the
/// header and `interval` above it are the starter's own.
fn table_for(e: &Edid) -> Option<&'static str> {
    TABLES.iter().find_map(|t| {
        let cfg: Config = toml::from_str(t).ok()?;
        cfg.monitor_for(e)?;
        t.find("[[monitor]]").map(|i| &t[i..])
    })
}

/// `feature` is written out, not left to its default: 0xDC is only a guess
/// for a monitor nobody has looked at, and an undeclared code can answer with
/// echoed values that look real (see ddc.rs).
fn skeleton(conn: &str, vendor: &str, model: &str, serial: &str) -> String {
    format!(
        "# {conn}: no shipped table for this monitor. As it stands, the daemon logs
# every OSD mode switch as \"color mode unlisted (sl=.. sh=..)\"; add a
# [[monitor.mode]] for each mode you want a profile for.
[[monitor]]
vendor = {vendor}
model = {model}
{serial}# The VCP code that reports the color mode. 0xDC is the MCCS \"Display
# Mode\" and only a guess for this monitor: check that `ddcutil capabilities`
# lists it. An undeclared code may answer with stale values that look real,
# or not move at all when you switch modes.
feature = 0xdc
# unmapped = \"icc/sRGB-v4.icc\"

# [[monitor.mode]]
# code = 0x0a
# name = \"sRGB\"
# icc = \"icc/sRGB-v4.icc\"
",
        vendor = toml_string(vendor),
        model = toml_string(model),
    )
}

/// A quoted, escaped TOML string: EDID text is the monitor's, not ours.
fn toml_string(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// Write the bundled files missing from icc/ beside `path`, then the config.
/// Directories are created 0700, files 0600.
pub fn write(path: &Path, text: &str) -> Result<()> {
    let dir = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    let icc = dir.join("icc");
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&icc)
        .with_context(|| format!("creating {}", icc.display()))?;
    for (name, bytes) in FILES {
        create_new(&icc.join(name), bytes)?;
    }
    if !create_new(path, text.as_bytes())? {
        anyhow::bail!("{} appeared while the starter was written", path.display());
    }
    Ok(())
}

/// Write `bytes` to `path` unless it already exists; `false` if it did. The
/// data goes to a temp file in the same directory first and is then
/// hard-linked into place: unlike rename, link refuses to replace, so a crash
/// leaves no half-written file and a concurrent writer loses nothing.
fn create_new(path: &Path, bytes: &[u8]) -> Result<bool> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp{}", std::process::id()));
    let tmp = Path::new(&tmp);
    // Only the link may answer AlreadyExists for "the file is there": a temp
    // left by a crashed run under a reused PID would otherwise read the same
    // way and skip a file the config then names.
    let linked = write_temp(tmp, bytes)
        .with_context(|| format!("writing {}", tmp.display()))
        .map(|()| fs::hard_link(tmp, path));
    if let Err(e) = fs::remove_file(tmp)
        && e.kind() != io::ErrorKind::NotFound
    {
        log::warn!("leaving {}: {e}", tmp.display());
    }
    match linked? {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e).with_context(|| format!("linking {}", path.display())),
    }
}

/// A fresh 0600 file with `bytes`, synced. A stale temp of the same name is
/// replaced: nothing but a dead run of ours writes that name.
fn write_temp(tmp: &Path, bytes: &[u8]) -> io::Result<()> {
    let open = || {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(tmp)
    };
    let mut f = match open() {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            fs::remove_file(tmp)?;
            open()?
        }
        other => other?,
    };
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Choice;

    fn edid(vendor: &str, name: Option<&str>) -> Edid {
        Edid {
            vendor: vendor.into(),
            name: name.map(Into::into),
            serial: Some("X1".into()),
        }
    }

    /// A fresh directory for one test under the temp dir, its name not
    /// guessable in advance (pid plus clock).
    fn scratch(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        std::env::temp_dir().join(format!("ddcolord-{name}-{}-{nanos}", std::process::id()))
    }

    fn with_serial(serial: &str) -> Edid {
        Edid {
            serial: Some(serial.into()),
            ..edid("BNQ", Some("BenQ PD3220U"))
        }
    }

    #[test]
    fn two_monitors_of_one_model_get_one_entry_each_told_apart_by_serial() {
        let (a, b) = (with_serial("A1"), with_serial("B2"));
        let text = render(&[("DP-2", &a), ("DP-3", &b)]).unwrap();
        let c: Config = toml::from_str(&text).unwrap();
        assert_eq!(c.monitor.len(), 2);
        assert_eq!(c.monitor_for(&a).unwrap().serial.as_deref(), Some("A1"));
        assert_eq!(c.monitor_for(&b).unwrap().serial.as_deref(), Some("B2"));
    }

    #[test]
    fn monitors_identical_down_to_the_serial_share_one_entry() {
        let a = with_serial("A1");
        let text = render(&[("DP-2", &a), ("DP-3", &a)]).unwrap();
        let c: Config = toml::from_str(&text).unwrap();
        assert_eq!(c.monitor.len(), 1);
        assert!(
            text.contains("# DP-3: the same vendor, model and serial"),
            "{text}"
        );
    }

    #[test]
    fn a_twin_without_a_serial_does_not_shadow_the_one_with_it_in_either_order() {
        let none = Edid {
            serial: None,
            ..with_serial("unused")
        };
        let s = with_serial("S");
        for outputs in [
            [("DP-1", &none), ("DP-2", &s)],
            [("DP-2", &s), ("DP-1", &none)],
        ] {
            let text = render(&outputs).unwrap();
            let c: Config = toml::from_str(&text).unwrap();
            c.validate().unwrap();
            assert_eq!(c.monitor.len(), 2, "{text}");
            assert_eq!(c.monitor_for(&s).unwrap().serial.as_deref(), Some("S"));
            assert_eq!(c.monitor_for(&none).unwrap().serial, None);
        }
    }

    #[test]
    fn twins_without_serials_get_one_entry_that_loads() {
        let none = Edid {
            serial: None,
            ..with_serial("unused")
        };
        let text = render(&[("DP-1", &none), ("DP-2", &none)]).unwrap();
        let c: Config = toml::from_str(&text).unwrap();
        c.validate().unwrap();
        assert_eq!(c.monitor.len(), 1);
        assert!(
            text.contains("# DP-2: the same vendor and model, and no serial"),
            "{text}"
        );
    }

    #[test]
    fn a_skeleton_spells_out_the_feature_it_guesses() {
        let text = render(&[("DP-1", &edid("XYZ", Some("M")))]).unwrap();
        assert!(text.contains("\nfeature = 0xdc\n"), "{text}");
        assert!(text.contains("ddcutil capabilities"), "{text}");
    }

    #[test]
    fn a_stale_temp_from_a_dead_run_does_not_skip_the_file() {
        let dir = scratch("stale");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f");
        let stale = dir.join(format!("f.tmp{}", std::process::id()));
        fs::write(&stale, b"old").unwrap();
        assert!(create_new(&path, b"new").unwrap());
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert!(!stale.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_known_monitor_gets_its_full_shipped_table() {
        let e = edid("BNQ", Some("BenQ PD3220U"));
        let text = render(&[("DP-2", &e)]).unwrap();
        let c: Config = toml::from_str(&text).unwrap();
        assert_eq!(c.monitor.len(), 1);
        assert!(c.monitor[0].mode.len() > 10);
        assert!(text.contains("# DP-2: the shipped table"), "{text}");
    }

    #[test]
    fn an_unknown_monitor_gets_a_skeleton_that_keeps_the_profile() {
        let e = edid("XYZ", Some("Odd \"Model\" \\ 27"));
        let text = render(&[("HDMI-A-1", &e)]).unwrap();
        let c: Config = toml::from_str(&text).unwrap();
        let m = c.monitor_for(&e).unwrap();
        assert!(m.mode.is_empty());
        assert_eq!(m.choice(0x0a, 0), Choice::Keep);
    }

    #[test]
    fn a_monitor_without_a_name_cannot_be_matched_so_nothing_is_rendered() {
        assert!(render(&[("DP-1", &edid("XYZ", None))]).is_none());
    }

    #[test]
    fn every_profile_a_shipped_table_names_is_bundled() {
        for t in TABLES {
            let c: Config = toml::from_str(t).unwrap();
            let m = &c.monitor[0];
            let named = m
                .mode
                .iter()
                .filter_map(|mode| mode.icc.as_deref())
                .chain(m.unmapped.as_deref());
            for p in named {
                let file = p.strip_prefix("icc").unwrap().to_str().unwrap();
                assert!(
                    FILES.iter().any(|(n, _)| *n == file),
                    "{file} is not bundled"
                );
            }
        }
    }

    #[test]
    fn the_written_starter_loads_and_is_never_overwritten() {
        let dir = scratch("starter");
        let path = dir.join("ddcolord").join("config.toml");
        let e = edid("BNQ", Some("BenQ PD3220U"));
        let text = render(&[("DP-2", &e)]).unwrap();
        write(&path, &text).unwrap();
        let c = Config::load(&path).unwrap();
        assert!(matches!(c.monitor[0].choice(0x22, 0), Choice::Profile(_)));

        // A user's edits, and a profile they swapped in, survive a rerun.
        fs::write(&path, "# mine\n").unwrap();
        let swapped = path.with_file_name("icc").join("sRGB-v4.icc");
        fs::write(&swapped, b"mine").unwrap();
        assert!(write(&path, &text).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "# mine\n");
        assert_eq!(fs::read(&swapped).unwrap(), b"mine");
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        fs::remove_dir_all(&dir).unwrap();
    }
}
