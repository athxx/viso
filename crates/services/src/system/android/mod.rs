//! The Android services, through `dev.viso.services.VisoServices`: the
//! view's haptic feedback, and `ACTION_VIEW` for URLs.

mod bridge;

use std::rc::Rc;

use jni_sys::jvalue;

use self::bridge::{Method, Pending, ask, outcome, with_java};
use crate::haptics::{Haptic, Haptics};
use crate::launcher::{Launcher, launch};
use crate::registry::Services;
use crate::reply::{Reply, ServiceResult};

pub(crate) fn services(_app: &str) -> Services {
    let android = Rc::new(Android);
    Services::from_parts(android.clone(), android)
}

struct Android;

pub(super) use self::bridge::{external_files_dir, files_dir};

impl Launcher for Android {
    fn open_url(&self, url: &str) -> Reply<()> {
        launch(url, |url| {
            let url = url.to_owned();
            ask(Pending::Done, move |java, token| {
                java.call_void(
                    Method::OpenUrl,
                    &[java.activity(), token, java.string(&url)?],
                )
            })
        })
    }
}

impl Haptics for Android {
    fn play(&self, haptic: Haptic) -> ServiceResult<()> {
        // The order `VisoServices.feedback` reads.
        let kind = match haptic {
            Haptic::Selection => 0,
            Haptic::Light => 1,
            Haptic::Medium => 2,
            Haptic::Heavy => 3,
            Haptic::Success => 4,
            Haptic::Warning => 5,
            Haptic::Error => 6,
        };
        let status = with_java(|java| {
            java.call_int(Method::Vibrate, &[java.activity(), jvalue { i: kind }])
        })?;
        outcome(status, None)
    }
}
