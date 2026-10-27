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
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[path = "macos_shell.rs"]
mod shell;

use super::drive::Drive;
use super::ime_text::ImeText;
use super::macos_access::{MacAccess, Sink, wake_pump};
use super::macos_clipboard;
use super::macos_panel::{LayerScanout, emulation};
use super::main_queue::post_to_main;
use super::memory_pressure::MemoryPressure;
use crate::placement::{DeskRect, fit_on_screens};
use viso_handle::{Scanout, ScanoutFormat};

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, ProtocolObject, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAllRomanInputSourcesLocaleIdentifier, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
    NSApplicationTerminateReply, NSBackingStoreType, NSBitmapFormat, NSBitmapImageRep, NSColor,
    NSColorSpace, NSCursor, NSDeviceRGBColorSpace, NSDragOperation, NSDraggingContext,
    NSDraggingDestination, NSDraggingInfo, NSDraggingItem, NSDraggingSession, NSDraggingSource,
    NSEvent, NSEventMask, NSEventModifierFlags, NSEventPhase, NSImage, NSMenu, NSMenuItem,
    NSPasteboard, NSPasteboardItem, NSPasteboardTypeFileURL, NSPasteboardTypeString,
    NSPasteboardTypeURL, NSPreferredScrollerStyleDidChangeNotification, NSScreen, NSScroller,
    NSScrollerStyle, NSSystemColorsDidChangeNotification, NSTextInputClient, NSTrackingArea,
    NSTrackingAreaOptions, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
    NSWindowTitleVisibility, NSWorkspace,
    NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSAttributedString, NSAttributedStringKey,
    NSCurrentLocaleDidChangeNotification, NSDate, NSDefaultRunLoopMode, NSLocale, NSNotification,
    NSNotificationCenter, NSObject, NSObjectProtocol, NSPoint, NSProcessInfo, NSRange,
    NSRangePointer, NSRect, NSSize, NSString, NSURL,
};

use crate::RawWindowHandle;
use crate::clipboard::{ClipboardData, ClipboardKinds, ClipboardReply, PasteReply};
use crate::control::{
    ControlFlow, EditLabels, EditMenu, EditMenuAction, LogicalRect, PlatformError, SurroundingText,
    TextEntry, WindowChrome, WindowConfig, WindowId,
};
use crate::cursor::{CursorCache, CursorImage};
use crate::drop::{DropAction, DropActions, DropKinds, DropOffer, DropPhase, DropReply, RawDrop};
use crate::event::{
    AcceptCell, Appearance, ClipboardShortcut, ColorScheme, CursorIcon, DisplayFeature,
    DisplayFeatureKind, KeyCode, Modifiers, PointerButtons, PointerPhase, RawEvent, RawKey,
    RawPointer, RawScroll, clipboard_shortcut,
};
use crate::handler::AppHandler;
use crate::locale;
use crate::menu::{Accel, Menu, SystemAction};
use crate::{LoopWaker, PlatformApp, Window};

/// Events the delegate/view produce, drained by the pump between OS events.
#[derive(Default)]
struct PumpQueue {
    events: VecDeque<RawEvent>,
    /// Windows asked to redraw; converted to `RedrawRequested` beats.
    redraws: VecDeque<WindowId>,
    /// Set true once a window has been asked to close and accepted.
    should_exit: bool,
    /// The pump's app and handler, exposed to delegates only while a pump
    /// turn has handed the thread to AppKit. A live-resize drag runs AppKit's
    /// nested modal loop inside that turn, which suspends our
    /// `nextEventMatchingMask:` loop entirely — so events the resize delegate
    /// enqueues cannot be drained until the drag ends. `windowDidResize:`,
    /// which AppKit calls *synchronously* inside that modal loop, reads this to
    /// drive a frame on the spot so the content tracks the drag live. `run()`
    /// installs it just before each AppKit turn, holding no borrow of the app
    /// or handler across the turn, and clears it right after (a guard clears
    /// it on unwind too), so the pointers are the only live path to either
    /// while they are set. See `MacApp::run` and `drain_and_drive`.
    drive: Option<Drive>,
    /// The flow the handler last asked for from inside an AppKit callback's
    /// drive, which the pump takes up after the turn: a timer armed there
    /// (a frame during a live resize, say) still wakes the loop.
    driven: Option<ControlFlow>,
    /// The appearance last reported, so the several AppKit sources that signal
    /// a change (every view's `viewDidChangeEffectiveAppearance`, the workspace
    /// accessibility notification) emit one `AppearanceChanged` per real change.
    appearance: Appearance,
    /// The locale last reported, so a notification that changes nothing the
    /// app sees emits no `LocaleChanged`.
    locale: String,
    /// Set by a [`LoopWaker`] kick from any thread; drained as one
    /// `RawEvent::Wakeup`.
    woken: Arc<AtomicBool>,
    /// The app's last copy, while the pasteboard may still hold it.
    clipboard: Option<macos_clipboard::Owned>,
}

impl PumpQueue {
    /// The next synthetic event: a pending wake first, then redraw beats,
    /// then the delegate/view queue. A wake goes ahead of the beats so a
    /// window that draws every beat cannot hold off what the wake delivers.
    fn pop(&mut self) -> Option<RawEvent> {
        if self.woken.swap(false, Ordering::AcqRel) {
            return Some(RawEvent::Wakeup);
        }
        if let Some(w) = self.redraws.pop_front() {
            return Some(RawEvent::RedrawRequested { window: w });
        }
        self.events.pop_front()
    }
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
    /// The tray icon in the menu bar, while the app shows one.
    status_item: Option<shell::StatusItem>,
    /// The kernel's memory-pressure notifications, queued as `LowMemory`.
    _memory_pressure: Option<MemoryPressure>,
    /// The game controllers, read from the first poll.
    gamepads: crate::gamepad::Gamepads,
}

impl MacApp {
    /// Write a copy's promised image now: it dies with the app otherwise.
    fn settle_clipboard(&self) {
        if let Some(owned) = &self.shared.borrow().clipboard {
            macos_clipboard::settle(&NSPasteboard::generalPasteboard(), owned);
        }
    }

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
            locale: current_locale(),
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
            // So does the scroller style, with Settings or a mouse plugged in.
            NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
                &app_delegate,
                sel!(accessibilityDisplayOptionsChanged:),
                Some(NSPreferredScrollerStyleDidChangeNotification),
                None,
            );
            // So does the accent color, which no view hears of.
            NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
                &app_delegate,
                sel!(accessibilityDisplayOptionsChanged:),
                Some(NSSystemColorsDidChangeNotification),
                None,
            );
            // The preferred languages change outside the app, in Settings.
            NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
                &app_delegate,
                sel!(currentLocaleChanged:),
                Some(NSCurrentLocaleDidChangeNotification),
                None,
            );
        }

        let pressure_queue = shared.clone();
        let memory_pressure = MemoryPressure::on_main_queue(move || {
            if let Ok(mut q) = pressure_queue.try_borrow_mut() {
                q.events.push_back(RawEvent::LowMemory);
            }
            // The source fires while the pump blocks in `nextEventMatchingMask:`,
            // which returns only for an event: post one so the warning is
            // answered now, not whenever the next input arrives.
            wake_pump();
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
            status_item: None,
            _memory_pressure: memory_pressure,
            gamepads: crate::gamepad::Gamepads::default(),
        })
    }

    /// The queue the accessibility adapters push their requests onto. They run
    /// inside AppKit callbacks, so a panic is caught before it can unwind into
    /// Objective-C.
    fn access_sink(&self) -> Sink {
        let shared = self.shared.clone();
        Rc::new(move |event| {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                shared.borrow_mut().events.push_back(event);
            }));
        })
    }

    /// Pull the next queued synthetic event (delegate/view-produced), if any.
    fn next_synthetic(&self) -> Option<RawEvent> {
        self.shared.borrow_mut().pop()
    }
}

/// Clears `PumpQueue::drive` when `run`'s loop ends, on every path including
/// unwind, so the erased pointers never outlive the `&mut` borrows they were
/// taken from.
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
/// SAFETY: the caller passes the drive stored in `PumpQueue::drive`, which is
/// set only while a pump turn has handed the thread to AppKit and holds no
/// borrow of the app or handler. This runs synchronously inside that turn, so
/// the `&mut` reborrows are the only live ones.
unsafe fn drain_and_drive(drive: Drive, shared: &Shared) {
    loop {
        let event = shared.borrow_mut().pop();
        let Some(event) = event else { break };
        // SAFETY: per the function contract.
        let (app, handler) = unsafe { drive.get() };
        let flow = deliver(app, handler, shared, event);
        shared.borrow_mut().driven = Some(flow);
    }
}

