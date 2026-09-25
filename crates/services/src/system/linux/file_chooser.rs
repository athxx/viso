//! Open and save through the XDG desktop portal's FileChooser, which the
//! desktop (GNOME, KDE, …) answers with its own dialog, sandboxed apps
//! included. Each dialog waits on its own thread.

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use super::bus::{detached, failed, failure, signals};
use crate::files::{FileDialogs, FileFilter, OpenOptions, PickedFile, SaveOptions};
use crate::reply::{Reply, ServiceError, ServiceResult};
use crate::system::read_picked;

const DESKTOP: &str = "org.freedesktop.portal.Desktop";
const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";
const FILE_CHOOSER: &str = "org.freedesktop.portal.FileChooser";
const REQUEST: &str = "org.freedesktop.portal.Request";

pub(super) struct PortalDialogs;

impl FileDialogs for PortalDialogs {
    fn open(&self, options: OpenOptions) -> Reply<Vec<PickedFile>> {
        detached("viso-file-dialog", move |connection| {
            let mut settings = HashMap::new();
            settings.insert("multiple", Value::from(options.multiple));
            insert_filters(&mut settings, &options.filters);
            let title = options.title.unwrap_or_default();
            let chosen = request(connection, "OpenFile", &title, settings)?;
            uris(chosen)?
                .iter()
                .map(|uri| read_picked(file_path(uri)?))
                .collect()
        })
    }

    fn save(&self, options: SaveOptions, contents: Vec<u8>) -> Reply<Option<PathBuf>> {
        detached("viso-file-dialog", move |connection| {
            let mut settings = HashMap::new();
            settings.insert("current_name", Value::from(options.suggested_name.as_str()));
            insert_filters(&mut settings, &options.filters);
            let title = options.title.unwrap_or_default();
            let chosen = request(connection, "SaveFile", &title, settings)?;
            let Some(uri) = uris(chosen)?.into_iter().next() else {
                return Err(ServiceError::Cancelled);
            };
            let path = file_path(&uri)?;
            std::fs::write(&path, contents).map_err(failed)?;
            Ok(Some(path))
        })
    }
}

/// Filters as the portal takes them: `a(sa(us))`, each pattern a glob (0).
fn insert_filters<'a>(settings: &mut HashMap<&'static str, Value<'a>>, filters: &[FileFilter]) {
    if filters.is_empty() {
        return;
    }
    let filters: Vec<(String, Vec<(u32, String)>)> = filters
        .iter()
        .map(|filter| {
            let patterns = if filter.extensions.is_empty() {
                vec![(0, "*".to_owned())]
            } else {
                filter
                    .extensions
                    .iter()
                    .map(|ext| (0, format!("*.{ext}")))
                    .collect()
            };
            (filter.name.clone(), patterns)
        })
        .collect();
    settings.insert("filters", Value::from(filters));
}

/// Make a portal request and wait for its `Response`: the results when the
/// user chose, `Cancelled` when they dismissed the dialog.
fn request(
    connection: &Connection,
    method: &str,
    title: &str,
    mut settings: HashMap<&'static str, Value<'_>>,
) -> ServiceResult<HashMap<String, OwnedValue>> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let token = format!(
        "viso_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let sender = connection
        .unique_name()
        .ok_or_else(|| failed("the bus connection has no name"))?
        .trim_start_matches(':')
        .replace('.', "_");
    // The portal names the request object after the sender and the token;
    // subscribing before the call means its answer cannot be missed.
    let expected = format!("{DESKTOP_PATH}/request/{sender}/{token}");
    let mut responses = signals(connection, &expected, REQUEST, "Response")?;
    settings.insert("handle_token", Value::from(token.clone()));
    settings.insert("modal", Value::from(true));
    let handle: OwnedObjectPath = connection
        .call_method(
            Some(DESKTOP),
            DESKTOP_PATH,
            Some(FILE_CHOOSER),
            method,
            &("", title, settings),
        )
        .map_err(failure)?
        .body()
        .deserialize()
        .map_err(failure)?;
    if handle.as_str() != expected {
        // A portal older than the naming scheme picked its own path.
        responses = signals(connection, handle.as_str(), REQUEST, "Response")?;
    }
    let message = responses
        .next()
        .ok_or(ServiceError::Cancelled)?
        .map_err(failure)?;
    let (code, results): (u32, HashMap<String, OwnedValue>) =
        message.body().deserialize().map_err(failure)?;
    match code {
        0 => Ok(results),
        1 => Err(ServiceError::Cancelled),
        _ => Err(failed("the portal ended the request")),
    }
}

fn uris(mut results: HashMap<String, OwnedValue>) -> ServiceResult<Vec<String>> {
    let Some(uris) = results.remove("uris") else {
        return Ok(Vec::new());
    };
    Vec::<String>::try_from(uris).map_err(failed)
}

/// The local path a `file://` URI names.
fn file_path(uri: &str) -> ServiceResult<PathBuf> {
    let rest = uri
        .strip_prefix("file://")
        .ok_or_else(|| failed(format!("not a local file: {uri}")))?;
    // An authority, if any, ends where the path begins.
    let path = &rest[rest.find('/').unwrap_or(rest.len())..];
    let mut bytes = Vec::with_capacity(path.len());
    let mut rest = path.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        let decoded = match (byte, tail) {
            (b'%', [high, low, ..]) => hex(*high).zip(hex(*low)).map(|(h, l)| h << 4 | l),
            _ => None,
        };
        match decoded {
            Some(decoded) => {
                bytes.push(decoded);
                rest = &tail[2..];
            }
            None => {
                bytes.push(byte);
                rest = tail;
            }
        }
    }
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

fn hex(digit: u8) -> Option<u8> {
    char::from(digit)
        .to_digit(16)
        .and_then(|d| u8::try_from(d).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_uri_decodes_to_its_path() {
        assert_eq!(
            file_path("file:///home/me/My%20Notes/caf%C3%A9.txt"),
            Ok(PathBuf::from("/home/me/My Notes/café.txt"))
        );
        assert_eq!(
            file_path("file://localhost/tmp/a%2"),
            Ok(PathBuf::from("/tmp/a%2"))
        );
        assert!(file_path("https://example.com/a").is_err());
    }
}
