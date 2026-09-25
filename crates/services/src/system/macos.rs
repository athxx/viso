//! The macOS services: AppKit panels and sharing picker, UserNotifications,
//! the Keychain, the trackpad's haptic engine.

use std::path::PathBuf;
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSApplication, NSHapticFeedbackManager, NSHapticFeedbackPattern,
    NSHapticFeedbackPerformanceTime, NSHapticFeedbackPerformer, NSModalResponse, NSModalResponseOK,
    NSOpenPanel, NSSavePanel, NSSharingServicePicker,
};
use objc2_foundation::{NSArray, NSPoint, NSRect, NSRectEdge, NSSize, NSString, NSURL};

use super::apple::{content_types, read_urls};
use super::keychain::Keychain;
use super::user_notifications::UserNotifications;
use crate::files::{FileDialogs, OpenOptions, PickedFile, SaveOptions};
use crate::haptics::{Haptic, Haptics};
use crate::registry::Services;
use crate::reply::{Completer, Reply, ServiceError, ServiceResult, reply};
use crate::share::{Share, ShareItem};

pub(crate) fn services(app: &str) -> Services {
    let notifications = Rc::new(UserNotifications);
    let appkit = Rc::new(AppKit);
    Services::from_parts(
        appkit.clone(),
        appkit.clone(),
        notifications.clone(),
        notifications,
        Rc::new(Keychain::new(app)),
        appkit,
    )
}

struct AppKit;

fn main_thread() -> ServiceResult<MainThreadMarker> {
    MainThreadMarker::new()
        .ok_or_else(|| ServiceError::Failed("AppKit services run on the main thread".into()))
}

/// Configure the panel both dialogs share and show it; `done` runs with
/// the panel once the user closed it with OK.
fn begin<T: 'static>(
    panel: Retained<NSSavePanel>,
    title: Option<&str>,
    completer: Completer<T>,
    done: impl Fn(&NSSavePanel) -> ServiceResult<T> + 'static,
) {
    if let Some(title) = title {
        panel.setMessage(Some(&NSString::from_str(title)));
    }
    let completer = std::cell::Cell::new(Some(completer));
    let shown = panel.clone();
    let handler = RcBlock::new(move |response: NSModalResponse| {
        let Some(completer) = completer.take() else {
            return;
        };
        completer.complete(if response == NSModalResponseOK {
            done(&shown)
        } else {
            Err(ServiceError::Cancelled)
        });
    });
    panel.beginWithCompletionHandler(&handler);
}

impl FileDialogs for AppKit {
    fn open(&self, options: OpenOptions) -> Reply<Vec<PickedFile>> {
        let mtm = match main_thread() {
            Ok(mtm) => mtm,
            Err(e) => return Reply::err(e),
        };
        let panel = NSOpenPanel::openPanel(mtm);
        panel.setCanChooseFiles(true);
        panel.setCanChooseDirectories(false);
        panel.setAllowsMultipleSelection(options.multiple);
        panel.setAllowedContentTypes(&content_types(&options.filters));
        let (completer, reply) = reply();
        let open = panel.clone();
        begin(
            Retained::into_super(panel),
            options.title.as_deref(),
            completer,
            move |_| read_urls(&open.URLs()),
        );
        reply
    }

    fn save(&self, options: SaveOptions, contents: Vec<u8>) -> Reply<Option<PathBuf>> {
        let mtm = match main_thread() {
            Ok(mtm) => mtm,
            Err(e) => return Reply::err(e),
        };
        let panel = NSSavePanel::savePanel(mtm);
        panel.setCanCreateDirectories(true);
        panel.setNameFieldStringValue(&NSString::from_str(&options.suggested_name));
        panel.setAllowedContentTypes(&content_types(&options.filters));
        let (completer, reply) = reply();
        begin(panel, options.title.as_deref(), completer, move |panel| {
            let path = panel
                .URL()
                .and_then(|url| url.to_file_path())
                .ok_or_else(|| ServiceError::Failed("the save panel chose no file".into()))?;
            std::fs::write(&path, &contents).map_err(|e| ServiceError::Failed(e.to_string()))?;
            Ok(Some(path))
        });
        reply
    }
}

impl Share for AppKit {
    fn share(&self, item: ShareItem) -> Reply<()> {
        let mtm = match main_thread() {
            Ok(mtm) => mtm,
            Err(e) => return Reply::err(e),
        };
        let item: Retained<AnyObject> = match item {
            ShareItem::Text(text) => {
                Retained::into_super(Retained::into_super(NSString::from_str(&text)))
            }
            ShareItem::Url(url) => match NSURL::URLWithString(&NSString::from_str(&url)) {
                Some(url) => Retained::into_super(Retained::into_super(url)),
                None => return Reply::err(ServiceError::Failed(format!("not a URL: {url}"))),
            },
        };
        let Some(view) = NSApplication::sharedApplication(mtm)
            .keyWindow()
            .and_then(|w| w.contentView())
        else {
            return Reply::err(ServiceError::Failed("no key window to share from".into()));
        };
        let items = NSArray::from_retained_slice(&[item]);
        // SAFETY: `initWithItems:` is the designated initializer and takes
        // any objects the sharing services understand.
        let picker = unsafe {
            NSSharingServicePicker::initWithItems(NSSharingServicePicker::alloc(), &items)
        };
        // The picker anchors to the middle of the window's top edge.
        let bounds = view.bounds();
        let anchor = NSRect::new(
            NSPoint::new(bounds.size.width / 2.0, bounds.size.height),
            NSSize::new(1.0, 1.0),
        );
        picker.showRelativeToRect_ofView_preferredEdge(anchor, &view, NSRectEdge::MinY);
        Reply::ready(Ok(()))
    }
}

impl Haptics for AppKit {
    fn play(&self, haptic: Haptic) -> ServiceResult<()> {
        let pattern = match haptic {
            Haptic::Selection => NSHapticFeedbackPattern::Alignment,
            Haptic::Success | Haptic::Warning | Haptic::Error => {
                NSHapticFeedbackPattern::LevelChange
            }
            _ => NSHapticFeedbackPattern::Generic,
        };
        NSHapticFeedbackManager::defaultPerformer().performFeedbackPattern_performanceTime(
            pattern,
            NSHapticFeedbackPerformanceTime::Default,
        );
        Ok(())
    }
}
