//! Native Android backend (JNI + NDK).
//!
//! The process is started by `dev.viso.VisoActivity` (`crates/platform/
//! android/java`), which loads the app's shared library. The activity's UI
//! thread only forwards: every callback becomes a [`Msg`] in a process-wide
//! inbox and wakes the loop thread's `ALooper`. The loop thread is spawned on
//! the first `nativeStart`, attaches to the VM and calls the app's `main`, so
//! [`PlatformApp::run`] owns its loop like on the desktop:
//! - frames come from `AChoreographer`, posted only while a redraw is pending
//!   and a surface exists;
//! - [`ControlFlow::WaitUntil`] and [`ControlFlow::Wait`] sleep in
//!   `ALooper_pollOnce`, which the inbox and the choreographer both wake;
//! - the surface is lent to the app between `surfaceChanged` and
//!   `surfaceDestroyed`. The activity waits in `surfaceDestroyed` until the
//!   loop has delivered [`RawEvent::SurfaceDestroyed`], so the GPU swapchain
//!   is gone before the system reclaims the window.
//!
//! The app has a single full-screen window, opened once the first surface
//! exists (that is when [`RawEvent::AppLaunched`] is delivered). When the
//! activity finishes the loop delivers `CloseRequested` and `WindowClosed`
//! and returns from `run`; when the app's `main` returns first, the activity
//! is finished for it.

mod jni;
mod ndk;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::{c_char, c_int, c_long, c_void};
use std::ptr;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use super::android_translate as translate;
use super::utf16::byte_offset;
use crate::control::{ControlFlow, LogicalRect, PlatformError, WindowConfig, WindowId};
use crate::event::{
    AcceptCell, Appearance, ClipboardReply, ClipboardShortcut, ColorScheme, CursorIcon, Insets,
    PointerButtons, PointerKind, RawEvent, RawImePreedit, RawKey, RawPointer, RawScroll, RawText,
    clipboard_shortcut,
};
use crate::handler::AppHandler;
use crate::menu::Menu;
use crate::{Instant, PlatformApp, RawWindowHandle, Window};

/// The only window's id.
const WINDOW: WindowId = WindowId(1);

// Cross-thread inbox.

/// A surface the activity lends the loop: an `ANativeWindow` reference the
/// value owns and releases on drop.
pub(super) struct Surface {
    window: *mut ndk::ANativeWindow,
    size: (u32, u32),
}

// SAFETY: `ANativeWindow` references are thread-safe reference counts; the
// value is only ever used by one thread at a time.
unsafe impl Send for Surface {}

impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: `window` carries the reference `ANativeWindow_fromSurface`
        // took for this value, released exactly once here.
        unsafe { ndk::ANativeWindow_release(self.window) };
    }
}

/// One `MotionEvent` sample batch: per pointer its id, `[x, y, pressure]`
/// in pixels, and tool type.
pub(super) struct Touch {
    pub action: i32,
    pub action_index: usize,
    pub ids: Vec<i32>,
    pub samples: Vec<f32>,
    pub tools: Vec<i32>,
    pub buttons: i32,
    pub meta: i32,
}

/// What the activity tells the loop.
pub(super) enum Msg {
    Surface(Surface),
    SurfaceDestroyed,
    Redraw,
    Visible(bool),
    Focus(bool),
    /// System bars + cutout, and the keyboard's height, in pixels.
    Insets {
        bars: [i32; 4],
        ime: i32,
    },
    Config {
        density: f64,
        appearance: i32,
    },
    LowMemory,
    Touch(Touch),
    /// Mouse wheel / touchpad scroll at `at`, in pixels.
    Scroll {
        at: [f32; 2],
        delta: [f32; 2],
        meta: i32,
    },
    Key {
        code: i32,
        pressed: bool,
        repeat: bool,
        meta: i32,
        text: Option<String>,
    },
    Preedit {
        text: String,
        caret_utf16: usize,
    },
    Commit(String),
    /// A context-menu edit: copy 0, cut 1, paste 2.
    Edit(i32),
    Destroy,
}

/// The loop thread's looper, set once it has one.
#[derive(Clone, Copy)]
struct Looper(*mut ndk::ALooper);

