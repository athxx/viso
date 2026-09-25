//! Notifications and their permission through UserNotifications (macOS and
//! iOS).
//!
//! The notification center exists only for a bundled app: outside a bundle
//! the framework raises, so both services answer
//! [`ServiceError::Unsupported`] there. A notification posted while the app
//! is frontmost is shown as a banner too; the system hides it by default.

use std::sync::Once;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, define_class, msg_send};
use objc2_foundation::{NSArray, NSBundle, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNErrorCode, UNErrorDomain,
    UNMutableNotificationContent, UNNotification, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationSettings, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

use crate::notifications::{Notification, Notifications};
use crate::permissions::{Permission, PermissionState, Permissions};
use crate::reply::{OnceCompleter, Reply, ServiceError, reply};

pub(crate) struct UserNotifications;

/// The center, or `None` outside an app bundle.
fn center() -> Option<Retained<UNUserNotificationCenter>> {
    NSBundle::mainBundle().bundleIdentifier()?;
    let center = UNUserNotificationCenter::currentNotificationCenter();
    static DELEGATE: Once = Once::new();
    DELEGATE.call_once(|| {
        let delegate = ForegroundPresenter::new();
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // The center holds its delegate weakly; this one lives as long as
        // the app.
        std::mem::forget(delegate);
    });
    Some(center)
}

fn failure(error: &NSError) -> ServiceError {
    // SAFETY: an immutable framework constant.
    let domain = unsafe { UNErrorDomain };
    if domain.is_some_and(|d| *error.domain() == *d)
        && error.code() == UNErrorCode::NotificationsNotAllowed.0
    {
        return ServiceError::Denied;
    }
    ServiceError::Failed(error.localizedDescription().to_string())
}

fn state(settings: &UNNotificationSettings) -> PermissionState {
    let status = settings.authorizationStatus();
    if status == UNAuthorizationStatus::NotDetermined {
        PermissionState::Prompt
    } else if status == UNAuthorizationStatus::Denied {
        PermissionState::Denied
    } else {
        PermissionState::Granted
    }
}

impl Notifications for UserNotifications {
    fn notify(&self, notification: Notification) -> Reply<()> {
        let Some(center) = center() else {
            return Reply::err(ServiceError::Unsupported);
        };
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&notification.title));
        content.setBody(&NSString::from_str(&notification.body));
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&notification.id),
            &content,
            None,
        );
        let (completer, reply) = reply();
        let slot = OnceCompleter::new(completer);
        let block = RcBlock::new(move |error: *mut NSError| {
            // SAFETY: the center passes a valid error or nil.
            let result = match unsafe { error.as_ref() } {
                None => Ok(()),
                Some(error) => Err(failure(error)),
            };
            slot.complete(result);
        });
        center.addNotificationRequest_withCompletionHandler(&request, Some(&block));
        reply
    }

    fn withdraw(&self, id: &str) {
        let Some(center) = center() else { return };
        let ids = NSArray::from_retained_slice(&[NSString::from_str(id)]);
        center.removePendingNotificationRequestsWithIdentifiers(&ids);
        center.removeDeliveredNotificationsWithIdentifiers(&ids);
    }
}

impl Permissions for UserNotifications {
    fn status(&self, permission: Permission) -> Reply<PermissionState> {
        let Permission::Notifications = permission;
        let Some(center) = center() else {
            return Reply::err(ServiceError::Unsupported);
        };
        let (completer, reply) = reply();
        let slot = OnceCompleter::new(completer);
        let block = RcBlock::new(move |settings: std::ptr::NonNull<UNNotificationSettings>| {
            // SAFETY: the center passes valid settings.
            slot.complete(Ok(state(unsafe { settings.as_ref() })));
        });
        center.getNotificationSettingsWithCompletionHandler(&block);
        reply
    }

    fn request(&self, permission: Permission) -> Reply<PermissionState> {
        let Permission::Notifications = permission;
        let Some(center) = center() else {
            return Reply::err(ServiceError::Unsupported);
        };
        let (completer, reply) = reply();
        let slot = OnceCompleter::new(completer);
        let block = RcBlock::new(move |granted: Bool, error: *mut NSError| {
            // SAFETY: the center passes a valid error or nil.
            let result = match unsafe { error.as_ref() } {
                Some(error) if !granted.as_bool() => Err(failure(error)),
                _ if granted.as_bool() => Ok(PermissionState::Granted),
                _ => Ok(PermissionState::Denied),
            };
            slot.complete(result);
        });
        let options = UNAuthorizationOptions::Alert
            | UNAuthorizationOptions::Sound
            | UNAuthorizationOptions::Badge;
        center.requestAuthorizationWithOptions_completionHandler(options, &block);
        reply
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and the presenter
    // has no ivars and no `Drop` impl.
    #[unsafe(super(NSObject))]
    #[name = "VisoNotificationPresenter"]
    struct ForegroundPresenter;

    unsafe impl NSObjectProtocol for ForegroundPresenter {}

    unsafe impl UNUserNotificationCenterDelegate for ForegroundPresenter {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }
    }
);

impl ForegroundPresenter {
    fn new() -> Retained<Self> {
        // SAFETY: `init` is NSObject's designated initializer.
        unsafe { msg_send![Self::alloc(), init] }
    }
}
