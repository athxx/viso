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

mod view;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_foundation::{
    NSDictionary, NSNotification, NSNotificationCenter, NSObjectProtocol, NSRunLoop,
    NSRunLoopCommonModes, NSString, NSTimer, NSValue, ns_string,
};
use objc2_quartz_core::{CADisplayLink, CAFrameRateRange};
use objc2_ui_kit::{
    UIAccessibilityContrast, UIAccessibilityDarkerSystemColorsEnabled,
    UIAccessibilityDarkerSystemColorsStatusDidChangeNotification,
    UIAccessibilityIsReduceMotionEnabled, UIAccessibilityReduceMotionStatusDidChangeNotification,
    UIApplication, UIApplicationDelegate, UIKeyboardFrameEndUserInfoKey,
    UIKeyboardWillChangeFrameNotification, UIPasteboard, UIResponder, UIScene,
    UISceneConfiguration, UISceneConnectionOptions, UISceneDelegate, UISceneSession, UIScreen,
    UITraitCollection, UITraitEnvironment, UIUserInterfaceStyle, UIViewController, UIWindow,
    UIWindowScene, UIWindowSceneDelegate,
};

use self::view::VisoView;
use crate::control::{ControlFlow, LogicalRect, PlatformError, WindowConfig, WindowId};
use crate::event::{Appearance, ColorScheme, CursorIcon, RawEvent};
use crate::handler::AppHandler;
use crate::menu::Menu;
use crate::{Instant, PlatformApp, RawWindowHandle, Window};

/// The only window's id.
const WINDOW: WindowId = WindowId(1);

/// Loop state shared by every UIKit callback. Main-thread only; each field
/// is borrowed briefly and never across a handler call, so callbacks the
/// handler's own UIKit calls trigger can queue events freely.
struct Loop {
    events: RefCell<VecDeque<RawEvent>>,
    /// A redraw is due at the next display beat.
    redraw: Cell<bool>,
    /// The runtime's handler, installed by `run` and never removed:
    /// `UIApplicationMain` does not return, so the borrow it came from lives
    /// for the rest of the process.
    handler: Cell<Option<NonNull<dyn AppHandler>>>,
    /// A drain is on the stack; nested callbacks only queue.
    driving: Cell<bool>,
    appearance: Cell<Appearance>,
    launched: Cell<bool>,
    suspended: Cell<bool>,
    focused: Cell<bool>,
    scene: RefCell<Option<Retained<UIWindowScene>>>,
    view: RefCell<Option<Retained<VisoView>>>,
    display_link: RefCell<Option<Retained<CADisplayLink>>>,
    wake_timer: RefCell<Option<Retained<NSTimer>>>,
    target: RefCell<Option<Retained<LoopTarget>>>,
}

