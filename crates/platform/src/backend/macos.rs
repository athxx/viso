//! Native macOS backend (objc2 / AppKit).
//!
//! Manual *pump* model rather than `[NSApp run]`: we call `finishLaunching`
//! once, then loop pulling one event at a time with
//! `nextEventMatchingMask:untilDate:inMode:dequeue:` and `sendEvent:`. The
//! `untilDate` is `distantFuture` when the runtime returns [`ControlFlow::Wait`]
//! (block, zero CPU), `distantPast` when it returns [`ControlFlow::Poll`]
//! (spin so the next display beat arrives promptly), and the timer's remaining
//! seconds when it returns [`ControlFlow::WaitUntil`] (block until a one-shot
//! timer is due, then synthesize a [`RawEvent::Wakeup`] so the scheduler fires
//! it in one frame). This keeps the frame loop fully under Viso's control — no
//! hidden AppKit run loop.
//!
//! Two AppKit objects feed the pump's shared queue:
//! - an `NSWindowDelegate` for resize/close/geometry, and
//! - a custom flipped content `NSView` (`VisoContentView`) that overrides the
//!   responder methods (mouse/scroll/key) and implements `NSTextInputClient`
//!   for IME. The view is *also* the GPU surface (`raw_handle` returns it), so
//!   there is one view per window doing both jobs.
//!
//! Both objects hold a raw `Rc<RefCell<..>>` to the pump's shared state and wrap
//! each callback in `catch_unwind` so a panic in Rust never unwinds across the
//! Objective-C frame.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::time::Instant;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, ProtocolObject, Sel};
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
    NSApplicationTerminateReply, NSBackingStoreType, NSEvent, NSEventMask, NSEventModifierFlags,
    NSMenu, NSMenuItem, NSTextInputClient, NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindow,
    NSWindowDelegate, NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSAttributedString, NSAttributedStringKey, NSDate,
    NSDefaultRunLoopMode, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSProcessInfo,
    NSRange, NSRangePointer, NSRect, NSSize, NSString,
};

use crate::RawWindowHandle;
use crate::control::{
    ControlFlow, LogicalRect, PlatformError, WindowChrome, WindowConfig, WindowId,
};
use crate::event::{
    AcceptCell, KeyCode, Modifiers, PointerButtons, PointerPhase, RawEvent, RawImePreedit, RawKey,
    RawPointer, RawScroll, RawText,
};
use crate::handler::AppHandler;
use crate::menu::{Accel, Menu, SystemAction};
use crate::{PlatformApp, Window};

/// Events the delegate/view produce, drained by the pump between OS events.
#[derive(Default)]
struct PumpQueue {
    events: VecDeque<RawEvent>,
    /// Windows asked to redraw; converted to `RedrawRequested` beats.
    redraws: VecDeque<WindowId>,
    /// Set true once a window has been asked to close and accepted.
    should_exit: bool,
}

/// Shared state the delegate/view mutate and the pump reads. `Rc<RefCell<..>>`
/// because all live on the main thread; no cross-thread sharing here.
type Shared = Rc<RefCell<PumpQueue>>;

/// The native macOS application.
pub struct MacApp {
    mtm: MainThreadMarker,
    app: Retained<NSApplication>,
    shared: Shared,
    next_window_id: u32,
    windows: Vec<MacWindow>,
    launched: bool,
    /// Kept alive: `setDelegate` holds only a weak reference.
    _app_delegate: Retained<AppDelegate>,
    /// Kept alive: each custom menu item's `-[NSMenuItem setTarget:]` holds only
    /// a weak reference, so the per-command target objects must outlive the menu.
    /// Replaced wholesale on each `set_menu`, dropping the previous menu's
    /// targets once AppKit has swapped in the new bar.
    menu_targets: Vec<Retained<MenuTarget>>,
}

impl MacApp {
    /// Acquire the shared application on the main thread.
    pub fn new() -> Result<Self, PlatformError> {
        let Some(mtm) = MainThreadMarker::new() else {
            return Err(PlatformError::Backend(
                "MacApp must be created on the main thread".into(),
            ));
        };
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        let shared: Shared = Rc::new(RefCell::new(PumpQueue::default()));

        // Standard "AppName" application menu with a "Quit AppName" item bound to
        // Command-Q. Without a main menu the OS has nowhere to route the Cmd+Q
        // shortcut, so the app cannot be quit from the keyboard. The Quit item
        // fires `quit:` on our delegate, which asks the pump to exit cleanly —
        // draining windows through the normal close path rather than the hard
        // `-[NSApplication terminate:]` that a manual pump cannot unwind.
        let app_delegate = AppDelegate::new(mtm, shared.clone());
        app.setDelegate(Some(ProtocolObject::from_ref(&*app_delegate)));
        install_main_menu(mtm, &app, &app_delegate);

        Ok(Self {
            mtm,
            app,
            shared,
            next_window_id: 1,
            windows: Vec::new(),
            launched: false,
            _app_delegate: app_delegate,
            menu_targets: Vec::new(),
        })
    }

    /// Pull the next queued synthetic event (delegate/view-produced), if any.
    fn next_synthetic(&self) -> Option<RawEvent> {
        let mut q = self.shared.borrow_mut();
        if let Some(w) = q.redraws.pop_front() {
            return Some(RawEvent::RedrawRequested { window: w });
        }
        q.events.pop_front()
    }
}

