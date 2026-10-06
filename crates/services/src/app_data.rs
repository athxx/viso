//! Where an app keeps what it carries from one run to the next: a directory
//! of its own on a native OS, the page origin's `localStorage` on the web.

#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;

/// The directory the app named `app` keeps the data it carries between runs
/// in, which may not exist yet: under `~/Library/Application Support` on
/// macOS, the sandbox's Application Support directory on iOS,
/// `$XDG_STATE_HOME` (or `~/.local/state`) on Linux and the BSDs,
/// `%LOCALAPPDATA%` on Windows and the activity's files directory on
/// Android. `None` where the OS names none.
#[cfg(not(target_arch = "wasm32"))]
pub fn app_data_dir(app: &str) -> Option<PathBuf> {
    crate::system::app_data_dir(app)
}

/// The page origin's `localStorage`, its keys prefixed by the app's name
/// so apps sharing an origin keep apart. A blob is stored as a string of
/// one UTF-16 unit a byte.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub struct LocalStorage {
    storage: web_sys::Storage,
    prefix: String,
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
impl LocalStorage {
    /// The storage of the app named `app`; `None` when the page has none
    /// (a sandboxed frame, storage disabled).
    pub fn open(app: &str) -> Option<LocalStorage> {
        let storage = web_sys::window()?.local_storage().ok()??;
        Some(LocalStorage {
            storage,
            prefix: format!("{app}/"),
        })
    }

    /// The blob stored under `key`.
    ///
    /// # Errors
    ///
    /// Why the storage could not be read.
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        let text = self
            .storage
            .get_item(&format!("{}{key}", self.prefix))
            .map_err(|e| format!("reading `{key}`: {e:?}"))?;
        Ok(text.map(|text| text.chars().map(|c| c as u32 as u8).collect()))
    }

    /// Stores `blob` under `key`.
    ///
    /// # Errors
    ///
    /// Why it was not stored, as a full quota.
    pub fn set(&self, key: &str, blob: &[u8]) -> Result<(), String> {
        let text: String = blob.iter().map(|&b| char::from(b)).collect();
        self.storage
            .set_item(&format!("{}{key}", self.prefix), &text)
            .map_err(|e| format!("writing `{key}`: {e:?}"))
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[test]
    fn a_mac_app_keeps_its_data_in_application_support() {
        let dir = super::app_data_dir("tally").expect("a home");
        assert!(
            dir.ends_with("Library/Application Support/tally"),
            "{dir:?}"
        );
    }
}
