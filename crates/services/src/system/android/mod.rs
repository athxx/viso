//! The Android services, through `dev.viso.services.VisoServices`: the
//! storage access framework's document pickers, the share chooser,
//! `NotificationManager` and the notification permission, values sealed by
//! an Android keystore key, and the view's haptic feedback.
//!
//! The pickers take no title. A saved document is a content URI, not a
//! path, so `save` answers `Ok(None)`.

mod bridge;

use std::path::PathBuf;
use std::rc::Rc;

use jni_sys::jvalue;

use self::bridge::{Method, Pending, ask, outcome, state, with_java};
use crate::files::{FileDialogs, OpenOptions, PickedFile, SaveOptions};
use crate::haptics::{Haptic, Haptics};
use crate::notifications::{Notification, Notifications};
use crate::permissions::{Permission, PermissionState, Permissions};
use crate::registry::Services;
use crate::reply::{Reply, ServiceResult};
use crate::secure_storage::SecureStorage;
use crate::share::{Share, ShareItem};

pub(crate) fn services(_app: &str) -> Services {
    let android = Rc::new(Android);
    Services::from_parts(
        android.clone(),
        android.clone(),
        android.clone(),
        android.clone(),
        android.clone(),
        android,
    )
}

struct Android;

pub(super) use self::bridge::files_dir;

impl FileDialogs for Android {
    fn open(&self, options: OpenOptions) -> Reply<Vec<PickedFile>> {
        // A filter without extensions admits any file, and so the picker.
        let extensions: Vec<String> = if options.filters.iter().any(|f| f.extensions.is_empty()) {
            Vec::new()
        } else {
            options
                .filters
                .iter()
                .flat_map(|f| f.extensions.iter().cloned())
                .collect()
        };
        ask(Pending::Files, move |java, token| {
            java.call_void(
                Method::OpenDocument,
                &[
                    java.activity(),
                    token,
                    java.strings(&extensions)?,
                    jvalue {
                        z: options.multiple,
                    },
                ],
            )
        })
    }

    fn save(&self, options: SaveOptions, contents: Vec<u8>) -> Reply<Option<PathBuf>> {
        let extension = options
            .filters
            .first()
            .and_then(|f| f.extensions.first())
            .cloned()
            .unwrap_or_default();
        ask(Pending::Saved, move |java, token| {
            java.call_void(
                Method::CreateDocument,
                &[
                    java.activity(),
                    token,
                    java.string(&options.suggested_name)?,
                    java.string(&extension)?,
                    java.bytes(&contents)?,
                ],
            )
        })
    }
}

impl Share for Android {
    fn share(&self, item: ShareItem) -> Reply<()> {
        let (ShareItem::Text(text) | ShareItem::Url(text)) = item;
        ask(Pending::Done, move |java, token| {
            java.call_void(
                Method::Share,
                &[java.activity(), token, java.string(&text)?],
            )
        })
    }
}

impl Notifications for Android {
    fn notify(&self, notification: Notification) -> Reply<()> {
        ask(Pending::Done, move |java, token| {
            java.call_void(
                Method::Notify,
                &[
                    java.activity(),
                    token,
                    java.string(&notification.id)?,
                    java.string(&notification.title)?,
                    java.string(&notification.body)?,
                ],
            )
        })
    }

    fn withdraw(&self, id: &str) {
        // Nothing to withdraw without the Java side.
        let _ = with_java(|java| {
            java.call_void(Method::Withdraw, &[java.activity(), java.string(id)?])
        });
    }
}

impl Permissions for Android {
    fn status(&self, permission: Permission) -> Reply<PermissionState> {
        let Permission::Notifications = permission;
        Reply::ready(with_java(|java| {
            java.call_int(Method::PermissionStatus, &[java.activity()])
                .map(state)
        }))
    }

    fn request(&self, permission: Permission) -> Reply<PermissionState> {
        let Permission::Notifications = permission;
        ask(Pending::State, |java, token| {
            java.call_void(Method::RequestPermission, &[java.activity(), token])
        })
    }
}

impl SecureStorage for Android {
    fn get(&self, key: &str) -> Reply<Option<Vec<u8>>> {
        ask(Pending::Bytes, |java, token| {
            java.call_void(
                Method::SecretGet,
                &[java.activity(), token, java.string(key)?],
            )
        })
    }

    fn set(&self, key: &str, value: &[u8]) -> Reply<()> {
        ask(Pending::Done, |java, token| {
            java.call_void(
                Method::SecretSet,
                &[
                    java.activity(),
                    token,
                    java.string(key)?,
                    java.bytes(value)?,
                ],
            )
        })
    }

    fn remove(&self, key: &str) -> Reply<()> {
        ask(Pending::Done, |java, token| {
            java.call_void(
                Method::SecretRemove,
                &[java.activity(), token, java.string(key)?],
            )
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