impl PlatformApp for MacApp {
    fn create_window(&mut self, config: WindowConfig) -> Result<WindowId, PlatformError> {
        let id = WindowId(self.next_window_id);
        self.next_window_id += 1;

        let (w, h) = config.logical_size;
        let content_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h));
        let mut style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Resizable
            | NSWindowStyleMask::Miniaturizable;
        // Self-drawn chrome keeps the native window (and its traffic lights) but
        // extends the content area under the title bar, so the app paints its own
        // caption over a full-height surface. The affordances stay live — only
        // the OS title text/background is stripped, below, after creation.
        if config.chrome == WindowChrome::SelfDrawn {
            style |= NSWindowStyleMask::FullSizeContentView;
        }

        // SAFETY: standard AppKit window init on the main thread; alloc is
        // main-thread-checked via `mtm`.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(self.mtm),
                content_rect,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setTitle(&NSString::from_str(&config.title));

        // Self-drawn chrome: strip the OS title bar's text and background so the
        // full-size content area shows through, then swap the titlebar
        // container's class so its transparent strip stops eating drags. Strict
        // order — hide the title, make the bar transparent, then defang the
        // container — matches AppKit's expectations; each step is a no-op for
        // native chrome, which keeps the OS-drawn title bar untouched.
        if config.chrome == WindowChrome::SelfDrawn {
            window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
            window.setTitlebarAppearsTransparent(true);
            defang_titlebar_container(&window);
        }

        window.center();

        let delegate = WindowDelegate::new(self.mtm, id, self.shared.clone(), config.chrome);
        let proto = ProtocolObject::from_ref(&*delegate);
        window.setDelegate(Some(proto));

        // Our own flipped content view is both the event source and the GPU
        // surface. Installing it as the content view replaces AppKit's default
        // one; the Metal backend later attaches a `CAMetalLayer` to it.
        let view = VisoContentView::new(self.mtm, id, self.shared.clone(), content_rect);
        window.setContentView(Some(&view));
        window.makeFirstResponder(Some(&view));

        window.makeKeyAndOrderFront(None);

        // Self-drawn chrome: report the initial traffic-light geometry so the app
        // can align its caption from the first frame, before any resize.
        if config.chrome == WindowChrome::SelfDrawn
            && let Some(buttons_rect) = traffic_lights_geom(&window)
        {
            self.shared
                .borrow_mut()
                .events
                .push_back(RawEvent::WindowChromeGeom {
                    window: id,
                    buttons_rect,
                });
        }

        self.windows.push(MacWindow {
            id,
            window,
            content_view: view,
            _delegate: delegate,
        });
        // The first frame is scheduled by the runtime after launch (paired with a
        // FirstFrame redraw reason), keeping a single beat source across backends.
        Ok(id)
    }

    fn run(&mut self, handler: &mut dyn AppHandler) {
        if !self.launched {
            self.app.finishLaunching();
            self.app.activate();
            self.launched = true;
        }

        if handler.handle(RawEvent::AppLaunched) == ControlFlow::Exit {
            return;
        }

        let mut flow = ControlFlow::Wait;
        loop {
            if self.shared.borrow().should_exit {
                break;
            }
            // Deliver any synthetic (delegate/view) events first — redraw beats,
            // resizes, closes, and the pointer/key/scroll/IME samples the view
            // enqueued while AppKit dispatched the last OS event.
            if let Some(event) = self.next_synthetic() {
                flow = handler.handle(event);
                if flow == ControlFlow::Exit {
                    break;
                }
                continue;
            }

            // Then pull one OS event. Block or spin per the last decision.
            // Each turn runs inside its own autorelease pool: AppKit's event
            // machinery vends autoreleased objects every iteration, and without
            // a pool draining them `nextEventMatchingMask:` degenerates into a
            // busy spin (it returns immediately instead of blocking on
            // `distantFuture`).
            let block = matches!(flow, ControlFlow::Wait | ControlFlow::WaitUntil(_));
            let got_event = autoreleasepool(|_| {
                let until = match flow {
                    // A one-shot timer's deadline: block only until it is due, so
                    // the pump wakes to fire it and otherwise spends zero CPU.
                    // `NSDate` from the remaining seconds (clamped at zero for a
                    // deadline already in the past, which fires on the next turn).
                    ControlFlow::WaitUntil(deadline) => {
                        let secs = deadline
                            .saturating_duration_since(Instant::now())
                            .as_secs_f64();
                        NSDate::dateWithTimeIntervalSinceNow(secs)
                    }
                    ControlFlow::Wait => NSDate::distantFuture(),
                    _ => NSDate::distantPast(),
                };
                // SAFETY: main-thread event pump; standard AppKit calls.
                let event = unsafe {
                    self.app.nextEventMatchingMask_untilDate_inMode_dequeue(
                        NSEventMask::Any,
                        Some(&until),
                        NSDefaultRunLoopMode,
                        true,
                    )
                };
                match event {
                    Some(ev) => {
                        // Forward to AppKit so the responder chain fires our
                        // view's overrides (mouse/key/scroll) and the window
                        // draws/interacts.
                        self.app.sendEvent(&ev);
                        true
                    }
                    None => false,
                }
            });
            if !got_event {
                match flow {
                    // A `WaitUntil` that returned no OS event means the timer
                    // deadline elapsed: synthesize a wakeup so the scheduler runs
                    // one frame and fires the due timer. The frame's own decision
                    // then re-arms the next deadline (or drops to `Wait`).
                    ControlFlow::WaitUntil(_) => {
                        flow = handler.handle(RawEvent::Wakeup);
                        if flow == ControlFlow::Exit {
                            break;
                        }
                    }
                    // Timed out on a poll with nothing pending and no animation:
                    // drop to blocking on the next turn.
                    _ if !block => flow = ControlFlow::Wait,
                    _ => {}
                }
            }
        }
    }

    fn window(&self, id: WindowId) -> Option<&dyn Window> {
        self.windows
            .iter()
            .find(|w| w.id == id)
            .map(|w| w as &dyn Window)
    }

    fn request_redraw(&mut self, window: WindowId) {
        self.shared.borrow_mut().redraws.push_back(window);
    }

    fn set_menu(&mut self, menu: &Menu) {
        // Only a `Main` root describes a menu bar; anything else is a no-op.
        let Menu::Main { items } = menu else {
            return;
        };
        // Build a fresh bar and the command targets it needs, then swap both in
        // atomically: the new targets replace the old only after `setMainMenu:`
        // has adopted the new bar, so no live menu item is ever left pointing at
        // a dropped target.
        let mut targets = Vec::new();
        let bar = NSMenu::new(self.mtm);
        for child in items {
            build_menu_node(
                self.mtm,
                &bar,
                child,
                &self.shared,
                &self._app_delegate,
                &mut targets,
            );
        }
        self.app.setMainMenu(Some(&bar));
        self.menu_targets = targets;
    }

    fn set_draggable_regions(&mut self, window: WindowId, regions: &[LogicalRect]) {
        // Replace the target window's cached caption drag regions. Cheap cold
        // path: called only when the app's caption layout changes, not per frame.
        // Copy the compact rects into the content view's cache so `mouseDown`
        // hit-tests them with no cross-boundary call. Unknown id is a no-op.
        let Some(win) = self.windows.iter().find(|w| w.id == window) else {
            return;
        };
        let mut cache = win.content_view.ivars().draggable_regions.borrow_mut();
        cache.clear();
        cache.extend_from_slice(regions);
    }

    fn close_window(&mut self, window: WindowId) {
        // Same close path as a user-driven close, initiated by the app: order the
        // NSWindow out (dropping its Retained releases the OS shell) and enqueue a
        // `WindowClosed` so the scheduler decrements its open-window count and the
        // driver tears the window's state down through the one close path.
        //
        // We synthesize `WindowClosed` here rather than leaning on a
        // `windowWillClose:` delegate callback: `-[NSWindow close]` bypasses
        // `windowShouldClose:`, and routing every close (user- or app-driven)
        // through this one enqueue keeps delivery deterministic and identical to
        // the headless backend. We do NOT set `should_exit` — closing one window
        // in a multi-window session must not terminate the pump; the scheduler's
        // open-window gate owns the exit decision. (The Phase-1 `should_exit`
        // shortcut in `windowShouldClose:` predates multi-window and is a
        // single-window wart to revisit when native multi-window lands.)
        let Some(pos) = self.windows.iter().position(|w| w.id == window) else {
            return; // Unknown id: no-op, matching headless.
        };
        let closed = self.windows.remove(pos);
        closed.window.close();
        self.shared
            .borrow_mut()
            .events
            .push_back(RawEvent::WindowClosed { window });
    }
}

