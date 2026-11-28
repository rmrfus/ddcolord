//! Desktop notifications over the session bus
//! (org.freedesktop.Notifications), for `notify = true`.
//!
//! A notification is a courtesy: a missing notification daemon or session
//! bus is logged and the profile switching carries on. Nothing here may
//! return an error into the poll loop, where a D-Bus Disconnected error
//! means the system bus and exits the process.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use dbus::arg::{PropMap, Variant};
use dbus::blocking::Connection;

use crate::config::Mode;

const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
// The poll loop waits on this call; notification daemons answer at once.
const TIMEOUT: Duration = Duration::from_secs(2);
// Icon Naming Specification, "Devices".
const ICON: &str = "video-display";
// Low: the user has just pressed the OSD button, so this only confirms it.
const URGENCY_LOW: u8 = 0;

/// The session bus connection, opened at the first notification, so a
/// daemon started outside a desktop session runs as before until then.
#[derive(Default)]
pub struct Notifier {
    conn: Option<Connection>,
}

impl Notifier {
    /// Show a notification, replacing the one with id `replaces` (0: none),
    /// so that stepping through modes leaves one notification, not a stack.
    /// Returns the new id; `None` after logging a failure.
    pub fn show(&mut self, replaces: u32, summary: &str, body: &str) -> Option<u32> {
        match self.call(replaces, summary, body) {
            Ok(id) => Some(id),
            Err(e) => {
                log::warn!("desktop notification: {e:#}");
                // A broken connection is reopened next time.
                self.conn = None;
                None
            }
        }
    }

    fn call(&mut self, replaces: u32, summary: &str, body: &str) -> Result<u32> {
        let conn = match &mut self.conn {
            Some(c) => c,
            None => self
                .conn
                .insert(Connection::new_session().context("connecting to the session bus")?),
        };
        let mut hints = PropMap::new();
        hints.insert("urgency".into(), Variant(Box::new(URGENCY_LOW)));
        let actions: Vec<&str> = Vec::new();
        let (id,): (u32,) = conn
            .with_proxy(NAME, PATH, TIMEOUT)
            .method_call(
                NAME,
                "Notify",
                (
                    "ddcolord", replaces, ICON, summary, body, actions, hints, -1_i32,
                ),
            )
            .context("Notify")?;
        Ok(id)
    }
}

/// Summary and body for a switch to `icc` on `connector`. `mode` is `None`
/// for an unlisted mode, which gets the `unmapped` profile.
pub fn text(model: &str, mode: Option<&Mode>, icc: &Path, connector: &str) -> (String, String) {
    let name = mode.map_or("unlisted mode", |m| &m.name);
    let file = icc.file_name().unwrap_or(icc.as_os_str()).to_string_lossy();
    (
        format!("{model}: {name}"),
        escape(&format!("{file} on {connector}")),
    )
}

/// The body may be markup (the body-markup capability; mako has it), so a
/// file named `a&b.icc` would come out garbled or not at all. The summary
/// is never markup.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(name: &str) -> Mode {
        Mode {
            code: 0x0a,
            sub: None,
            name: name.into(),
            icc: None,
        }
    }

    #[test]
    fn names_the_mode_and_the_file_not_the_path() {
        let (summary, body) = text(
            "BenQ PD3220U",
            Some(&mode("sRGB")),
            Path::new("/nix/store/x-benq/srgb.icc"),
            "DP-2",
        );
        assert_eq!(summary, "BenQ PD3220U: sRGB");
        assert_eq!(body, "srgb.icc on DP-2");
    }

    #[test]
    fn an_unlisted_mode_says_so() {
        let (summary, _) = text("BenQ PD3220U", None, Path::new("/p/sRGB-v4.icc"), "DP-2");
        assert_eq!(summary, "BenQ PD3220U: unlisted mode");
    }

    #[test]
    fn markup_in_the_body_is_escaped_and_the_summary_left_alone() {
        let (summary, body) = text(
            "M",
            Some(&mode("Movie & <Game>")),
            Path::new("/p/a&b<c>.icc"),
            "DP-1",
        );
        assert_eq!(summary, "M: Movie & <Game>");
        assert_eq!(body, "a&amp;b&lt;c&gt;.icc on DP-1");
    }
}