// SAFETY: `ALooper_wake` is documented as callable from any thread; that is
// the only use other threads make of the pointer.
unsafe impl Send for Looper {}

struct Inbox {
    msgs: VecDeque<(Msg, Option<u64>)>,
    looper: Option<Looper>,
    /// The last ticket handed to a waiting sender, and the last the loop
    /// finished.
    issued: u64,
    handled: u64,
    /// The loop thread has ended; nothing will read the inbox again.
    done: bool,
}

static INBOX: Mutex<Inbox> = Mutex::new(Inbox {
    msgs: VecDeque::new(),
    looper: None,
    issued: 0,
    handled: 0,
    done: false,
});
static HANDLED: Condvar = Condvar::new();

fn inbox() -> MutexGuard<'static, Inbox> {
    INBOX.lock().unwrap_or_else(|e| e.into_inner())
}

fn wake(inbox: &Inbox) {
    if let Some(Looper(looper)) = inbox.looper {
        // SAFETY: see `Looper`; the looper was acquired and is never
        // released.
        unsafe { ndk::ALooper_wake(looper) };
    }
}

/// Queue `msg` for the loop thread.
pub(super) fn send(msg: Msg) {
    let mut inbox = inbox();
    if inbox.done {
        return;
    }
    inbox.msgs.push_back((msg, None));
    wake(&inbox);
}

/// Queue `msg` and wait (at most `timeout`) until the loop has handled it.
pub(super) fn send_and_wait(msg: Msg, timeout: Duration) {
    let mut inbox = inbox();
    if inbox.done {
        return;
    }
    inbox.issued += 1;
    let ticket = inbox.issued;
    inbox.msgs.push_back((msg, Some(ticket)));
    wake(&inbox);
    let _ = HANDLED
        .wait_timeout_while(inbox, timeout, |i| i.handled < ticket && !i.done)
        .unwrap_or_else(|e| e.into_inner());
}

fn receive() -> Option<(Msg, Option<u64>)> {
    inbox().msgs.pop_front()
}

fn acknowledge(ticket: u64) {
    let mut inbox = inbox();
    inbox.handled = inbox.handled.max(ticket);
    HANDLED.notify_all();
}

// The loop thread.

/// The app's `main`, as the C runtime would call it.
type Main = unsafe extern "C" fn(c_int, *const *const c_char) -> c_int;

/// Find `main` in the app's own library. `RTLD_DEFAULT` would find the
/// zygote's.
fn find_main() -> Option<Main> {
    // SAFETY: `dladdr` fills `info` for an address inside a loaded library
    // (this function's own); `dlopen` with `RTLD_NOLOAD` only returns a
    // handle to that already-loaded library, and `dlsym` a symbol of it.
    // The symbol named `main` is the C entry point rustc generates for the
    // app binary, which has the `Main` signature.
    unsafe {
        let mut info: libc::Dl_info = std::mem::zeroed();
        if libc::dladdr(jni::JNI_OnLoad as *const c_void, &mut info) == 0
            || info.dli_fname.is_null()
        {
            return None;
        }
        let library = libc::dlopen(info.dli_fname, libc::RTLD_NOLOAD | libc::RTLD_NOW);
        if library.is_null() {
            return None;
        }
        let main = libc::dlsym(library, c"main".as_ptr());
        (!main.is_null()).then(|| std::mem::transmute::<*mut c_void, Main>(main))
    }
}