/// A native macOS window plus its retained content view and delegate.
pub struct MacWindow {
    id: WindowId,
    window: Retained<NSWindow>,
    /// The window's content view — our flipped, event-handling `VisoContentView`.
    /// Retained for GPU surface attachment via `raw_handle`.
    content_view: Retained<VisoContentView>,
    /// Kept alive: `setDelegate` holds only a weak reference.
    _delegate: Retained<WindowDelegate>,
}

impl Window for MacWindow {
    fn id(&self) -> WindowId {
        self.id
    }

    fn request_redraw(&self) {
        self.window.setViewsNeedDisplay(true);
    }

    fn set_title(&mut self, title: &str) {
        self.window.setTitle(&NSString::from_str(title));
    }

    fn scale_factor(&self) -> f64 {
        self.window.backingScaleFactor()
    }

    fn inner_size(&self) -> (u32, u32) {
        let frame = self.content_view.frame();
        let scale = self.window.backingScaleFactor();
        (
            (frame.size.width * scale) as u32,
            (frame.size.height * scale) as u32,
        )
    }

    fn raw_handle(&self) -> RawWindowHandle {
        // The `NSView` pointer stays valid for the window's lifetime; the GPU
        // layer must not outlive this `MacWindow`.
        let ns_view = Retained::as_ptr(&self.content_view) as *mut core::ffi::c_void;
        RawWindowHandle::AppKit { ns_view }
    }
}

/// Ivars for the window delegate: which window it serves, the shared queue, and
/// whether the window uses self-drawn chrome (so resize re-reports the native
/// traffic-light geometry, and native-chrome windows never do).
struct DelegateIvars {
    window: WindowId,
    shared: Shared,
    chrome: WindowChrome,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VisoWindowDelegate"]
    #[ivars = DelegateIvars]
    struct WindowDelegate;

    unsafe impl NSObjectProtocol for WindowDelegate {}

    unsafe impl NSWindowDelegate for WindowDelegate {
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &NSWindow) -> bool {
            let ivars = self.ivars();
            let window = ivars.window;
            let shared = ivars.shared.clone();
            // AppKit demands a synchronous accept/deny, but Viso's handler runs
            // later on the pump — so we cannot honor a veto here without
            // deferring the actual close. Phase 1 accepts every close: enqueue
            // the veto-handshake event (the app still sees it, its `accept`
            // starts true and is not read back) followed by the WindowClosed
            // the scheduler counts against open windows, then let AppKit close.
            // catch_unwind: never let a Rust panic unwind through ObjC.
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let mut q = shared.borrow_mut();
                q.events.push_back(RawEvent::CloseRequested {
                    window,
                    accept: AcceptCell::new(),
                });
                q.events.push_back(RawEvent::WindowClosed { window });
                q.should_exit = true;
            }));
            true
        }

        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, notification: &NSNotification) {
            let ivars = self.ivars();
            let window = ivars.window;
            let shared = ivars.shared.clone();
            let chrome = ivars.chrome;
            let _ = catch_unwind(AssertUnwindSafe(|| {
                // The notification's object is the NSWindow.
                let nswin = notification
                    .object()
                    .and_then(|o| o.downcast::<NSWindow>().ok());
                let (scale, width, height) = nswin
                    .as_ref()
                    .map(|nswin| {
                        let scale = nswin.backingScaleFactor();
                        let size = nswin
                            .contentView()
                            .map(|v| v.frame().size)
                            .unwrap_or(NSSize::new(0.0, 0.0));
                        (
                            scale,
                            (size.width * scale) as u32,
                            (size.height * scale) as u32,
                        )
                    })
                    .unwrap_or((1.0, 0, 0));
                // Self-drawn chrome: the traffic lights reposition with the title
                // bar on resize, so re-measure and re-report so the app can keep
                // its caption aligned.
                let buttons_rect = if chrome == WindowChrome::SelfDrawn {
                    nswin.as_deref().and_then(traffic_lights_geom)
                } else {
                    None
                };
                let mut q = shared.borrow_mut();
                q.events.push_back(RawEvent::ScaleFactorChanged {
                    window,
                    scale,
                    width,
                    height,
                });
                if let Some(buttons_rect) = buttons_rect {
                    q.events.push_back(RawEvent::WindowChromeGeom {
                        window,
                        buttons_rect,
                    });
                }
                q.redraws.push_back(window);
            }));
        }
    }
);

