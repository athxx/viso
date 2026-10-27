//! Native iOS / iPadOS backend (objc2 / UIKit).
//!
//! UIKit owns the loop: [`PlatformApp::run`] parks the handler and calls
//! `UIApplicationMain`, which never returns. Every UIKit callback queues raw
//! events and drains them through the handler on the spot ([`drive`]), then
//! arranges the next wake the handler asked for:
//! - frames come from a `CADisplayLink` that runs only while a redraw is
//!   pending, at up to the display's maximum rate;
//! - [`ControlFlow::WaitUntil`] arms a one-shot `NSTimer` that delivers
//!   [`RawEvent::Wakeup`];
//! - [`ControlFlow::Wait`] and [`ControlFlow::Poll`] leave the run loop idle
//!   until the next input or frame.
//!
//! The app has a single full-screen window. With a scene manifest in the
//! Info.plist it is attached to the `UIWindowScene` the system connects
//! (launch is reported then); without one, to the main screen once launching
//! finishes. Its root view ([`view::VisoView`]) is the GPU surface and the
//! first responder: touches, pointer hover and scroll, hardware keys, and
//! `UITextInput` composition all arrive there.

mod access;
mod clipboard;
mod lock;
mod view;

// `UNUserNotificationCenter`, whose badge setter replaces the application's
// from iOS 16.
#[link(name = "UserNotifications", kind = "framework")]
unsafe extern "C" {}

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_foundation::{
    NSArray, NSCurrentLocaleDidChangeNotification, NSDictionary, NSLocale, NSNotification,
    NSNotificationCenter, NSObjectProtocol, NSRunLoop, NSRunLoopCommonModes, NSSet, NSString,
    NSTimer, NSURL, NSUserActivity, NSUserActivityTypeBrowsingWeb, NSValue, ns_string,
};
use objc2_quartz_core::{CADisplayLink, CAFrameRateRange};
use objc2_ui_kit::UIOpenURLContext;
use objc2_ui_kit::{
    UIAccessibilityContrast, UIAccessibilityDarkerSystemColorsEnabled,
    UIAccessibilityDarkerSystemColorsStatusDidChangeNotification,
    UIAccessibilityIsReduceMotionEnabled, UIAccessibilityReduceMotionStatusDidChangeNotification,
    UIAccessibilitySwitchControlStatusDidChangeNotification,
    UIAccessibilityVoiceOverStatusDidChangeNotification, UIApplication, UIApplicationDelegate,
    UIContentSizeCategory, UIContentSizeCategoryAccessibilityExtraExtraExtraLarge,
    UIContentSizeCategoryAccessibilityExtraExtraLarge,
    UIContentSizeCategoryAccessibilityExtraLarge, UIContentSizeCategoryAccessibilityLarge,
    UIContentSizeCategoryAccessibilityMedium, UIContentSizeCategoryExtraExtraExtraLarge,
    UIContentSizeCategoryExtraExtraLarge, UIContentSizeCategoryExtraLarge,
    UIContentSizeCategoryExtraSmall, UIContentSizeCategoryMedium, UIContentSizeCategorySmall,
    UIKeyboardFrameEndUserInfoKey, UIKeyboardWillChangeFrameNotification,
    UIPointerLockStateDidChangeNotification, UIResponder, UIScene, UISceneConfiguration,
    UISceneConnectionOptions, UISceneDelegate, UISceneSession, UIScreen, UITraitCollection,
    UITraitEnvironment, UIUserActivityRestoring, UIUserInterfaceStyle, UIViewController, UIWindow,
    UIWindowScene, UIWindowSceneDelegate,
};

use self::view::VisoView;
use super::drive::Drive;
use crate::control::{ControlFlow, PlatformError, TextEntry, WindowConfig, WindowId};
use crate::event::{Appearance, ColorScheme, CursorIcon, RawEvent};
use crate::handler::AppHandler;
use crate::locale;
use crate::menu::Menu;
use crate::{Instant, PlatformApp, RawWindowHandle, SemanticsDelta, Window};

/// The only window's id.
const WINDOW: WindowId = WindowId(1);

