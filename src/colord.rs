//! colord over D-Bus: a display device per output, its default profile set to
//! the ICC file the monitor's color mode calls for.
//!
//! Devices and profiles are created with scope "temp": colord drops them the
//! moment this process's bus connection closes, so a crash or a stop leaves
//! no stale profile for applications to keep trusting. The price is that a
//! colord restart drops them too, and the poll loop recreates whatever is
//! missing on its next round: nothing is cached across rounds for that reason.
//!
//! Applications find the device by metadata, not by ID: colord-gtk (darktable)
//! calls `FindDeviceByProperty("XRANDR_name", <connector>)`.

use std::collections::HashMap;
use std::fs::File;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use dbus::Path as ObjPath;
use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;
use dbus::blocking::{Connection, Proxy};

const NAME: &str = "org.freedesktop.ColorManager";
const ROOT: &str = "/org/freedesktop/ColorManager";
const MANAGER: &str = "org.freedesktop.ColorManager";
const DEVICE: &str = "org.freedesktop.ColorManager.Device";
const NOT_FOUND: &str = "org.freedesktop.ColorManager.NotFound";
const DISCONNECTED: &str = "org.freedesktop.DBus.Error.Disconnected";
const TIMEOUT: Duration = Duration::from_secs(5);

pub type Obj = ObjPath<'static>;

/// What colord is told about a display.
pub struct Display<'a> {
    /// The output's sysfs name, for the device ID.
    pub output: &'a str,
    /// The connector name, for `XRANDR_name`.
    pub connector: &'a str,
    pub vendor: &'a str,
    pub model: &'a str,
    pub serial: Option<&'a str>,
}

pub struct Colord {
    conn: Connection,
}

/// True when the error means the bus connection itself is gone.
pub fn is_disconnect(e: &anyhow::Error) -> bool {
    e.chain()
        .filter_map(|c| c.downcast_ref::<dbus::Error>())
        .any(|d| d.name() == Some(DISCONNECTED))
}

/// From the sysfs name (`ddcolord-card1-DP-2`), not the connector, which
/// two GPUs can share.
pub fn device_id(output: &str) -> String {
    format!("ddcolord-{output}")
}

impl Colord {
    pub fn connect() -> Result<Colord> {
        let conn = Connection::new_system().context("connecting to the system bus")?;
        Ok(Colord { conn })
    }

    fn proxy<'a>(&'a self, path: &'a Obj) -> Proxy<'a, &'a Connection> {
        self.conn.with_proxy(NAME, path.clone(), TIMEOUT)
    }

    fn manager(&self) -> Proxy<'_, &Connection> {
        self.conn.with_proxy(NAME, ROOT, TIMEOUT)
    }

    /// A Find* call's reply, with colord's NotFound error as `None`.
    fn found(got: Result<(Obj,), dbus::Error>, method: &str) -> Result<Option<Obj>> {
        match got {
            Ok((path,)) => Ok(Some(path)),
            Err(e) if e.name() == Some(NOT_FOUND) => Ok(None),
            Err(e) => Err(e).with_context(|| format!("colord {method}")),
        }
    }

    pub fn device(&self, output: &str) -> Result<Option<Obj>> {
        let id = device_id(output);
        let got = self
            .manager()
            .method_call(MANAGER, "FindDeviceById", (id.as_str(),));
        Self::found(got, "FindDeviceById")
    }

    /// Whatever device colord-gtk would hand an application for this output.
    pub fn device_for_xrandr(&self, connector: &str) -> Result<Option<Obj>> {
        let got =
            self.manager()
                .method_call(MANAGER, "FindDeviceByProperty", ("XRANDR_name", connector));
        Self::found(got, "FindDeviceByProperty")
    }

    pub fn create_device(&self, d: &Display<'_>) -> Result<Obj> {
        let mut props = HashMap::from([
            ("Kind", "display"),
            ("Mode", "physical"),
            ("Colorspace", "rgb"),
            ("Vendor", d.vendor),
            ("Model", d.model),
            ("XRANDR_name", d.connector),
        ]);
        if let Some(s) = d.serial {
            props.insert("Serial", s);
        }
        let id = device_id(d.output);
        let (path,): (Obj,) = self
            .manager()
            .method_call(MANAGER, "CreateDevice", (id.as_str(), "temp", props))
            .with_context(|| format!("colord CreateDevice {id}"))?;
        Ok(path)
    }

    pub fn delete_device(&self, path: &Obj) -> Result<()> {
        self.manager()
            .method_call::<(), _, _, _>(MANAGER, "DeleteDevice", (path.clone(),))
            .with_context(|| format!("colord DeleteDevice {path}"))
    }

    /// The colord profile for an ICC file, registering it if colord has none.
    /// The file goes over as an fd, so colord need not be able to open the
    /// path itself (a profile under a 0700 home directory).
    pub fn profile(&self, file: &Path) -> Result<Obj> {
        let Some(name) = file.to_str() else {
            anyhow::bail!("ICC path {} is not UTF-8", file.display());
        };
        let got = self
            .manager()
            .method_call(MANAGER, "FindProfileByFilename", (name,));
        if let Some(p) = Self::found(got, "FindProfileByFilename")? {
            return Ok(p);
        }
        let fd: OwnedFd = File::open(file)
            .with_context(|| format!("opening {name}"))?
            .into();
        let id = profile_id(file);
        let props = HashMap::from([("Filename", name)]);
        let (path,): (Obj,) = self
            .manager()
            .method_call(
                MANAGER,
                "CreateProfileWithFd",
                (id.as_str(), "temp", fd, props),
            )
            .with_context(|| format!("colord CreateProfileWithFd {name}"))?;
        Ok(path)
    }

    pub fn default_profile(&self, device: &Obj) -> Result<Option<Obj>> {
        let profiles: Vec<Obj> = self
            .proxy(device)
            .get(DEVICE, "Profiles")
            .with_context(|| format!("colord {device} Profiles"))?;
        Ok(profiles.into_iter().next())
    }

    pub fn make_default(&self, device: &Obj, profile: &Obj) -> Result<()> {
        let p = self.proxy(device);
        let profiles: Vec<Obj> = p
            .get(DEVICE, "Profiles")
            .with_context(|| format!("colord {device} Profiles"))?;
        if !profiles.contains(profile) {
            p.method_call::<(), _, _, _>(DEVICE, "AddProfile", ("hard", profile.clone()))
                .with_context(|| format!("colord AddProfile {profile}"))?;
        }
        p.method_call::<(), _, _, _>(DEVICE, "MakeProfileDefault", (profile.clone(),))
            .with_context(|| format!("colord MakeProfileDefault {profile}"))
    }
}

/// Readable and stable per path: the file stem plus an FNV-1a hash of the
/// whole path, so two `sRGB.icc` in different directories do not collide.
fn profile_id(file: &Path) -> String {
    let hash = file
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0x811c_9dc5_u32, |h, b| {
            (h ^ u32::from(*b)).wrapping_mul(0x0100_0193)
        });
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    format!("ddcolord-{stem}-{hash:08x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_file_name_in_two_directories_gets_two_profile_ids() {
        let a = profile_id(Path::new("/a/sRGB.icc"));
        let b = profile_id(Path::new("/b/sRGB.icc"));
        assert_ne!(a, b);
        assert!(a.starts_with("ddcolord-sRGB-"), "{a}");
        assert_eq!(a, profile_id(Path::new("/a/sRGB.icc")));
    }
}
