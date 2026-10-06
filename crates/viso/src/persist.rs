//! The store the app's views persist their `@persist` states through,
//! installed at launch: a directory store in `persist/` under the app's data
//! directory, opened the first time a state is persisted, or the page
//! origin's `localStorage` on the web. Each view stores every committed
//! change; a suspend — the app going to the background, a window closing,
//! the loop ending — makes the writes durable.

use viso_ui::{NodeStore, StateStore};
use viso_view::persist::SharedStore;
#[cfg(not(target_arch = "wasm32"))]
use viso_view::persist::{DirStore, LazyStore};

/// Installs the store of the app named `app`; nothing persists where the OS
/// names no place for it.
pub(crate) fn install(app: &str) {
    viso_view::install_persistence(store(app));
}

#[cfg(not(target_arch = "wasm32"))]
fn store(app: &str) -> Option<SharedStore> {
    let dir = viso_services::app_data_dir(app)?.join("persist");
    Some(SharedStore::new(LazyStore::new(move || {
        DirStore::open(&dir).map_err(|e| format!("cannot open `{}`: {e}", dir.display()))
    })))
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn store(app: &str) -> Option<SharedStore> {
    let storage = viso_services::LocalStorage::open(app)?;
    Some(SharedStore::new(web::LocalStore {
        storage,
        failed: None,
    }))
}

#[cfg(all(target_arch = "wasm32", not(target_os = "unknown")))]
fn store(_app: &str) -> Option<SharedStore> {
    None
}

/// Makes the writes of the views of one window durable, reporting what did
/// not load or store.
pub(crate) fn suspend(store: &mut NodeStore, states: &StateStore) {
    store.suspend(states);
    report();
}

/// Reports the persisted states that did not load and the writes that
/// failed.
pub(crate) fn report() {
    for report in viso_view::take_persist_reports() {
        let key = if report.key.is_empty() {
            String::new()
        } else {
            format!(" `{}`", report.key)
        };
        eprintln!(
            "[viso] error[{}]: @persist{key}: {}",
            report.code, report.message
        );
    }
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod web {
    use viso_view::persist::PersistStore;

    /// `localStorage`, which writes as it is called, so a flush only
    /// reports the first write that failed.
    pub(super) struct LocalStore {
        pub(super) storage: viso_services::LocalStorage,
        pub(super) failed: Option<String>,
    }

    impl PersistStore for LocalStore {
        fn load(&mut self, key: &str) -> Result<Option<Vec<u8>>, String> {
            self.storage.get(key)
        }

        fn store(&mut self, key: &str, blob: Vec<u8>) {
            if let Err(error) = self.storage.set(key, &blob) {
                self.failed.get_or_insert(error);
            }
        }

        fn flush(&mut self) -> Result<(), String> {
            self.failed.take().map_or(Ok(()), Err)
        }
    }
}