/// Loop state shared by every UIKit callback. Main-thread only; each field
/// is borrowed briefly and never across a handler call, so callbacks the
/// handler's own UIKit calls trigger can queue events freely.
struct Loop {
    events: RefCell<VecDeque<RawEvent>>,
    /// A redraw is due at the next display beat.
    redraw: Cell<bool>,
    /// The app and the runtime's handler, installed by `run` and never
    /// removed: `UIApplicationMain` does not return, so the borrows they came
    /// from live for the rest of the process.
    drive: Cell<Option<Drive>>,
    /// A drain is on the stack; nested callbacks only queue.
    driving: Cell<bool>,
    appearance: Cell<Appearance>,
    locale: RefCell<String>,
    launched: Cell<bool>,
    suspended: Cell<bool>,
    focused: Cell<bool>,
    scene: RefCell<Option<Retained<UIWindowScene>>>,
    view: RefCell<Option<Retained<VisoView>>>,
    display_link: RefCell<Option<Retained<CADisplayLink>>>,
    wake_timer: RefCell<Option<Retained<NSTimer>>>,
    target: RefCell<Option<Retained<LoopTarget>>>,
    access: RefCell<access::Access>,
    /// The status bar and home indicator are hidden.
    fullscreen: Cell<bool>,
    /// The app asked for the pointer locked.
    lock_wanted: Cell<bool>,
    /// The scene's pointer is locked, as the system last said.
    pointer_locked: Cell<bool>,
    controller: RefCell<Option<Retained<VisoViewController>>>,
}

thread_local! {
    static LOOP: Loop = Loop {
        events: RefCell::new(VecDeque::new()),
        redraw: Cell::new(false),
        drive: Cell::new(None),
        driving: Cell::new(false),
        appearance: Cell::new(Appearance::default()),
        locale: RefCell::new(String::new()),
        launched: Cell::new(false),
        suspended: Cell::new(false),
        focused: Cell::new(false),
        scene: RefCell::new(None),
        view: RefCell::new(None),
        display_link: RefCell::new(None),
        wake_timer: RefCell::new(None),
        target: RefCell::new(None),
        access: RefCell::new(access::Access::new()),
        fullscreen: Cell::new(false),
        lock_wanted: Cell::new(false),
        pointer_locked: Cell::new(false),
        controller: RefCell::new(None),
    };
}

/// Queue an event for the next drain.
fn push(event: RawEvent) {
    LOOP.with(|l| l.events.borrow_mut().push_back(event));
}

/// Resets `Loop::driving` on every exit from [`drive`], unwinding included.
struct DrivingGuard;

impl Drop for DrivingGuard {
    fn drop(&mut self) {
        LOOP.with(|l| l.driving.set(false));
    }
}

/// Hand every queued event to the handler, then arm the wake its last
/// answer asked for. A call made while a drain is already running (a UIKit
/// callback triggered by the handler's own request) returns at once: the
/// running drain picks the new events up.
fn drive() {
    let Some(drive) = LOOP.with(|l| {
        if l.driving.get() {
            return None;
        }
        let drive = l.drive.get()?;
        l.driving.set(true);
        Some(drive)
    }) else {
        return;
    };
    let _guard = DrivingGuard;
    let mut last = None;
    while let Some(event) = LOOP.with(|l| l.events.borrow_mut().pop_front()) {
        // SAFETY: the drive was taken from the borrows passed to `run`, which
        // are never released (`UIApplicationMain` does not return) and which
        // `run` no longer touches. `driving` admits one drain at a time, so
        // these are the only live reborrows.
        let (app, handler) = unsafe { drive.get() };
        last = Some(deliver(app, handler, event));
    }
    if let Some(flow) = last {
        schedule_wake(flow);
    }
}

/// Hand one event to the handler, completing the clipboard handshakes: a
/// `CopyRequested` reply the handler filled is written to the pasteboard once
/// the handler returns, and a `PasteRequested` reads the kinds it named and
/// delivers them when there are some.
fn deliver(
    app: &mut dyn PlatformApp,
    handler: &mut dyn AppHandler,
    event: RawEvent,
) -> ControlFlow {
    enum After {
        Copy(crate::ClipboardReply),
        Paste(WindowId, crate::PasteReply),
    }
    let after = match &event {
        RawEvent::CopyRequested { reply, .. } => Some(After::Copy(reply.clone())),
        RawEvent::PasteRequested { window, reply } => Some(After::Paste(*window, reply.clone())),
        _ => None,
    };
    let flow = handler.handle(app, event);
    match after {
        Some(After::Copy(reply)) => {
            if let Some(data) = reply.take() {
                clipboard::write(data);
            }
        }
        Some(After::Paste(window, reply)) => {
            let kinds = reply.get();
            if kinds.any() {
                let data = clipboard::read(kinds);
                if !data.is_empty() {
                    push(RawEvent::Paste {
                        window,
                        data,
                        token: None,
                    });
                }
            }
        }
        None => {}
    }
    flow
}

/// Arm (or disarm) the one-shot timer behind [`ControlFlow::WaitUntil`].
/// iOS apps do not exit, so [`ControlFlow::Exit`] only idles the loop.
fn schedule_wake(flow: ControlFlow) {
    if let Some(old) = LOOP.with(|l| l.wake_timer.take()) {
        old.invalidate();
    }
    let ControlFlow::WaitUntil(deadline) = flow else {
        return;
    };
    let Some(target) = LOOP.with(|l| l.target.borrow().clone()) else {
        return;
    };
    let delay = deadline.saturating_duration_since(Instant::now());
    // SAFETY: `LoopTarget` implements `wake:` with the `(&self, &NSTimer)`
    // signature a timer calls; the timer retains its target until it fires.
    let timer = unsafe {
        NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
            delay.as_secs_f64(),
            &target,
            sel!(wake:),
            None,
            false,
        )
    };
    LOOP.with(|l| *l.wake_timer.borrow_mut() = Some(timer));
}

