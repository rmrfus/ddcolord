//! ddcolord - keep a colord display profile in step with the monitor's OSD
//! color mode, read over DDC/CI.

mod colord;
mod config;
mod ddc;
mod drm;
mod edid;
mod i2c;
mod notify_ready;
mod starter;

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;

use colord::Colord;
use config::{Choice, Config, Monitor};
use drm::{BusLookup, Output};
use edid::Edid;
use i2c::Bus;

#[derive(Parser)]
#[command(version, about = "Keep colord's display profile in step with the monitor's color mode", long_about = None)]
struct Args {
    /// Config to load [default: $XDG_CONFIG_HOME/ddcolord/config.toml]
    #[arg(short, long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Load the config, report problems, exit
    #[arg(long, conflicts_with = "probe")]
    check: bool,

    /// Show connected monitors, their DDC bus and color mode, then exit
    #[arg(long)]
    probe: bool,

    /// Log every poll
    #[arg(short, long)]
    debug: bool,
}

/// Per-output state that has to survive between polls.
#[derive(Debug, Default)]
struct Tracked {
    /// Cached bus, kept for as long as the connector stays connected: bus
    /// numbers belong to the port. Looking again after a failure would only
    /// find the same bus, re-read every adapter's EDID, and log it each poll.
    bus: Option<u32>,
    /// Last (SL, SH) logged, so a mode is logged once, not every poll.
    mode: Option<(u8, u8)>,
    /// Last profile logged.
    profile: Option<PathBuf>,
    /// Last error logged, so a monitor asleep for an hour logs one line.
    error: Option<String>,
}

impl Tracked {
    fn fail(&mut self, connector: &str, msg: String) {
        if self.error.as_ref() != Some(&msg) {
            log::warn!("{connector}: {msg}");
            self.error = Some(msg);
        }
    }

    fn ok(&mut self, connector: &str) {
        if self.error.take().is_some() {
            log::info!("{connector}: recovered");
        }
    }
}

fn config_path(arg: Option<PathBuf>) -> Result<PathBuf> {
    match arg {
        Some(p) => Ok(p),
        None => config::default_path(),
    }
}

/// The EDID identity in the form a `[[monitor]]` entry wants. Absent
/// descriptors are left out rather than printed as "", which would never match.
fn identity(e: &Edid) -> String {
    let mut s = format!("vendor = \"{}\"", e.vendor);
    match &e.name {
        Some(n) => s.push_str(&format!("  model = \"{n}\"")),
        None => s.push_str("  (no monitor name in the EDID, so no entry can match it)"),
    }
    if let Some(serial) = &e.serial {
        s.push_str(&format!("  serial = \"{serial}\""));
    }
    s
}

/// Read the mode feature. `None` when the bus is busy or the read failed;
/// failures are reported through `t`.
fn read_mode(o: &Output, all: &[Output], m: &Monitor, t: &mut Tracked) -> Option<ddc::Vcp> {
    let conn = &o.connector;
    let n = match t.bus {
        Some(n) => n,
        None => match drm::find_bus(o, all) {
            Ok(BusLookup::Found(n)) => {
                log::info!("{conn}: DDC/CI on /dev/i2c-{n}");
                t.bus = Some(n);
                n
            }
            Ok(BusLookup::Busy(n)) => {
                log::debug!("{conn}: /dev/i2c-{n} is locked by another process, skipping");
                return None;
            }
            Ok(BusLookup::NotFound) => {
                t.fail(conn, "no I2C bus returns this monitor's EDID".into());
                return None;
            }
            Err(e) => {
                t.fail(conn, format!("{e:#}"));
                return None;
            }
        },
    };
    let bus = match Bus::open(n) {
        Ok(Some(bus)) => bus,
        Ok(None) => {
            log::debug!("{conn}: /dev/i2c-{n} is locked by another process, skipping");
            return None;
        }
        Err(e) => {
            t.fail(conn, format!("{e:#}"));
            return None;
        }
    };
    match ddc::get_vcp(&bus, m.feature) {
        Ok(v) => Some(v),
        Err(e) => {
            t.fail(conn, format!("/dev/i2c-{n}: {e:#}"));
            None
        }
    }
}