thread_local! {
    static LOOP: Loop = Loop {
        events: RefCell::new(VecDeque::new()),
        redraw: Cell::new(false),
        handler: Cell::new(None),
        driving: Cell::new(false),
        appearance: Cell::new(Appearance::default()),
        launched: Cell::new(false),
        suspended: Cell::new(false),
        focused: Cell::new(false),
        scene: RefCell::new(None),
        view: RefCell::new(None),
        display_link: RefCell::new(None),
        wake_timer: RefCell::new(None),
        target: RefCell::new(None),
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
    let Some(mut handler) = LOOP.with(|l| {
        if l.driving.get() {
            return None;
        }
        let handler = l.handler.get()?;
        l.driving.set(true);
        Some(handler)
    }) else {
        return;
    };
    let _guard = DrivingGuard;
    let mut last = None;
    while let Some(event) = LOOP.with(|l| l.events.borrow_mut().pop_front()) {
        // SAFETY: the pointer was taken from the `&mut dyn AppHandler` passed
        // to `run`, which is never released (`UIApplicationMain` does not
        // return). `driving` admits one drain at a time, so this is the only
        // live reborrow.
        last = Some(deliver(unsafe { handler.as_mut() }, event));
    }
    if let Some(flow) = last {
        schedule_wake(flow);
    }
}

/// Hand one event to the handler, completing the copy handshake: a
/// `CopyRequested` reply the handler filled is written to the pasteboard once
/// the handler returns.
fn deliver(handler: &mut dyn AppHandler, event: RawEvent) -> ControlFlow {
    let reply = match &event {
        RawEvent::CopyRequested { reply, .. } => Some(reply.clone()),
        _ => None,
    };
    let flow = handler.handle(event);
    if let Some(text) = reply.and_then(|r| r.take()) {
        write_pasteboard(&text);
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

fn write_pasteboard(text: &str) {
    // SAFETY: `setString:` copies the string; the general pasteboard is
    // process-wide and valid for the app's life.
    unsafe { UIPasteboard::generalPasteboard().setString(Some(&NSString::from_str(text))) };
}

fn read_pasteboard() -> Option<String> {
    // SAFETY: as in `write_pasteboard`.
    unsafe { UIPasteboard::generalPasteboard().string() }.map(|s| s.to_string())
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
/// `didFinishLaunching` for an app without scenes.
fn launch() {
    if !LOOP.with(|l| l.launched.replace(true)) {
        LOOP.with(|l| l.focused.set(true));
        push(RawEvent::AppLaunched);
        drive();
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

/// Run `f` without letting a panic unwind into UIKit.
fn guarded(f: impl FnOnce()) {
    let _ = catch_unwind(AssertUnwindSafe(f));
}

/// The native iOS application.
pub struct IosApp {
    mtm: MainThreadMarker,
    window: Option<IosWindow>,
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
            *l.target.borrow_mut() = Some(target);
        });
        Ok(Self { mtm, window: None })
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
        let controller = UIViewController::new(mtm);
        controller.setView(Some(&view));
        LOOP.with(|l| *l.view.borrow_mut() = Some(view.clone()));

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
            _controller: controller,
        });
        Ok(WINDOW)
    }

    fn run(&mut self, handler: &mut dyn AppHandler) {
        // SAFETY: only the trait object's lifetime is erased.
        // `UIApplicationMain` never returns, so `handler`'s borrow outlives
        // every use `drive` makes of the pointer.
        let handler: NonNull<dyn AppHandler> = unsafe {
            std::mem::transmute::<NonNull<dyn AppHandler + '_>, NonNull<dyn AppHandler + 'static>>(
                NonNull::from(handler),
            )
        };
        LOOP.with(|l| l.handler.set(Some(handler)));
        // Register the classes UIKit instantiates by name.
        let _ = (AppDelegate::class(), SceneDelegate::class());
        UIApplication::main(None, Some(ns_string!("VisoAppDelegate")), self.mtm);
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

    fn set_menu(&mut self, _menu: &Menu) {}

    fn close_window(&mut self, _window: WindowId) {}

    fn set_clipboard_text(&mut self, text: &str) {
        write_pasteboard(text);
    }

    fn request_paste(&mut self, window: WindowId) {
        if let Some(text) = read_pasteboard() {
            push(RawEvent::Paste { window, text });
        }
    }

    fn set_cursor(&mut self, _window: WindowId, _icon: CursorIcon) {}

    fn set_ime_area(&mut self, window: WindowId, caret: Option<LogicalRect>) {
        if let Some(w) = self.window.as_ref().filter(|_| window == WINDOW) {
            w.view.set_ime_area(caret);
        }
    }

    fn show_soft_keyboard(&mut self, window: WindowId, show: bool) {
        if let Some(w) = self.window.as_ref().filter(|_| window == WINDOW) {
            w.view.show_soft_keyboard(show);
        }
    }

    fn appearance(&self) -> Appearance {
        LOOP.with(|l| l.appearance.get())
    }

    fn framed_windows(&self) -> bool {
        false
    }
}

pub struct IosWindow {
    view: Retained<VisoView>,
    _window: Retained<UIWindow>,
    _controller: Retained<UIViewController>,
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
                let Some(value) = info.objectForKey(unsafe { UIKeyboardFrameEndUserInfoKey })
                else {
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
            _options: &UISceneConnectionOptions,
        ) {
            guarded(|| {
                let Some(scene) = scene.downcast_ref::<UIWindowScene>() else {
                    return;
                };
                LOOP.with(|l| *l.scene.borrow_mut() = Some(scene.retain()));
                launch();
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
