//! The macOS services: the trackpad's haptic engine, `NSWorkspace` opening
//! URLs.

use std::rc::Rc;

use objc2_app_kit::{
    NSHapticFeedbackManager, NSHapticFeedbackPattern, NSHapticFeedbackPerformanceTime,
    NSHapticFeedbackPerformer, NSWorkspace,
};
use objc2_foundation::{NSString, NSURL};

use crate::haptics::{Haptic, Haptics};
use crate::launcher::{Launcher, launch};
use crate::registry::Services;
use crate::reply::{Reply, ServiceError, ServiceResult};

pub(crate) fn services(_app: &str) -> Services {
    let appkit = Rc::new(AppKit);
    Services::from_parts(appkit.clone(), appkit)
}

struct AppKit;

impl Launcher for AppKit {
    fn open_url(&self, url: &str) -> Reply<()> {
        launch(url, |url| {
            let Some(url) = NSURL::URLWithString(&NSString::from_str(url)) else {
                return Reply::err(ServiceError::Failed(format!("not a URL: {url}")));
            };
            // No app here opens the URL's scheme when the workspace refuses.
            let opened = NSWorkspace::sharedWorkspace().openURL(&url);
            Reply::ready(if opened {
                Ok(())
            } else {
                Err(ServiceError::Unsupported)
            })
        })
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