/// Make colord's default profile for this output the one the mode asks for.
fn apply(
    colord: &Colord,
    o: &Output,
    e: &Edid,
    m: &Monitor,
    v: ddc::Vcp,
    t: &mut Tracked,
) -> Result<()> {
    let conn = &o.connector;
    if t.mode != Some((v.sl, v.sh)) {
        let name = m.mode(v.sl, v.sh).map_or("unlisted", |mode| &mode.name);
        log::info!(
            "{conn}: color mode {name} (sl={:#04x} sh={:#04x})",
            v.sl,
            v.sh
        );
        t.mode = Some((v.sl, v.sh));
    }
    let Choice::Profile(icc) = m.choice(v.sl, v.sh) else {
        return Ok(());
    };

    let device = match colord.device(conn)? {
        Some(d) => d,
        None => {
            if let Some(other) = colord.device_for_xrandr(conn)? {
                log::warn!(
                    "{conn}: colord already has {other} for XRANDR_name={conn}; \
                     applications may be handed that one instead"
                );
            }
            let d = colord.create_device(&colord::Display {
                connector: conn,
                vendor: &e.vendor,
                model: e.name.as_deref().unwrap_or(&m.model),
                serial: e.serial.as_deref(),
            })?;
            log::info!("{conn}: created colord device {d}");
            d
        }
    };
    let profile = colord.profile(icc)?;
    if colord.default_profile(&device)?.as_ref() != Some(&profile) {
        // At debug, so a second writer fighting over the default shows up.
        log::debug!("{conn}: MakeProfileDefault {profile}");
        colord.make_default(&device, &profile)?;
    }
    // Logged on change of what we asked for, not on whether colord needed
    // the call: colord re-applies remembered device-profile pairs from its
    // mapping.db when a profile is created, so it is often already default.
    if t.profile.as_deref() != Some(icc) {
        log::info!("{conn}: profile {}", icc.display());
        t.profile = Some(icc.to_path_buf());
    }
    Ok(())
}

#[derive(Debug, Default)]
struct State {
    tracked: BTreeMap<String, Tracked>,
    /// Connected outputs we do not drive, with the reason last logged; a
    /// reason is logged once, and again only when it changes. Without the
    /// line, a typo in `model` looks exactly like "unplugged".
    ignored: BTreeMap<String, String>,
}

impl State {
    fn ignore(&mut self, connector: &str, why: String) {
        if self.ignored.get(connector) != Some(&why) {
            log::info!("{connector}: {why}");
            self.ignored.insert(connector.to_string(), why);
        }
    }
}

fn poll(cfg: &Config, colord: &Colord, st: &mut State) -> Result<()> {
    let outputs = drm::connected()?;
    st.ignored
        .retain(|c, _| outputs.iter().any(|o| &o.connector == c));
    let mut present = Vec::new();
    for o in &outputs {
        match edid::parse(&o.edid) {
            Ok(e) => match cfg.monitor_for(&e) {
                Some(m) => {
                    st.ignored.remove(&o.connector);
                    present.push((o, e, m));
                }
                None => st.ignore(
                    &o.connector,
                    format!("{} matches no [[monitor]]", identity(&e)),
                ),
            },
            Err(err) => st.ignore(&o.connector, format!("unreadable EDID: {err:#}")),
        }
    }

    // Outputs that went away take their colord device with them, so nothing
    // hands an application the profile of a monitor that is not there. The
    // entry stays tracked until the delete succeeds, so a failed one is
    // retried next poll.
    let gone: Vec<String> = st
        .tracked
        .keys()
        .filter(|c| !present.iter().any(|(o, _, _)| &o.connector == *c))
        .cloned()
        .collect();
    for conn in gone {
        let deleted = colord.device(&conn).and_then(|d| match d {
            Some(d) => colord.delete_device(&d),
            None => Ok(()),
        });
        match deleted {
            Ok(()) => {
                st.tracked.remove(&conn);
                log::info!("{conn}: disconnected");
            }
            Err(e) if colord::is_disconnect(&e) => return Err(e),
            Err(e) => {
                if let Some(t) = st.tracked.get_mut(&conn) {
                    t.fail(&conn, format!("removing the colord device: {e:#}"));
                }
            }
        }
    }

    for (o, e, m) in &present {
        let t = st.tracked.entry(o.connector.clone()).or_insert_with(|| {
            log::info!("{}: {} {}", o.connector, m.vendor, m.model);
            Tracked::default()
        });
        let Some(v) = read_mode(o, &outputs, m, t) else {
            continue;
        };
        log::debug!("{}: {v:?}", o.connector);
        match apply(colord, o, e, m, v, t) {
            Ok(()) => t.ok(&o.connector),
            Err(err) if colord::is_disconnect(&err) => return Err(err),
            Err(err) => t.fail(&o.connector, format!("{err:#}")),
        }
    }
    Ok(())
}

fn daemon(cfg: &Config) -> Result<()> {
    let colord = Colord::connect()?;
    let mut st = State::default();
    notify_ready::ready();
    log::info!("polling every {}s", cfg.interval);
    loop {
        // A lost bus connection would also have taken our temp devices with
        // it; exiting lets systemd restart us onto a fresh one.
        if let Err(e) = poll(cfg, &colord, &mut st) {
            if colord::is_disconnect(&e) {
                return Err(e.context("lost the system bus"));
            }
            log::warn!("{e:#}");
        }
        sleep(Duration::from_secs(cfg.interval));
    }
}

