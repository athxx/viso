//! Open a URL through the XDG desktop portal's OpenURI, which hands it to
//! the desktop's choice of app (sandboxed apps included); a desktop without
//! the portal opens it with `xdg-open`. Each opening runs on its own thread.

use std::collections::HashMap;
use std::process::{Command, Stdio};

use zbus::blocking::Connection;
use zbus::zvariant::Value;

use crate::launcher::{Launcher, launch};
use crate::reply::{Reply, ServiceError, ServiceResult, reply};

const DESKTOP: &str = "org.freedesktop.portal.Desktop";
const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";
const OPEN_URI: &str = "org.freedesktop.portal.OpenURI";

pub(super) struct PortalLauncher;

impl Launcher for PortalLauncher {
    fn open_url(&self, url: &str) -> Reply<()> {
        launch(url, |url| {
            let url = url.to_owned();
            let (completer, reply) = reply();
            let spawned = std::thread::Builder::new()
                .name("viso-open-uri".into())
                .spawn(move || completer.complete(open(&url)));
            match spawned {
                Ok(_) => reply,
                Err(e) => Reply::err(ServiceError::Failed(e.to_string())),
            }
        })
    }
}

fn open(url: &str) -> ServiceResult<()> {
    if let Ok(connection) = Connection::session() {
        let options: HashMap<&str, Value<'_>> = HashMap::new();
        let asked = connection.call_method(
            Some(DESKTOP),
            DESKTOP_PATH,
            Some(OPEN_URI),
            "OpenURI",
            &("", url, options),
        );
        if asked.is_ok() {
            return Ok(());
        }
    }
    let status = Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| ServiceError::Unsupported)?;
    // `xdg-open` exits 3 when no app opens the URL, 4 when the opening
    // failed.
    match status.code() {
        Some(0) => Ok(()),
        Some(3) => Err(ServiceError::Unsupported),
        _ => Err(ServiceError::Failed(format!("xdg-open answered {status}"))),
    }
}