/// The body of the loop thread `nativeStart` spawns.
pub(super) fn loop_thread() {
    if jni::attach_current_thread(c"viso-main") {
        // SAFETY: this thread has no looper yet; the one prepared here is
        // acquired so the inbox may keep a pointer to it for the process.
        let looper = unsafe {
            let looper = ndk::ALooper_prepare(ndk::ALOOPER_PREPARE_ALLOW_NON_CALLBACKS);
            ndk::ALooper_acquire(looper);
            looper
        };
        LOOP.with(|l| l.looper.set(looper));
        {
            let mut inbox = inbox();
            inbox.looper = Some(Looper(looper));
            wake(&inbox);
        }
        match find_main() {
            Some(main) => {
                let argv: [*const c_char; 2] = [c"viso".as_ptr(), ptr::null()];
                // SAFETY: `main` is the app's C entry point; `argv` is a
                // NULL-terminated array of NUL-terminated strings that
                // outlives the call.
                unsafe { main(1, argv.as_ptr()) };
            }
            None => jni::log_warning("the app library exports no `main`"),
        }
    }
    let leftover = {
        let mut inbox = inbox();
        inbox.done = true;
        HANDLED.notify_all();
        std::mem::take(&mut inbox.msgs)
    };
    // Surfaces still queued are released here, outside the lock.
    drop(leftover);
    if !LOOP.with(|l| l.finishing.get()) {
        jni::finish();
    }
    jni::detach_current_thread();
}

// Loop-thread state.

/// Loop state, owned by the loop thread. Each field is borrowed briefly and
/// never across a handler call.
struct Loop {
    events: RefCell<VecDeque<RawEvent>>,
    looper: Cell<*mut ndk::ALooper>,
    choreographer: Cell<*mut ndk::AChoreographer>,
    post_frame64: Cell<Option<ndk::PostFrameCallback64>>,
    /// A choreographer callback is outstanding.
    frame_posted: Cell<bool>,
    /// A redraw is due at the next frame.
    redraw: Cell<bool>,
    surface: RefCell<Option<Surface>>,
    /// A surface taken away, released once its loss has been delivered.
    retired: RefCell<Option<Surface>>,
    /// The last surface size reported.
    size: Cell<(u32, u32)>,
    window_open: Cell<bool>,
    launched: Cell<bool>,
    density: Cell<f64>,
    appearance: Cell<Appearance>,
    bars: Cell<[i32; 4]>,
    ime: Cell<i32>,
    visible: Cell<bool>,
    focused: Cell<bool>,
    /// The mouse buttons last reported.
    mouse: Cell<PointerButtons>,
    cursor: Cell<i32>,
    /// The activity is finishing: `run` returns once the queue drains.
    finishing: Cell<bool>,
}

thread_local! {
    static LOOP: Loop = Loop {
        events: RefCell::new(VecDeque::new()),
        looper: Cell::new(ptr::null_mut()),
        choreographer: Cell::new(ptr::null_mut()),
        post_frame64: Cell::new(None),
        frame_posted: Cell::new(false),
        redraw: Cell::new(false),
        surface: RefCell::new(None),
        retired: RefCell::new(None),
        size: Cell::new((0, 0)),
        window_open: Cell::new(false),
        launched: Cell::new(false),
        density: Cell::new(1.0),
        appearance: Cell::new(Appearance::default()),
        bars: Cell::new([0; 4]),
        ime: Cell::new(0),
        visible: Cell::new(true),
        focused: Cell::new(true),
        mouse: Cell::new(PointerButtons::NONE),
        cursor: Cell::new(translate::pointer_icon(CursorIcon::Default)),
        finishing: Cell::new(false),
    };
}

fn push(event: RawEvent) {
    LOOP.with(|l| l.events.borrow_mut().push_back(event));
}

/// Push `event` only while the window is open.
fn push_to_window(event: RawEvent) {
    if LOOP.with(|l| l.window_open.get()) {
        push(event);
    }
}

fn appearance_of(bits: i32) -> Appearance {
    Appearance {
        color_scheme: if bits & 1 != 0 {
            ColorScheme::Dark
        } else {
            ColorScheme::Light
        },
        high_contrast: bits & 2 != 0,
        reduce_motion: bits & 4 != 0,
    }
}

fn safe_area() -> Insets {
    LOOP.with(|l| translate::insets(l.bars.get(), l.density.get()))
}

fn keyboard_height() -> f64 {
    LOOP.with(|l| f64::from(l.ime.get().max(0)) / l.density.get().max(f64::MIN_POSITIVE))
}

/// Ask for a frame at the next vsync.
fn request_frame() {
    LOOP.with(|l| l.redraw.set(true));
    post_frame();
}

