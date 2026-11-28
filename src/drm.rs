//! Connected DRM outputs, and the I2C bus that reaches each one's monitor.
//!
//! An output is known by its sysfs name (`card1-DP-2`), which is unique; the
//! connector part (`DP-2`) is the name sway uses for the output and the
//! `XRANDR_name` colord clients look a display up by, and two GPUs can each
//! have a `DP-1`.
//!
//! amdgpu and i915 link the connector to its bus (`ddc`, or an `i2c-N`
//! child). NVIDIA's proprietary driver links nothing: its adapters hang off
//! the PCI device, one per port, with no hint which is which. The fallback
//! reads the EDID at 0x50 on each adapter of the same PCI device and keeps
//! the one whose EDID matches the connector's. Only that GPU's adapters are
//! probed: the SMBus controllers carry RAM SPD EEPROMs at the same 0x50.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::edid;
use crate::i2c::{Bus, Op};

const EDID_ADDR: u16 = 0x50;

#[derive(Debug, Clone)]
pub struct Output {
    /// The sysfs entry, `card1-DP-2`: unique among outputs, so it keys
    /// everything per output and names it in logs.
    pub name: String,
    /// `DP-2`, which another card can have too.
    pub connector: String,
    sysfs: PathBuf,
    pub edid: Vec<u8>,
}

/// What a bus lookup found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusLookup {
    Found(u32),
    /// No bus matched, and this one was locked by another process, so the
    /// lookup decided nothing; try again later.
    Busy(u32),
    NotFound,
}

/// Connected outputs, including those whose EDID is missing or short (a KVM
/// that does not pass it through): callers report those rather than lose
/// them without a word.
pub fn connected() -> Result<Vec<Output>> {
    let mut out = Vec::new();
    let dir = Path::new("/sys/class/drm");
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // card1-DP-2; render nodes and bare cards have no dash.
        let Some((card, connector)) = name.split_once('-') else {
            continue;
        };
        if !card.starts_with("card") {
            continue;
        }
        let sysfs = entry.path();
        let Ok(status) = fs::read_to_string(sysfs.join("status")) else {
            continue;
        };
        if status.trim() != "connected" {
            continue;
        }
        let edid = fs::read(sysfs.join("edid")).unwrap_or_default();
        out.push(Output {
            name: name.to_string(),
            connector: connector.to_string(),
            sysfs,
            edid,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// `all` is every connected output, `out` among them. An error is a setup
/// problem (no access to /dev/i2c-N, i2c-dev not loaded, two monitors with
/// the same EDID), never "nothing on this port": an adapter with no monitor
/// behind it is just not a match.
pub fn find_bus(out: &Output, all: &[Output]) -> Result<BusLookup> {
    if let Some(n) = linked_bus(&out.sysfs) {
        return Ok(BusLookup::Found(n));
    }
    let gpu = gpu_of(&out.sysfs);
    let Some(base) = out.edid.get(..edid::BASE_LEN) else {
        return Ok(BusLookup::NotFound);
    };
    // Twins are ruled out from sysfs, before any I2C read: probing alone
    // cannot do it, because one failed read on the twin's bus would leave a
    // single "match" and bind this output to the other monitor for good.
    if let Some(twin) = all.iter().find(|o| {
        o.name != out.name && o.edid.get(..edid::BASE_LEN) == Some(base) && gpu_of(&o.sysfs) == gpu
    }) {
        bail!(
            "{} has a byte-identical EDID, and this driver does not link \
             connectors to I2C buses, so the two cannot be told apart",
            twin.name
        );
    }
    let Some(gpu) = gpu else {
        return Ok(BusLookup::NotFound);
    };
    let mut busy = None;
    let mut found = Vec::new();
    for n in sibling_buses(&gpu) {
        match edid_matches(n, base)? {
            Some(true) => found.push(n),
            Some(false) => {}
            None => busy = busy.or(Some(n)),
        }
    }
    match (found.as_slice(), busy) {
        ([n], _) => Ok(BusLookup::Found(*n)),
        ([], Some(n)) => Ok(BusLookup::Busy(n)),
        ([], None) => Ok(BusLookup::NotFound),
        // Twins were excluded above; this is an EDID on a bus whose connector
        // DRM does not report as connected.
        (many, _) => bail!(
            "this EDID answers on {}",
            many.iter()
                .map(|n| format!("i2c-{n}"))
                .collect::<Vec<_>>()
                .join(" and ")
        ),
    }
}

/// The PCI function behind a connector: card1-DP-2/device is card1, and
/// card1/device is the GPU.
fn gpu_of(sysfs: &Path) -> Option<PathBuf> {
    fs::canonicalize(sysfs.join("device/device")).ok()
}

/// `ddc` symlink, or an `i2c-N` child (DP AUX channels).
fn linked_bus(sysfs: &Path) -> Option<u32> {
    if let Ok(target) = fs::read_link(sysfs.join("ddc"))
        && let Some(n) = target
            .file_name()
            .and_then(|f| f.to_str())
            .and_then(bus_number)
    {
        return Some(n);
    }
    fs::read_dir(sysfs)
        .ok()?
        .filter_map(|e| e.ok()?.file_name().to_str().and_then(bus_number))
        .min()
}

fn bus_number(name: &str) -> Option<u32> {
    name.strip_prefix("i2c-")?.parse().ok()
}

/// i2c adapters whose parent is the given PCI device.
fn sibling_buses(gpu: &Path) -> Vec<u32> {
    let Ok(entries) = fs::read_dir("/sys/bus/i2c/devices") else {
        return Vec::new();
    };
    let mut buses: Vec<u32> = entries
        .filter_map(|e| {
            let e = e.ok()?;
            let n = bus_number(e.file_name().to_str()?)?;
            let real = fs::canonicalize(e.path()).ok()?;
            (real.parent()? == gpu).then_some(n)
        })
        .collect();
    buses.sort_unstable();
    buses
}

/// `None` when the bus is locked by someone else. Failing to open the bus is
/// an error. A failed EDID read counts as "no monitor here": an empty port
/// fails exactly that way, and since twins are excluded before probing, a
/// transient failure on the right bus only makes this lookup come back
/// NotFound, to be retried next poll.
fn edid_matches(n: u32, want: &[u8]) -> Result<Option<bool>> {
    let Some(bus) = Bus::open(n)? else {
        return Ok(None);
    };
    let mut got = [0u8; edid::BASE_LEN];
    if let Err(e) = bus.transfer(EDID_ADDR, &mut [Op::Write(&[0]), Op::Read(&mut got)]) {
        log::debug!("i2c-{n}: no EDID at {EDID_ADDR:#04x}: {e}");
        return Ok(Some(false));
    }
    Ok(Some(want == got))
}