impl WindowDelegate {
    fn new(
        mtm: MainThreadMarker,
        window: WindowId,
        shared: Shared,
        chrome: WindowChrome,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars {
            window,
            shared,
            chrome,
        });
        unsafe { msg_send![super(this), init] }
    }
}

/// Ivars for the application delegate: the pump's shared queue, so the menu's
/// Quit action and a Dock-driven terminate both request a clean pump exit.
struct AppDelegateIvars {
    shared: Shared,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VisoAppDelegate"]
    #[ivars = AppDelegateIvars]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        /// A Dock/system-driven quit (right-click Dock → Quit, logout) routes
        /// through here. Deny AppKit's own `terminate:` — which would call
        /// `exit()` out from under the manual pump — and instead ask the pump
        /// to break out, so windows tear down through the one close path.
        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _sender: &NSApplication) -> NSApplicationTerminateReply {
            let shared = self.ivars().shared.clone();
            let _ = catch_unwind(AssertUnwindSafe(|| {
                shared.borrow_mut().should_exit = true;
            }));
            NSApplicationTerminateReply::TerminateCancel
        }
    }

    impl AppDelegate {
        /// Target of the "Quit" menu item (Command-Q). Requests a clean pump
        /// exit rather than a hard terminate.
        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            let shared = self.ivars().shared.clone();
            let _ = catch_unwind(AssertUnwindSafe(|| {
                shared.borrow_mut().should_exit = true;
            }));
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker, shared: Shared) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars { shared });
        unsafe { msg_send![super(this), init] }
    }
}

/// Build and install the minimal main menu: one "AppName" submenu holding a
/// "Quit AppName" item wired to Command-Q. The item targets the app delegate's
/// `quit:` so the shortcut ends in a clean pump exit.
fn install_main_menu(mtm: MainThreadMarker, app: &NSApplication, delegate: &AppDelegate) {
    let name = NSProcessInfo::processInfo().processName();

    let main_menu = NSMenu::new(mtm);
    let app_item = NSMenuItem::new(mtm);
    main_menu.addItem(&app_item);

    let app_submenu = NSMenu::new(mtm);
    let quit_title = NSString::from_str(&format!("Quit {name}"));
    // SAFETY: standard AppKit menu construction on the main thread. The target
    // is the app delegate installed just before this call.
    let quit_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &quit_title,
            Some(sel!(quit:)),
            &NSString::from_str("q"),
        )
    };
    unsafe {
        quit_item.setTarget(Some(delegate));
    }
    app_submenu.addItem(&quit_item);
    app_item.setSubmenu(Some(&app_submenu));

    app.setMainMenu(Some(&main_menu));
}

/// The `hitTest:` override for the swizzled titlebar container. Calls the stock
/// implementation, then walks the hit view's superview chain looking for an
/// `NSButton`: a hit inside a traffic-light button keeps working (return the
/// stock hit), while a hit anywhere else in the transparent titlebar strip
/// returns `nil` so the event falls through to the full-size content view. That
/// is what lets the app's own caption own the top strip — drags, clicks, and
/// custom controls there reach [`VisoContentView`] instead of being swallowed by
/// AppKit's title bar.
///
/// # Safety
/// Installed only as the `hitTest:` method of a subclass of
/// `NSTitlebarContainerView`, whose signature is `(NSPoint) -> id`.
extern "C" fn titlebar_hit_test(this: *mut AnyObject, _sel: Sel, point: NSPoint) -> *mut AnyObject {
    // SAFETY: `this` is an NSTitlebarContainerView subclass instance; `hitTest:`
    // takes an NSPoint and returns an autoreleased view (or nil). We only read
    // the returned view's class and superview chain, never retaining it.
    unsafe {
        let this = &*this;
        let superclass = this.class().superclass().expect("subclass has superclass");
        let hit: *mut AnyObject = msg_send![super(this, superclass), hitTest: point];
        let button_class = AnyClass::get(c"NSButton");
        let mut view = hit;
        while !view.is_null() {
            if let Some(button_class) = button_class {
                let is_button: bool = msg_send![view, isKindOfClass: button_class];
                if is_button {
                    return hit;
                }
            }
            view = msg_send![view, superview];
        }
        core::ptr::null_mut()
    }
}

thread_local! {
    /// The swizzle subclass, registered once on the main thread. `None` before
    /// first use or if `NSTitlebarContainerView` is unavailable; `Some(ptr)`
    /// caches the registered class so every self-drawn window reuses it (a class
    /// name can be registered only once). Raw pointer because `&'static
    /// AnyClass` is not `Sync`; only ever touched on the main thread.
    static TITLEBAR_CONTAINER_SUBCLASS: Cell<Option<*const AnyClass>> = const { Cell::new(None) };
}