/// Post a choreographer callback if a redraw is due, there is a surface to
/// draw into, and none is outstanding.
fn post_frame() {
    LOOP.with(|l| {
        let due = l.redraw.get() && l.surface.borrow().is_some() && !l.frame_posted.get();
        let choreographer = l.choreographer.get();
        if !due || choreographer.is_null() {
            return;
        }
        l.frame_posted.set(true);
        // SAFETY: `choreographer` belongs to this thread's looper; the
        // callbacks take no data and run on this thread.
        unsafe {
            match l.post_frame64.get() {
                Some(post) => post(choreographer, on_frame64, ptr::null_mut()),
                None => {
                    ndk::AChoreographer_postFrameCallback(choreographer, on_frame, ptr::null_mut())
                }
            }
        }
    });
}

fn frame() {
    LOOP.with(|l| {
        l.frame_posted.set(false);
        let ready = l.window_open.get() && l.surface.borrow().is_some();
        if ready && l.redraw.replace(false) {
            push(RawEvent::RedrawRequested { window: WINDOW });
        }
    });
}

unsafe extern "C" fn on_frame(_frame_time_nanos: c_long, _data: *mut c_void) {
    frame();
}

unsafe extern "C" fn on_frame64(_frame_time_nanos: i64, _data: *mut c_void) {
    frame();
}

/// Queue the copy handshake for the window.
fn copy(cut: bool) {
    push_to_window(RawEvent::CopyRequested {
        window: WINDOW,
        cut,
        reply: ClipboardReply::new(),
    });
}

fn paste() {
    if LOOP.with(|l| l.window_open.get())
        && let Some(text) = jni::clipboard()
    {
        push(RawEvent::Paste {
            window: WINDOW,
            text,
        });
    }
}

/// Turn one message into raw events.
fn apply(msg: Msg) {
    match msg {
        Msg::Surface(surface) => apply_surface(surface),
        Msg::SurfaceDestroyed => {
            let old = LOOP.with(|l| l.surface.borrow_mut().take());
            if let Some(old) = old {
                push_to_window(RawEvent::SurfaceDestroyed { window: WINDOW });
                LOOP.with(|l| *l.retired.borrow_mut() = Some(old));
            }
        }
        Msg::Redraw => request_frame(),
        Msg::Visible(visible) => {
            let changed = LOOP.with(|l| l.visible.replace(visible)) != visible;
            if changed && LOOP.with(|l| l.launched.get()) {
                push(if visible {
                    RawEvent::Resumed
                } else {
                    RawEvent::Suspended
                });
            }
        }
        Msg::Focus(focused) => {
            if LOOP.with(|l| l.focused.replace(focused)) != focused {
                push_to_window(RawEvent::WindowFocused {
                    window: WINDOW,
                    focused,
                });
            }
        }
        Msg::Insets { bars, ime } => {
            let (old_bars, old_ime) = LOOP.with(|l| (l.bars.replace(bars), l.ime.replace(ime)));
            if old_bars != bars {
                push_to_window(RawEvent::SafeAreaChanged {
                    window: WINDOW,
                    insets: safe_area(),
                });
            }
            if old_ime != ime {
                push_to_window(RawEvent::KeyboardInsetChanged {
                    window: WINDOW,
                    height: keyboard_height(),
                });
            }
        }
        Msg::Config {
            density,
            appearance,
        } => {
            let appearance = appearance_of(appearance);
            let launched = LOOP.with(|l| l.launched.get());
            if LOOP.with(|l| l.appearance.replace(appearance)) != appearance && launched {
                push(RawEvent::AppearanceChanged(appearance));
            }
            if LOOP.with(|l| l.density.replace(density)) != density {
                let (width, height) = LOOP.with(|l| l.size.get());
                push_to_window(RawEvent::ScaleFactorChanged {
                    window: WINDOW,
                    scale: density,
                    width,
                    height,
                });
                push_to_window(RawEvent::SafeAreaChanged {
                    window: WINDOW,
                    insets: safe_area(),
                });
                push_to_window(RawEvent::KeyboardInsetChanged {
                    window: WINDOW,
                    height: keyboard_height(),
                });
            }
        }
        Msg::LowMemory => {
            if LOOP.with(|l| l.launched.get()) {
                push(RawEvent::LowMemory);
            }
        }
        Msg::Touch(touch) => apply_touch(&touch),
        Msg::Scroll { at, delta, meta } => {
            let d = LOOP.with(|l| l.density.get());
            push_to_window(RawEvent::Scroll(RawScroll {
                window: WINDOW,
                x: f64::from(at[0]) / d,
                y: f64::from(at[1]) / d,
                delta_x: f64::from(delta[0]) / d,
                delta_y: f64::from(delta[1]) / d,
                modifiers: translate::modifiers(meta),
            }));
        }
        Msg::Key {
            code,
            pressed,
            repeat,
            meta,
            text,
        } => {
            if !LOOP.with(|l| l.window_open.get()) {
                return;
            }
            let key = translate::key_code(code);
            let modifiers = translate::modifiers(meta);
            push(RawEvent::Key(RawKey {
                window: WINDOW,
                code: key,
                pressed,
                repeat,
                modifiers,
            }));
            if !pressed {
                return;
            }
            match clipboard_shortcut(key, modifiers) {
                Some(ClipboardShortcut::Copy) => copy(false),
                Some(ClipboardShortcut::Cut) => copy(true),
                Some(ClipboardShortcut::Paste) => paste(),
                None => {
                    if let Some(text) = text.filter(|_| !translate::is_shortcut(meta)) {
                        push(RawEvent::Text(RawText {
                            window: WINDOW,
                            text,
                        }));
                    }
                }
            }
        }
        Msg::Preedit { text, caret_utf16 } => {
            let caret = byte_offset(&text, caret_utf16);
            push_to_window(RawEvent::ImePreedit(RawImePreedit {
                window: WINDOW,
                text,
                caret,
            }));
        }
        Msg::Commit(text) => push_to_window(RawEvent::Text(RawText {
            window: WINDOW,
            text,
        })),
        Msg::Edit(0) => copy(false),
        Msg::Edit(1) => copy(true),
        Msg::Edit(2) => paste(),
        Msg::Edit(_) => {}
        Msg::Destroy => {
            LOOP.with(|l| l.finishing.set(true));
            if LOOP.with(|l| l.window_open.replace(false)) {
                push(RawEvent::CloseRequested {
                    window: WINDOW,
                    accept: AcceptCell::new(),
                });
                push(RawEvent::WindowClosed { window: WINDOW });
            }
        }
    }
}

