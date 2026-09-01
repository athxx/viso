//! Native macOS backend (objc2 / AppKit).
//!
//! Follows makepad's *manual pump* model rather than `[NSApp run]`: we call
//! `finishLaunching` once, then loop pulling one event at a time with
//! `nextEventMatchingMask:untilDate:inMode:dequeue:` and `sendEvent:`. The
//! `untilDate` is `distantFuture` when the runtime returns [`ControlFlow::Wait`]
//! (block, zero CPU) and `distantPast` when it returns [`ControlFlow::Poll`]
//! (spin so the next display beat arrives promptly). This keeps the frame loop
//! fully under Viso's control — no hidden AppKit run loop, no makepad
//! `AppMain` adapter.
//!
//! Redraw beats are driven from the window's `drawRect:` path via an
//! `NSWindowDelegate`; resize/close/geometry also arrive through the delegate.
//! The delegate holds a raw pointer back to the pump's shared state and wraps
//! each callback in `catch_unwind` so a panic in Rust never unwinds across the
//! Objective-C frame.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEventMask, NSView,
    NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSDate, NSDefaultRunLoopMode, NSNotification, NSObject, NSObjectProtocol,
    NSPoint, NSRect, NSSize, NSString,
};

use crate::RawWindowHandle;
use crate::control::{ControlFlow, PlatformError, WindowConfig, WindowId};
use crate::event::{AcceptCell, RawEvent};
use crate::handler::AppHandler;
use crate::{PlatformApp, Window};

/// Events the delegate produces, drained by the pump between OS events.
#[derive(Default)]
struct PumpQueue {
    events: VecDeque<RawEvent>,
    /// Windows asked to redraw; converted to `RedrawRequested` beats.
    redraws: VecDeque<WindowId>,
    /// Set true once a window has been asked to close and accepted.
    should_exit: bool,
}

/// Shared state the delegate mutates and the pump reads. `Rc<RefCell<..>>`
/// because both live on the main thread; no cross-thread sharing here.
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

    /// Pull the next queued synthetic event (delegate-produced), if any.
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

        window.makeKeyAndOrderFront(None);

        // Grab the content view now for GPU surface attachment (§9 native
        // handles). AppKit gives every titled window a default content view;
        // the Metal backend attaches a `CAMetalLayer` to it.
        let content_view = window.contentView();

        self.windows.push(MacWindow {
            id,
            window,
            content_view,
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
            // Deliver any synthetic (delegate) events first — these are the
            // redraw beats, resizes and closes the delegate enqueued.
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
            // `distantFuture`). This mirrors makepad's per-turn pool.
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
                        // Forward to AppKit so the window actually
                        // draws/interacts; our own event vocabulary is produced
                        // by the delegate.
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

/// A native macOS window plus its retained delegate.
pub struct MacWindow {
    id: WindowId,
    window: Retained<NSWindow>,
    /// The window's content `NSView`, retained for GPU surface attachment.
    /// `None` only in the degenerate case of a window with no content view.
    content_view: Option<Retained<NSView>>,
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
        let frame = self.window.contentView().map(|v| v.frame());
        let scale = self.window.backingScaleFactor();
        match frame {
            Some(r) => (
                (r.size.width * scale) as u32,
                (r.size.height * scale) as u32,
            ),
            None => (0, 0),
        }
    }

    fn raw_handle(&self) -> RawWindowHandle {
        // The `NSView` pointer stays valid for the window's lifetime; the GPU
        // layer must not outlive this `MacWindow`.
        let ns_view = self
            .content_view
            .as_ref()
            .map(|v| Retained::as_ptr(v) as *mut core::ffi::c_void)
            .unwrap_or(core::ptr::null_mut());
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
            let accept = AcceptCell::new();
            let window = ivars.window;
            let shared = ivars.shared.clone();
            // catch_unwind: never let a Rust panic unwind through ObjC.
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let mut q = shared.borrow_mut();
                q.events.push_back(RawEvent::CloseRequested {
                    window,
                    accept: accept.clone(),
                });
            }));
            // Phase 1 always accepts; also mark the app to exit and emit the
            // WindowClosed follow-up the scheduler counts against open windows.
            if accept.is_accepted() {
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    let mut q = ivars.shared.borrow_mut();
                    q.events.push_back(RawEvent::WindowClosed { window });
                    q.should_exit = true;
                }));
                true
            } else {
                false
            }
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
