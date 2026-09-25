//! Share through the system share sheet (`DataTransferManager`), attached
//! to the active window.

use std::cell::RefCell;
use std::sync::Arc;

use windows::ApplicationModel::DataTransfer::{
    DataPackage, DataRequestedEventArgs, DataTransferManager,
};
use windows::Foundation::{TypedEventHandler, Uri};
use windows::Win32::UI::Shell::IDataTransferManagerInterop;
use windows::core::{HSTRING, factory};

use crate::reply::{OnceCompleter, Reply, ServiceError, reply};
use crate::share::{Share, ShareItem};

pub(super) struct ShareSheet {
    /// The sheet's title; Windows requires one.
    title: HSTRING,
    /// The data handler of the last share, replaced by the next.
    requested: RefCell<Option<(DataTransferManager, i64)>>,
}

impl ShareSheet {
    pub(super) fn new(app: &str) -> Self {
        Self {
            title: HSTRING::from(app),
            requested: RefCell::new(None),
        }
    }

    fn show(
        &self,
        item: ShareItem,
        completer: Arc<OnceCompleter<()>>,
    ) -> windows::core::Result<()> {
        let window = super::owner();
        let interop = factory::<DataTransferManager, IDataTransferManagerInterop>()?;
        // SAFETY: `window` is this thread's window (or null, which the call
        // rejects with an error).
        let manager: DataTransferManager = unsafe { interop.GetForWindow(window) }?;
        if let Some((previous, token)) = self.requested.borrow_mut().take() {
            previous.RemoveDataRequested(token)?;
        }
        let title = self.title.clone();
        let token = manager.DataRequested(&TypedEventHandler::new(
            move |_, args: windows::core::Ref<DataRequestedEventArgs>| {
                let Some(args) = args.as_ref() else {
                    return Ok(());
                };
                let data = args.Request()?.Data()?;
                fill(&data, &title, &item)?;
                let done = completer.clone();
                data.ShareCompleted(&TypedEventHandler::new(move |_, _| {
                    done.complete(Ok(()));
                    Ok(())
                }))?;
                let cancelled = completer.clone();
                // Older systems lack the event; the reply then cancels when
                // the next share replaces this handler.
                let _ = data.ShareCanceled(&TypedEventHandler::new(move |_, _| {
                    cancelled.complete(Err(ServiceError::Cancelled));
                    Ok(())
                }));
                Ok(())
            },
        ))?;
        *self.requested.borrow_mut() = Some((manager, token));
        // SAFETY: as for GetForWindow.
        unsafe { interop.ShowShareUIForWindow(window) }
    }
}

fn fill(data: &DataPackage, title: &HSTRING, item: &ShareItem) -> windows::core::Result<()> {
    data.Properties()?.SetTitle(title)?;
    match item {
        ShareItem::Text(text) => data.SetText(&HSTRING::from(text.as_str())),
        ShareItem::Url(url) => {
            let url = HSTRING::from(url.as_str());
            data.SetWebLink(&Uri::CreateUri(&url)?)?;
            data.SetText(&url)
        }
    }
}

impl Share for ShareSheet {
    fn share(&self, item: ShareItem) -> Reply<()> {
        let (completer, reply) = reply();
        let completer = Arc::new(OnceCompleter::new(completer));
        if let Err(e) = self.show(item, completer.clone()) {
            completer.complete(Err(ServiceError::Failed(e.message())));
        }
        reply
    }
}