fn apply_surface(surface: Surface) {
    let size = surface.size;
    let same = LOOP.with(|l| {
        l.surface
            .borrow()
            .as_ref()
            .is_some_and(|s| s.window == surface.window)
    });
    if same {
        // A resize of the surface already lent; drop the extra reference.
        drop(surface);
        LOOP.with(|l| {
            if let Some(s) = l.surface.borrow_mut().as_mut() {
                s.size = size;
            }
        });
    } else {
        let old = LOOP.with(|l| l.surface.borrow_mut().replace(surface));
        if let Some(old) = old {
            push_to_window(RawEvent::SurfaceDestroyed { window: WINDOW });
            LOOP.with(|l| *l.retired.borrow_mut() = Some(old));
        }
        if !LOOP.with(|l| l.launched.replace(true)) {
            LOOP.with(|l| l.size.set(size));
            push(RawEvent::AppLaunched);
            return;
        }
        push_to_window(RawEvent::SurfaceCreated { window: WINDOW });
    }
    if LOOP.with(|l| l.size.replace(size)) != size {
        push_to_window(RawEvent::Resized {
            window: WINDOW,
            width: size.0,
            height: size.1,
        });
    }
    post_frame();
}

fn apply_touch(touch: &Touch) {
    if !LOOP.with(|l| l.window_open.get()) {
        return;
    }
    let density = LOOP.with(|l| l.density.get());
    let now = translate::buttons(touch.buttons);
    let reported = LOOP.with(|l| l.mouse.get());
    let modifiers = translate::modifiers(touch.meta);
    let mut saw_mouse = false;
    for (i, (&id, &tool)) in touch.ids.iter().zip(&touch.tools).enumerate() {
        let kind = translate::pointer_kind(tool);
        let Some(phase) =
            translate::phase(touch.action, touch.action_index, i, kind, reported, now)
        else {
            continue;
        };
        let sample = &touch.samples[i * 3..i * 3 + 3];
        let (buttons, pressure) = if kind == PointerKind::Mouse {
            saw_mouse = true;
            (now, if now.is_empty() { 0.0 } else { 0.5 })
        } else if translate::touching(touch.action, kind, phase) {
            (
                PointerButtons(PointerButtons::PRIMARY.0 | now.0),
                sample[2].clamp(0.0, 1.0),
            )
        } else {
            (PointerButtons::NONE, 0.0)
        };
        push(RawEvent::Pointer(RawPointer {
            window: WINDOW,
            pointer: translate::pointer_id(kind, id),
            kind,
            x: f64::from(sample[0]) / density,
            y: f64::from(sample[1]) / density,
            pressure,
            buttons,
            modifiers,
            phase,
        }));
    }
    if saw_mouse {
        LOOP.with(|l| l.mouse.set(now));
    }
}