/// Register (once) and return the `NSTitlebarContainerView` subclass whose
/// `hitTest:` is [`titlebar_hit_test`]. Returns `None` if AppKit has no
/// `NSTitlebarContainerView` class (it should always exist on macOS, but the
/// caller stays defensive). Main-thread only — class registration is not
/// thread-safe, and every window is created on the main thread.
fn titlebar_container_subclass() -> Option<*const AnyClass> {
    TITLEBAR_CONTAINER_SUBCLASS.with(|slot| {
        if let Some(cached) = slot.get() {
            return Some(cached);
        }
        let base = AnyClass::get(c"NSTitlebarContainerView")?;
        let mut builder = ClassBuilder::new(c"VisoTitlebarContainerView", base)?;
        // SAFETY: `hitTest:` on an NSView subclass is `(NSPoint) -> id`, matching
        // `titlebar_hit_test`'s signature; the subclass base is
        // NSTitlebarContainerView, so the selector's encoding is verified.
        unsafe {
            builder.add_method(
                sel!(hitTest:),
                titlebar_hit_test as extern "C" fn(*mut AnyObject, Sel, NSPoint) -> *mut AnyObject,
            );
        }
        let cls: *const AnyClass = builder.register();
        slot.set(Some(cls));
        Some(cls)
    })
}

/// Swap the AppKit titlebar container's class for the Viso subclass whose
/// `hitTest:` keeps only the traffic-light buttons hittable. With a transparent
/// full-size-content title bar, the stock container otherwise eats every drag
/// and click in the top strip — so a control the app draws in its own caption
/// never sees the mouse. Locates the container by walking two superviews up from
/// the close button (`standardWindowButton(0)` → `NSTitlebarView` →
/// `NSTitlebarContainerView`); every step is defensive, so if AppKit's private
/// view tree ever changes shape the window simply keeps stock behavior instead
/// of breaking.
fn defang_titlebar_container(window: &NSWindow) {
    let Some(subclass) = titlebar_container_subclass() else {
        return;
    };
    // SAFETY: standard AppKit introspection on the main thread. We read the
    // close button's superview chain and, only after verifying the container's
    // class, re-point it at our subclass (which differs from the base only in
    // `hitTest:`). No object is retained past this call.
    unsafe {
        use objc2_app_kit::NSWindowButton;
        let Some(close) = window.standardWindowButton(NSWindowButton::CloseButton) else {
            return;
        };
        let Some(titlebar) = close.superview() else {
            return;
        };
        let Some(container) = titlebar.superview() else {
            return;
        };
        let Some(container_class) = AnyClass::get(c"NSTitlebarContainerView") else {
            return;
        };
        let is_container: bool = msg_send![&*container, isKindOfClass: container_class];
        if !is_container {
            return;
        }
        let _ = AnyObject::set_class(&container, &*subclass);
    }
}

/// The bounding box of the window's three traffic-light buttons, in logical
/// points relative to the content view (top-left origin), or `None` if the
/// buttons are absent or the geometry is transiently bogus.
///
/// Each button's frame is converted from its superview into the content view's
/// coordinate space and the three are unioned. The content view is flipped
/// (top-left origin), so the converted rects are already in the app's
/// convention — no vertical flip. During a fullscreen transition the content
/// view can resize a beat before the buttons reposition, momentarily placing
/// them in the lower half of the view; that geometry is discarded (the buttons
/// always live near the top) so the app never aligns its caption to a bogus
/// box.
fn traffic_lights_geom(window: &NSWindow) -> Option<LogicalRect> {
    use objc2_app_kit::NSWindowButton;
    let close = window.standardWindowButton(NSWindowButton::CloseButton)?;
    let mini = window.standardWindowButton(NSWindowButton::MiniaturizeButton)?;
    let zoom = window.standardWindowButton(NSWindowButton::ZoomButton)?;
    let content = window.contentView()?;
    let h = content.frame().size.height;

    // Convert a button's frame into the content view's (flipped, top-left)
    // space and return (top, left, right, bottom).
    let to_edges = |btn: &objc2_app_kit::NSView| -> Option<(f64, f64, f64, f64)> {
        // SAFETY: standard AppKit view geometry on the main thread — reading a
        // button's superview and frame and converting the rect between two live
        // views. All operate on views owned by the live window.
        let superview = unsafe { btn.superview() }?;
        let frame = btn.frame();
        let r: NSRect = unsafe { msg_send![&*content, convertRect: frame, fromView: &*superview] };
        let top = r.origin.y;
        let left = r.origin.x;
        let right = r.origin.x + r.size.width;
        let bottom = r.origin.y + r.size.height;
        Some((top, left, right, bottom))
    };

    let (t0, l0, r0, b0) = to_edges(&close)?;
    let (t1, l1, r1, b1) = to_edges(&mini)?;
    let (t2, l2, r2, b2) = to_edges(&zoom)?;

    let top = t0.min(t1).min(t2);
    let left = l0.min(l1).min(l2);
    let right = r0.max(r1).max(r2);
    let bottom = b0.max(b1).max(b2);

    if top > h * 0.5 {
        return None;
    }

    Some(LogicalRect::new(left, top, right - left, bottom - top))
}

/// Ivars for a custom menu-command target: the app-assigned command id and the
/// pump's shared queue. One instance backs each [`Menu::Item`]; picking the item
/// fires `menuAction:` here, which enqueues the command for the runtime.
struct MenuTargetIvars {
    command: u32,
    shared: Shared,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VisoMenuTarget"]
    #[ivars = MenuTargetIvars]
    struct MenuTarget;

    unsafe impl NSObjectProtocol for MenuTarget {}

    impl MenuTarget {
        /// Selector wired to a custom menu item. Enqueues the item's command so
        /// the runtime sees it as a `RawEvent::MenuCommand` on the next pump turn
        /// — the menu action stays off the OS's synchronous call stack.
        #[unsafe(method(menuAction:))]
        fn menu_action(&self, _sender: Option<&AnyObject>) {
            let ivars = self.ivars();
            let command = ivars.command;
            let shared = ivars.shared.clone();
            let _ = catch_unwind(AssertUnwindSafe(|| {
                shared
                    .borrow_mut()
                    .events
                    .push_back(RawEvent::MenuCommand {
                        id: crate::menu::MenuCommandId(command),
                    });
            }));
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker, command: u32, shared: Shared) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(MenuTargetIvars { command, shared });
        unsafe { msg_send![super(this), init] }
    }
}