/// Ask for a frame at the next display beat.
fn request_frame() {
    LOOP.with(|l| {
        l.redraw.set(true);
        if let Some(link) = l.display_link.borrow().as_ref() {
            link.setPaused(false);
        }
    });
}

/// The appearance `traits` describe, with the accessibility settings that
/// have no trait.
fn appearance_of(traits: &UITraitCollection) -> Appearance {
    // SAFETY: plain property reads on a live trait collection.
    let (style, contrast) =
        unsafe { (traits.userInterfaceStyle(), traits.accessibilityContrast()) };
    Appearance {
        color_scheme: if style == UIUserInterfaceStyle::Dark {
            ColorScheme::Dark
        } else {
            ColorScheme::Light
        },
        high_contrast: contrast == UIAccessibilityContrast::High
            || UIAccessibilityDarkerSystemColorsEnabled(),
        reduce_motion: UIAccessibilityIsReduceMotionEnabled(),
        // SAFETY: as above.
        text_scale: text_scale_of(&*unsafe { traits.preferredContentSizeCategory() }),
        overlay_scroll_bars: true,
        // An app's tint, not the system, is its accent.
        accent: None,
        double_click: None,
    }
}

/// The text scale of a Dynamic Type size: its body text size over the
/// default (`Large`, 17 pt) body size.
fn text_scale_of(category: &UIContentSizeCategory) -> f32 {
    // SAFETY: the categories are immutable UIKit constants.
    let sizes = unsafe {
        [
            (UIContentSizeCategoryExtraSmall, 14.0),
            (UIContentSizeCategorySmall, 15.0),
            (UIContentSizeCategoryMedium, 16.0),
            (UIContentSizeCategoryExtraLarge, 19.0),
            (UIContentSizeCategoryExtraExtraLarge, 21.0),
            (UIContentSizeCategoryExtraExtraExtraLarge, 23.0),
            (UIContentSizeCategoryAccessibilityMedium, 28.0),
            (UIContentSizeCategoryAccessibilityLarge, 33.0),
            (UIContentSizeCategoryAccessibilityExtraLarge, 40.0),
            (UIContentSizeCategoryAccessibilityExtraExtraLarge, 47.0),
            (UIContentSizeCategoryAccessibilityExtraExtraExtraLarge, 53.0),
        ]
    };
    sizes
        .iter()
        .find(|(name, _)| name.isEqualToString(category))
        .map_or(1.0, |(_, size)| size / 17.0)
}

/// The user's first preferred language, as a BCP 47 tag.
fn current_locale() -> String {
    NSLocale::preferredLanguages()
        .firstObject()
        .map_or_else(|| "und".to_owned(), |tag| locale::bcp47(&tag.to_string()))
}

/// Re-read the locale and queue `LocaleChanged` if it moved.
fn report_locale() {
    let now = current_locale();
    if LOOP.with(|l| l.locale.replace(now.clone())) != now {
        push(RawEvent::LocaleChanged { locale: now });
    }
}

/// Re-read the appearance and queue `AppearanceChanged` if it moved.
fn report_appearance(traits: &UITraitCollection) {
    let now = appearance_of(traits);
    if LOOP.with(|l| l.appearance.replace(now)) != now {
        push(RawEvent::AppearanceChanged(now));
    }
}

/// Report the launch once: from the scene connection, or from
/// `didFinishLaunching` for an app without scenes. It goes ahead of
/// anything queued before it.
fn launch() {
    if !LOOP.with(|l| l.launched.replace(true)) {
        LOOP.with(|l| {
            l.focused.set(true);
            l.events.borrow_mut().push_front(RawEvent::AppLaunched);
        });
        drive();
    }
}

/// Queue the opening of `url`.
fn open_url(url: &NSURL) {
    if let Some(text) = url.absoluteString() {
        push(RawEvent::OpenUrl {
            url: text.to_string(),
        });
    }
}

/// Queue the opening of each URL `contexts` carry.
fn open_contexts(contexts: &NSSet<UIOpenURLContext>) {
    for context in contexts.iter() {
        open_url(&context.URL());
    }
}