/// Hand one event to the handler, completing the copy handshake.
fn deliver(handler: &mut dyn AppHandler, event: RawEvent) -> ControlFlow {
    let reply = match &event {
        RawEvent::CopyRequested { reply, .. } => Some(reply.clone()),
        _ => None,
    };
    let flow = handler.handle(event);
    if let Some(text) = reply.and_then(|r| r.take()) {
        jni::set_clipboard(&text);
    }
    flow
}

/// Deliver every queued event; the last answer, or `flow` if none.
fn deliver_all(handler: &mut dyn AppHandler, mut flow: ControlFlow) -> ControlFlow {
    while let Some(event) = LOOP.with(|l| l.events.borrow_mut().pop_front()) {
        flow = deliver(handler, event);
        if flow == ControlFlow::Exit {
            break;
        }
    }
    flow
}

/// The `ALooper_pollOnce` timeout for `deadline`, rounded up so the wake
/// never comes early.
fn timeout_millis(deadline: Instant, now: Instant) -> c_int {
    let wait = deadline.saturating_duration_since(now);
    let millis = wait.as_nanos().div_ceil(1_000_000);
    millis.min(c_int::MAX as u128) as c_int
}

/// The native Android application. Created by the app's `main` on the loop
/// thread.
pub struct AndroidApp {
    window: Option<AndroidWindow>,
}

impl AndroidApp {
    pub fn new() -> Result<Self, PlatformError> {
        if LOOP.with(|l| l.looper.get().is_null()) {
            return Err(PlatformError::Backend(
                "AndroidApp must be created on the thread VisoActivity starts".into(),
            ));
        }
        let launch = jni::launch_parameters();
        // SAFETY: this thread has a looper, which the choreographer
        // requires. `dlsym` looks up an optional libandroid entry point
        // (API 29) whose signature `PostFrameCallback64` matches the NDK
        // header.
        let (choreographer, post64) = unsafe {
            let post = libc::dlsym(
                libc::RTLD_DEFAULT,
                c"AChoreographer_postFrameCallback64".as_ptr(),
            );
            (
                ndk::AChoreographer_getInstance(),
                (!post.is_null())
                    .then(|| std::mem::transmute::<*mut c_void, ndk::PostFrameCallback64>(post)),
            )
        };
        LOOP.with(|l| {
            l.choreographer.set(choreographer);
            l.post_frame64.set(post64);
            l.density.set(launch.density);
            l.appearance.set(appearance_of(launch.appearance));
        });
        Ok(Self { window: None })
    }
}

impl PlatformApp for AndroidApp {
    fn create_window(&mut self, config: WindowConfig) -> Result<WindowId, PlatformError> {
        if self.window.is_some() {
            return Err(PlatformError::WindowCreation(
                "Android apps have a single window".into(),
            ));
        }
        LOOP.with(|l| l.window_open.set(true));
        jni::set_title(&config.title);
        push(RawEvent::SafeAreaChanged {
            window: WINDOW,
            insets: safe_area(),
        });
        push(RawEvent::KeyboardInsetChanged {
            window: WINDOW,
            height: keyboard_height(),
        });
        if !LOOP.with(|l| l.focused.get()) {
            push(RawEvent::WindowFocused {
                window: WINDOW,
                focused: false,
            });
        }
        self.window = Some(AndroidWindow);
        Ok(WINDOW)
    }

