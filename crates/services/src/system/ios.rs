//! The iOS services: UIKit's feedback generators, `UIApplication` opening
//! URLs.

use std::cell::Cell;
use std::rc::Rc;

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSDictionary, NSString, NSURL};
use objc2_ui_kit::{
    UIApplication, UIImpactFeedbackGenerator, UIImpactFeedbackStyle,
    UINotificationFeedbackGenerator, UINotificationFeedbackType, UISelectionFeedbackGenerator,
};

use crate::haptics::{Haptic, Haptics};
use crate::launcher::{Launcher, launch};
use crate::registry::Services;
use crate::reply::{Reply, ServiceError, ServiceResult, reply};

pub(crate) fn services(_app: &str) -> Services {
    let uikit = Rc::new(UiKit);
    Services::from_parts(uikit.clone(), uikit)
}

struct UiKit;

fn main_thread() -> ServiceResult<MainThreadMarker> {
    MainThreadMarker::new()
        .ok_or_else(|| ServiceError::Failed("UIKit services run on the main thread".into()))
}

impl Launcher for UiKit {
    fn open_url(&self, url: &str) -> Reply<()> {
        launch(url, |url| {
            let mtm = match main_thread() {
                Ok(mtm) => mtm,
                Err(e) => return Reply::err(e),
            };
            let Some(url) = NSURL::URLWithString(&NSString::from_str(url)) else {
                return Reply::err(ServiceError::Failed(format!("not a URL: {url}")));
            };
            let (completer, reply) = reply();
            let completer = Cell::new(Some(completer));
            // No app here opens the URL's scheme when UIKit answers no.
            let handler = RcBlock::new(move |opened: Bool| {
                if let Some(completer) = completer.take() {
                    completer.complete(if opened.as_bool() {
                        Ok(())
                    } else {
                        Err(ServiceError::Unsupported)
                    });
                }
            });
            // SAFETY: an empty options dictionary is a valid one, and the
            // handler takes the `BOOL` UIKit calls it with on the main thread.
            unsafe {
                UIApplication::sharedApplication(mtm).openURL_options_completionHandler(
                    &url,
                    &NSDictionary::new(),
                    Some(&handler),
                );
            }
            reply
        })
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