/// Recursively translate one [`Menu`] node into AppKit objects, appending it to
/// `parent`. Custom items get a freshly built [`MenuTarget`] (pushed onto
/// `targets` so it outlives the menu, since `setTarget:` is weak); system items
/// wire to the OS responder chain (or the app delegate's clean-exit `quit:`).
fn build_menu_node(
    mtm: MainThreadMarker,
    parent: &NSMenu,
    node: &Menu,
    shared: &Shared,
    delegate: &AppDelegate,
    targets: &mut Vec<Retained<MenuTarget>>,
) {
    match node {
        // A nested `Main` is meaningless below the root — flatten its children
        // into the parent so a malformed tree still renders sensibly.
        Menu::Main { items } => {
            for child in items {
                build_menu_node(mtm, parent, child, shared, delegate, targets);
            }
        }
        Menu::Sub { name, items } => {
            let item = NSMenuItem::new(mtm);
            item.setTitle(&NSString::from_str(name));
            let submenu = NSMenu::new(mtm);
            // The submenu's title drives the bar label AppKit shows for it.
            submenu.setTitle(&NSString::from_str(name));
            for child in items {
                build_menu_node(mtm, &submenu, child, shared, delegate, targets);
            }
            item.setSubmenu(Some(&submenu));
            parent.addItem(&item);
        }
        Menu::Item {
            name,
            command,
            accel,
            enabled,
        } => {
            let (key, mask) = accel_to_key_equivalent(accel.as_ref());
            // SAFETY: standard AppKit menu-item construction on the main thread.
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(name),
                    Some(sel!(menuAction:)),
                    &NSString::from_str(&key),
                )
            };
            if let Some(mask) = mask {
                item.setKeyEquivalentModifierMask(mask);
            }
            item.setEnabled(*enabled);
            let target = MenuTarget::new(mtm, command.0, shared.clone());
            unsafe {
                item.setTarget(Some(&target));
            }
            targets.push(target);
            parent.addItem(&item);
        }
        Menu::System { action, name } => {
            let item = build_system_item(mtm, *action, name.as_deref(), delegate);
            parent.addItem(&item);
        }
        Menu::Line => {
            parent.addItem(&NSMenuItem::separatorItem(mtm));
        }
    }
}

/// Build a standard OS-action menu item. Quit routes to the app delegate's
/// `quit:` (a clean pump exit, not the hard `terminate:`); the rest use the
/// conventional responder-chain selectors the OS already implements.
fn build_system_item(
    mtm: MainThreadMarker,
    action: SystemAction,
    name: Option<&str>,
    delegate: &AppDelegate,
) -> Retained<NSMenuItem> {
    let app_name = NSProcessInfo::processInfo().processName();
    let (default_title, key, sel) = match action {
        SystemAction::Quit => (format!("Quit {app_name}"), "q", sel!(quit:)),
        SystemAction::CloseWindow => ("Close".to_string(), "w", sel!(performClose:)),
        SystemAction::Hide => (format!("Hide {app_name}"), "h", sel!(hide:)),
        SystemAction::Minimize => ("Minimize".to_string(), "m", sel!(performMiniaturize:)),
    };
    let title = name.map(str::to_string).unwrap_or(default_title);
    // SAFETY: standard AppKit menu-item construction on the main thread.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(&title),
            Some(sel),
            &NSString::from_str(key),
        )
    };
    // Quit targets our delegate explicitly (clean pump exit); the others take
    // nil target so the OS walks the responder chain to the key window / app.
    if matches!(action, SystemAction::Quit) {
        unsafe {
            item.setTarget(Some(delegate));
        }
    }
    item
}

/// Translate a Viso [`Accel`] into the `(keyEquivalent, modifier mask)` pair
/// AppKit wants. Returns an empty key (and `None` mask) when the accelerator is
/// unset, which leaves the item shortcut-free.
fn accel_to_key_equivalent(accel: Option<&Accel>) -> (String, Option<NSEventModifierFlags>) {
    let Some(accel) = accel.filter(|a| a.is_set()) else {
        return (String::new(), None);
    };
    let mut mask = NSEventModifierFlags::empty();
    // The primary accelerator is Command on macOS.
    if accel.primary || accel.control {
        // `control` maps to the Control key; `primary` folds to Command here.
        if accel.primary {
            mask |= NSEventModifierFlags::Command;
        }
        if accel.control {
            mask |= NSEventModifierFlags::Control;
        }
    }
    if accel.shift {
        mask |= NSEventModifierFlags::Shift;
    }
    if accel.alt {
        mask |= NSEventModifierFlags::Option;
    }
    (accel.key.clone(), Some(mask))
}