/// Hand one event to the handler, completing the clipboard handshakes: a
/// `CopyRequested` reply the handler filled is written to the pasteboard once
/// the handler returns, and a `PasteRequested` reads the kinds it took.
fn deliver(
    app: &mut dyn PlatformApp,
    handler: &mut dyn AppHandler,
    shared: &Shared,
    event: RawEvent,
) -> ControlFlow {
    enum After {
        Copy(ClipboardReply),
        Paste(WindowId, PasteReply),
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
                write_clipboard(shared, data);
            }
        }
        Some(After::Paste(window, reply)) => {
            let kinds = reply.get();
            if kinds.any() {
                let data = read_clipboard(shared, kinds);
                if !data.is_empty() {
                    shared.borrow_mut().events.push_back(RawEvent::Paste {
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

/// Replace what the general pasteboard holds with `data`.
fn write_clipboard(shared: &Shared, data: ClipboardData) {
    let owned = macos_clipboard::write(&NSPasteboard::generalPasteboard(), data);
    shared.borrow_mut().clipboard = Some(owned);
}

/// What the general pasteboard holds of `kinds`.
fn read_clipboard(shared: &Shared, kinds: ClipboardKinds) -> ClipboardData {
    let pasteboard = NSPasteboard::generalPasteboard();
    let queue = shared.borrow();
    macos_clipboard::read(&pasteboard, kinds, queue.clipboard.as_ref())
}

/// What a drag's pasteboard offers: its files, its first link that is no
/// file, and its text.
fn pasteboard_offer(pasteboard: &NSPasteboard) -> DropOffer {
    // SAFETY: the pasteboard type names are immutable AppKit constants.
    let (file, url, string) = unsafe {
        (
            NSPasteboardTypeFileURL,
            NSPasteboardTypeURL,
            NSPasteboardTypeString,
        )
    };
    let mut offer = DropOffer::default();
    if let Some(types) = pasteboard.types() {
        offer.kinds = DropKinds {
            text: types.containsObject(string),
            uri: types.containsObject(url),
            files: types.containsObject(file),
        };
    }
    for item in pasteboard
        .pasteboardItems()
        .iter()
        .flat_map(|items| items.iter())
    {
        if let Some(path) = item
            .stringForType(file)
            .and_then(|uri| crate::drop::file_path(&uri.to_string()))
        {
            offer.files.push(path);
        } else if offer.uri.is_none()
            && let Some(uri) = item.stringForType(url)
        {
            offer.uri = Some(uri.to_string());
        }
    }
    // A file's URL is a URL too: what is left of the links is none.
    offer.kinds.uri &= offer.uri.is_some() || offer.files.is_empty();
    offer.text = pasteboard.stringForType(string).map(|s| s.to_string());
    offer
}

/// The actions an AppKit drag's source allows.
fn drag_actions(mask: NSDragOperation) -> DropActions {
    DropActions {
        moves: mask.intersects(NSDragOperation::Move | NSDragOperation::Generic),
        copies: mask.contains(NSDragOperation::Copy),
        links: mask.contains(NSDragOperation::Link),
    }
}

/// The AppKit operation of `action`.
fn drag_operation(action: Option<DropAction>) -> NSDragOperation {
    match action {
        Some(DropAction::Move) => NSDragOperation::Move,
        Some(DropAction::Copy) => NSDragOperation::Copy,
        Some(DropAction::Link) => NSDragOperation::Link,
        None => NSDragOperation::None,
    }
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
        // macOS has no system text size an app can read.
        text_scale: 1.0,
        overlay_scroll_bars: NSScroller::preferredScrollerStyle(MainThreadMarker::from(app))
            == NSScrollerStyle::Overlay,
        accent: NSColor::controlAccentColor()
            .colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())
            .map(|c| {
                [c.redComponent(), c.greenComponent(), c.blueComponent()]
                    .map(|v| (v as f32).clamp(0.0, 1.0))
            }),
        // System Settings > Mouse > Double-click speed.
        double_click: core::time::Duration::try_from_secs_f64(NSEvent::doubleClickInterval())
            .ok()
            .filter(|d| !d.is_zero()),
    }
}

/// The user's first preferred language, as a BCP 47 tag.
fn current_locale() -> String {
    NSLocale::preferredLanguages()
        .firstObject()
        .map_or_else(|| "und".to_owned(), |tag| locale::bcp47(&tag.to_string()))
}

/// Re-read the locale and enqueue `LocaleChanged` if it moved.
fn report_locale(shared: &Shared) {
    let now = current_locale();
    let mut q = shared.borrow_mut();
    if q.locale != now {
        q.locale.clone_from(&now);
        q.events.push_back(RawEvent::LocaleChanged { locale: now });
    }
}

/// The camera housing of a notched display where it crosses `window`'s
/// content: in windowed mode it sits in the menu bar, so only a window that
/// reaches the top edge (full screen, or placed there) meets it.
fn notch_features(window: &NSWindow) -> Vec<DisplayFeature> {
    let Some(screen) = window.screen() else {
        return Vec::new();
    };
    let insets = screen.safeAreaInsets();
    let (left, right) = (
        screen.auxiliaryTopLeftArea(),
        screen.auxiliaryTopRightArea(),
    );
    if insets.top <= 0.0 || left.size.width <= 0.0 || right.size.width <= 0.0 {
        return Vec::new();
    }
    let frame = screen.frame();
    // The auxiliary areas are documented in screen coordinates; place them
    // in the global space the window frame uses when they are not already.
    let inside = left.origin.x >= frame.origin.x
        && left.origin.y >= frame.origin.y
        && left.origin.x + left.size.width <= frame.origin.x + frame.size.width;
    let dx = if inside { 0.0 } else { frame.origin.x };
    let top = frame.origin.y + frame.size.height;
    let notch_x0 = left.origin.x + left.size.width + dx;
    let notch_x1 = right.origin.x + dx;
    let notch_y0 = top - insets.top;
    let content = window.contentRectForFrameRect(window.frame());
    let x0 = notch_x0.max(content.origin.x);
    let x1 = notch_x1.min(content.origin.x + content.size.width);
    let y0 = notch_y0.max(content.origin.y);
    let y1 = top.min(content.origin.y + content.size.height);
    if x1 <= x0 || y1 <= y0 {
        return Vec::new();
    }
    vec![DisplayFeature {
        kind: DisplayFeatureKind::Cutout,
        bounds: LogicalRect::new(
            x0 - content.origin.x,
            content.origin.y + content.size.height - y1,
            x1 - x0,
            y1 - y0,
        ),
    }]
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

        // A window standing in for an embedded panel takes the panel's size,
        // each panel pixel `zoom` screen pixels a side, and keeps it.
        let screen = NSScreen::mainScreen(self.mtm);
        let backing = screen.as_ref().map_or(1.0, |s| s.backingScaleFactor());
        let room = screen.as_ref().map_or((f64::MAX, f64::MAX), |s| {
            let free = s.visibleFrame().size;
            (free.width * 0.9, free.height * 0.9)
        });
        let emulated = emulation(config.cpu_frames, backing, room);
        let (w, h) = match &emulated {
            Some(e) => panel_points(e.size, e.zoom, backing),
            None => config.logical_size,
        };
        let content_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h));
        let mut style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable;
        if emulated.is_none() {
            style |= NSWindowStyleMask::Resizable;
        }
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
        match &emulated {
            Some(e) => window.setTitle(&NSString::from_str(&format!(
                "{} — {}",
                config.title, e.note
            ))),
            None => window.setTitle(&NSString::from_str(&config.title)),
        }

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

        // A position puts the frame's top-left there, kept on the screens;
        // otherwise the window is centred.
        match config.position {
            Some((x, y)) => {
                let size = window.frame().size;
                let wanted = DeskRect::new(x, y, size.width, size.height);
                let fitted = fit_on_screens(wanted, &screen_areas(self.mtm));
                let top = primary_screen_height(self.mtm).unwrap_or(0.0);
                let frame = NSRect::new(
                    NSPoint::new(fitted.x, top - (fitted.y + fitted.height)),
                    NSSize::new(fitted.width, fitted.height),
                );
                window.setFrame_display(frame, false);
            }
            None => window.center(),
        }
        if config.maximized && emulated.is_none() {
            window.zoom(None);
        }

        let delegate = WindowDelegate::new(self.mtm, id, self.shared.clone(), config.chrome);
        let proto = ProtocolObject::from_ref(&*delegate);
        window.setDelegate(Some(proto));

        // Our own flipped content view is both the event source and the GPU
        // surface. Installing it as the content view replaces AppKit's default
        // one; the Metal backend later attaches a `CAMetalLayer` to it.
        let view = VisoContentView::new(self.mtm, id, self.shared.clone(), content_rect);
        window.setContentView(Some(&view));
        // A CPU-drawn window shows its frames as the content view layer's
        // image.
        let pixels = config.cpu_frames.then(|| {
            let backing = window.backingScaleFactor();
            if let Some(e) = &emulated {
                let (w, h) = panel_points(e.size, e.zoom, backing);
                window.setContentSize(NSSize::new(w, h));
            }
            Rc::new(match &emulated {
                Some(e) => LayerScanout::new(&view, e.format, backing, e.zoom, Some(e.scale)),
                None => LayerScanout::new(&view, ScanoutFormat::Xrgb8888, backing, 1.0, None),
            })
        });
        if let Some(pixels) = &pixels {
            view.ivars().logical.set(pixels.logical_per_point());
            view.ivars().pixels.replace(Some(pixels.clone()));
            delegate.ivars().pixels.replace(Some(pixels.clone()));
        }
        window.makeFirstResponder(Some(&view));
        let access = MacAccess::attach(&view, id, self.access_sink());

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
            access,
            pixels,
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

        if handler.handle(self, RawEvent::AppLaunched) == ControlFlow::Exit {
            self.settle_clipboard();
            return;
        }

        // Clears the drive on every exit path, including unwind out of an
        // AppKit turn.
        let queue = self.shared.clone();
        let _drive_guard = DriveGuard(queue.clone());

        let mut flow = ControlFlow::Wait;
        // Whether a redraw beat was just delivered: the pump then takes what
        // the OS has before the next synthetic event, so a window that draws
        // every beat does not starve input.
        let mut drew = false;
        loop {
            if self.shared.borrow().should_exit {
                break;
            }
            // Deliver any synthetic (delegate/view) events first — redraw beats,
            // resizes, closes, and the pointer/key/scroll/IME samples the view
            // enqueued while AppKit dispatched the last OS event.
            if !drew && let Some(event) = self.next_synthetic() {
                if let RawEvent::WindowFocused { window, focused } = event
                    && let Some(win) = self.windows.iter_mut().find(|w| w.id == window)
                {
                    win.access.set_focused(focused);
                    win.content_view.set_key(focused);
                    // The pointer goes back to the user with the focus.
                    if !focused && win.content_view.release_pointer_lock() {
                        self.shared
                            .borrow_mut()
                            .events
                            .push_front(RawEvent::PointerLockChanged {
                                window,
                                locked: false,
                            });
                    }
                }
                drew = matches!(event, RawEvent::RedrawRequested { .. });
                let shared = self.shared.clone();
                flow = deliver(self, handler, &shared, event);
                if flow == ControlFlow::Exit {
                    break;
                }
                continue;
            }
            let yielding = std::mem::take(&mut drew);

            // Then pull one OS event. Block or spin per the last decision.
            // Each turn runs inside its own autorelease pool: AppKit's event
            // machinery vends autoreleased objects every iteration, and without
            // a pool draining them `nextEventMatchingMask:` degenerates into a
            // busy spin (it returns immediately instead of blocking on
            // `distantFuture`).
            let block = !yielding && matches!(flow, ControlFlow::Wait | ControlFlow::WaitUntil(_));
            // AppKit may re-enter during the turn (a live resize drives frames
            // from `windowDidResize:`), reaching the app and handler through
            // the drive alone: the turn touches neither, only its own handle
            // on NSApplication.
            let nsapp = self.app.clone();
            queue.borrow_mut().drive = Some(Drive::new(self, handler));
            let got_event = autoreleasepool(|_| {
                let until = match flow {
                    // After a beat: only what is already queued.
                    _ if yielding => NSDate::distantPast(),
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
                    nsapp.nextEventMatchingMask_untilDate_inMode_dequeue(
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
                        nsapp.sendEvent(&ev);
                        true
                    }
                    None => false,
                }
            });
            queue.borrow_mut().drive = None;
            let driven = queue.borrow_mut().driven.take();
            if let Some(driven) = driven {
                flow = driven;
                if flow == ControlFlow::Exit {
                    break;
                }
                continue;
            }
            if !got_event && !yielding {
                match flow {
                    // A `WaitUntil` that returned no OS event means the timer
                    // deadline elapsed: synthesize a wakeup so the scheduler runs
                    // one frame and fires the due timer. The frame's own decision
                    // then re-arms the next deadline (or drops to `Wait`).
                    ControlFlow::WaitUntil(_) => {
                        flow = handler.handle(self, RawEvent::Wakeup);
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
        self.settle_clipboard();
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

    fn loop_waker(&self) -> LoopWaker {
        extern "C" fn kick(_: *mut std::ffi::c_void) {
            // A pump blocked in `nextEventMatchingMask:` returns only for an
            // event, so post one; the pump then drains the flag.
            let _ = catch_unwind(wake_pump);
        }
        let woken = self.shared.borrow().woken.clone();
        LoopWaker::new(move || {
            woken.store(true, Ordering::Release);
            post_to_main(kick);
        })
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

    fn set_tray(&mut self, tray: Option<&crate::Tray>) {
        match (tray, &mut self.status_item) {
            (None, item) => *item = None,
            (Some(tray), Some(item)) => {
                item.update(self.mtm, tray, &self.shared, &self._app_delegate);
            }
            (Some(tray), item @ None) => {
                *item = Some(shell::StatusItem::new(
                    self.mtm,
                    tray,
                    &self.shared,
                    &self._app_delegate,
                ));
            }
        }
    }

    fn set_badge(&mut self, badge: Option<&crate::AppBadge>) {
        shell::set_badge(self.mtm, badge);
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

    fn set_fullscreen(&mut self, window: WindowId, fullscreen: bool) {
        // AppKit animates the transition and reports it through the window
        // delegate's will-enter/will-exit callbacks.
        let Some(win) = self.windows.iter().find(|w| w.id == window) else {
            return;
        };
        let is = win
            .window
            .styleMask()
            .contains(NSWindowStyleMask::FullScreen);
        if is != fullscreen {
            win.window.toggleFullScreen(None);
        }
    }

    fn update_accessibility(&mut self, window: WindowId, delta: crate::SemanticsDelta) {
        if let Some(win) = self.windows.iter_mut().find(|w| w.id == window) {
            win.access.update(delta);
        }
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
        closed.content_view.release_pointer_lock();
        closed.window.close();
        self.shared
            .borrow_mut()
            .events
            .push_back(RawEvent::WindowClosed { window });
    }

    fn set_clipboard(&mut self, data: ClipboardData) {
        write_clipboard(&self.shared, data);
    }

    fn request_paste(&mut self, window: WindowId, kinds: ClipboardKinds, token: u32) {
        let data = read_clipboard(&self.shared, kinds);
        self.shared.borrow_mut().events.push_back(RawEvent::Paste {
            window,
            data,
            token: Some(token),
        });
    }

    fn set_cursor(&mut self, window: WindowId, icon: CursorIcon) {
        if let Some(win) = self.windows.iter().find(|w| w.id == window) {
            win.content_view.set_cursor_icon(icon);
        }
    }

    fn set_pointer_lock(&mut self, window: WindowId, lock: bool) {
        if let Some(win) = self.windows.iter().find(|w| w.id == window) {
            win.content_view.set_pointer_lock(lock);
        }
    }

    fn set_text_entry(&mut self, window: WindowId, entry: Option<TextEntry>) {
        if let Some(win) = self.windows.iter().find(|w| w.id == window) {
            win.content_view.set_text_entry(entry);
        }
    }

    fn set_surrounding_text(&mut self, window: WindowId, text: Option<&SurroundingText>) {
        if let Some(win) = self.windows.iter().find(|w| w.id == window) {
            win.content_view.set_surrounding_text(text);
        }
    }

    fn show_context_menu(&mut self, window: WindowId, menu: EditMenu, labels: EditLabels) {
        if let Some(win) = self.windows.iter().find(|w| w.id == window) {
            win.content_view.queue_context_menu(menu, labels);
        }
    }

    fn show_soft_keyboard(&mut self, _window: WindowId, _show: bool) {}

    fn start_drag(&mut self, window: WindowId, drag: crate::DragOut) -> bool {
        self.windows
            .iter()
            .find(|w| w.id == window)
            .is_some_and(|win| win.content_view.start_drag(&drag))
    }

    fn appearance(&self) -> Appearance {
        current_appearance(&self.app)
    }

    fn locale(&self) -> String {
        current_locale()
    }

    fn display_features(&self, window: WindowId) -> Vec<DisplayFeature> {
        self.windows
            .iter()
            .find(|w| w.id == window)
            .map(|w| notch_features(&w.window))
            .unwrap_or_default()
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
    /// The content view's accessibility adapter.
    access: MacAccess,
    /// The frames of a CPU-drawn window.
    pixels: Option<Rc<LayerScanout>>,
}

/// The scale a window reports and its surface's size in pixels: the screen's
/// backing scale and the content view's size at it, or a CPU-drawn window's
/// frame metrics.
fn window_metrics(nswin: Option<&NSWindow>, pixels: Option<&LayerScanout>) -> (f64, u32, u32) {
    if let Some(pixels) = pixels {
        return pixels.metrics();
    }
    let Some(nswin) = nswin else {
        return (1.0, 0, 0);
    };
    let scale = nswin.backingScaleFactor();
    let size = nswin
        .contentView()
        .map_or(NSSize::new(0.0, 0.0), |v| v.frame().size);
    (
        scale,
        (size.width * scale) as u32,
        (size.height * scale) as u32,
    )
}

/// The points a panel of `size` pixels takes, each pixel `zoom` screen pixels
/// a side on a screen of `backing` scale.
fn panel_points(size: (u32, u32), zoom: f64, backing: f64) -> (f64, f64) {
    (
        f64::from(size.0) * zoom / backing,
        f64::from(size.1) * zoom / backing,
    )
}

/// The height of the primary screen, whose bottom-left is Cocoa's desktop
/// origin and whose top-left is the one [`Window::outer_position`] measures
/// from.
fn primary_screen_height(mtm: MainThreadMarker) -> Option<f64> {
    NSScreen::screens(mtm)
        .firstObject()
        .map(|screen| screen.frame().size.height)
}

/// `window`'s outer top-left corner in top-left desktop points.
fn top_left_of(window: &NSWindow) -> Option<(f64, f64)> {
    let frame = window.frame();
    let top = primary_screen_height(window.mtm())?;
    Some((frame.origin.x, top - (frame.origin.y + frame.size.height)))
}

/// Each screen's area outside the menu bar and the Dock, in top-left
/// desktop points.
fn screen_areas(mtm: MainThreadMarker) -> Vec<DeskRect> {
    let top = primary_screen_height(mtm).unwrap_or(0.0);
    NSScreen::screens(mtm)
        .iter()
        .map(|screen| {
            let f = screen.visibleFrame();
            DeskRect::new(
                f.origin.x,
                top - (f.origin.y + f.size.height),
                f.size.width,
                f.size.height,
            )
        })
        .collect()
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
        match &self.pixels {
            Some(pixels) => pixels.metrics().0,
            None => self.window.backingScaleFactor(),
        }
    }

    fn inner_size(&self) -> (u32, u32) {
        if let Some(pixels) = &self.pixels {
            let (_, w, h) = pixels.metrics();
            return (w, h);
        }
        let frame = self.content_view.frame();
        let scale = self.window.backingScaleFactor();
        (
            (frame.size.width * scale) as u32,
            (frame.size.height * scale) as u32,
        )
    }

    fn outer_position(&self) -> Option<(f64, f64)> {
        top_left_of(&self.window)
    }

    fn mode(&self) -> crate::WindowMode {
        if self.window.isMiniaturized() {
            crate::WindowMode::Minimized
        } else if self
            .window
            .styleMask()
            .contains(NSWindowStyleMask::FullScreen)
        {
            crate::WindowMode::Fullscreen
        } else if self.window.isZoomed() {
            crate::WindowMode::Maximized
        } else {
            crate::WindowMode::Normal
        }
    }

    fn raw_handle(&self) -> RawWindowHandle {
        // The `NSView` pointer stays valid for the window's lifetime; the GPU
        // layer must not outlive this `MacWindow`.
        if let Some(pixels) = &self.pixels {
            return RawWindowHandle::Scanout {
                scanout: Rc::as_ptr(pixels) as *const dyn Scanout,
            };
        }
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
    /// The display features last reported for the window.
    features: RefCell<Vec<DisplayFeature>>,
    /// The frames of a CPU-drawn window, whose metrics it reports.
    pixels: RefCell<Option<Rc<LayerScanout>>>,
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
            // deferring the actual close. Every close is accepted: enqueue
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
                let (scale, width, height) =
                    window_metrics(nswin.as_deref(), ivars.pixels.borrow().as_deref());
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
                if let Some(nswin) = &nswin {
                    self.report_features(nswin);
                }
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
                    // SAFETY: `drive` is what `MacApp::run` installed for the
                    // AppKit turn this callback runs inside, on the main thread,
                    // while the pump holds no borrow of the app or handler. See
                    // `drain_and_drive`.
                    unsafe { drain_and_drive(drive, &shared) };
                }
            }));
        }

        #[unsafe(method(windowDidMove:))]
        fn window_did_move(&self, notification: &NSNotification) {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                if let Some(nswin) = notification
                    .object()
                    .and_then(|o| o.downcast::<NSWindow>().ok())
                {
                    self.report_features(&nswin);
                    if let Some((x, y)) = top_left_of(&nswin) {
                        let ivars = self.ivars();
                        ivars.shared.borrow_mut().events.push_back(RawEvent::Moved {
                            window: ivars.window,
                            x,
                            y,
                        });
                    }
                }
            }));
        }

        #[unsafe(method(windowDidChangeScreen:))]
        fn window_did_change_screen(&self, notification: &NSNotification) {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                if let Some(nswin) = notification
                    .object()
                    .and_then(|o| o.downcast::<NSWindow>().ok())
                {
                    self.report_features(&nswin);
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
            features: RefCell::new(Vec::new()),
            pixels: RefCell::new(None),
        });
        // SAFETY: `this` is freshly allocated with its ivars set; `NSObject`'s
        // `init` is its designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    /// Re-reads the display features crossing `nswin` and enqueues
    /// `DisplayFeaturesChanged` if they moved.
    fn report_features(&self, nswin: &NSWindow) {
        let ivars = self.ivars();
        let now = notch_features(nswin);
        if *ivars.features.borrow() != now {
            ivars.features.replace(now.clone());
            ivars
                .shared
                .borrow_mut()
                .events
                .push_back(RawEvent::DisplayFeaturesChanged {
                    window: ivars.window,
                    features: now,
                });
        }
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

        /// A URL of a scheme the app declares, whether it launched the app
        /// or arrived while it runs; the pump delivers it after launch.
        #[unsafe(method(application:openURLs:))]
        fn open_urls(&self, _app: &NSApplication, urls: &NSArray<NSURL>) {
            for url in urls.iter() {
                if let Some(text) = url.absoluteString() {
                    self.push(RawEvent::OpenUrl {
                        url: text.to_string(),
                    });
                }
            }
            wake_pump();
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

        #[unsafe(method(currentLocaleChanged:))]
        fn current_locale_changed(&self, _notification: &NSNotification) {
            let shared = self.ivars().shared.clone();
            let _ = catch_unwind(AssertUnwindSafe(|| report_locale(&shared)));
            wake_pump();
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker, shared: Shared) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars { shared });
        // SAFETY: `this` is freshly allocated with its ivars set; `NSObject`'s
        // `init` is its designated initializer.
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
    // SAFETY: `delegate` outlives the menu: the application holds it for
    // the run, and AppKit keeps the target weakly.
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
        // SAFETY: both views are live and in one window; the rect is a plain value.
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
        // SAFETY: `this` is freshly allocated with its ivars set; `NSObject`'s
        // `init` is its designated initializer.
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
            // SAFETY: `target` outlives the item: it is kept in `targets` beside the
            // menu, and AppKit keeps the target weakly.
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
        // SAFETY: `delegate` outlives the menu: the application holds it for
        // the run, and AppKit keeps the target weakly.
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
/// pointer-button mask, and the text the input method edits.
struct ViewIvars {
    window: WindowId,
    shared: Shared,
    /// Buttons currently held, as a [`PointerButtons`] mask.
    buttons: Cell<u8>,
    /// The text the input method edits: the focused field's around its
    /// caret, and the composition under way.
    ime: RefCell<ImeText>,
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
    /// Where the mouse's pointer is pinned while it is locked to this view,
    /// in Cocoa desktop points; `None` while it is free.
    lock_pin: Cell<Option<NSPoint>>,
    /// How far the last warp moved the cursor, in desktop points, which the
    /// next mouse event's delta counts as motion and the lock takes back out.
    warp_debt: Cell<(f64, f64)>,
    /// A text field has focus: key events go through the input context.
    ime_enabled: Cell<bool>,
    /// The focused field's caret, logical points in this view.
    ime_caret: Cell<Option<LogicalRect>>,
    /// The focused field takes a secret.
    secure_field: Cell<bool>,
    /// The focused field takes a number, which no input method composes.
    number_field: Cell<bool>,
    /// The input context keeps to Roman keyboards.
    roman: Cell<bool>,
    /// The window is key, so its focused field takes the keyboard.
    key: Cell<bool>,
    /// Holds secure event input while a secure field of the key window has
    /// focus.
    secure_input: SecureInput,
    /// The app's logical points in one of this view's points: one, or the
    /// emulated panel's pixels over its scale.
    logical: Cell<f64>,
    /// The frames of a CPU-drawn window, rescaled when the screen changes.
    pixels: RefCell<Option<Rc<LayerScanout>>>,
    /// The context menu asked for, popped up from the run loop.
    context_menu: Cell<Option<(EditMenu, EditLabels)>>,
    /// The trackpad's gestures under way: a magnify, a rotate, as bits.
    gestures: Cell<u8>,
    /// What a drag from outside over the view offers, read as it came, and
    /// the app's answer.
    arrival: RefCell<Option<(DropOffer, DropReply)>>,
    /// The last press or drag of a mouse button, which a drag the app
    /// carries out of the view starts from.
    last_mouse: RefCell<Option<Retained<NSEvent>>>,
    /// The actions the drag the app carried out of the view allows, while
    /// it runs.
    drag_out: Cell<Option<NSDragOperation>>,
}

/// The magnify among a view's trackpad gestures.
const MAGNIFY: u8 = 1;
/// The rotate among them.
const ROTATE: u8 = 2;

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

        // The window moved to a screen of another backing scale: its surface
        // has as many more or fewer pixels, and an emulated panel keeps its
        // size in screen pixels.
        #[unsafe(method(viewDidChangeBackingProperties))]
        fn view_did_change_backing_properties(&self) {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let Some(nswin) = self.window() else {
                    return;
                };
                let ivars = self.ivars();
                let backing = nswin.backingScaleFactor();
                let pixels = ivars.pixels.borrow().clone();
                if let Some(pixels) = &pixels {
                    if pixels.zoom() != 1.0 {
                        let (_, w, h) = pixels.metrics();
                        let (w, h) = panel_points((w, h), pixels.zoom(), backing);
                        nswin.setContentSize(NSSize::new(w, h));
                    }
                    pixels.rescale(backing);
                    ivars.logical.set(pixels.logical_per_point());
                }
                let (scale, width, height) = window_metrics(Some(&nswin), pixels.as_deref());
                let mut q = ivars.shared.borrow_mut();
                q.events.push_back(RawEvent::ScaleFactorChanged {
                    window: ivars.window,
                    scale,
                    width,
                    height,
                });
                q.redraws.push_back(ivars.window);
            }));
        }

        // Rebuild the tracking area on every geometry change so `mouseMoved`
        // and `mouseExited` fire across the whole current bounds.
        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            // SAFETY: AppKit calls this on the main thread with `self` live; the
            // tracking areas read and replaced are this view's own.
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

        #[unsafe(method(selectAll:))]
        fn select_all(&self, _sender: Option<&AnyObject>) {
            let window = self.ivars().window;
            self.push(RawEvent::EditMenu {
                window,
                action: EditMenuAction::SelectAll,
            });
        }

        #[unsafe(method(visoPopContextMenu))]
        fn pop_context_menu(&self) {
            let _ = catch_unwind(AssertUnwindSafe(|| self.pop_menu()));
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
            self.note_mouse(event);
            self.pointer(event, PointerPhase::Down, PointerButtons::PRIMARY, true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            self.pointer(event, PointerPhase::Up, PointerButtons::PRIMARY, false);
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            self.note_mouse(event);
            self.pointer(event, PointerPhase::Down, PointerButtons::SECONDARY, true);
        }

        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, event: &NSEvent) {
            self.pointer(event, PointerPhase::Up, PointerButtons::SECONDARY, false);
        }

        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, event: &NSEvent) {
            if !self.back_button(event, true) {
                self.note_mouse(event);
                self.pointer(event, PointerPhase::Down, PointerButtons::MIDDLE, true);
            }
        }

        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, event: &NSEvent) {
            if !self.back_button(event, false) {
                self.pointer(event, PointerPhase::Up, PointerButtons::MIDDLE, false);
            }
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            self.moved(event);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.note_mouse(event);
            self.moved(event);
        }

        #[unsafe(method(rightMouseDragged:))]
        fn right_mouse_dragged(&self, event: &NSEvent) {
            self.note_mouse(event);
            self.moved(event);
        }

        #[unsafe(method(otherMouseDragged:))]
        fn other_mouse_dragged(&self, event: &NSEvent) {
            self.note_mouse(event);
            self.moved(event);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, event: &NSEvent) {
            // A locked pointer stays where it is, inside.
            if self.ivars().lock_pin.get().is_some() {
                return;
            }
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
                let k = ivars.logical.get();
                let sample = RawScroll {
                    window: ivars.window,
                    x,
                    y,
                    delta_x: -dx * k,
                    delta_y: -dy * k,
                    modifiers: modifiers_of(event),
                    phase: scroll_phase(event),
                };
                self.push(RawEvent::Scroll(sample));
            }));
        }

        #[unsafe(method(magnifyWithEvent:))]
        fn magnify_with_event(&self, event: &NSEvent) {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                self.gesture(event, MAGNIFY, 1.0 + event.magnification(), 0.0);
            }));
        }

        #[unsafe(method(rotateWithEvent:))]
        fn rotate_with_event(&self, event: &NSEvent) {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                // AppKit turns counterclockwise in degrees.
                let turn = -f64::from(event.rotation()).to_radians();
                self.gesture(event, ROTATE, 1.0, turn);
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

    // The `NSTextInputClient` protocol: the input method composes, commits
    // and reads the text around the caret through it. Ranges count UTF-16
    // units of the text the app shows, the composition included.
    unsafe impl NSTextInputClient for VisoContentView {
        #[unsafe(method(insertText:replacementRange:))]
        unsafe fn insert_text(&self, string: &AnyObject, replacement: NSRange) {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let text = any_to_string(string);
                let ivars = self.ivars();
                let mut ime = ivars.ime.borrow_mut();
                let over = self.range_bytes(&ime, replacement);
                let event = ime.insert(ivars.window, &text, over);
                drop(ime);
                if let Some(event) = event {
                    self.push(event);
                }
            }));
        }

        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        unsafe fn set_marked_text(
            &self,
            string: &AnyObject,
            selected: NSRange,
            replacement: NSRange,
        ) {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let text = any_to_string(string);
                let ivars = self.ivars();
                let selected_units = selected;
                let at = |units: usize| super::utf16::byte_offset(&text, units);
                let selected = (
                    at(selected.location),
                    at(selected.location.saturating_add(selected.length)),
                );
                let clauses = marked_clauses(string, &text, selected_units);
                let mut ime = ivars.ime.borrow_mut();
                let over = self.range_bytes(&ime, replacement);
                let event = ime.set_marked(ivars.window, &text, selected, clauses, over);
                drop(ime);
                if let Some(event) = event {
                    self.push(event);
                }
            }));
        }

        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                // The composition is accepted as it stands.
                let ivars = self.ivars();
                let event = ivars.ime.borrow_mut().unmark(ivars.window);
                if let Some(event) = event {
                    self.push(event);
                }
            }));
        }

        #[unsafe(method(hasMarkedText))]
        fn has_marked_text(&self) -> bool {
            self.ivars().ime.borrow().is_marked()
        }

        #[unsafe(method(markedRange))]
        fn marked_range(&self) -> NSRange {
            let ime = self.ivars().ime.borrow();
            match ime.marked() {
                Some((a, b)) => units_range(&ime, a, b),
                None => NSRange::new(NOT_FOUND, 0),
            }
        }

        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> NSRange {
            let ime = self.ivars().ime.borrow();
            let (a, b) = ime.selection();
            units_range(&ime, a, b)
        }

        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        unsafe fn attributed_substring(
            &self,
            range: NSRange,
            actual: NSRangePointer,
        ) -> Option<Retained<NSAttributedString>> {
            catch_unwind(AssertUnwindSafe(|| {
                let ime = self.ivars().ime.borrow();
                let (a, b) = self.range_bytes(&ime, range)?;
                if a == b && ime.text().is_empty() {
                    return None;
                }
                if !actual.is_null() {
                    // SAFETY: AppKit passes either null (checked above) or a
                    // pointer to an `NSRange` it owns for this call.
                    unsafe { *actual = units_range(&ime, a, b) };
                }
                Some(NSAttributedString::from_nsstring(&NSString::from_str(
                    &ime.text()[a..b],
                )))
            }))
            .ok()
            .flatten()
        }

        // The input method marks its clauses, and underlines the one it
        // converts thick.
        #[unsafe(method_id(validAttributesForMarkedText))]
        fn valid_attributes(&self) -> Retained<NSArray<NSAttributedStringKey>> {
            NSArray::from_retained_slice(&[
                NSString::from_str(CLAUSE_SEGMENT),
                NSString::from_str(UNDERLINE_STYLE),
            ])
        }

        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        unsafe fn first_rect(&self, _range: NSRange, _actual: NSRangePointer) -> NSRect {
            // The candidate window parks under this rect: the focused field's
            // caret, or the view's origin until the app has reported one.
            let k = self.ivars().logical.get();
            let local = match self.ivars().ime_caret.get() {
                Some(r) => NSRect::new(
                    NSPoint::new(r.x / k, r.y / k),
                    NSSize::new(r.width / k, r.height / k),
                ),
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
            NOT_FOUND
        }

        #[unsafe(method(doCommandBySelector:))]
        unsafe fn do_command(&self, _selector: Sel) {}
    }

    unsafe impl NSDraggingDestination for VisoContentView {
        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            catch_unwind(AssertUnwindSafe(|| self.drag_over(sender, true)))
                .unwrap_or(NSDragOperation::None)
        }

        #[unsafe(method(draggingUpdated:))]
        fn dragging_updated(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            catch_unwind(AssertUnwindSafe(|| self.drag_over(sender, false)))
                .unwrap_or(NSDragOperation::None)
        }

        #[unsafe(method(draggingExited:))]
        fn dragging_exited(&self, _sender: Option<&ProtocolObject<dyn NSDraggingInfo>>) {
            let _ = catch_unwind(AssertUnwindSafe(|| self.drag_leave()));
        }

        // Ask for the state of the app as it answers, even with the drag
        // held still.
        #[unsafe(method(wantsPeriodicDraggingUpdates))]
        fn wants_periodic_dragging_updates(&self) -> bool {
            true
        }

        #[unsafe(method(prepareForDragOperation:))]
        fn prepare_for_drag_operation(&self, _sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            true
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag_operation(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            catch_unwind(AssertUnwindSafe(|| self.drag_drop(sender))).unwrap_or(false)
        }
    }

    unsafe impl NSDraggingSource for VisoContentView {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn dragging_source_mask(
            &self,
            _session: &NSDraggingSession,
            _context: NSDraggingContext,
        ) -> NSDragOperation {
            self.ivars().drag_out.get().unwrap_or(NSDragOperation::None)
        }

        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn dragging_source_ended(
            &self,
            _session: &NSDraggingSession,
            _at: NSPoint,
            operation: NSDragOperation,
        ) {
            let _ = catch_unwind(AssertUnwindSafe(|| self.drag_out_ended(operation)));
        }
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
            ime: RefCell::new(ImeText::default()),
            draggable_regions: RefCell::new(Vec::new()),
            cursor: Cell::new(CursorIcon::Default),
            cursor_hidden: Cell::new(false),
            lock_pin: Cell::new(None),
            warp_debt: Cell::new((0.0, 0.0)),
            ime_enabled: Cell::new(true),
            ime_caret: Cell::new(None),
            secure_field: Cell::new(false),
            number_field: Cell::new(false),
            roman: Cell::new(false),
            key: Cell::new(false),
            secure_input: SecureInput(Cell::new(false)),
            logical: Cell::new(1.0),
            pixels: RefCell::new(None),
            context_menu: Cell::new(None),
            gestures: Cell::new(0),
            arrival: RefCell::new(None),
            last_mouse: RefCell::new(None),
            drag_out: Cell::new(None),
        });
        // SAFETY: `this` is a freshly allocated view with its ivars set; `NSView`'s
        // designated initializer takes the frame.
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        // SAFETY: the pasteboard type names are immutable AppKit constants.
        let types = unsafe {
            [
                NSPasteboardTypeFileURL,
                NSPasteboardTypeURL,
                NSPasteboardTypeString,
            ]
        };
        this.registerForDraggedTypes(&NSArray::from_slice(&types));
        this
    }

    /// A drag from outside came over the view, or moved over it: the app
    /// hears where, and the OS the app's answer to the step before.
    fn drag_over(
        &self,
        sender: &ProtocolObject<dyn NSDraggingInfo>,
        entered: bool,
    ) -> NSDragOperation {
        let ivars = self.ivars();
        let (offer, reply) = {
            let mut arrival = ivars.arrival.borrow_mut();
            if entered || arrival.is_none() {
                *arrival = Some((
                    pasteboard_offer(&sender.draggingPasteboard()),
                    DropReply::new(),
                ));
            }
            arrival.clone().unwrap_or_default()
        };
        let (x, y) = self.drag_location(sender);
        self.push(RawEvent::Drop(RawDrop {
            window: ivars.window,
            phase: DropPhase::Over,
            x,
            y,
            offer,
            reply: reply.clone(),
        }));
        self.deliver_now();
        drag_operation(drag_actions(sender.draggingSourceOperationMask()).settle(reply.get()))
    }

    /// The drag left the view, or was given up over it.
    fn drag_leave(&self) {
        let ivars = self.ivars();
        let Some((offer, reply)) = ivars.arrival.borrow_mut().take() else {
            return;
        };
        self.push(RawEvent::Drop(RawDrop {
            window: ivars.window,
            phase: DropPhase::Leave,
            x: 0.0,
            y: 0.0,
            offer,
            reply,
        }));
        self.deliver_now();
    }

    /// The drag was let go over the view: the app takes what it carries,
    /// read afresh. Returns whether the app said it would.
    fn drag_drop(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
        let ivars = self.ivars();
        let reply = ivars
            .arrival
            .borrow_mut()
            .take()
            .map(|(_, reply)| reply)
            .unwrap_or_default();
        let (x, y) = self.drag_location(sender);
        self.push(RawEvent::Drop(RawDrop {
            window: ivars.window,
            phase: DropPhase::Drop,
            x,
            y,
            offer: pasteboard_offer(&sender.draggingPasteboard()),
            reply: reply.clone(),
        }));
        self.deliver_now();
        reply.get().is_some()
    }

    /// Hands the queued events to the app at once, inside the AppKit turn
    /// this callback runs in, so the OS hears the app's answer to this step.
    fn deliver_now(&self) {
        let shared = self.ivars().shared.clone();
        let drive = shared.borrow().drive;
        if let Some(drive) = drive {
            // SAFETY: `drive` is what `MacApp::run` installed for the AppKit
            // turn this callback runs inside, on the main thread, while the
            // pump holds no borrow of the app or handler. See
            // `drain_and_drive`.
            unsafe { drain_and_drive(drive, &shared) };
        }
    }

    /// Where a drag is, in this flipped view's logical points.
    fn drag_location(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> (f64, f64) {
        let p = self.convertPoint_fromView(sender.draggingLocation(), None);
        let k = self.ivars().logical.get();
        (p.x * k, p.y * k)
    }

    /// The event location in this flipped view's coordinates (logical points,
    /// origin at the top-left).
    fn location(&self, event: &NSEvent) -> (f64, f64) {
        let window_point = event.locationInWindow();
        let p = self.convertPoint_fromView(window_point, None);
        let k = self.ivars().logical.get();
        (p.x * k, p.y * k)
    }

    fn note_mouse(&self, event: &NSEvent) {
        *self.ivars().last_mouse.borrow_mut() = Some(objc2::Message::retain(event));
    }

    /// Carries `drag` on from the last mouse press or drag as a drag AppKit
    /// runs. Returns whether it started.
    fn start_drag(&self, drag: &crate::DragOut) -> bool {
        let ivars = self.ivars();
        let Some(event) = ivars.last_mouse.borrow().clone() else {
            return false;
        };
        if ivars.buttons.get() == 0 || ivars.drag_out.get().is_some() {
            return false;
        }
        let workspace = NSWorkspace::sharedWorkspace();
        let at = self.convertPoint_fromView(event.locationInWindow(), None);
        let item = |writer: &ProtocolObject<dyn objc2_app_kit::NSPasteboardWriting>,
                    icon: Retained<objc2_app_kit::NSImage>,
                    i: usize| {
            let item = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), writer);
            let shift = i as f64 * 4.0;
            let frame = NSRect::new(
                NSPoint::new(at.x - 16.0 + shift, at.y - 16.0 + shift),
                NSSize::new(32.0, 32.0),
            );
            // SAFETY: the contents are an image, which a dragging item draws.
            unsafe { item.setDraggingFrame_contents(frame, Some(&icon)) };
            item
        };
        let mut items = Vec::new();
        for (i, file) in drag.files.iter().enumerate() {
            let path = NSString::from_str(&file.to_string_lossy());
            let url = NSURL::fileURLWithPath(&path);
            items.push(item(
                ProtocolObject::from_ref(&*url),
                workspace.iconForFile(&path),
                i,
            ));
        }
        if items.is_empty() {
            let board = NSPasteboardItem::new();
            // SAFETY: the pasteboard type names are immutable AppKit constants.
            let (url_type, string_type) = unsafe { (NSPasteboardTypeURL, NSPasteboardTypeString) };
            if let Some(uri) = &drag.uri {
                board.setString_forType(&NSString::from_str(uri), url_type);
            }
            let text = drag.plain_text();
            if text.is_empty() {
                return false;
            }
            board.setString_forType(&NSString::from_str(&text), string_type);
            let kind = if drag.uri.is_some() {
                "public.url"
            } else {
                "public.plain-text"
            };
            #[allow(deprecated)]
            let icon = workspace.iconForFileType(&NSString::from_str(kind));
            items.push(item(ProtocolObject::from_ref(&*board), icon, 0));
        }
        let mut mask = NSDragOperation::None;
        for (allowed, op) in [
            (drag.actions.copies, NSDragOperation::Copy),
            (drag.actions.moves, NSDragOperation::Move),
            (drag.actions.links, NSDragOperation::Link),
        ] {
            if allowed {
                mask |= op;
            }
        }
        ivars.drag_out.set(Some(mask));
        let session = self.beginDraggingSessionWithItems_event_source(
            &NSArray::from_retained_slice(&items),
            &event,
            ProtocolObject::from_ref(self),
        );
        session.setAnimatesToStartingPositionsOnCancelOrFail(true);
        true
    }

    /// The drag the app carried out ended, doing `operation`: AppKit kept the
    /// button's release, so none is held now.
    fn drag_out_ended(&self, operation: NSDragOperation) {
        let ivars = self.ivars();
        ivars.drag_out.set(None);
        ivars.buttons.set(0);
        let action = if operation.contains(NSDragOperation::Move) {
            Some(DropAction::Move)
        } else if operation.contains(NSDragOperation::Copy) {
            Some(DropAction::Copy)
        } else if operation.contains(NSDragOperation::Link) {
            Some(DropAction::Link)
        } else {
            None
        };
        self.push(RawEvent::DragEnded {
            window: ivars.window,
            action,
        });
        self.deliver_now();
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

    /// A mouse's back button (button 3) is the system back action, not a
    /// pointer button: enqueue it as [`KeyCode::Back`]. Whether `event` was it.
    fn back_button(&self, event: &NSEvent, pressed: bool) -> bool {
        if event.buttonNumber() != 3 {
            return false;
        }
        self.push(RawEvent::Key(RawKey {
            window: self.ivars().window,
            code: KeyCode::Back,
            pressed,
            repeat: false,
            modifiers: modifiers_of(event),
        }));
        true
    }

    /// Enqueue a pointer sample, folding the button transition into the tracked
    /// mask so every sample reports the full chord currently held.
    /// A mouse move: a sample of where the pointer went, or while the
    /// pointer is locked how far the mouse moved.
    fn moved(&self, event: &NSEvent) {
        if self.ivars().lock_pin.get().is_none() {
            self.pointer(event, PointerPhase::Moved, PointerButtons::NONE, false);
            return;
        }
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let ivars = self.ivars();
            let k = ivars.logical.get();
            let (wx, wy) = ivars.warp_debt.take();
            let (dx, dy) = ((event.deltaX() - wx) * k, (event.deltaY() - wy) * k);
            if dx != 0.0 || dy != 0.0 {
                let window = ivars.window;
                crate::event::queue_motion(&mut ivars.shared.borrow_mut().events, window, dx, dy);
            }
            self.hold_pin();
        }));
    }

    /// Locks the mouse's pointer to this view, or lets it go, reporting the
    /// change. A lock is refused while the app is not active or the window
    /// is not key: the pointer belongs to whatever has the focus.
    fn set_pointer_lock(&self, lock: bool) {
        let ivars = self.ivars();
        if lock == ivars.lock_pin.get().is_some() {
            return;
        }
        let window = ivars.window;
        if !lock {
            self.release_pointer_lock();
        } else {
            let mtm = self.mtm();
            let focused = NSApplication::sharedApplication(mtm).isActive()
                && self.window().is_some_and(|w| w.isKeyWindow());
            let Some(pin) = focused.then(|| self.lock_point()).flatten() else {
                self.push(RawEvent::PointerLockChanged {
                    window,
                    locked: false,
                });
                return;
            };
            ivars.lock_pin.set(Some(pin));
            NSCursor::hide();
            // SAFETY: plain CoreGraphics calls taking values. A warp holds
            // back the hardware's own mouse events for a quarter second by
            // default, which would swallow the first motion and clicks.
            unsafe {
                CGSetLocalEventsSuppressionInterval(0.0);
            }
            self.warp_to_pin(pin);
            // SAFETY: as above; freezing the cursor where it is pinned keeps
            // the mouse's motion coming as deltas.
            unsafe {
                CGAssociateMouseAndMouseCursorPosition(0);
            }
        }
        self.push(RawEvent::PointerLockChanged {
            window,
            locked: lock,
        });
    }

    /// Lets the locked pointer go where it was pinned. Returns whether it
    /// was locked.
    fn release_pointer_lock(&self) -> bool {
        if self.ivars().lock_pin.take().is_none() {
            return false;
        }
        self.ivars().warp_debt.set((0.0, 0.0));
        // SAFETY: a plain CoreGraphics call taking a value.
        unsafe {
            CGAssociateMouseAndMouseCursorPosition(1);
        }
        NSCursor::unhide();
        true
    }

    /// Where to pin a locked pointer: the middle of the part of this view
    /// on its screen's visible area, so neither a screen edge nor the Dock
    /// takes its clicks.
    fn lock_point(&self) -> Option<NSPoint> {
        let window = self.window()?;
        let bounds = self.bounds();
        let in_window = self.convertRect_toView(bounds, None);
        let rect = window.convertRectToScreen(in_window);
        let visible = window.screen().map_or(rect, |screen| screen.visibleFrame());
        let lo_x = rect.origin.x.max(visible.origin.x);
        let hi_x = (rect.origin.x + rect.size.width).min(visible.origin.x + visible.size.width);
        let lo_y = rect.origin.y.max(visible.origin.y);
        let hi_y = (rect.origin.y + rect.size.height).min(visible.origin.y + visible.size.height);
        let mid = |lo: f64, hi: f64, at: f64, len: f64| {
            if lo < hi {
                (lo + hi) * 0.5
            } else {
                at + len * 0.5
            }
        };
        Some(NSPoint::new(
            mid(lo_x, hi_x, rect.origin.x, rect.size.width),
            mid(lo_y, hi_y, rect.origin.y, rect.size.height),
        ))
    }

    /// Puts a locked pointer back on its pin when it drifted, as it does
    /// when the system brings the cursor's association back on its own.
    fn hold_pin(&self) {
        let Some(pin) = self.ivars().lock_pin.get() else {
            return;
        };
        let at = NSEvent::mouseLocation();
        if (at.x - pin.x).abs() + (at.y - pin.y).abs() > 2.0 {
            self.warp_to_pin(pin);
            // SAFETY: a plain CoreGraphics call taking a value.
            unsafe {
                CGAssociateMouseAndMouseCursorPosition(0);
            }
        }
    }

    /// Moves the cursor onto the lock's pin, owing the move back to the
    /// next delta. Cocoa's y grows up, a delta's down.
    fn warp_to_pin(&self, pin: NSPoint) {
        let at = NSEvent::mouseLocation();
        let (wx, wy) = self.ivars().warp_debt.get();
        self.ivars()
            .warp_debt
            .set((wx + pin.x - at.x, wy + at.y - pin.y));
        warp_to(self.mtm(), pin);
    }

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

    /// Pop the context menu asked for up from the run loop, once the frame
    /// that asked has finished.
    fn queue_context_menu(&self, menu: EditMenu, labels: EditLabels) {
        self.ivars().context_menu.set(Some((menu, labels)));
        // SAFETY: `performSelector:withObject:afterDelay:` is NSObject's,
        // `visoPopContextMenu` is this class's method taking no argument,
        // and the run loop retains the view until it runs.
        unsafe {
            let _: () = msg_send![
                self,
                performSelector: sel!(visoPopContextMenu),
                withObject: Option::<&AnyObject>::None,
                afterDelay: 0.0f64
            ];
        }
    }

    /// Track the context menu at its place until the user picks or leaves
    /// it, then release the buttons its tracking swallowed and report it
    /// closed.
    fn pop_menu(&self) {
        let ivars = self.ivars();
        let Some((menu, labels)) = ivars.context_menu.take() else {
            return;
        };
        let mtm = self.mtm();
        let ns = NSMenu::new(mtm);
        ns.setAutoenablesItems(false);
        let items = [
            (labels.cut, sel!(cut:), menu.cut),
            (labels.copy, sel!(copy:), menu.copy),
            (labels.paste, sel!(paste:), menu.paste),
            (labels.select_all, sel!(selectAll:), menu.select_all),
        ];
        for (at, (title, action, enabled)) in items.into_iter().enumerate() {
            if at == 3 {
                ns.addItem(&NSMenuItem::separatorItem(mtm));
            }
            // SAFETY: standard AppKit menu-item construction on the main
            // thread; the menu holds the item and the view outlives the
            // tracking below.
            let item = unsafe {
                let item = NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(title),
                    Some(action),
                    &NSString::from_str(""),
                );
                item.setTarget(Some(self));
                item
            };
            item.setEnabled(enabled);
            ns.addItem(&item);
        }
        let k = ivars.logical.get();
        let at = NSPoint::new(menu.at.x / k, menu.at.y / k);
        ns.popUpMenuPositioningItem_atLocation_inView(None, at, Some(self));
        let held = ivars.buttons.replace(0);
        if held != 0
            && let Some(window) = self.window()
        {
            let p = self.convertPoint_fromView(window.mouseLocationOutsideOfEventStream(), None);
            self.push(RawEvent::Pointer(RawPointer::mouse(
                ivars.window,
                p.x * k,
                p.y * k,
                PointerButtons(0),
                Modifiers::default(),
                PointerPhase::Up,
            )));
        }
        self.push(RawEvent::EditMenu {
            window: ivars.window,
            action: EditMenuAction::Closed,
        });
    }

    /// Answer a clipboard gesture: copy and cut ask the app for its selection
    /// (written back by `deliver`), paste asks it what it takes (read by
    /// `deliver`).
    fn clipboard(&self, shortcut: ClipboardShortcut) {
        let window = self.ivars().window;
        let event = match shortcut {
            ClipboardShortcut::Copy | ClipboardShortcut::Cut => RawEvent::CopyRequested {
                window,
                cut: shortcut == ClipboardShortcut::Cut,
                reply: ClipboardReply::new(),
            },
            ClipboardShortcut::Paste => RawEvent::PasteRequested {
                window,
                reply: PasteReply::new(),
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

    fn set_text_entry(&self, entry: Option<TextEntry>) {
        let ivars = self.ivars();
        let caret = entry.map(|e| e.caret);
        ivars.ime_enabled.set(caret.is_some());
        ivars.ime_caret.set(caret);
        ivars.secure_field.set(entry.is_some_and(|e| e.secure));
        ivars
            .number_field
            .set(entry.is_some_and(|e| e.kind.numeric()));
        self.sync_secure_input();
        let Some(ctx) = self.inputContext() else {
            return;
        };
        if caret.is_some() {
            ctx.invalidateCharacterCoordinates();
        } else {
            // Focus left the field: a composition under way ends on both
            // sides, and the input method sees no text.
            let mut ime = ivars.ime.borrow_mut();
            let dropped = ime.cancel(ivars.window);
            ime.show(None);
            drop(ime);
            if let Some(event) = dropped {
                ctx.discardMarkedText();
                self.push(event);
            }
        }
    }

    /// Show the input method the text around the focused field's caret.
    fn set_surrounding_text(&self, text: Option<&SurroundingText>) {
        self.ivars().ime.borrow_mut().show(text);
    }

    /// The bytes of the input method's text that UTF-16 range `range`
    /// names, `None` for `NSNotFound`.
    fn range_bytes(&self, ime: &ImeText, range: NSRange) -> Option<(usize, usize)> {
        (range.location != NOT_FOUND).then(|| {
            ime.span(
                ime.byte(range.location),
                ime.byte(range.location.saturating_add(range.length)),
            )
        })
    }

    /// Records whether the window is key, holding secure input only while
    /// it is.
    fn set_key(&self, key: bool) {
        self.ivars().key.set(key);
        self.sync_secure_input();
    }

    /// Holds secure event input while a secure field of the key window has
    /// focus, and keeps the input method to Roman keyboards (no composition,
    /// no candidates) as `NSSecureTextField` does while it does or a number
    /// field has focus.
    fn sync_secure_input(&self) {
        let ivars = self.ivars();
        let want = ivars.secure_field.get() && ivars.key.get();
        ivars.secure_input.set(want);
        let roman = want || ivars.number_field.get();
        if ivars.roman.replace(roman) == roman {
            return;
        }
        if let Some(ctx) = self.inputContext() {
            let roman = roman.then(|| {
                // SAFETY: an AppKit constant string, initialized before any
                // code of the app runs and never mutated.
                NSArray::from_slice(&[unsafe { NSAllRomanInputSourcesLocaleIdentifier }])
            });
            ctx.setAllowedInputSourceLocales(roman.as_deref());
        }
    }

    /// Push one raw event onto the shared queue.
    fn push(&self, event: RawEvent) {
        self.ivars().shared.borrow_mut().events.push_back(event);
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGWarpMouseCursorPosition(point: NSPoint) -> i32;
    fn CGAssociateMouseAndMouseCursorPosition(connected: u32) -> i32;
    fn CGSetLocalEventsSuppressionInterval(seconds: f64) -> i32;
}

/// Moves the cursor to Cocoa desktop point `point`.
fn warp_to(mtm: MainThreadMarker, point: NSPoint) {
    let top = primary_screen_height(mtm).unwrap_or(0.0);
    // SAFETY: a plain CoreGraphics call taking a value; it measures from the
    // primary screen's top left, Cocoa from its bottom left.
    unsafe {
        CGWarpMouseCursorPosition(NSPoint::new(point.x, top - point.y));
    }
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn EnableSecureEventInput() -> i32;
    fn DisableSecureEventInput() -> i32;
}

/// Whether a view holds the process's secure event input, which keeps other
/// processes from reading the keyboard; it lets go when the view goes.
struct SecureInput(Cell<bool>);

impl SecureInput {
    /// Holds or lets go of secure input, reporting whether that changed.
    /// The system counts holds per process, so each view balances its own.
    fn set(&self, hold: bool) -> bool {
        if self.0.get() == hold {
            return false;
        }
        self.0.set(hold);
        // SAFETY: Carbon calls taking nothing, on the main thread the view
        // lives on; every enable is matched by one disable (here or on drop).
        unsafe {
            if hold {
                EnableSecureEventInput();
            } else {
                DisableSecureEventInput();
            }
        }
        true
    }
}

impl Drop for SecureInput {
    fn drop(&mut self) {
        self.set(false);
    }
}

impl VisoContentView {
    /// Reports trackpad gesture `which` of `event` scaling by `scale` and
    /// turning by `rotation`, as one gesture with the other while either runs.
    fn gesture(&self, event: &NSEvent, which: u8, scale: f64, rotation: f64) {
        use crate::GesturePhase;
        let ivars = self.ivars();
        let has = |flag: NSEventPhase| event.phase().0 & flag.0 != 0;
        let before = ivars.gestures.get();
        let now = if has(NSEventPhase::Began) {
            before | which
        } else if has(NSEventPhase::Ended) || has(NSEventPhase::Cancelled) {
            before & !which
        } else {
            before
        };
        ivars.gestures.set(now);
        let phase = match (before, now) {
            (0, 0) => return,
            (0, _) => GesturePhase::Began,
            (_, 0) => GesturePhase::Ended,
            _ => GesturePhase::Changed,
        };
        let (x, y) = self.location(event);
        self.push(RawEvent::Gesture(crate::RawGesture {
            window: ivars.window,
            x,
            y,
            scale,
            rotation,
            modifiers: modifiers_of(event),
            phase,
        }));
    }
}

/// Where a scroll event stands in its trackpad gesture: the fingers' phase,
/// else the coast's, else none (a wheel).
fn scroll_phase(event: &NSEvent) -> crate::ScrollPhase {
    use crate::ScrollPhase;
    let has = |phase: NSEventPhase, flag: NSEventPhase| phase.0 & flag.0 != 0;
    let fingers = event.phase();
    let coast = event.momentumPhase();
    if has(fingers, NSEventPhase::Began) || has(fingers, NSEventPhase::MayBegin) {
        ScrollPhase::Began
    } else if has(fingers, NSEventPhase::Changed) || has(fingers, NSEventPhase::Stationary) {
        ScrollPhase::Changed
    } else if has(fingers, NSEventPhase::Ended) || has(fingers, NSEventPhase::Cancelled) {
        ScrollPhase::Ended
    } else if has(coast, NSEventPhase::Ended) || has(coast, NSEventPhase::Cancelled) {
        ScrollPhase::MomentumEnded
    } else if coast.0 != 0 {
        ScrollPhase::Momentum
    } else {
        ScrollPhase::None
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
        C::Custom(cursor) => {
            return Some(
                CUSTOM_CURSORS
                    .with_borrow_mut(|cache| cache.get(cursor, image_cursor).cloned())
                    .unwrap_or_else(NSCursor::arrowCursor),
            );
        }
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

thread_local! {
    /// The image cursors built so far, on the main thread that shows them.
    static CUSTOM_CURSORS: RefCell<CursorCache<Retained<NSCursor>>> =
        const { RefCell::new(CursorCache::new()) };
}

/// An `NSCursor` showing `image`: one sRGB bitmap of its pixels, sized in
/// points by its scale so AppKit draws it sharp at that density.
fn image_cursor(image: &CursorImage) -> Option<Retained<NSCursor>> {
    let (w, h) = (image.width() as isize, image.height() as isize);
    // SAFETY: a null planes pointer asks the rep to allocate its own
    // buffer of `h` rows of `w * 4` bytes, which the arguments describe:
    // 8-bit RGBA, interleaved, unpremultiplied.
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            w,
            h,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            NSBitmapFormat::AlphaNonpremultiplied,
            w * 4,
            32,
        )
    }?;
    let data = rep.bitmapData();
    if data.is_null() || rep.bytesPerRow() != w * 4 {
        return None;
    }
    // SAFETY: the rep owns `bytesPerRow * h` bytes at `data`, checked to be
    // exactly the image's length, and nothing else holds them yet.
    unsafe {
        std::ptr::copy_nonoverlapping(image.rgba().as_ptr(), data, image.rgba().len());
    }
    let rep = rep
        .bitmapImageRepByRetaggingWithColorSpace(&NSColorSpace::sRGBColorSpace())
        .unwrap_or(rep);
    let k = f64::from(image.scale());
    let size = NSSize::new(w as f64 / k, h as f64 / k);
    rep.setSize(size);
    let ns_image = NSImage::initWithSize(NSImage::alloc(), size);
    ns_image.addRepresentation(&rep);
    let [hx, hy] = image.hotspot();
    let hot = NSPoint::new(f64::from(hx) / k, f64::from(hy) / k);
    Some(NSCursor::initWithImage_hotSpot(
        NSCursor::alloc(),
        &ns_image,
        hot,
    ))
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
/// The marked-text attribute numbering an input method's clauses.
const CLAUSE_SEGMENT: &str = "NSMarkedClauseSegment";

/// The underline-style attribute; the converting clause's is thick.
const UNDERLINE_STYLE: &str = "NSUnderline";

/// `NSUnderlineStyleThick`.
const UNDERLINE_THICK: isize = 2;

/// The clauses of marked text `string` (holding `text`): its clause
/// segments, and the converting one, the stretch underlined thick, else
/// `selected` (UTF-16) when it spans any text.
fn marked_clauses(string: &AnyObject, text: &str, selected: NSRange) -> crate::PreeditClauses {
    let selected = (selected.length > 0 && selected.location != NOT_FOUND)
        .then(|| (selected.location, selected.location + selected.length));
    let Some(marked) = string.downcast_ref::<NSAttributedString>() else {
        return crate::PreeditClauses::from_utf16(text, &[], selected);
    };
    let runs = |name: &str| {
        let key = NSString::from_str(name);
        let len = marked.length();
        let mut runs = Vec::new();
        let mut at = 0;
        while at < len {
            let mut range = NSRange::new(at, 0);
            // SAFETY: `range` is a live `NSRange` for AppKit to write the
            // run's extent into; `at` is within the string.
            let value = unsafe { marked.attribute_atIndex_effectiveRange(&key, at, &mut range) };
            if range.length == 0 || range.location + range.length <= at {
                break;
            }
            let style = value
                .and_then(|v| v.downcast::<objc2_foundation::NSNumber>().ok())
                .map(|n| n.integerValue());
            runs.push(((range.location, range.location + range.length), style));
            at = range.location + range.length;
        }
        runs
    };
    let clauses: Vec<_> = runs(CLAUSE_SEGMENT)
        .into_iter()
        .filter(|(_, n)| n.is_some())
        .map(|(r, _)| r)
        .collect();
    let thick = runs(UNDERLINE_STYLE)
        .into_iter()
        .find(|(_, style)| style.is_some_and(|s| s & 0xff >= UNDERLINE_THICK))
        .map(|(r, _)| r);
    crate::PreeditClauses::from_utf16(text, &clauses, thick.or(selected))
}

/// `NSNotFound`: no such range or index.
const NOT_FOUND: usize = isize::MAX as usize;

/// Bytes `a..b` of the input method's text as a UTF-16 range.
fn units_range(ime: &ImeText, a: usize, b: usize) -> NSRange {
    let start = ime.units(a);
    NSRange::new(start, ime.units(b) - start)
}

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
    use crate::backend::headless::HeadlessApp;

    #[test]
    fn a_drag_s_pasteboard_offers_its_files_and_its_text() {
        use objc2_app_kit::{NSPasteboardItem, NSPasteboardWriting};
        let pasteboard = NSPasteboard::pasteboardWithUniqueName();
        // SAFETY: the pasteboard type names are immutable AppKit constants.
        let (file, string) = unsafe { (NSPasteboardTypeFileURL, NSPasteboardTypeString) };
        let item = NSPasteboardItem::new();
        item.setString_forType(&NSString::from_str("file:///tmp/a%20b.txt"), file);
        item.setString_forType(&NSString::from_str("a b.txt"), string);
        let items = [ProtocolObject::<dyn NSPasteboardWriting>::from_retained(
            item,
        )];
        pasteboard.clearContents();
        assert!(pasteboard.writeObjects(&NSArray::from_retained_slice(&items)));
        let offer = pasteboard_offer(&pasteboard);
        assert_eq!(offer.files, [std::path::PathBuf::from("/tmp/a b.txt")]);
        assert_eq!(offer.text.as_deref(), Some("a b.txt"));
        assert!(offer.kinds.files && offer.kinds.text && !offer.kinds.uri);
        assert_eq!(
            drag_operation(drag_actions(NSDragOperation::Copy).settle(Some(DropAction::Move))),
            NSDragOperation::Copy
        );
    }

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
        fn handle(&mut self, _app: &mut dyn PlatformApp, event: RawEvent) -> ControlFlow {
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
        let mut app = HeadlessApp::new();
        let drive = Drive::new(&mut app, &mut handler);
        // SAFETY: `app` and `handler` outlive the call and are not otherwise
        // borrowed during it.
        unsafe { drain_and_drive(drive, &shared) };

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
        let mut app = HeadlessApp::new();
        let drive = Drive::new(&mut app, &mut handler);
        // SAFETY: as above — both outlive the call, with no other borrow.
        unsafe { drain_and_drive(drive, &shared) };

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
            fn handle(&mut self, _app: &mut dyn PlatformApp, event: RawEvent) -> ControlFlow {
                if let RawEvent::CopyRequested { reply, .. } = &event {
                    // Answer then take it back, so the test leaves the real
                    // pasteboard untouched while exercising the reply slot.
                    reply.set(ClipboardData::text("x"));
                    assert_eq!(reply.take(), Some(ClipboardData::text("x")));
                }
                ControlFlow::Wait
            }
        }
        let reply = ClipboardReply::new();
        let flow = deliver(
            &mut HeadlessApp::new(),
            &mut Answer,
            &Shared::default(),
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