/// Queue the web link `activity` continues, if it browses one: a universal
/// link to a host the app declares.
fn open_activity(activity: &NSUserActivity) -> bool {
    // SAFETY: an immutable string constant Foundation exports.
    let browsing = unsafe { NSUserActivityTypeBrowsingWeb };
    match activity.webpageURL() {
        Some(url) if activity.activityType().isEqualToString(browsing) => {
            open_url(&url);
            true
        }
        _ => false,
    }
}

/// Foreground/background transitions, reported once however many of the
/// app and scene callbacks announce them.
fn set_suspended(suspended: bool) {
    if LOOP.with(|l| l.suspended.replace(suspended)) != suspended {
        push(if suspended {
            RawEvent::Suspended
        } else {
            RawEvent::Resumed
        });
        drive();
    }
}

fn set_focused(focused: bool) {
    // The pointer goes back to the user with the focus.
    if !focused && let Some(controller) = LOOP.with(|l| l.controller.borrow().clone()) {
        lock::set(&controller, false);
    }
    if LOOP.with(|l| l.focused.replace(focused)) != focused {
        if LOOP.with(|l| l.view.borrow().is_some()) {
            push(RawEvent::WindowFocused {
                window: WINDOW,
                focused,
            });
        }
        drive();
    }
}

/// Runs `f` without letting a panic unwind into UIKit. Every frame of the
/// app runs inside a UIKit callback, so a panic ends the app as an unwind
/// out of `main` would — its message already written by the panic hook — by
/// aborting: the system records the signal, which is how the app is known to
/// have failed.
fn guarded(f: impl FnOnce()) {
    if catch_unwind(AssertUnwindSafe(f)).is_err() {
        std::process::abort();
    }
}

/// The native iOS application.
pub struct IosApp {
    mtm: MainThreadMarker,
    window: Option<IosWindow>,
    /// The game controllers, read from the first poll.
    gamepads: crate::gamepad::Gamepads,
}

impl IosApp {
    pub fn new() -> Result<Self, PlatformError> {
        let Some(mtm) = MainThreadMarker::new() else {
            return Err(PlatformError::Backend(
                "IosApp must be created on the main thread".into(),
            ));
        };
        let target = LoopTarget::new(mtm);
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: each selector is implemented by `LoopTarget` with the
        // `(&self, &NSNotification)` signature the center calls, and the
        // target lives in `LOOP` for the rest of the process. The names are
        // immutable UIKit constants.
        unsafe {
            for (selector, name) in [
                (
                    sel!(keyboardWillChangeFrame:),
                    UIKeyboardWillChangeFrameNotification,
                ),
                (
                    sel!(accessibilityChanged:),
                    UIAccessibilityReduceMotionStatusDidChangeNotification,
                ),
                (
                    sel!(accessibilityChanged:),
                    UIAccessibilityDarkerSystemColorsStatusDidChangeNotification,
                ),
                (
                    sel!(assistiveTechnologyChanged:),
                    UIAccessibilityVoiceOverStatusDidChangeNotification,
                ),
                (
                    sel!(assistiveTechnologyChanged:),
                    UIAccessibilitySwitchControlStatusDidChangeNotification,
                ),
                (sel!(localeChanged:), NSCurrentLocaleDidChangeNotification),
                (
                    sel!(pointerLockChanged:),
                    UIPointerLockStateDidChangeNotification,
                ),
            ] {
                center.addObserver_selector_name_object(&target, selector, Some(name), None);
            }
        }
        // SAFETY: reading the current trait collection has no preconditions
        // on the main thread.
        let traits = unsafe { UITraitCollection::currentTraitCollection() };
        let appearance = appearance_of(&traits);
        LOOP.with(|l| {
            l.appearance.set(appearance);
            *l.locale.borrow_mut() = current_locale();
            *l.target.borrow_mut() = Some(target);
        });
        Ok(Self {
            mtm,
            window: None,
            gamepads: crate::gamepad::Gamepads::default(),
        })
    }
}

