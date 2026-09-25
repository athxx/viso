//! The iOS services: the document picker and activity sheet presented from
//! the key window's topmost controller, UserNotifications, the Keychain,
//! UIKit's feedback generators.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_foundation::{NSArray, NSError, NSString, NSURL};
use objc2_ui_kit::{
    UIActivityType, UIActivityViewController, UIApplication, UIDocumentPickerDelegate,
    UIDocumentPickerViewController, UIImpactFeedbackGenerator, UIImpactFeedbackStyle,
    UINotificationFeedbackGenerator, UINotificationFeedbackType, UISelectionFeedbackGenerator,
    UIViewController, UIWindowScene,
};
use objc2_uniform_type_identifiers::UTTypeItem;

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
    let uikit = Rc::new(UiKit);
    Services::from_parts(
        uikit.clone(),
        uikit.clone(),
        notifications.clone(),
        notifications,
        Rc::new(Keychain::new(app)),
        uikit,
    )
}

struct UiKit;

fn main_thread() -> ServiceResult<MainThreadMarker> {
    MainThreadMarker::new()
        .ok_or_else(|| ServiceError::Failed("UIKit services run on the main thread".into()))
}

/// The controller a sheet is presented from: the key window's root, or
/// whatever it already presents.
fn presenter(mtm: MainThreadMarker) -> ServiceResult<Retained<UIViewController>> {
    let app = UIApplication::sharedApplication(mtm);
    let window = app
        .connectedScenes()
        .iter()
        .filter_map(|scene| scene.downcast::<UIWindowScene>().ok())
        .find_map(|scene| scene.keyWindow());
    #[allow(deprecated)]
    let window = window.or_else(|| app.keyWindow());
    let mut top = window
        .and_then(|w| w.rootViewController())
        .ok_or_else(|| ServiceError::Failed("no window to present from".into()))?;
    while let Some(next) = top.presentedViewController() {
        top = next;
    }
    Ok(top)
}

/// What a picker answers once the user chose or cancelled.
enum Pending {
    Open(Completer<Vec<PickedFile>>),
    /// The exported temporary copy, removed once the picker closes.
    Save(Completer<Option<PathBuf>>, PathBuf),
}

thread_local! {
    /// The picker holds its delegate weakly; open pickers' delegates live
    /// here until they answer.
    static LIVE: RefCell<Vec<Retained<PickerDelegate>>> = const { RefCell::new(Vec::new()) };
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the ivars are
    // dropped by the class and there is no `Drop` impl.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VisoDocumentPickerDelegate"]
    #[ivars = RefCell<Option<Pending>>]
    struct PickerDelegate;

    unsafe impl NSObjectProtocol for PickerDelegate {}

    unsafe impl UIDocumentPickerDelegate for PickerDelegate {
        #[unsafe(method(documentPicker:didPickDocumentsAtURLs:))]
        fn did_pick(&self, _picker: &UIDocumentPickerViewController, urls: &NSArray<NSURL>) {
            self.finish(Ok(urls));
        }

        #[unsafe(method(documentPickerWasCancelled:))]
        fn cancelled(&self, _picker: &UIDocumentPickerViewController) {
            self.finish(Err(ServiceError::Cancelled));
        }
    }
);

impl PickerDelegate {
    fn new(mtm: MainThreadMarker, pending: Pending) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RefCell::new(Some(pending)));
        // SAFETY: `init` is NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    fn finish(&self, urls: ServiceResult<&NSArray<NSURL>>) {
        let pending = self.ivars().borrow_mut().take();
        match pending {
            Some(Pending::Open(completer)) => completer.complete(urls.and_then(|urls| {
                // The picker hands out temporary copies: keep the contents,
                // not the path.
                let files = read_urls(urls)?;
                Ok(files
                    .into_iter()
                    .map(|mut file| {
                        if let Some(path) = file.path.take() {
                            let _ = std::fs::remove_file(path);
                        }
                        file
                    })
                    .collect())
            })),
            Some(Pending::Save(completer, copy)) => {
                let _ = std::fs::remove_file(&copy);
                completer
                    .complete(urls.map(|urls| urls.iter().next().and_then(|u| u.to_file_path())));
            }
            None => {}
        }
        LIVE.with(|live| live.borrow_mut().retain(|d| !std::ptr::eq(&**d, self)));
    }
}

fn present_picker(
    mtm: MainThreadMarker,
    picker: Retained<UIDocumentPickerViewController>,
    pending: Pending,
) -> Result<(), Pending> {
    let top = match presenter(mtm) {
        Ok(top) => top,
        Err(_) => return Err(pending),
    };
    let delegate = PickerDelegate::new(mtm, pending);
    picker.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    LIVE.with(|live| live.borrow_mut().push(delegate));
    top.presentViewController_animated_completion(&picker, true, None);
    Ok(())
}

