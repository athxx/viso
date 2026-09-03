//! Native macOS backend (objc2 / AppKit).
//!
//! Manual *pump* model rather than `[NSApp run]`: we call `finishLaunching`
//! once, then loop pulling one event at a time with
//! `nextEventMatchingMask:untilDate:inMode:dequeue:` and `sendEvent:`. The
//! `untilDate` is `distantFuture` when the runtime returns [`ControlFlow::Wait`]
//! (block, zero CPU) and `distantPast` when it returns [`ControlFlow::Poll`]
//! (spin so the next display beat arrives promptly). This keeps the frame loop
//! fully under Viso's control — no hidden AppKit run loop.
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

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEvent, NSEventMask,
    NSEventModifierFlags, NSTextInputClient, NSTrackingArea, NSTrackingAreaOptions, NSView,
    NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSAttributedString, NSAttributedStringKey, NSDate,
    NSDefaultRunLoopMode, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRange,
    NSRangePointer, NSRect, NSSize, NSString,
};

use crate::RawWindowHandle;
use crate::control::{ControlFlow, PlatformError, WindowConfig, WindowId};
use crate::event::{
    AcceptCell, KeyCode, Modifiers, PointerButtons, PointerPhase, RawEvent, RawImePreedit, RawKey,
    RawPointer, RawScroll, RawText,
};
use crate::handler::AppHandler;
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
        Ok(Self {
            mtm,
            app,
            shared: Rc::new(RefCell::new(PumpQueue::default())),
            next_window_id: 1,
            windows: Vec::new(),
            launched: false,
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
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Resizable
            | NSWindowStyleMask::Miniaturizable;

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
        window.center();

        let delegate = WindowDelegate::new(self.mtm, id, self.shared.clone());
        let proto = ProtocolObject::from_ref(&*delegate);
        window.setDelegate(Some(proto));

        // Our own flipped content view is both the event source and the GPU
        // surface. Installing it as the content view replaces AppKit's default
        // one; the Metal backend later attaches a `CAMetalLayer` to it.
        let view = VisoContentView::new(self.mtm, id, self.shared.clone(), content_rect);
        window.setContentView(Some(&view));
        window.makeFirstResponder(Some(&view));

        window.makeKeyAndOrderFront(None);

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
                let until = if block {
                    NSDate::distantFuture()
                } else {
                    NSDate::distantPast()
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
            // Timed out (poll with no event) — if nothing is pending and we're
            // not animating, drop to blocking on the next turn.
            if !got_event && !block {
                flow = ControlFlow::Wait;
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

/// Ivars for the window delegate: which window it serves and the shared queue.
struct DelegateIvars {
    window: WindowId,
    shared: Shared,
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
            let _ = catch_unwind(AssertUnwindSafe(|| {
                // The notification's object is the NSWindow.
                let obj = notification.object();
                let (scale, width, height) = obj
                    .and_then(|o| o.downcast::<NSWindow>().ok())
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
                let mut q = shared.borrow_mut();
                q.events.push_back(RawEvent::ScaleFactorChanged {
                    window,
                    scale,
                    width,
                    height,
                });
                q.redraws.push_back(window);
            }));
        }
    }
);

impl WindowDelegate {
    fn new(mtm: MainThreadMarker, window: WindowId, shared: Shared) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars { window, shared });
        unsafe { msg_send![super(this), init] }
    }
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