/// Ivars for the content view: window identity, the shared queue, the current
/// pointer-button mask, and the live IME composition string.
struct ViewIvars {
    window: WindowId,
    shared: Shared,
    /// Buttons currently held, as a [`PointerButtons`] mask.
    buttons: Cell<u8>,
    /// The current IME composition (marked) text, echoed to the widget as a
    /// preedit. Empty when no composition is active.
    marked: RefCell<String>,
    /// Draggable caption regions for self-drawn chrome, logical points,
    /// top-left origin — the cache the app pushes down via
    /// [`PlatformApp::set_draggable_regions`](crate::PlatformApp::set_draggable_regions).
    /// A primary press inside one starts a native window drag instead of routing
    /// a pointer event. Empty (the default) means the whole content area routes
    /// normally, so native-chrome windows never take the drag path.
    draggable_regions: RefCell<Vec<LogicalRect>>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "VisoContentView"]
    #[ivars = ViewIvars]
    struct VisoContentView;

    unsafe impl NSObjectProtocol for VisoContentView {}

    impl VisoContentView {
        // A flipped view puts the origin at the top-left, so a point converted
        // from the window is already in Viso's coordinate convention — no
        // manual `frame.height - y` flip needed anywhere below.
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        // Rebuild the tracking area on every geometry change so `mouseMoved`
        // and `mouseExited` fire across the whole current bounds.
        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
                let options = NSTrackingAreaOptions::MouseEnteredAndExited
                    | NSTrackingAreaOptions::MouseMoved
                    | NSTrackingAreaOptions::ActiveInKeyWindow
                    | NSTrackingAreaOptions::InVisibleRect;
                let area = NSTrackingArea::initWithRect_options_owner_userInfo(
                    NSTrackingArea::alloc(),
                    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)),
                    options,
                    Some(self.as_ref()),
                    None,
                );
                self.addTrackingArea(&area);
            }));
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            // A primary press inside a declared draggable caption region begins a
            // native window drag and is *not* routed as a pointer event — the app
            // asked for that strip to behave like a title bar. Traffic-light
            // clicks never reach here (the swizzled titlebar container keeps them
            // to itself), so the two mechanisms don't fight. Everything else,
            // including every press when no regions are declared, falls through to
            // the normal pointer path.
            if self.try_begin_window_drag(event) {
                return;
            }
            self.pointer(event, PointerPhase::Down, PointerButtons::PRIMARY, true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            self.pointer(event, PointerPhase::Up, PointerButtons::PRIMARY, false);
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            self.pointer(event, PointerPhase::Down, PointerButtons::SECONDARY, true);
        }

        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, event: &NSEvent) {
            self.pointer(event, PointerPhase::Up, PointerButtons::SECONDARY, false);
        }

        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, event: &NSEvent) {
            self.pointer(event, PointerPhase::Down, PointerButtons::MIDDLE, true);
        }

        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, event: &NSEvent) {
            self.pointer(event, PointerPhase::Up, PointerButtons::MIDDLE, false);
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            self.pointer(event, PointerPhase::Moved, PointerButtons::NONE, false);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.pointer(event, PointerPhase::Moved, PointerButtons::NONE, false);
        }

        #[unsafe(method(rightMouseDragged:))]
        fn right_mouse_dragged(&self, event: &NSEvent) {
            self.pointer(event, PointerPhase::Moved, PointerButtons::NONE, false);
        }

        #[unsafe(method(otherMouseDragged:))]
        fn other_mouse_dragged(&self, event: &NSEvent) {
            self.pointer(event, PointerPhase::Moved, PointerButtons::NONE, false);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, event: &NSEvent) {
            self.pointer(event, PointerPhase::Left, PointerButtons::NONE, false);
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let (x, y) = self.location(event);
                // Precise (trackpad) deltas are already in points; line-based
                // (mouse wheel) deltas count wheel notches, scaled to a nominal
                // line height. Negate so a natural downward gesture reports a
                // positive delta ("content moves down"), matching the runtime's
                // scroll convention.
                let (dx, dy) = if event.hasPreciseScrollingDeltas() {
                    (event.scrollingDeltaX(), event.scrollingDeltaY())
                } else {
                    const LINE: f64 = 16.0;
                    (event.scrollingDeltaX() * LINE, event.scrollingDeltaY() * LINE)
                };
                let ivars = self.ivars();
                let sample = RawScroll {
                    window: ivars.window,
                    x,
                    y,
                    delta_x: -dx,
                    delta_y: -dy,
                    modifiers: modifiers_of(event),
                };
                self.push(RawEvent::Scroll(sample));
            }));
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let ivars = self.ivars();
                let code = keycode_of(event);
                self.push(RawEvent::Key(RawKey {
                    window: ivars.window,
                    code,
                    pressed: true,
                    repeat: event.isARepeat(),
                    modifiers: modifiers_of(event),
                }));
                // Route through the input context so IME composition and
                // `insertText:`/`setMarkedText:` fire. For plain (non-composed)
                // typing this yields the committed characters via `insertText:`.
                if let Some(ctx) = self.inputContext() {
                    let _: bool = ctx.handleEvent(event);
                }
            }));
        }

        #[unsafe(method(keyUp:))]
        fn key_up(&self, event: &NSEvent) {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let ivars = self.ivars();
                self.push(RawEvent::Key(RawKey {
                    window: ivars.window,
                    code: keycode_of(event),
                    pressed: false,
                    repeat: false,
                    modifiers: modifiers_of(event),
                }));
            }));
        }
    }

    // The `NSTextInputClient` protocol: IME composition + committed text.
    unsafe impl NSTextInputClient for VisoContentView {
        #[unsafe(method(insertText:replacementRange:))]
        unsafe fn insert_text(&self, string: &AnyObject, _replacement: NSRange) {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let text = any_to_string(string);
                let ivars = self.ivars();
                // A commit ends any composition: clear the marked state (no
                // extra empty-preedit event — the committed text supersedes it).
                ivars.marked.borrow_mut().clear();
                if !text.is_empty() {
                    self.push(RawEvent::Text(RawText {
                        window: ivars.window,
                        text,
                    }));
                }
            }));
        }

        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        unsafe fn set_marked_text(
            &self,
            string: &AnyObject,
            _selected: NSRange,
            _replacement: NSRange,
        ) {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let text = any_to_string(string);
                let ivars = self.ivars();
                *ivars.marked.borrow_mut() = text.clone();
                // Echo the composing string inline as a preedit (empty = clear).
                self.push(RawEvent::ImePreedit(RawImePreedit {
                    window: ivars.window,
                    caret: text.len(),
                    text,
                }));
            }));
        }

        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let ivars = self.ivars();
                if !ivars.marked.borrow().is_empty() {
                    ivars.marked.borrow_mut().clear();
                    // The composition was discarded (Escape / session break):
                    // an empty preedit clears the inline preview.
                    self.push(RawEvent::ImePreedit(RawImePreedit {
                        window: ivars.window,
                        text: String::new(),
                        caret: 0,
                    }));
                }
            }));
        }

        #[unsafe(method(hasMarkedText))]
        fn has_marked_text(&self) -> bool {
            !self.ivars().marked.borrow().is_empty()
        }

        #[unsafe(method(markedRange))]
        fn marked_range(&self) -> NSRange {
            let len = self.ivars().marked.borrow().len();
            if len == 0 {
                NSRange::new(usize::MAX, 0)
            } else {
                NSRange::new(0, len)
            }
        }

        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> NSRange {
            NSRange::new(0, 0)
        }

        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        unsafe fn attributed_substring(
            &self,
            _range: NSRange,
            _actual: NSRangePointer,
        ) -> Option<Retained<NSAttributedString>> {
            None
        }

        #[unsafe(method_id(validAttributesForMarkedText))]
        fn valid_attributes(&self) -> Retained<NSArray<NSAttributedStringKey>> {
            NSArray::new()
        }

        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        unsafe fn first_rect(&self, _range: NSRange, _actual: NSRangePointer) -> NSRect {
            // The caret rect (used to park the IME candidate window) is not yet
            // tracked; report the view's screen origin so the panel appears near
            // the window rather than at (0,0).
            let local = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
            let in_window = self.convertRect_toView(local, None);
            match self.window() {
                Some(w) => w.convertRectToScreen(in_window),
                None => in_window,
            }
        }

        #[unsafe(method(characterIndexForPoint:))]
        fn character_index(&self, _point: NSPoint) -> usize {
            0
        }

        #[unsafe(method(doCommandBySelector:))]
        unsafe fn do_command(&self, _selector: Sel) {}
    }
);