impl PlatformApp for IosApp {
    fn create_window(&mut self, config: WindowConfig) -> Result<WindowId, PlatformError> {
        if self.window.is_some() {
            return Err(PlatformError::WindowCreation(
                "iOS apps have a single window".into(),
            ));
        }
        let mtm = self.mtm;
        let scene = LOOP.with(|l| l.scene.borrow().clone());
        let window = match &scene {
            Some(scene) => UIWindow::initWithWindowScene(UIWindow::alloc(mtm), scene),
            // Without scenes the window covers the main screen.
            #[allow(deprecated)]
            None => {
                let bounds = UIScreen::mainScreen(mtm).bounds();
                UIWindow::initWithFrame(UIWindow::alloc(mtm), bounds)
            }
        };
        if let Some(scene) = &scene {
            scene.setTitle(Some(&NSString::from_str(&config.title)));
        }
        let screen = window.screen();
        let view = VisoView::new(mtm, WINDOW, window.bounds(), screen.nativeScale());
        // SAFETY: `init` is UIViewController's designated initializer with
        // no arguments; the subclass adds no state.
        let controller: Retained<VisoViewController> =
            unsafe { msg_send![VisoViewController::alloc(mtm), init] };
        controller.setView(Some(&view));
        LOOP.with(|l| *l.view.borrow_mut() = Some(view.clone()));
        LOOP.with(|l| *l.controller.borrow_mut() = Some(controller.clone()));

        let target = LOOP.with(|l| l.target.borrow().clone());
        if let Some(target) = target {
            // SAFETY: `LoopTarget` implements `tick:` with the
            // `(&self, &CADisplayLink)` signature the link calls; the link
            // retains its target and lives in `LOOP` for the process.
            let link =
                unsafe { CADisplayLink::displayLinkWithTarget_selector(&target, sel!(tick:)) };
            let max = screen.maximumFramesPerSecond().max(60) as f32;
            link.setPreferredFrameRateRange(CAFrameRateRange {
                minimum: 30.0_f32.min(max),
                maximum: max,
                preferred: max,
            });
            link.setPaused(!LOOP.with(|l| l.redraw.get()));
            // SAFETY: the main run loop and its common-modes constant are
            // valid for the process; common modes keep frames flowing while
            // UIKit tracks a touch.
            unsafe { link.addToRunLoop_forMode(&NSRunLoop::mainRunLoop(), NSRunLoopCommonModes) };
            LOOP.with(|l| *l.display_link.borrow_mut() = Some(link));
        }

        window.setRootViewController(Some(&controller));
        window.makeKeyAndVisible();
        view.becomeFirstResponder();
        report_appearance(&view.traitCollection());
        self.window = Some(IosWindow {
            view,
            _window: window,
            controller,
        });
        Ok(WINDOW)
    }

    fn run(&mut self, handler: &mut dyn AppHandler) {
        // `UIApplicationMain` never returns, so both borrows outlive every use
        // `drive` makes of them; from here on only the drive reaches them.
        let mtm = self.mtm;
        LOOP.with(|l| l.drive.set(Some(Drive::new(self, handler))));
        // Register the classes UIKit instantiates by name.
        let _ = (AppDelegate::class(), SceneDelegate::class());
        UIApplication::main(None, Some(ns_string!("VisoAppDelegate")), mtm);
    }

    fn window(&self, id: WindowId) -> Option<&dyn Window> {
        self.window
            .as_ref()
            .filter(|_| id == WINDOW)
            .map(|w| w as &dyn Window)
    }

    fn request_redraw(&mut self, window: WindowId) {
        if window == WINDOW {
            request_frame();
        }
    }

    fn update_accessibility(&mut self, window: WindowId, delta: SemanticsDelta) {
        if let Some(w) = self.window.as_ref().filter(|_| window == WINDOW) {
            access::update(&w.view, delta);
        }
    }

    fn set_menu(&mut self, _menu: &Menu) {}

    fn close_window(&mut self, _window: WindowId) {}

    fn set_clipboard(&mut self, data: crate::ClipboardData) {
        clipboard::write(data);
    }

    fn request_paste(&mut self, window: WindowId, kinds: crate::ClipboardKinds, token: u32) {
        push(RawEvent::Paste {
            window,
            data: clipboard::read(kinds),
            token: Some(token),
        });
    }

    fn set_fullscreen(&mut self, window: WindowId, fullscreen: bool) {
        let Some(shown) = self.window.as_ref().filter(|_| window == WINDOW) else {
            return;
        };
        if LOOP.with(|l| l.fullscreen.replace(fullscreen)) != fullscreen {
            shown.controller.setNeedsStatusBarAppearanceUpdate();
            shown.controller.setNeedsUpdateOfHomeIndicatorAutoHidden();
            push(RawEvent::FullscreenChanged { window, fullscreen });
        }
    }

    fn set_cursor(&mut self, _window: WindowId, _icon: CursorIcon) {}

    fn set_pointer_lock(&mut self, window: WindowId, lock: bool) {
        if let Some(shown) = self.window.as_ref().filter(|_| window == WINDOW) {
            lock::set(&shown.controller, lock);
        }
    }

    fn set_text_entry(&mut self, window: WindowId, entry: Option<TextEntry>) {
        if let Some(w) = self.window.as_ref().filter(|_| window == WINDOW) {
            w.view.set_text_entry(entry);
        }
    }

    fn set_surrounding_text(&mut self, window: WindowId, text: Option<&crate::SurroundingText>) {
        if let Some(w) = self.window.as_ref().filter(|_| window == WINDOW) {
            w.view.set_surrounding_text(text);
        }
    }

    fn show_soft_keyboard(&mut self, window: WindowId, show: bool) {
        if let Some(w) = self.window.as_ref().filter(|_| window == WINDOW) {
            w.view.show_soft_keyboard(show);
        }
    }