    fn run(&mut self, handler: &mut dyn AppHandler) {
        let mut flow = ControlFlow::Wait;
        loop {
            if LOOP.with(|l| !l.events.borrow().is_empty()) {
                flow = deliver_all(handler, flow);
                if flow == ControlFlow::Exit {
                    break;
                }
                continue;
            }
            if let Some((msg, ticket)) = receive() {
                apply(msg);
                flow = deliver_all(handler, flow);
                // The loss is delivered: the swapchain is gone, the window
                // may go.
                drop(LOOP.with(|l| l.retired.borrow_mut().take()));
                if let Some(ticket) = ticket {
                    acknowledge(ticket);
                }
                if flow == ControlFlow::Exit || LOOP.with(|l| l.finishing.get()) {
                    break;
                }
                continue;
            }
            let timeout = match flow {
                ControlFlow::Exit => break,
                ControlFlow::Poll => {
                    flow = ControlFlow::Wait;
                    0
                }
                ControlFlow::Wait => -1,
                ControlFlow::WaitUntil(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        push(RawEvent::Wakeup);
                        continue;
                    }
                    timeout_millis(deadline, now)
                }
            };
            // SAFETY: this thread has a looper; the out-pointers are
            // optional and passed as null.
            let polled = unsafe {
                ndk::ALooper_pollOnce(timeout, ptr::null_mut(), ptr::null_mut(), ptr::null_mut())
            };
            if polled == ndk::ALOOPER_POLL_ERROR {
                jni::log_warning("ALooper_pollOnce failed");
                break;
            }
        }
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

    fn close_window(&mut self, window: WindowId) {
        if window != WINDOW || self.window.take().is_none() {
            return;
        }
        LOOP.with(|l| {
            l.window_open.set(false);
            l.finishing.set(true);
        });
        jni::finish();
        push(RawEvent::WindowClosed { window });
    }

    fn set_clipboard_text(&mut self, text: &str) {
        jni::set_clipboard(text);
    }

    fn request_paste(&mut self, window: WindowId) {
        if window == WINDOW {
            paste();
        }
    }

    fn set_cursor(&mut self, window: WindowId, icon: CursorIcon) {
        let icon = translate::pointer_icon(icon);
        if window == WINDOW && LOOP.with(|l| l.cursor.replace(icon)) != icon {
            jni::set_pointer_icon(icon);
        }
    }

    fn set_ime_area(&mut self, window: WindowId, caret: Option<LogicalRect>) {
        if window != WINDOW {
            return;
        }
        let d = LOOP.with(|l| l.density.get());
        jni::set_text_input(caret.map(|r| [r.x, r.y, r.width, r.height].map(|v| (v * d) as f32)));
    }

    fn show_soft_keyboard(&mut self, window: WindowId, show: bool) {
        if window == WINDOW {
            jni::set_soft_keyboard(show);
        }
    }

    fn appearance(&self) -> Appearance {
        LOOP.with(|l| l.appearance.get())
    }

    fn framed_windows(&self) -> bool {
        false
    }
}

/// The activity's window; its state lives in the loop thread's [`Loop`].
pub struct AndroidWindow;

impl Window for AndroidWindow {
    fn id(&self) -> WindowId {
        WINDOW
    }

    fn request_redraw(&self) {
        request_frame();
    }

    fn set_title(&mut self, title: &str) {
        jni::set_title(title);
    }

    fn scale_factor(&self) -> f64 {
        LOOP.with(|l| l.density.get())
    }

    fn inner_size(&self) -> (u32, u32) {
        LOOP.with(|l| l.size.get())
    }

    fn raw_handle(&self) -> RawWindowHandle {
        let window = LOOP.with(|l| {
            l.surface
                .borrow()
                .as_ref()
                .map_or(ptr::null_mut(), |s| s.window)
        });
        RawWindowHandle::AndroidNdk {
            a_native_window: window.cast(),
        }
    }
}