impl FileDialogs for UiKit {
    fn open(&self, options: OpenOptions) -> Reply<Vec<PickedFile>> {
        let mtm = match main_thread() {
            Ok(mtm) => mtm,
            Err(e) => return Reply::err(e),
        };
        let mut types = content_types(&options.filters);
        if types.is_empty() {
            // SAFETY: an immutable framework constant.
            types = NSArray::from_slice(&[unsafe { UTTypeItem }]);
        }
        let picker = UIDocumentPickerViewController::initForOpeningContentTypes_asCopy(
            UIDocumentPickerViewController::alloc(mtm),
            &types,
            true,
        );
        picker.setAllowsMultipleSelection(options.multiple);
        let (completer, reply) = reply();
        if let Err(Pending::Open(completer)) = present_picker(mtm, picker, Pending::Open(completer))
        {
            completer.complete(Err(ServiceError::Failed(
                "no window to present from".into(),
            )));
        }
        reply
    }

    fn save(&self, options: SaveOptions, contents: Vec<u8>) -> Reply<Option<PathBuf>> {
        let mtm = match main_thread() {
            Ok(mtm) => mtm,
            Err(e) => return Reply::err(e),
        };
        let name = if options.suggested_name.is_empty() {
            "Untitled"
        } else {
            options.suggested_name.as_str()
        };
        let copy = std::env::temp_dir().join(name);
        if let Err(e) = std::fs::write(&copy, &contents) {
            return Reply::err(ServiceError::Failed(e.to_string()));
        }
        let Some(url) = NSURL::from_file_path(&copy) else {
            return Reply::err(ServiceError::Failed(
                "the export path is not a file URL".into(),
            ));
        };
        let picker = UIDocumentPickerViewController::initForExportingURLs_asCopy(
            UIDocumentPickerViewController::alloc(mtm),
            &NSArray::from_retained_slice(&[url]),
            true,
        );
        let (completer, reply) = reply();
        if let Err(Pending::Save(completer, copy)) =
            present_picker(mtm, picker, Pending::Save(completer, copy))
        {
            let _ = std::fs::remove_file(copy);
            completer.complete(Err(ServiceError::Failed(
                "no window to present from".into(),
            )));
        }
        reply
    }
}

impl Share for UiKit {
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
        let top = match presenter(mtm) {
            Ok(top) => top,
            Err(e) => return Reply::err(e),
        };
        let items = NSArray::from_retained_slice(&[item]);
        // SAFETY: the items are an NSString or an NSURL, both of which the
        // activity sheet accepts.
        let sheet = unsafe {
            UIActivityViewController::initWithActivityItems_applicationActivities(
                UIActivityViewController::alloc(mtm),
                &items,
                None,
            )
        };
        let (completer, reply) = reply();
        let completer = Cell::new(Some(completer));
        let handler = RcBlock::new(
            move |_: *mut UIActivityType, completed: Bool, _: *mut NSArray, error: *mut NSError| {
                let Some(completer) = completer.take() else {
                    return;
                };
                // SAFETY: the sheet passes a valid error or nil.
                completer.complete(match unsafe { error.as_ref() } {
                    Some(error) => Err(ServiceError::Failed(
                        error.localizedDescription().to_string(),
                    )),
                    None if completed.as_bool() => Ok(()),
                    None => Err(ServiceError::Cancelled),
                });
            },
        );
        // SAFETY: the pointer is a live block; the sheet copies it.
        unsafe { sheet.setCompletionWithItemsHandler(RcBlock::as_ptr(&handler)) };
        // On iPad the sheet is a popover and needs an anchor: the bottom
        // middle of the presenting view.
        if let (Some(popover), Some(view)) = (sheet.popoverPresentationController(), top.view()) {
            let bounds = view.bounds();
            popover.setSourceView(Some(&view));
            popover.setSourceRect(objc2_foundation::NSRect::new(
                objc2_foundation::NSPoint::new(bounds.size.width / 2.0, bounds.size.height),
                objc2_foundation::NSSize::new(1.0, 1.0),
            ));
        }
        top.presentViewController_animated_completion(&sheet, true, None);
        reply
    }
}

impl Haptics for UiKit {
    fn play(&self, haptic: Haptic) -> ServiceResult<()> {
        let mtm = main_thread()?;
        // The view-bound constructors that replace these need iOS 17.5.
        #[allow(deprecated)]
        let impact = |style| {
            UIImpactFeedbackGenerator::initWithStyle(UIImpactFeedbackGenerator::alloc(mtm), style)
                .impactOccurred();
        };
        let notify = |kind| UINotificationFeedbackGenerator::new(mtm).notificationOccurred(kind);
        match haptic {
            Haptic::Selection => UISelectionFeedbackGenerator::new(mtm).selectionChanged(),
            Haptic::Light => impact(UIImpactFeedbackStyle::Light),
            Haptic::Medium => impact(UIImpactFeedbackStyle::Medium),
            Haptic::Heavy => impact(UIImpactFeedbackStyle::Heavy),
            Haptic::Success => notify(UINotificationFeedbackType::Success),
            Haptic::Warning => notify(UINotificationFeedbackType::Warning),
            Haptic::Error => notify(UINotificationFeedbackType::Error),
        }
        Ok(())
    }
}