    fn set_edit_menu(&mut self, window: WindowId, menu: Option<crate::EditMenu>) {
        if let Some(w) = self.window.as_ref().filter(|_| window == WINDOW) {
            w.view.set_edit_menu(menu);
        }
    }

    fn set_loupe(&mut self, window: WindowId, loupe: Option<crate::Loupe>) {
        if let Some(w) = self.window.as_ref().filter(|_| window == WINDOW) {
            w.view.set_loupe(loupe);
        }
    }

    fn appearance(&self) -> Appearance {
        LOOP.with(|l| l.appearance.get())
    }

    fn locale(&self) -> String {
        LOOP.with(|l| l.locale.borrow().clone())
    }

    fn framed_windows(&self) -> bool {
        false
    }

    fn capabilities(&self) -> crate::PlatformCaps {
        // An iPad's pointer locks while the app fills the screen (iPadOS 14).
        let pad = MainThreadMarker::new().is_some_and(|mtm| {
            objc2_ui_kit::UIDevice::currentDevice(mtm).userInterfaceIdiom()
                == objc2_ui_kit::UIUserInterfaceIdiom::Pad
        });
        crate::PlatformCaps {
            pointer_lock: pad,
            badge: true,
            ..crate::PlatformCaps::MOBILE
        }
    }

    /// The home-screen icon shows counts alone, and only once the user let
    /// the app badge it: the app asks for notification authorization with
    /// the badge option first (a notifications extension does). Without it
    /// the system ignores the count.
    fn set_badge(&mut self, badge: Option<&crate::AppBadge>) {
        let count = badge.and_then(crate::AppBadge::count).unwrap_or(0);
        let count = isize::try_from(count).unwrap_or(isize::MAX);
        if let Some(class) = objc2::runtime::AnyClass::get(c"UNUserNotificationCenter") {
            // SAFETY: `currentNotificationCenter` is a class method returning
            // the shared center (or nil), retained by the caller here.
            let center: Option<Retained<AnyObject>> =
                unsafe { msg_send![class, currentNotificationCenter] };
            if let Some(center) = center {
                // SAFETY: `respondsToSelector:` on a live object.
                let has: bool = unsafe {
                    msg_send![&center, respondsToSelector: sel!(setBadgeCount:withCompletionHandler:)]
                };
                if has {
                    let done: Option<&block2::DynBlock<dyn Fn(*mut objc2_foundation::NSError)>> =
                        None;
                    // SAFETY: the center answers the selector (iOS 16 and
                    // later); the count is an `NSInteger` and the completion
                    // handler may be nil.
                    let () = unsafe {
                        msg_send![&center, setBadgeCount: count, withCompletionHandler: done]
                    };
                    return;
                }
            }
        }
        #[allow(deprecated)]
        UIApplication::sharedApplication(self.mtm).setApplicationIconBadgeNumber(count);
    }

    fn loop_waker(&self) -> crate::LoopWaker {
        extern "C" fn kick(_: *mut std::ffi::c_void) {
            push(RawEvent::Wakeup);
            drive();
        }
        crate::LoopWaker::new(|| super::main_queue::post_to_main(kick))
    }

    fn poll_gamepads(&mut self, events: &mut Vec<crate::GamepadEvent>) {
        if !self.gamepads.started() {
            let waker = self.loop_waker();
            self.gamepads.start(waker);
        }
        self.gamepads.poll(events);
    }

    fn rumble_gamepad(&mut self, id: crate::GamepadId, rumble: crate::Rumble) -> bool {
        self.gamepads.rumble(id, rumble)
    }
}

pub struct IosWindow {
    view: Retained<VisoView>,
    _window: Retained<UIWindow>,
    controller: Retained<VisoViewController>,
}

impl Window for IosWindow {
    fn id(&self) -> WindowId {
        WINDOW
    }

    fn request_redraw(&self) {
        request_frame();
    }

    fn set_title(&mut self, title: &str) {
        if let Some(scene) = LOOP.with(|l| l.scene.borrow().clone()) {
            scene.setTitle(Some(&NSString::from_str(title)));
        }
    }

    fn scale_factor(&self) -> f64 {
        self.view.contentScaleFactor()
    }

    fn inner_size(&self) -> (u32, u32) {
        self.view.pixel_size()
    }

    fn raw_handle(&self) -> RawWindowHandle {
        RawWindowHandle::UiKit {
            ui_view: Retained::as_ptr(&self.view) as *mut c_void,
        }
    }
}

