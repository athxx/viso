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

use crate::Instant;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::rc::Rc;

use super::memory_pressure::MemoryPressure;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, ProtocolObject, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication, NSApplicationActivationPolicy,
    NSApplicationDelegate, NSApplicationTerminateReply, NSBackingStoreType, NSCursor, NSEvent,
    NSEventMask, NSEventModifierFlags, NSMenu, NSMenuItem, NSPasteboard, NSPasteboardTypeString,
    NSTextInputClient, NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindow, NSWindowDelegate,
    NSWindowStyleMask, NSWindowTitleVisibility, NSWorkspace,
    NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification,
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
    AcceptCell, Appearance, ClipboardReply, ClipboardShortcut, ColorScheme, CursorIcon, KeyCode,
    Modifiers, PointerButtons, PointerPhase, RawEvent, RawImePreedit, RawKey, RawPointer,
    RawScroll, RawText, clipboard_shortcut,
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
    /// The pump's `handler`, exposed to delegates only for the lifetime of
    /// `run()`. A live-resize drag hands the thread to AppKit's nested modal
    /// run loop, which suspends our `nextEventMatchingMask:` loop entirely — so
    /// events the resize delegate enqueues cannot be drained until the drag
    /// ends. `windowDidResize:`, which AppKit calls *synchronously* inside that
    /// modal loop, reads this pointer to drive a frame on the spot so the
    /// content tracks the drag live. `run()` installs the pointer before its
    /// loop and clears it on the way out (a guard clears it on unwind too), so
    /// the pointer is valid for exactly as long as the handler borrow lives and
    /// is never aliased: the pump's own `handler.handle` calls and a delegate's
    /// re-entrant drive never overlap in time (the pump is parked in AppKit
    /// when the delegate runs). See `MacApp::run` and `drain_and_drive`.
    drive: Option<NonNull<dyn AppHandler>>,
    /// The appearance last reported, so the several AppKit sources that signal
    /// a change (every view's `viewDidChangeEffectiveAppearance`, the workspace
    /// accessibility notification) emit one `AppearanceChanged` per real change.
    appearance: Appearance,
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
    /// The kernel's memory-pressure notifications, queued as `LowMemory`.
    _memory_pressure: Option<MemoryPressure>,
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
        let shared: Shared = Rc::new(RefCell::new(PumpQueue {
            appearance: current_appearance(&app),
            ..PumpQueue::default()
        }));

        // Standard "AppName" application menu with a "Quit AppName" item bound to
        // Command-Q. Without a main menu the OS has nowhere to route the Cmd+Q
        // shortcut, so the app cannot be quit from the keyboard. The Quit item
        // fires `quit:` on our delegate, which asks the pump to exit cleanly —
        // draining windows through the normal close path rather than the hard
        // `-[NSApplication terminate:]` that a manual pump cannot unwind.
        let app_delegate = AppDelegate::new(mtm, shared.clone());
        app.setDelegate(Some(ProtocolObject::from_ref(&*app_delegate)));
        install_main_menu(mtm, &app, &app_delegate);
        // Contrast and reduce-motion live in the workspace's accessibility
        // options, which have no per-view callback.
        // SAFETY: the selector is implemented by `AppDelegate` with the
        // `(&self, &NSNotification)` signature the center calls; the delegate is
        // kept alive by `MacApp` and a deallocated observer is unregistered by
        // the center itself.
        unsafe {
            NSWorkspace::sharedWorkspace()
                .notificationCenter()
                .addObserver_selector_name_object(
                    &app_delegate,
                    sel!(accessibilityDisplayOptionsChanged:),
                    Some(NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification),
                    None,
                );
        }

        let pressure_queue = shared.clone();
        let memory_pressure = MemoryPressure::on_main_queue(move || {
            if let Ok(mut q) = pressure_queue.try_borrow_mut() {
                q.events.push_back(RawEvent::LowMemory);
            }
        });

        Ok(Self {
            mtm,
            app,
            shared,
            next_window_id: 1,
            windows: Vec::new(),
            launched: false,
            _app_delegate: app_delegate,
            menu_targets: Vec::new(),
            _memory_pressure: memory_pressure,
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

/// Clears `PumpQueue::drive` when `run`'s loop ends, on every path including
/// unwind, so the erased handler pointer never outlives the `&mut` borrow it
/// was taken from.
struct DriveGuard(Shared);

impl Drop for DriveGuard {
    fn drop(&mut self) {
        self.0.borrow_mut().drive = None;
    }
}

/// Drain every queued synthetic event through the handler, running the frames
/// they schedule. Called from `windowDidResize:` while AppKit's modal resize
/// loop owns the thread, so the drag paints live instead of stalling until it
/// ends. Each event is popped under a short borrow that is released before
/// `handle` runs, so the handler may re-enter the queue (e.g. request another
/// redraw) without a borrow conflict.
///
/// SAFETY: the caller passes the pointer stored in `PumpQueue::drive`, which is
/// valid for the whole of `MacApp::run` (installed before its loop, cleared by
/// `DriveGuard`). This runs synchronously inside AppKit's nested loop, where the
/// pump's own `handler.handle` is parked, so the `&mut` reborrow is unaliased.
unsafe fn drain_and_drive(mut handler: NonNull<dyn AppHandler>, shared: &Shared) {
    loop {
        let event = {
            let mut q = shared.borrow_mut();
            if let Some(w) = q.redraws.pop_front() {
                Some(RawEvent::RedrawRequested { window: w })
            } else {
                q.events.pop_front()
            }
        };
        let Some(event) = event else { break };
        // SAFETY: see the function contract — the pointer is live and unaliased
        // for the duration of this call (the pump's own `handler.handle` is
        // parked in AppKit while a delegate drives, so the reborrow never
        // overlaps another live one).
        deliver(unsafe { handler.as_mut() }, event);
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

/// Replace the general pasteboard's contents with plain text.
fn write_pasteboard(text: &str) {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    // SAFETY: `NSPasteboardTypeString` is an immutable AppKit constant,
    // initialized before any Rust code runs.
    let kind = unsafe { NSPasteboardTypeString };
    pasteboard.setString_forType(&NSString::from_str(text), kind);
}

/// The general pasteboard's plain text, if it holds any.
fn read_pasteboard() -> Option<String> {
    // SAFETY: as in `write_pasteboard`.
    let kind = unsafe { NSPasteboardTypeString };
    NSPasteboard::generalPasteboard()
        .stringForType(kind)
        .map(|s| s.to_string())
}

/// The system appearance as AppKit reports it right now.
fn current_appearance(app: &NSApplication) -> Appearance {
    // SAFETY: the appearance names are immutable AppKit constants.
    let (aqua, dark) = unsafe { (NSAppearanceNameAqua, NSAppearanceNameDarkAqua) };
    let names = NSArray::from_slice(&[aqua, dark]);
    let is_dark = app
        .effectiveAppearance()
        .bestMatchFromAppearancesWithNames(&names)
        .is_some_and(|best| best.isEqualToString(dark));
    let workspace = NSWorkspace::sharedWorkspace();
    Appearance {
        color_scheme: if is_dark {
            ColorScheme::Dark
        } else {
            ColorScheme::Light
        },
        high_contrast: workspace.accessibilityDisplayShouldIncreaseContrast(),
        reduce_motion: workspace.accessibilityDisplayShouldReduceMotion(),
    }
}

/// Re-read the appearance and enqueue `AppearanceChanged` if it moved.
fn report_appearance(shared: &Shared, mtm: MainThreadMarker) {
    let now = current_appearance(&NSApplication::sharedApplication(mtm));
    let mut q = shared.borrow_mut();
    if q.appearance != now {
        q.appearance = now;
        q.events.push_back(RawEvent::AppearanceChanged(now));
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
            chrome: config.chrome,
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

        // Expose the handler to the resize delegate for the duration of the
        // pump loop so a live-resize drag can drive frames re-entrantly (see
        // `PumpQueue::drive`). The guard clears the pointer on every exit path,
        // including unwind, so it never outlives this borrow of `handler`.
        // SAFETY: `handler` outlives `_drive_guard` (both are bound by this
        // stack frame) and the guard's `Drop` clears the pointer before `run`
        // returns, so the stored pointer never outlives this `&mut` borrow. The
        // `transmute` only erases the handler's lifetime to `'static`; the guard
        // upholds the true (shorter) lifetime dynamically.
        let drive: NonNull<dyn AppHandler> = unsafe {
            let ptr: *mut dyn AppHandler = handler;
            NonNull::new_unchecked(std::mem::transmute::<
                *mut dyn AppHandler,
                *mut (dyn AppHandler + 'static),
            >(ptr))
        };
        self.shared.borrow_mut().drive = Some(drive);
        let _drive_guard = DriveGuard(self.shared.clone());

        let mut flow = ControlFlow::Wait;
        loop {
            if self.shared.borrow().should_exit {
                break;
            }
            // Deliver any synthetic (delegate/view) events first — redraw beats,
            // resizes, closes, and the pointer/key/scroll/IME samples the view
            // enqueued while AppKit dispatched the last OS event.
            if let Some(event) = self.next_synthetic() {
                flow = deliver(handler, event);
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
        closed.content_view.set_cursor_icon(CursorIcon::Default);
        closed.window.close();
        self.shared
            .borrow_mut()
            .events
            .push_back(RawEvent::WindowClosed { window });
    }

    fn set_clipboard_text(&mut self, text: &str) {
        write_pasteboard(text);
    }

    fn request_paste(&mut self, window: WindowId) {
        if let Some(text) = read_pasteboard() {
            self.shared
                .borrow_mut()
                .events
                .push_back(RawEvent::Paste { window, text });
        }
    }

    fn set_cursor(&mut self, window: WindowId, icon: CursorIcon) {
        if let Some(win) = self.windows.iter().find(|w| w.id == window) {
            win.content_view.set_cursor_icon(icon);
        }
    }

    fn set_ime_area(&mut self, window: WindowId, caret: Option<LogicalRect>) {
        if let Some(win) = self.windows.iter().find(|w| w.id == window) {
            win.content_view.set_ime_area(caret);
        }
    }

    fn show_soft_keyboard(&mut self, _window: WindowId, _show: bool) {}

    fn appearance(&self) -> Appearance {
        current_appearance(&self.app)
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
    /// Who draws this window's chrome. `chrome_geom` reports the traffic-light box
    /// only for `SelfDrawn` (a `Native` window's OS title bar owns its own
    /// buttons, outside our layout).
    chrome: WindowChrome,
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

    fn chrome_geom(&self) -> Option<LogicalRect> {
        // Only a self-drawn-chrome window keeps native traffic lights over its
        // full-size content view; a native-chrome window's buttons live in the OS
        // title bar, outside the app's layout. Same box the create path enqueues
        // as the first `WindowChromeGeom`, read synchronously so the build sees it.
        if self.chrome != WindowChrome::SelfDrawn {
            return None;
        }
        traffic_lights_geom(&self.window)
    }
}

/// Ivars for the window delegate: which window it serves, the shared queue,
/// whether the window uses self-drawn chrome (so resize re-reports the native
/// traffic-light geometry, and native-chrome windows never do), and whether it is
/// currently fullscreen (a `Cell` because the delegate methods take `&self`).
struct DelegateIvars {
    window: WindowId,
    shared: Shared,
    chrome: WindowChrome,
    /// Set on the will-enter/will-exit fullscreen transitions. While `true` the
    /// OS hides the traffic lights and draws its own title bar, so `windowDidResize`
    /// must not re-measure or re-report the (now absent) traffic-light box.
    is_fullscreen: Cell<bool>,
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
            let is_fullscreen = ivars.is_fullscreen.get();
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
                // its caption aligned. In fullscreen the OS hides the traffic
                // lights and draws its own title bar, so there is no box to report
                // (and the caption is hidden anyway) — skip it; the surface size
                // still changed, so `ScaleFactorChanged` below fires regardless.
                let buttons_rect = if chrome == WindowChrome::SelfDrawn && !is_fullscreen {
                    nswin.as_deref().and_then(traffic_lights_geom)
                } else {
                    None
                };
                {
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
                }
                // AppKit calls this synchronously inside its modal resize loop,
                // which has parked our pump. Drive the frames right here so the
                // content tracks the drag live instead of snapping only once the
                // drag ends. Outside a live resize (e.g. a programmatic resize)
                // the pump is running normally and `drive` may be absent — then
                // the events just drain on the next pump turn as before.
                let drive = shared.borrow().drive;
                if let Some(drive) = drive {
                    // SAFETY: `drive` is the pointer `MacApp::run` installed for
                    // the lifetime of its loop; this callback runs on the main
                    // thread inside that loop, where the pump's own `handle` is
                    // suspended, so the reborrow is unaliased. See `drain_and_drive`.
                    unsafe { drain_and_drive(drive, &shared) };
                }
            }));
        }

        #[unsafe(method(windowDidBecomeKey:))]
        fn window_did_become_key(&self, _notification: &NSNotification) {
            self.push_focus(true);
        }

        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            self.push_focus(false);
        }

        #[unsafe(method(windowWillEnterFullScreen:))]
        fn window_will_enter_full_screen(&self, _notification: &NSNotification) {
            let ivars = self.ivars();
            let window = ivars.window;
            let shared = ivars.shared.clone();
            // Flip the flag and report the transition at its *start* (the will-hook),
            // so a self-drawn caption hides as the animation begins rather than after
            // it settles — matching how the OS auto-hides its own title bar. Gating
            // `is_fullscreen` here also suppresses the traffic-light re-report from
            // the resize events the transition animation fires.
            ivars.is_fullscreen.set(true);
            let _ = catch_unwind(AssertUnwindSafe(|| {
                shared
                    .borrow_mut()
                    .events
                    .push_back(RawEvent::FullscreenChanged {
                        window,
                        fullscreen: true,
                    });
            }));
        }

        #[unsafe(method(windowWillExitFullScreen:))]
        fn window_will_exit_full_screen(&self, _notification: &NSNotification) {
            let ivars = self.ivars();
            let window = ivars.window;
            let shared = ivars.shared.clone();
            // Restore at the start of the exit animation: clear the flag so the
            // resize events fired during the animation re-report the traffic-light
            // box, and tell the app to un-hide its caption.
            ivars.is_fullscreen.set(false);
            let _ = catch_unwind(AssertUnwindSafe(|| {
                shared
                    .borrow_mut()
                    .events
                    .push_back(RawEvent::FullscreenChanged {
                        window,
                        fullscreen: false,
                    });
            }));
        }

        #[unsafe(method(windowDidFailToEnterFullScreen:))]
        fn window_did_fail_to_enter_full_screen(&self, _window: &NSWindow) {
            let ivars = self.ivars();
            let window = ivars.window;
            let shared = ivars.shared.clone();
            // The enter transition was aborted after the will-hook already flipped
            // the flag and hid the caption. Roll both back so the window is not left
            // showing OS chrome with the caption still hidden.
            ivars.is_fullscreen.set(false);
            let _ = catch_unwind(AssertUnwindSafe(|| {
                shared
                    .borrow_mut()
                    .events
                    .push_back(RawEvent::FullscreenChanged {
                        window,
                        fullscreen: false,
                    });
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
            is_fullscreen: Cell::new(false),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn push_focus(&self, focused: bool) {
        let ivars = self.ivars();
        let window = ivars.window;
        let _ = catch_unwind(AssertUnwindSafe(|| {
            ivars
                .shared
                .borrow_mut()
                .events
                .push_back(RawEvent::WindowFocused { window, focused });
        }));
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

        #[unsafe(method(applicationDidHide:))]
        fn application_did_hide(&self, _notification: &NSNotification) {
            self.push(RawEvent::Suspended);
        }

        #[unsafe(method(applicationDidUnhide:))]
        fn application_did_unhide(&self, _notification: &NSNotification) {
            self.push(RawEvent::Resumed);
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

        #[unsafe(method(accessibilityDisplayOptionsChanged:))]
        fn accessibility_display_options_changed(&self, _notification: &NSNotification) {
            let shared = self.ivars().shared.clone();
            let mtm = self.mtm();
            let _ = catch_unwind(AssertUnwindSafe(|| report_appearance(&shared, mtm)));
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker, shared: Shared) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars { shared });
        unsafe { msg_send![super(this), init] }
    }

    fn push(&self, event: RawEvent) {
        let shared = self.ivars().shared.clone();
        let _ = catch_unwind(AssertUnwindSafe(|| {
            shared.borrow_mut().events.push_back(event);
        }));
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
        // The super-class for the `hitTest:` super-call MUST be the stock
        // `NSTitlebarContainerView`, resolved by name, NOT `this.class().superclass()`.
        // Entering/leaving fullscreen makes AppKit re-swizzle the container into a
        // dynamic subclass of ours (its hit-test compatibility layer,
        // `___setUpHitTestingMethodCompatibility`); `this`'s runtime superclass then
        // points back at a class that still carries this very method, so a super-call
        // relative to it re-enters `titlebar_hit_test` forever until the stack
        // overflows. Anchoring the super-call at the fixed stock base always reaches
        // AppKit's own implementation and can never re-enter ours.
        let Some(base) = AnyClass::get(c"NSTitlebarContainerView") else {
            return core::ptr::null_mut();
        };
        let hit: *mut AnyObject = msg_send![super(this, base), hitTest: point];
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
        SystemAction::Copy => ("Copy".to_string(), "c", sel!(copy:)),
        SystemAction::Cut => ("Cut".to_string(), "x", sel!(cut:)),
        SystemAction::Paste => ("Paste".to_string(), "v", sel!(paste:)),
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
    /// The cursor the app asked for over this view.
    cursor: Cell<CursorIcon>,
    /// Whether this view currently holds one `[NSCursor hide]` (the count is
    /// global, so each view balances its own).
    cursor_hidden: Cell<bool>,
    /// A text field has focus: key events go through the input context.
    ime_enabled: Cell<bool>,
    /// The focused field's caret, logical points in this view.
    ime_caret: Cell<Option<LogicalRect>>,
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
                for old in self.trackingAreas().iter() {
                    self.removeTrackingArea(&old);
                }
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

        // AppKit re-applies cursor rects on every enter/move, so the app's
        // cursor is registered as one rect covering the view.
        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                if let Some(cursor) = ns_cursor(self.ivars().cursor.get()) {
                    self.addCursorRect_cursor(self.bounds(), &cursor);
                }
            }));
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                self.sync_cursor_hidden(true);
            }));
        }

        #[unsafe(method(viewDidChangeEffectiveAppearance))]
        fn view_did_change_effective_appearance(&self) {
            let shared = self.ivars().shared.clone();
            let mtm = self.mtm();
            let _ = catch_unwind(AssertUnwindSafe(|| report_appearance(&shared, mtm)));
        }

        // Modifier keys produce no keyDown/keyUp; their transitions arrive
        // here and are reported as key presses and releases.
        #[unsafe(method(flagsChanged:))]
        fn flags_changed(&self, event: &NSEvent) {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let code = keycode_of(event);
                let Some(pressed) = modifier_key_down(code, event.modifierFlags()) else {
                    return;
                };
                let window = self.ivars().window;
                let modifiers = modifiers_of(event);
                let key = |pressed| {
                    RawEvent::Key(RawKey {
                        window,
                        code,
                        pressed,
                        repeat: false,
                        modifiers,
                    })
                };
                self.push(key(pressed));
                // Caps Lock reports its lock state, not the key: each toggle
                // is one full press.
                if code == KeyCode::CapsLock {
                    self.push(key(!pressed));
                }
            }));
        }

        // Edit-menu actions and the responder chain's standard clipboard
        // selectors.
        #[unsafe(method(copy:))]
        fn copy(&self, _sender: Option<&AnyObject>) {
            let _ = catch_unwind(AssertUnwindSafe(|| self.clipboard(ClipboardShortcut::Copy)));
        }

        #[unsafe(method(cut:))]
        fn cut(&self, _sender: Option<&AnyObject>) {
            let _ = catch_unwind(AssertUnwindSafe(|| self.clipboard(ClipboardShortcut::Cut)));
        }

        #[unsafe(method(paste:))]
        fn paste(&self, _sender: Option<&AnyObject>) {
            let _ = catch_unwind(AssertUnwindSafe(|| self.clipboard(ClipboardShortcut::Paste)));
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
            let _ = catch_unwind(AssertUnwindSafe(|| self.sync_cursor_hidden(false)));
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
                let modifiers = modifiers_of(event);
                self.push(RawEvent::Key(RawKey {
                    window: ivars.window,
                    code,
                    pressed: true,
                    repeat: event.isARepeat(),
                    modifiers,
                }));
                // Without an Edit menu no key equivalent claims Command-C/X/V,
                // so the shortcut reaches the view and is handled here.
                if let Some(shortcut) = clipboard_shortcut(code, modifiers) {
                    self.clipboard(shortcut);
                    return;
                }
                // Route through the input context so IME composition and
                // `insertText:`/`setMarkedText:` fire. For plain (non-composed)
                // typing this yields the committed characters via `insertText:`.
                if ivars.ime_enabled.get()
                    && let Some(ctx) = self.inputContext()
                {
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
            // The candidate window parks under this rect: the focused field's
            // caret, or the view's origin until the app has reported one.
            let local = match self.ivars().ime_caret.get() {
                Some(r) => NSRect::new(NSPoint::new(r.x, r.y), NSSize::new(r.width, r.height)),
                None => NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)),
            };
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
            cursor: Cell::new(CursorIcon::Default),
            cursor_hidden: Cell::new(false),
            ime_enabled: Cell::new(true),
            ime_caret: Cell::new(None),
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
            self.push(RawEvent::Pointer(RawPointer::mouse(
                ivars.window,
                x,
                y,
                PointerButtons(mask),
                modifiers_of(event),
                phase,
            )));
        }));
    }

    /// Answer a clipboard gesture: copy and cut ask the app for its selection
    /// (written back by `deliver`), paste hands it the pasteboard's text.
    fn clipboard(&self, shortcut: ClipboardShortcut) {
        let window = self.ivars().window;
        let event = match shortcut {
            ClipboardShortcut::Copy | ClipboardShortcut::Cut => RawEvent::CopyRequested {
                window,
                cut: shortcut == ClipboardShortcut::Cut,
                reply: ClipboardReply::new(),
            },
            ClipboardShortcut::Paste => match read_pasteboard() {
                Some(text) => RawEvent::Paste { window, text },
                None => return,
            },
        };
        self.push(event);
    }

    fn set_cursor_icon(&self, icon: CursorIcon) {
        let ivars = self.ivars();
        if ivars.cursor.replace(icon) == icon {
            return;
        }
        let inside = self.mouse_inside();
        self.sync_cursor_hidden(inside);
        if let Some(window) = self.window() {
            window.invalidateCursorRectsForView(self);
        }
        // Cursor rects apply on the next mouse move; show the change now.
        if inside && let Some(cursor) = ns_cursor(icon) {
            cursor.set();
        }
    }

    /// Hold `[NSCursor hide]` exactly while the pointer is inside and the app
    /// asked for [`CursorIcon::Hidden`].
    fn sync_cursor_hidden(&self, inside: bool) {
        let ivars = self.ivars();
        let want = inside && ivars.cursor.get() == CursorIcon::Hidden;
        if want != ivars.cursor_hidden.replace(want) {
            if want {
                NSCursor::hide();
            } else {
                NSCursor::unhide();
            }
        }
    }

    fn mouse_inside(&self) -> bool {
        let Some(window) = self.window() else {
            return false;
        };
        let p = self.convertPoint_fromView(window.mouseLocationOutsideOfEventStream(), None);
        let b = self.bounds();
        p.x >= b.origin.x
            && p.y >= b.origin.y
            && p.x < b.origin.x + b.size.width
            && p.y < b.origin.y + b.size.height
    }

    fn set_ime_area(&self, caret: Option<LogicalRect>) {
        let ivars = self.ivars();
        ivars.ime_enabled.set(caret.is_some());
        ivars.ime_caret.set(caret);
        let Some(ctx) = self.inputContext() else {
            return;
        };
        if caret.is_some() {
            ctx.invalidateCharacterCoordinates();
        } else if !ivars.marked.borrow().is_empty() {
            // Focus left the field mid-composition: drop it on both sides.
            ctx.discardMarkedText();
            ivars.marked.borrow_mut().clear();
            self.push(RawEvent::ImePreedit(RawImePreedit {
                window: ivars.window,
                text: String::new(),
                caret: 0,
            }));
        }
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

/// Map an `NSEvent`'s hardware `keyCode` (a physical position, independent of
/// the keyboard layout) onto the platform [`KeyCode`]. Unknown codes ride as
/// `Other(scancode)` so higher layers can still route them.
fn keycode_of(event: &NSEvent) -> KeyCode {
    keycode_from_scancode(event.keyCode())
}

fn keycode_from_scancode(scancode: u16) -> KeyCode {
    use KeyCode as K;
    match scancode {
        0x00 => K::A,
        0x01 => K::S,
        0x02 => K::D,
        0x03 => K::F,
        0x04 => K::H,
        0x05 => K::G,
        0x06 => K::Z,
        0x07 => K::X,
        0x08 => K::C,
        0x09 => K::V,
        0x0a => K::IntlBackslash,
        0x0b => K::B,
        0x0c => K::Q,
        0x0d => K::W,
        0x0e => K::E,
        0x0f => K::R,
        0x10 => K::Y,
        0x11 => K::T,
        0x12 => K::Digit1,
        0x13 => K::Digit2,
        0x14 => K::Digit3,
        0x15 => K::Digit4,
        0x16 => K::Digit6,
        0x17 => K::Digit5,
        0x18 => K::Equal,
        0x19 => K::Digit9,
        0x1a => K::Digit7,
        0x1b => K::Minus,
        0x1c => K::Digit8,
        0x1d => K::Digit0,
        0x1e => K::BracketRight,
        0x1f => K::O,
        0x20 => K::U,
        0x21 => K::BracketLeft,
        0x22 => K::I,
        0x23 => K::P,
        0x24 => K::Enter,
        0x25 => K::L,
        0x26 => K::J,
        0x27 => K::Quote,
        0x28 => K::K,
        0x29 => K::Semicolon,
        0x2a => K::Backslash,
        0x2b => K::Comma,
        0x2c => K::Slash,
        0x2d => K::N,
        0x2e => K::M,
        0x2f => K::Period,
        0x30 => K::Tab,
        0x31 => K::Space,
        0x32 => K::Backquote,
        0x33 => K::Backspace,
        0x35 => K::Escape,
        0x36 => K::LogoRight,
        0x37 => K::LogoLeft,
        0x38 => K::ShiftLeft,
        0x39 => K::CapsLock,
        0x3a => K::AltLeft,
        0x3b => K::ControlLeft,
        0x3c => K::ShiftRight,
        0x3d => K::AltRight,
        0x3e => K::ControlRight,
        0x3f => K::Fn,
        0x40 => K::F17,
        0x41 => K::NumpadDecimal,
        0x43 => K::NumpadMultiply,
        0x45 => K::NumpadAdd,
        0x47 => K::NumLock,
        0x48 => K::VolumeUp,
        0x49 => K::VolumeDown,
        0x4a => K::VolumeMute,
        0x4b => K::NumpadDivide,
        0x4c => K::Enter,
        0x4e => K::NumpadSubtract,
        0x4f => K::F18,
        0x50 => K::F19,
        0x51 => K::NumpadEqual,
        0x52 => K::Numpad0,
        0x53 => K::Numpad1,
        0x54 => K::Numpad2,
        0x55 => K::Numpad3,
        0x56 => K::Numpad4,
        0x57 => K::Numpad5,
        0x58 => K::Numpad6,
        0x59 => K::Numpad7,
        0x5a => K::F20,
        0x5b => K::Numpad8,
        0x5c => K::Numpad9,
        0x5d => K::IntlYen,
        0x5e => K::IntlRo,
        0x5f => K::NumpadComma,
        0x60 => K::F5,
        0x61 => K::F6,
        0x62 => K::F7,
        0x63 => K::F3,
        0x64 => K::F8,
        0x65 => K::F9,
        0x66 => K::Lang2,
        0x67 => K::F11,
        0x68 => K::Lang1,
        0x69 => K::F13,
        0x6a => K::F16,
        0x6b => K::F14,
        0x6d => K::F10,
        0x6e => K::ContextMenu,
        0x6f => K::F12,
        0x71 => K::F15,
        0x72 => K::Insert,
        0x73 => K::Home,
        0x74 => K::PageUp,
        0x75 => K::Delete,
        0x76 => K::F4,
        0x77 => K::End,
        0x78 => K::F2,
        0x79 => K::PageDown,
        0x7a => K::F1,
        0x7b => K::Left,
        0x7c => K::Right,
        0x7d => K::Down,
        0x7e => K::Up,
        other => K::Other(u32::from(other)),
    }
}

/// Whether the modifier key `code` is down after a `flagsChanged:`, read from
/// the per-side device bits of the flags (`NX_DEVICE*KEYMASK`) so a left and
/// right key held together release independently. `None` for a non-modifier.
fn modifier_key_down(code: KeyCode, flags: NSEventModifierFlags) -> Option<bool> {
    let device = match code {
        KeyCode::ControlLeft => 0x0001,
        KeyCode::ShiftLeft => 0x0002,
        KeyCode::ShiftRight => 0x0004,
        KeyCode::LogoLeft => 0x0008,
        KeyCode::LogoRight => 0x0010,
        KeyCode::AltLeft => 0x0020,
        KeyCode::AltRight => 0x0040,
        KeyCode::ControlRight => 0x2000,
        KeyCode::CapsLock => return Some(flags.contains(NSEventModifierFlags::CapsLock)),
        KeyCode::Fn => return Some(flags.contains(NSEventModifierFlags::Function)),
        _ => return None,
    };
    Some(flags.0 & device != 0)
}

/// The AppKit cursor for `icon`; `None` for [`CursorIcon::Hidden`]. Shapes
/// AppKit only vends on newer releases or through undocumented class methods
/// are looked up by selector and fall back to the nearest public cursor.
fn ns_cursor(icon: CursorIcon) -> Option<Retained<NSCursor>> {
    use CursorIcon as C;
    Some(match icon {
        C::Hidden => return None,
        C::Default => NSCursor::arrowCursor(),
        C::Pointer => NSCursor::pointingHandCursor(),
        C::Text => NSCursor::IBeamCursor(),
        C::VerticalText => NSCursor::IBeamCursorForVerticalLayout(),
        C::Crosshair => NSCursor::crosshairCursor(),
        C::Grab => NSCursor::openHandCursor(),
        C::Grabbing => NSCursor::closedHandCursor(),
        C::NotAllowed => NSCursor::operationNotAllowedCursor(),
        C::ContextMenu => NSCursor::contextualMenuCursor(),
        C::Copy => NSCursor::dragCopyCursor(),
        C::Alias => NSCursor::dragLinkCursor(),
        C::ResizeEw | C::ResizeCol => {
            class_cursor(sel!(columnResizeCursor)).unwrap_or_else(|| legacy_resize_cursor(true))
        }
        C::ResizeNs | C::ResizeRow => {
            class_cursor(sel!(rowResizeCursor)).unwrap_or_else(|| legacy_resize_cursor(false))
        }
        C::Move => class_cursor(sel!(_moveCursor)).unwrap_or_else(NSCursor::openHandCursor),
        C::Wait | C::Progress => {
            class_cursor(sel!(busyButClickableCursor)).unwrap_or_else(NSCursor::arrowCursor)
        }
        C::Help => class_cursor(sel!(_helpCursor)).unwrap_or_else(NSCursor::arrowCursor),
        C::ZoomIn => class_cursor(sel!(zoomInCursor)).unwrap_or_else(NSCursor::arrowCursor),
        C::ZoomOut => class_cursor(sel!(zoomOutCursor)).unwrap_or_else(NSCursor::arrowCursor),
        C::ResizeNesw => class_cursor(sel!(_windowResizeNorthEastSouthWestCursor))
            .unwrap_or_else(NSCursor::crosshairCursor),
        C::ResizeNwse => class_cursor(sel!(_windowResizeNorthWestSouthEastCursor))
            .unwrap_or_else(NSCursor::crosshairCursor),
    })
}

/// The two-way resize cursors of releases that predate `columnResizeCursor` /
/// `rowResizeCursor`.
#[allow(deprecated)]
fn legacy_resize_cursor(horizontal: bool) -> Retained<NSCursor> {
    if horizontal {
        NSCursor::resizeLeftRightCursor()
    } else {
        NSCursor::resizeUpDownCursor()
    }
}

/// A cursor from an `NSCursor` class method that may not exist on this
/// release, called only after `respondsToSelector:` confirms it.
fn class_cursor(sel: Sel) -> Option<Retained<NSCursor>> {
    let class = NSCursor::class();
    if !class.responds_to(sel) {
        return None;
    }
    // SAFETY: the class answers `sel`, and every such selector is a
    // zero-argument class method returning an autoreleased `NSCursor`;
    // `performSelector:` returns it unretained and `retain` takes ownership.
    unsafe {
        let obj: *mut AnyObject = msg_send![class, performSelector: sel];
        Retained::retain(obj.cast::<NSCursor>())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in handler that records the events it receives, so a test can
    /// assert the order `drain_and_drive` delivers them. It may also re-enqueue
    /// a redraw on its first call to exercise the re-entrant path the modal
    /// resize loop relies on (the handler requesting another frame mid-drain).
    struct RecordingHandler {
        seen: Vec<RawEvent>,
        /// A window to re-enqueue once, on the first `handle` call, or `None`.
        reenqueue_once: Option<(Shared, WindowId)>,
    }

    impl AppHandler for RecordingHandler {
        fn handle(&mut self, event: RawEvent) -> ControlFlow {
            if let Some((shared, window)) = self.reenqueue_once.take() {
                shared.borrow_mut().redraws.push_back(window);
            }
            self.seen.push(event);
            ControlFlow::Wait
        }
    }

    fn is_redraw(event: &RawEvent, window: WindowId) -> bool {
        matches!(event, RawEvent::RedrawRequested { window: w } if *w == window)
    }

    #[test]
    fn drains_redraws_before_events_then_empties() {
        let shared: Shared = Rc::new(RefCell::new(PumpQueue::default()));
        {
            let mut q = shared.borrow_mut();
            q.events.push_back(RawEvent::Wakeup);
            q.redraws.push_back(WindowId(7));
        }

        let mut handler = RecordingHandler {
            seen: Vec::new(),
            reenqueue_once: None,
        };
        // SAFETY: `handler` outlives the call and no other borrow of it exists,
        // so the reborrow inside `drain_and_drive` is unaliased.
        let ptr = NonNull::from(&mut handler as &mut dyn AppHandler);
        unsafe { drain_and_drive(ptr, &shared) };

        // Redraw is converted and delivered ahead of the plain event, matching
        // `next_synthetic`'s ordering; nothing is left queued.
        assert_eq!(handler.seen.len(), 2);
        assert!(is_redraw(&handler.seen[0], WindowId(7)));
        assert!(matches!(handler.seen[1], RawEvent::Wakeup));
        let q = shared.borrow();
        assert!(q.events.is_empty());
        assert!(q.redraws.is_empty());
    }

    #[test]
    fn drains_events_reenqueued_mid_drive() {
        let shared: Shared = Rc::new(RefCell::new(PumpQueue::default()));
        shared.borrow_mut().events.push_back(RawEvent::Wakeup);

        // The first `handle` re-enqueues a redraw, as a frame requesting another
        // redraw would; the loop must pick it up rather than stop at one event.
        let mut handler = RecordingHandler {
            seen: Vec::new(),
            reenqueue_once: Some((shared.clone(), WindowId(3))),
        };
        // SAFETY: as above — `handler` outlives the call, no aliasing borrow.
        let ptr = NonNull::from(&mut handler as &mut dyn AppHandler);
        unsafe { drain_and_drive(ptr, &shared) };

        assert_eq!(handler.seen.len(), 2);
        assert!(matches!(handler.seen[0], RawEvent::Wakeup));
        assert!(is_redraw(&handler.seen[1], WindowId(3)));
        assert!(shared.borrow().events.is_empty());
        assert!(shared.borrow().redraws.is_empty());
    }

    #[test]
    fn scancodes_map_to_distinct_physical_keys() {
        let mut seen = std::collections::HashMap::new();
        for code in 0u16..=0x7f {
            let key = keycode_from_scancode(code);
            if matches!(key, KeyCode::Other(_)) {
                continue;
            }
            // Return and keypad Enter are the one intended pair.
            if let Some(prev) = seen.insert(key, code) {
                assert_eq!((prev, code), (0x24, 0x4c), "{key:?} mapped twice");
            }
        }
        assert_eq!(keycode_from_scancode(0x00), KeyCode::A);
        assert_eq!(keycode_from_scancode(0x1d), KeyCode::Digit0);
        assert_eq!(keycode_from_scancode(0x7a), KeyCode::F1);
        assert_eq!(keycode_from_scancode(0x52), KeyCode::Numpad0);
        assert_eq!(keycode_from_scancode(0x34), KeyCode::Other(0x34));
        let letters = (0u16..=0x7f)
            .filter(|c| {
                let k = format!("{:?}", keycode_from_scancode(*c));
                k.len() == 1 && k.as_bytes()[0].is_ascii_uppercase()
            })
            .count();
        assert_eq!(letters, 26);
    }

    #[test]
    fn modifier_sides_release_independently() {
        // Both shifts held, then the left one released.
        let both = NSEventModifierFlags(NSEventModifierFlags::Shift.0 | 0x0002 | 0x0004);
        let right_only = NSEventModifierFlags(NSEventModifierFlags::Shift.0 | 0x0004);
        assert_eq!(modifier_key_down(KeyCode::ShiftLeft, both), Some(true));
        assert_eq!(
            modifier_key_down(KeyCode::ShiftLeft, right_only),
            Some(false)
        );
        assert_eq!(
            modifier_key_down(KeyCode::ShiftRight, right_only),
            Some(true)
        );
        assert_eq!(
            modifier_key_down(KeyCode::CapsLock, NSEventModifierFlags::CapsLock),
            Some(true)
        );
        assert_eq!(modifier_key_down(KeyCode::A, both), None);
    }

    #[test]
    fn missing_cursor_selectors_fall_back() {
        // Cursor objects need a window-server connection a test process lacks,
        // so only the probe and the Hidden case are checked here.
        assert!(class_cursor(sel!(visoNoSuchCursor)).is_none());
        assert!(ns_cursor(CursorIcon::Hidden).is_none());
    }

    #[test]
    fn deliver_hands_back_the_copy_reply_only_for_copy_requests() {
        struct Answer;
        impl AppHandler for Answer {
            fn handle(&mut self, event: RawEvent) -> ControlFlow {
                if let RawEvent::CopyRequested { reply, .. } = &event {
                    // Answer then take it back, so the test leaves the real
                    // pasteboard untouched while exercising the reply slot.
                    reply.set("x".into());
                    assert_eq!(reply.take().as_deref(), Some("x"));
                }
                ControlFlow::Wait
            }
        }
        let reply = ClipboardReply::new();
        let flow = deliver(
            &mut Answer,
            RawEvent::CopyRequested {
                window: WindowId(1),
                cut: false,
                reply: reply.clone(),
            },
        );
        assert_eq!(flow, ControlFlow::Wait);
        assert_eq!(reply.take(), None);
    }
}
