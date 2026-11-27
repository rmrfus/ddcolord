//! Connected DRM outputs, and the I2C bus that reaches each one's monitor.
//!
//! The connector name in sysfs (`card1-DP-2` → `DP-2`) is the name sway uses
//! for the output and the `XRANDR_name` colord clients look a display up by.
//!
//! amdgpu and i915 link the connector to its bus (`ddc`, or an `i2c-N`
//! child). NVIDIA's proprietary driver links nothing: its adapters hang off
//! the PCI device, one per port, with no hint which is which. The fallback
//! reads the EDID at 0x50 on each adapter of the same PCI device and keeps
//! the one whose EDID matches the connector's. Only that GPU's adapters are
//! probed: the SMBus controllers carry RAM SPD EEPROMs at the same 0x50.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::edid;
use crate::i2c::{Bus, Op};

const EDID_ADDR: u16 = 0x50;

#[derive(Debug, Clone)]
pub struct Output {
    pub connector: String,
    sysfs: PathBuf,
    pub edid: Vec<u8>,
}

/// What a bus lookup found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusLookup {
    Found(u32),
    /// A candidate bus was locked by another process; try again later.
    Busy,
    NotFound,
}

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
        if edid.len() < edid::BASE_LEN {
            continue;
        }
        out.push(Output {
            connector: connector.to_string(),
            sysfs,
            edid,
        });
    }
    out.sort_by(|a, b| a.connector.cmp(&b.connector));
    Ok(out)
}

pub fn find_bus(out: &Output) -> BusLookup {
    if let Some(n) = linked_bus(&out.sysfs) {
        return BusLookup::Found(n);
    }
    let mut busy = false;
    for n in sibling_buses(&out.sysfs) {
        match edid_matches(n, &out.edid) {
            Ok(Some(true)) => return BusLookup::Found(n),
            Ok(Some(false)) => {}
            Ok(None) => busy = true,
            Err(e) => log::debug!("{}: i2c-{n}: {e:#}", out.connector),
        }
    }
    if busy {
        BusLookup::Busy
    } else {
        BusLookup::NotFound
    }
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

/// i2c adapters whose parent is the PCI device behind this connector's card.
fn sibling_buses(sysfs: &Path) -> Vec<u32> {
    // card1-DP-2/device → card1; card1/device → the PCI function.
    let Ok(gpu) = fs::canonicalize(sysfs.join("device/device")) else {
        return Vec::new();
    };
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

/// `None` when the bus is locked by someone else.
fn edid_matches(n: u32, want: &[u8]) -> Result<Option<bool>> {
    let Some(bus) = Bus::open(n)? else {
        return Ok(None);
    };
    let mut got = [0u8; edid::BASE_LEN];
    bus.transfer(EDID_ADDR, &mut [Op::Write(&[0]), Op::Read(&mut got)])
        .with_context(|| format!("reading EDID at {EDID_ADDR:#04x}"))?;
    Ok(Some(want.get(..edid::BASE_LEN) == Some(&got[..])))
}