define_class!(
    // SAFETY: UIViewController may be subclassed to answer how its screen
    // shows; `VisoViewController` adds no state and has no `Drop` impl.
    #[unsafe(super(UIViewController, UIResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VisoViewController"]
    struct VisoViewController;

    unsafe impl NSObjectProtocol for VisoViewController {}

    impl VisoViewController {
        #[unsafe(method(prefersStatusBarHidden))]
        fn prefers_status_bar_hidden(&self) -> bool {
            LOOP.with(|l| l.fullscreen.get())
        }

        #[unsafe(method(prefersHomeIndicatorAutoHidden))]
        fn prefers_home_indicator_auto_hidden(&self) -> bool {
            LOOP.with(|l| l.fullscreen.get())
        }

        #[unsafe(method(prefersPointerLocked))]
        fn prefers_pointer_locked(&self) -> bool {
            lock::wanted()
        }
    }
);

define_class!(
    // SAFETY: NSObject has no subclassing requirements; `LoopTarget` has no
    // `Drop` impl.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VisoLoopTarget"]
    struct LoopTarget;

    unsafe impl NSObjectProtocol for LoopTarget {}

    impl LoopTarget {
        #[unsafe(method(tick:))]
        fn tick(&self, link: &CADisplayLink) {
            guarded(|| {
                if LOOP.with(|l| l.redraw.replace(false)) {
                    push(RawEvent::RedrawRequested { window: WINDOW });
                    drive();
                }
                // Idle between redraws: a paused link costs nothing.
                if !LOOP.with(|l| l.redraw.get()) {
                    link.setPaused(true);
                }
            });
        }

        #[unsafe(method(wake:))]
        fn wake(&self, _timer: &NSTimer) {
            guarded(|| {
                LOOP.with(|l| l.wake_timer.take());
                push(RawEvent::Wakeup);
                drive();
            });
        }

        #[unsafe(method(keyboardWillChangeFrame:))]
        fn keyboard_will_change_frame(&self, note: &NSNotification) {
            guarded(|| {
                let Some(view) = LOOP.with(|l| l.view.borrow().clone()) else {
                    return;
                };
                let Some(info) = note.userInfo() else { return };
                // SAFETY: UIKit documents the user info as an
                // `NSDictionary<NSString, id>`; the key is an immutable
                // UIKit constant.
                let info: &NSDictionary<NSString, AnyObject> = unsafe { info.cast_unchecked() };
                // SAFETY: as above, an immutable UIKit constant.
                let key = unsafe { UIKeyboardFrameEndUserInfoKey };
                let Some(value) = info.objectForKey(key) else {
                    return;
                };
                let Some(value) = value.downcast_ref::<NSValue>() else {
                    return;
                };
                view.keyboard_frame_changed(value);
            });
        }

        #[unsafe(method(accessibilityChanged:))]
        fn accessibility_changed(&self, _note: &NSNotification) {
            guarded(|| {
                let traits = match LOOP.with(|l| l.view.borrow().clone()) {
                    Some(view) => view.traitCollection(),
                    // SAFETY: as in `IosApp::new`.
                    None => unsafe { UITraitCollection::currentTraitCollection() },
                };
                report_appearance(&traits);
                drive();
            });
        }

        #[unsafe(method(localeChanged:))]
        fn locale_changed(&self, _note: &NSNotification) {
            guarded(|| {
                report_locale();
                drive();
            });
        }

        #[unsafe(method(pointerLockChanged:))]
        fn pointer_lock_changed(&self, _note: &NSNotification) {
            guarded(lock::changed);
        }

        #[unsafe(method(assistiveTechnologyChanged:))]
        fn assistive_technology_changed(&self, _note: &NSNotification) {
            guarded(access::assistive_technology_changed);
        }
    }
);

