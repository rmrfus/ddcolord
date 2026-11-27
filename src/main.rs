//! ddcolord - keep a colord display profile in step with the monitor's OSD
//! color mode, read over DDC/CI.

mod colord;
mod config;
mod ddc;
mod drm;
mod edid;
mod i2c;
mod notify_ready;

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
    /// Cached bus; dropped on any DDC failure so the next poll looks again.
    bus: Option<u32>,
    /// Last (SL, SH) logged, so a mode is logged once, not every poll.
    mode: Option<(u8, u8)>,
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

/// Connected outputs that a `[[monitor]]` entry matches.
fn matched(cfg: &Config, outputs: Vec<Output>) -> Vec<(Output, Edid, &Monitor)> {
    outputs
        .into_iter()
        .filter_map(|o| {
            let e = edid::parse(&o.edid).ok()?;
            let m = cfg.monitor_for(&e)?;
            Some((o, e, m))
        })
        .collect()
}

/// Read the mode feature. `None` when the bus is busy or the read failed;
/// failures are reported through `t`.
fn read_mode(o: &Output, m: &Monitor, t: &mut Tracked) -> Option<ddc::Vcp> {
    let conn = &o.connector;
    let n = match t.bus {
        Some(n) => n,
        None => match drm::find_bus(o) {
            BusLookup::Found(n) => {
                log::info!("{conn}: DDC/CI on /dev/i2c-{n}");
                t.bus = Some(n);
                n
            }
            BusLookup::Busy => {
                log::debug!("{conn}: an I2C bus is locked by another process, skipping");
                return None;
            }
            BusLookup::NotFound => {
                t.fail(conn, "no I2C bus returns this monitor's EDID".into());
                return None;
            }
        },
    };
    let got = Bus::open(n).and_then(|bus| match bus {
        Some(bus) => ddc::get_vcp(&bus, m.feature).map(Some),
        None => Ok(None),
    });
    match got {
        Ok(Some(v)) => Some(v),
        Ok(None) => {
            log::debug!("{conn}: /dev/i2c-{n} is locked by another process, skipping");
            None
        }
        Err(e) => {
            t.bus = None;
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
        colord.make_default(&device, &profile)?;
        log::info!("{conn}: profile {}", icc.display());
    }
    Ok(())
}

fn poll(cfg: &Config, colord: &Colord, tracked: &mut BTreeMap<String, Tracked>) -> Result<()> {
    let present = matched(cfg, drm::connected()?);

    // Outputs that went away take their colord device with them, so nothing
    // hands an application the profile of a monitor that is not there.
    let gone: Vec<String> = tracked
        .keys()
        .filter(|c| !present.iter().any(|(o, _, _)| &o.connector == *c))
        .cloned()
        .collect();
    for conn in gone {
        tracked.remove(&conn);
        log::info!("{conn}: disconnected");
        if let Some(d) = colord.device(&conn)? {
            colord.delete_device(&d)?;
        }
    }

    for (o, e, m) in &present {
        let t = tracked.entry(o.connector.clone()).or_insert_with(|| {
            log::info!("{}: {} {}", o.connector, m.vendor, m.model);
            Tracked::default()
        });
        let Some(v) = read_mode(o, m, t) else {
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
    if cfg.monitor.is_empty() {
        bail!("the config lists no [[monitor]]; see ddcolord.toml(5)");
    }
    let colord = Colord::connect()?;
    let mut tracked = BTreeMap::new();
    notify_ready::ready();
    log::info!("polling every {}s", cfg.interval);
    loop {
        // A lost bus connection would also have taken our temp devices with
        // it; exiting lets systemd restart us onto a fresh one.
        if let Err(e) = poll(cfg, &colord, &mut tracked) {
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
        out.push_str("no connected output has an EDID\n");
    }
    for o in &outputs {
        let e = match edid::parse(&o.edid) {
            Ok(e) => e,
            Err(err) => {
                out.push_str(&format!("{}: unreadable EDID: {err:#}\n", o.connector));
                continue;
            }
        };
        out.push_str(&format!(
            "{}: vendor = \"{}\"  model = \"{}\"  serial = \"{}\"\n",
            o.connector,
            e.vendor,
            e.name.as_deref().unwrap_or(""),
            e.serial.as_deref().unwrap_or(""),
        ));
        let m = cfg.and_then(|c| c.monitor_for(&e));
        let feature = m.map_or(0xdc, |m| m.feature);
        let n = match drm::find_bus(o) {
            BusLookup::Found(n) => n,
            BusLookup::Busy => {
                out.push_str("  bus:  locked by another process, try again\n");
                continue;
            }
            BusLookup::NotFound => {
                out.push_str("  bus:  none returns this EDID\n");
                continue;
            }
        };
        out.push_str(&format!("  bus:  /dev/i2c-{n}\n"));
        let v = match Bus::open(n)? {
            Some(bus) => ddc::get_vcp(&bus, feature),
            None => Err(anyhow::anyhow!("locked by another process, try again")),
        };
        match v {
            Ok(v) => out.push_str(&format!(
                "  {feature:#04x}: mh={:#04x} ml={:#04x} sh={:#04x} sl={:#04x}\n",
                v.mh, v.ml, v.sh, v.sl
            )),
            Err(err) => {
                out.push_str(&format!("  {feature:#04x}: {err:#}\n"));
                continue;
            }
        }
        let Some(m) = m else {
            out.push_str("  no [[monitor]] entry matches\n");
            continue;
        };
        let v = match v {
            Ok(v) => v,
            Err(_) => continue,
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

fn run(args: Args) -> Result<()> {
    let explicit = args.config.is_some();
    let path = config_path(args.config)?;
    if args.probe {
        // Probing is how a config gets written, so a missing default is fine.
        let cfg = if explicit || path.exists() {
            Some(Config::load(&path)?)
        } else {
            None
        };
        return probe(cfg.as_ref());
    }
    let cfg = Config::load(&path)?;
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