/// Human-readable report for writing a config. Works without one.
fn probe(cfg: Option<&Config>) -> Result<()> {
    let mut out = String::new();
    let outputs = drm::connected()?;
    if outputs.is_empty() {
        out.push_str("no connected output\n");
    }
    for o in &outputs {
        let e = match edid::parse(&o.edid) {
            Ok(e) => e,
            Err(err) => {
                out.push_str(&format!("{}: unreadable EDID: {err:#}\n", o.connector));
                continue;
            }
        };
        out.push_str(&format!("{}: {}\n", o.connector, identity(&e)));
        let m = cfg.and_then(|c| c.monitor_for(&e));
        let feature = m.map_or(0xdc, |m| m.feature);
        let n = match drm::find_bus(o, &outputs) {
            Ok(BusLookup::Found(n)) => n,
            Ok(BusLookup::Busy(_)) => {
                out.push_str("  bus:  locked by another process, try again\n");
                continue;
            }
            Ok(BusLookup::NotFound) => {
                out.push_str("  bus:  none returns this EDID\n");
                continue;
            }
            Err(err) => {
                out.push_str(&format!("  bus:  {err:#}\n"));
                continue;
            }
        };
        out.push_str(&format!("  bus:  /dev/i2c-{n}\n"));
        let v = Bus::open(n).and_then(|bus| match bus {
            Some(bus) => ddc::get_vcp(&bus, feature),
            None => bail!("locked by another process, try again"),
        });
        let v = match v {
            Ok(v) => v,
            Err(err) => {
                out.push_str(&format!("  {feature:#04x}: {err:#}\n"));
                continue;
            }
        };
        out.push_str(&format!(
            "  {feature:#04x}: mh={:#04x} ml={:#04x} sh={:#04x} sl={:#04x}\n",
            v.mh, v.ml, v.sh, v.sl
        ));
        let Some(m) = m else {
            out.push_str("  no [[monitor]] entry matches\n");
            continue;
        };
        let name = m.mode(v.sl, v.sh).map_or("unlisted", |mode| &mode.name);
        let profile = match m.choice(v.sl, v.sh) {
            Choice::Profile(p) => p.display().to_string(),
            Choice::Keep => "(keep the current profile)".into(),
        };
        out.push_str(&format!("  mode: {name} -> {profile}\n"));
    }
    write_stdout(&out)
}

fn write_stdout(s: &str) -> Result<()> {
    let mut stdout = io::stdout().lock();
    match stdout.write_all(s.as_bytes()).and_then(|()| stdout.flush()) {
        Err(e) if e.kind() != io::ErrorKind::BrokenPipe => Err(e).context("writing to stdout"),
        _ => Ok(()),
    }
}

/// First start: write a config for what is connected now, rather than make
/// every new user go hunting for the format.
fn write_starter(path: &std::path::Path) -> Result<()> {
    let outputs = drm::connected()?;
    let parsed: Vec<(&str, Edid)> = outputs
        .iter()
        .filter_map(|o| Some((o.connector.as_str(), edid::parse(&o.edid).ok()?)))
        .collect();
    let refs: Vec<(&str, &Edid)> = parsed.iter().map(|(c, e)| (*c, e)).collect();
    let Some(text) = starter::render(&refs) else {
        bail!(
            "no config at {}, and no connected monitor whose EDID names it \
             to write a starter for (ddcolord --probe shows why)",
            path.display()
        );
    };
    starter::write(path, &text)?;
    // Only outputs the starter has an entry for; the others are reported by
    // the poll loop once it runs.
    let covered: Vec<String> = refs
        .iter()
        .filter_map(|(c, e)| Some(format!("{c} ({} {})", e.vendor, e.name.as_deref()?)))
        .collect();
    log::warn!(
        "no config at {}: wrote a starter for {} and running with it; \
         edit it and restart to apply changes (ddcolord.toml(5))",
        path.display(),
        covered.join(", ")
    );
    Ok(())
}

fn run(args: Args) -> Result<()> {
    let explicit = args.config.is_some();
    let path = config_path(args.config)?;
    if args.probe {
        // Probing must work before any config exists, and never writes one.
        let cfg = if explicit || path.exists() {
            Some(Config::load(&path)?)
        } else {
            None
        };
        return probe(cfg.as_ref());
    }
    // symlink_metadata, not exists(): a dangling link (dotfiles pointing at
    // a moved file) is a config the user has, and reading it should fail
    // loudly rather than have a starter collide with it.
    if !explicit && path.symlink_metadata().is_err() {
        if args.check {
            bail!(
                "no config at {}; start ddcolord once and it writes a starter",
                path.display()
            );
        }
        write_starter(&path)?;
    }
    let cfg = Config::load(&path)?;
    // Here and not in Config::load: --probe on a config still being written
    // is fine, but --check must fail on what the daemon would refuse.
    if cfg.monitor.is_empty() {
        bail!(
            "{} lists no [[monitor]]; see ddcolord.toml(5)",
            path.display()
        );
    }
    log::info!(
        "loaded {} monitor(s) from {}",
        cfg.monitor.len(),
        path.display()
    );
    if args.check {
        return Ok(());
    }
    daemon(&cfg)
}

fn main() -> ExitCode {
    let args = Args::parse();
    let default = if args.debug { "debug" } else { "info" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default)).init();
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ddcolord: {e:#}");
            ExitCode::FAILURE
        }
    }
}