impl LoopTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: `init` is NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    // SAFETY: UIResponder has no subclassing requirements; the delegate has
    // no `Drop` impl.
    #[unsafe(super(UIResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VisoAppDelegate"]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl UIApplicationDelegate for AppDelegate {
        #[unsafe(method(application:didFinishLaunchingWithOptions:))]
        fn did_finish_launching(
            &self,
            _app: &UIApplication,
            _options: Option<&NSDictionary>,
        ) -> bool {
            guarded(|| {
                let manifest = objc2_foundation::NSBundle::mainBundle()
                    .objectForInfoDictionaryKey(ns_string!("UIApplicationSceneManifest"));
                // With scenes, launch waits for the scene to connect.
                if manifest.is_none() {
                    launch();
                }
            });
            true
        }

        #[unsafe(method_id(application:configurationForConnectingSceneSession:options:))]
        fn configuration_for_scene(
            &self,
            _app: &UIApplication,
            session: &UISceneSession,
            _options: &UISceneConnectionOptions,
        ) -> Retained<UISceneConfiguration> {
            let config = UISceneConfiguration::configurationWithName_sessionRole(
                None,
                &session.role(),
                self.mtm(),
            );
            // SAFETY: `SceneDelegate` is a UIResponder conforming to
            // `UIWindowSceneDelegate`, as the configuration requires.
            unsafe { config.setDelegateClass(Some(SceneDelegate::class())) };
            config
        }

        #[unsafe(method(applicationDidBecomeActive:))]
        fn did_become_active(&self, _app: &UIApplication) {
            guarded(|| set_focused(true));
        }

        #[unsafe(method(applicationWillResignActive:))]
        fn will_resign_active(&self, _app: &UIApplication) {
            guarded(|| set_focused(false));
        }

        #[unsafe(method(applicationDidEnterBackground:))]
        fn did_enter_background(&self, _app: &UIApplication) {
            guarded(|| set_suspended(true));
        }

        #[unsafe(method(applicationWillEnterForeground:))]
        fn will_enter_foreground(&self, _app: &UIApplication) {
            guarded(|| set_suspended(false));
        }

        #[unsafe(method(applicationDidReceiveMemoryWarning:))]
        fn did_receive_memory_warning(&self, _app: &UIApplication) {
            guarded(|| {
                push(RawEvent::LowMemory);
                drive();
            });
        }

        /// A URL of a declared scheme, for an app without scenes.
        #[unsafe(method(application:openURL:options:))]
        fn application_open_url(
            &self,
            _app: &UIApplication,
            url: &NSURL,
            _options: &NSDictionary<NSString, AnyObject>,
        ) -> bool {
            guarded(|| {
                open_url(url);
                drive();
            });
            true
        }

        /// A universal link, for an app without scenes.
        #[unsafe(method(application:continueUserActivity:restorationHandler:))]
        fn continue_activity(
            &self,
            _app: &UIApplication,
            activity: &NSUserActivity,
            _restore: &block2::DynBlock<
                dyn Fn(*mut NSArray<ProtocolObject<dyn UIUserActivityRestoring>>),
            >,
        ) -> bool {
            let mut opened = false;
            guarded(|| {
                opened = open_activity(activity);
                drive();
            });
            opened
        }
    }
);

define_class!(
    // SAFETY: UIResponder has no subclassing requirements; the delegate has
    // no `Drop` impl.
    #[unsafe(super(UIResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VisoSceneDelegate"]
    struct SceneDelegate;

    unsafe impl NSObjectProtocol for SceneDelegate {}

    unsafe impl UISceneDelegate for SceneDelegate {
        #[unsafe(method(scene:willConnectToSession:options:))]
        fn will_connect(
            &self,
            scene: &UIScene,
            _session: &UISceneSession,
            options: &UISceneConnectionOptions,
        ) {
            guarded(|| {
                let Some(scene) = scene.downcast_ref::<UIWindowScene>() else {
                    return;
                };
                LOOP.with(|l| *l.scene.borrow_mut() = Some(scene.retain()));
                launch();
                // What the app was launched to open comes with the
                // connection, not through the calls below.
                // Both sets are nil on a plain launch, though the headers
                // declare them non-null: read them as optional.
                // SAFETY: both getters take no arguments and answer a set
                // of the element type named, or nil.
                let contexts: Option<Retained<NSSet<UIOpenURLContext>>> =
                    unsafe { msg_send![options, URLContexts] };
                if let Some(contexts) = contexts {
                    open_contexts(&contexts);
                }
                // SAFETY: as above.
                let activities: Option<Retained<NSSet<NSUserActivity>>> =
                    unsafe { msg_send![options, userActivities] };
                for activity in activities.iter().flat_map(|set| set.iter()) {
                    open_activity(&activity);
                }
                drive();
            });
        }

        #[unsafe(method(scene:openURLContexts:))]
        fn open_url_contexts(&self, _scene: &UIScene, contexts: &NSSet<UIOpenURLContext>) {
            guarded(|| {
                open_contexts(contexts);
                drive();
            });
        }

        #[unsafe(method(scene:continueUserActivity:))]
        fn continue_activity(&self, _scene: &UIScene, activity: &NSUserActivity) {
            guarded(|| {
                open_activity(activity);
                drive();
            });
        }

        #[unsafe(method(sceneDidBecomeActive:))]
        fn did_become_active(&self, _scene: &UIScene) {
            guarded(|| set_focused(true));
        }

        #[unsafe(method(sceneWillResignActive:))]
        fn will_resign_active(&self, _scene: &UIScene) {
            guarded(|| set_focused(false));
        }

        #[unsafe(method(sceneDidEnterBackground:))]
        fn did_enter_background(&self, _scene: &UIScene) {
            guarded(|| set_suspended(true));
        }

        #[unsafe(method(sceneWillEnterForeground:))]
        fn will_enter_foreground(&self, _scene: &UIScene) {
            guarded(|| set_suspended(false));
        }
    }

    unsafe impl UIWindowSceneDelegate for SceneDelegate {}
);