impl VisoContentView {
    fn new(
        mtm: MainThreadMarker,
        window: WindowId,
        shared: Shared,
        frame: NSRect,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ViewIvars {
            window,
            shared,
            buttons: Cell::new(0),
            marked: RefCell::new(String::new()),
            draggable_regions: RefCell::new(Vec::new()),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        this
    }

    /// The event location in this flipped view's coordinates (logical points,
    /// origin at the top-left).
    fn location(&self, event: &NSEvent) -> (f64, f64) {
        let window_point = event.locationInWindow();
        let p = self.convertPoint_fromView(window_point, None);
        (p.x, p.y)
    }

    /// If the press falls inside a declared draggable caption region, start a
    /// native window drag and report `true` (the caller then skips the normal
    /// pointer route). Returns `false` — the common case — when no region is
    /// declared or the press misses them all, so nothing is done and the press
    /// routes as usual. Hit-testing walks the cached slice with no allocation.
    fn try_begin_window_drag(&self, event: &NSEvent) -> bool {
        let (x, y) = self.location(event);
        let hit = self
            .ivars()
            .draggable_regions
            .borrow()
            .iter()
            .any(|r| r.contains(x, y));
        if !hit {
            return false;
        }
        if let Some(window) = self.window() {
            // Hand the drag to AppKit; it tracks the mouse until release.
            window.performWindowDragWithEvent(event);
        }
        true
    }

    /// Enqueue a pointer sample, folding the button transition into the tracked
    /// mask so every sample reports the full chord currently held.
    fn pointer(&self, event: &NSEvent, phase: PointerPhase, button: PointerButtons, pressed: bool) {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let ivars = self.ivars();
            let mut mask = ivars.buttons.get();
            if !button.is_empty() {
                if pressed {
                    mask |= button.0;
                } else {
                    mask &= !button.0;
                }
                ivars.buttons.set(mask);
            }
            let (x, y) = self.location(event);
            self.push(RawEvent::Pointer(RawPointer {
                window: ivars.window,
                x,
                y,
                buttons: PointerButtons(mask),
                modifiers: modifiers_of(event),
                phase,
            }));
        }));
    }

    /// Push one raw event onto the shared queue.
    fn push(&self, event: RawEvent) {
        self.ivars().shared.borrow_mut().events.push_back(event);
    }
}

/// Decode an `NSEvent`'s modifier flags into the platform mirror.
fn modifiers_of(event: &NSEvent) -> Modifiers {
    let flags = event.modifierFlags();
    Modifiers {
        shift: flags.contains(NSEventModifierFlags::Shift),
        control: flags.contains(NSEventModifierFlags::Control),
        alt: flags.contains(NSEventModifierFlags::Option),
        logo: flags.contains(NSEventModifierFlags::Command),
    }
}

/// Map an `NSEvent`'s hardware `keyCode` onto the minimal platform [`KeyCode`].
/// Named keys use their macOS scancodes; everything else rides as
/// `Other(scancode)` so higher layers can still route it.
fn keycode_of(event: &NSEvent) -> KeyCode {
    let scancode = event.keyCode() as u32;
    match scancode {
        0x35 => KeyCode::Escape,
        0x24 | 0x4c => KeyCode::Enter, // Return, keypad Enter
        0x31 => KeyCode::Space,
        0x30 => KeyCode::Tab,
        0x33 => KeyCode::Backspace,
        0x7b => KeyCode::Left,
        0x7c => KeyCode::Right,
        0x7d => KeyCode::Down,
        0x7e => KeyCode::Up,
        0x75 => KeyCode::Delete, // forward delete
        0x73 => KeyCode::Home,
        0x77 => KeyCode::End,
        other => KeyCode::Other(other),
    }
}

/// Extract the string from an `NSString` or `NSAttributedString` argument (the
/// two types AppKit passes to the text-input methods).
fn any_to_string(string: &AnyObject) -> String {
    // Per the NSTextInputClient contract the argument is always an NSString or
    // NSAttributedString; downcast defensively and read its characters.
    if let Some(s) = string.downcast_ref::<NSString>() {
        s.to_string()
    } else if let Some(a) = string.downcast_ref::<NSAttributedString>() {
        a.string().to_string()
    } else {
        String::new()
    }
}
