//! Open and save through the common item dialogs, each on its own STA
//! thread: the dialog runs a modal loop, which must not stall the UI loop.

use std::path::PathBuf;

use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
    CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_ALLOWMULTISELECT, FOS_FORCEFILESYSTEM, FileOpenDialog, FileSaveDialog, IFileDialog,
    IFileOpenDialog, IFileSaveDialog, IShellItem, SIGDN_FILESYSPATH,
};
use windows::core::{HRESULT, HSTRING, PCWSTR};

use crate::files::{FileDialogs, FileFilter, OpenOptions, PickedFile, SaveOptions};
use crate::reply::{Reply, ServiceError, ServiceResult, reply};
use crate::system::read_picked;

pub(super) struct ItemDialogs;

impl FileDialogs for ItemDialogs {
    fn open(&self, options: OpenOptions) -> Reply<Vec<PickedFile>> {
        on_worker(move |owner| {
            let paths = open(owner, &options).map_err(failure)?;
            paths.into_iter().map(read_picked).collect()
        })
    }

    fn save(&self, options: SaveOptions, contents: Vec<u8>) -> Reply<Option<PathBuf>> {
        on_worker(move |owner| {
            let path = save(owner, &options).map_err(failure)?;
            std::fs::write(&path, contents).map_err(|e| ServiceError::Failed(e.to_string()))?;
            Ok(Some(path))
        })
    }
}

/// Run `show` on a new STA thread, owned by (and disabling) the active
/// window.
fn on_worker<T: Send + 'static>(
    show: impl FnOnce(HWND) -> ServiceResult<T> + Send + 'static,
) -> Reply<T> {
    // A window handle is not `Send`; it is a plain value across threads.
    let owner = super::owner().0 as usize;
    let (completer, reply) = reply();
    let spawned = std::thread::Builder::new()
        .name("viso-file-dialog".into())
        .spawn(move || {
            // SAFETY: initializes COM for this new thread; balanced below.
            let init =
                unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
            if let Err(e) = init.ok() {
                completer.complete(Err(failure(e)));
                return;
            }
            completer.complete(show(HWND(owner as *mut _)));
            // SAFETY: balances the CoInitializeEx above; `show` released
            // every COM object it made.
            unsafe { CoUninitialize() };
        });
    match spawned {
        Ok(_) => reply,
        Err(e) => Reply::err(ServiceError::Failed(e.to_string())),
    }
}

fn failure(error: windows::core::Error) -> ServiceError {
    if error.code() == HRESULT::from_win32(ERROR_CANCELLED.0) {
        ServiceError::Cancelled
    } else {
        ServiceError::Failed(error.message())
    }
}

/// Apply what both dialogs share.
fn configure(
    dialog: &IFileDialog,
    title: Option<&str>,
    filters: &[FileFilter],
) -> windows::core::Result<()> {
    if let Some(title) = title {
        // SAFETY: a live dialog on this thread; the string outlives the call.
        unsafe { dialog.SetTitle(&HSTRING::from(title)) }?;
    }
    if !filters.is_empty() {
        let strings: Vec<(HSTRING, HSTRING)> = filters
            .iter()
            .map(|filter| {
                let spec = if filter.extensions.is_empty() {
                    "*.*".to_owned()
                } else {
                    let patterns: Vec<String> =
                        filter.extensions.iter().map(|e| format!("*.{e}")).collect();
                    patterns.join(";")
                };
                (HSTRING::from(filter.name.as_str()), HSTRING::from(spec))
            })
            .collect();
        let specs: Vec<COMDLG_FILTERSPEC> = strings
            .iter()
            .map(|(name, spec)| COMDLG_FILTERSPEC {
                pszName: PCWSTR(name.as_ptr()),
                pszSpec: PCWSTR(spec.as_ptr()),
            })
            .collect();
        // SAFETY: the specs point into `strings`, alive across the call,
        // which copies them.
        unsafe { dialog.SetFileTypes(&specs) }?;
    }
    // SAFETY: a live dialog on this thread.
    unsafe { dialog.SetOptions(dialog.GetOptions()? | FOS_FORCEFILESYSTEM) }
}

fn path_of(item: &IShellItem) -> windows::core::Result<PathBuf> {
    // SAFETY: GetDisplayName returns a NUL-terminated CoTaskMem string,
    // read once and freed here.
    unsafe {
        let name = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = String::from_utf16_lossy(name.as_wide());
        CoTaskMemFree(Some(name.0 as *const _));
        Ok(PathBuf::from(path))
    }
}

fn open(owner: HWND, options: &OpenOptions) -> windows::core::Result<Vec<PathBuf>> {
    // SAFETY: COM is initialized on this thread, and FileOpenDialog is the
    // in-process class implementing IFileOpenDialog.
    let dialog: IFileOpenDialog =
        unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) }?;
    configure(&dialog, options.title.as_deref(), &options.filters)?;
    // SAFETY: a live dialog on this thread; `owner` is a window handle or
    // null, both of which Show accepts.
    unsafe {
        if options.multiple {
            dialog.SetOptions(dialog.GetOptions()? | FOS_ALLOWMULTISELECT)?;
        }
        dialog.Show(Some(owner))?;
        let items = dialog.GetResults()?;
        (0..items.GetCount()?)
            .map(|i| path_of(&items.GetItemAt(i)?))
            .collect()
    }
}

fn save(owner: HWND, options: &SaveOptions) -> windows::core::Result<PathBuf> {
    // SAFETY: COM is initialized on this thread, and FileSaveDialog is the
    // in-process class implementing IFileSaveDialog.
    let dialog: IFileSaveDialog =
        unsafe { CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER) }?;
    configure(&dialog, options.title.as_deref(), &options.filters)?;
    // SAFETY: a live dialog on this thread; the strings outlive the calls;
    // `owner` is a window handle or null.
    unsafe {
        if !options.suggested_name.is_empty() {
            dialog.SetFileName(&HSTRING::from(options.suggested_name.as_str()))?;
        }
        if let Some(extension) = options.filters.first().and_then(|f| f.extensions.first()) {
            dialog.SetDefaultExtension(&HSTRING::from(extension.as_str()))?;
        }
        dialog.Show(Some(owner))?;
        path_of(&dialog.GetResult()?)
    }
}
