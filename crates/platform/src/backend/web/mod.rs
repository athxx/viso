//! Web backend (wasm-bindgen / web-sys).
//!
//! The browser owns the loop: [`PlatformApp::run`] parks the handler and
//! returns at once. Every DOM callback queues raw events and drains them
//! through the handler on the spot ([`drive`]), then arranges the next wake
//! the handler asked for:
//! - frames come from `requestAnimationFrame`, requested only while a redraw
//!   is pending ([`ControlFlow::Poll`] asks for one too);
//! - [`ControlFlow::WaitUntil`] arms a `setTimeout` that delivers
//!   [`RawEvent::Wakeup`];
//! - [`ControlFlow::Wait`] leaves the page idle until the next input.
//!
//! The app has a single window: a `<canvas data-viso-canvas="1">` filling the
//! element marked `data-viso-root` (the body without one). The GPU backend
//! finds the canvas by that attribute and owns its backing size; this backend
//! reports the size it should have, in device pixels. Keyboard focus sits on
//! a hidden `<textarea>` ([`input`]), which receives keys, text, composition
//! and clipboard events and opens the soft keyboard on touch devices.

mod input;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ptr::NonNull;
use std::rc::Rc;

use js_sys::{Array, Reflect};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::{Closure, JsValue};
use web_sys::{
    AddEventListenerOptions, Document, Event, EventTarget, FocusOptions, HtmlCanvasElement,
    HtmlElement, HtmlTextAreaElement, MediaQueryList, ResizeObserver, ResizeObserverBoxOptions,
    ResizeObserverEntry, ResizeObserverOptions, ResizeObserverSize,
};

use super::web_translate::{css_cursor, css_px, is_apple, keyboard_inset, pixels};
use crate::control::{ControlFlow, LogicalRect, PlatformError, WindowConfig, WindowId};
use crate::event::{Appearance, ColorScheme, CursorIcon, Insets, PointerButtons, RawEvent};
use crate::handler::AppHandler;
use crate::menu::Menu;
use crate::{Instant, PlatformApp, RawWindowHandle, Window};

/// The only window's id.
const WINDOW: WindowId = WindowId(1);

/// The canvas's `data-viso-canvas` value, which the GPU backend looks up.
const CANVAS_ID: u32 = 1;

const CANVAS_STYLE: &str = "display:block;width:100%;height:100%;touch-action:none;\
    outline:none;user-select:none;-webkit-user-select:none;-webkit-touch-callout:none;\
    -webkit-tap-highlight-color:transparent";

/// The focus target: invisible, never zoomed into (iOS zooms inputs under
/// 16px), and never scrolled or resized by the page.
const INPUT_STYLE: &str = "position:fixed;left:0;top:0;width:1px;height:1em;margin:0;\
    padding:0;border:0;outline:none;opacity:0;color:transparent;caret-color:transparent;\
    background:transparent;font-size:16px;resize:none;overflow:hidden;white-space:pre;\
    pointer-events:none";

/// Reads the safe-area insets back as computed padding.
const PROBE_STYLE: &str = "position:fixed;left:0;top:0;width:0;height:0;visibility:hidden;\
    pointer-events:none;padding:env(safe-area-inset-top) env(safe-area-inset-right) \
    env(safe-area-inset-bottom) env(safe-area-inset-left)";

/// Loop state shared by every DOM callback. The page is single-threaded;
/// each field is borrowed briefly and never across a handler call, so
/// callbacks the handler's own DOM calls trigger can queue events freely.
struct Loop {
    events: RefCell<VecDeque<RawEvent>>,
    /// A redraw is due at the next animation frame.
    redraw: Cell<bool>,
    /// The runtime's handler, installed by `run` and never removed: the
    /// caller keeps it alive for the rest of the page's life.
    handler: Cell<Option<NonNull<dyn AppHandler>>>,
    /// A drain is on the stack; nested callbacks only queue.
    driving: Cell<bool>,
    appearance: Cell<Appearance>,
    suspended: Cell<bool>,
    focused: Cell<bool>,
    /// The pending `requestAnimationFrame` and `setTimeout` handles.
    frame: Cell<Option<i32>>,
    timeout: Cell<Option<i32>>,
    on_frame: Closure<dyn FnMut(f64)>,
    on_timeout: Closure<dyn FnMut()>,
    /// The last `CopyRequested` answer (`Some(None)`: nothing to copy), kept
    /// for the `copy`/`cut` event that follows the shortcut's key press.
    copied: RefCell<Option<Option<String>>>,
    page: RefCell<Option<Rc<Page>>>,
}

thread_local! {
    static LOOP: Loop = Loop {
        events: RefCell::new(VecDeque::new()),
        redraw: Cell::new(false),
        handler: Cell::new(None),
        driving: Cell::new(false),
        appearance: Cell::new(Appearance::default()),
        suspended: Cell::new(false),
        focused: Cell::new(false),
        frame: Cell::new(None),
        timeout: Cell::new(None),
        on_frame: Closure::new(|_: f64| animation_frame()),
        on_timeout: Closure::new(wake),
        copied: RefCell::new(None),
        page: RefCell::new(None),
    };
}

/// Queue an event for the next drain.
fn push(event: RawEvent) {
    LOOP.with(|l| l.events.borrow_mut().push_back(event));
}

/// The window's page state, while the window exists.
fn current_page() -> Option<Rc<Page>> {
    LOOP.with(|l| l.page.borrow().clone())
}

fn dom_window() -> web_sys::Window {
    web_sys::window().expect("the web backend runs in a browser window")
}

fn document() -> Document {
    dom_window()
        .document()
        .expect("the browser window has a document")
}

/// Hand every queued event to the handler, then arm the wake its last
/// answer asked for. A call made while a drain is already running (a DOM
/// callback triggered by the handler's own request) returns at once: the
/// running drain picks the new events up. A panic aborts the module, so
/// there is no unwinding to reset `driving` for.
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
    let mut last = None;
    while let Some(event) = LOOP.with(|l| l.events.borrow_mut().pop_front()) {
        // SAFETY: the pointer was taken from the `&mut dyn AppHandler` passed
        // to `run`, whose caller keeps it alive for the rest of the page
        // (`Scheduler::run_detached` leaks it). `driving` admits one drain at
        // a time, so this is the only live reborrow.
        last = Some(deliver(unsafe { handler.as_mut() }, event));
    }
    LOOP.with(|l| l.driving.set(false));
    if let Some(flow) = last {
        schedule_wake(flow);
    }
}

/// Hand one event to the handler, completing the copy handshake: the
/// answer to a `CopyRequested` is kept for the clipboard event in progress.
fn deliver(handler: &mut dyn AppHandler, event: RawEvent) -> ControlFlow {
    let reply = match &event {
        RawEvent::CopyRequested { reply, .. } => Some(reply.clone()),
        _ => None,
    };
    let flow = handler.handle(event);
    if let Some(reply) = reply {
        LOOP.with(|l| *l.copied.borrow_mut() = Some(reply.take()));
    }
    flow
}

/// Arm (or disarm) the wake behind the handler's last answer.
fn schedule_wake(flow: ControlFlow) {
    let window = dom_window();
    if let Some(handle) = LOOP.with(|l| l.timeout.take()) {
        window.clear_timeout_with_handle(handle);
    }
    match flow {
        ControlFlow::Poll => request_frame(),
        ControlFlow::WaitUntil(deadline) => {
            let ms = deadline
                .saturating_duration_since(Instant::now())
                .as_secs_f64()
                * 1000.0;
            let ms = ms.ceil().min(f64::from(i32::MAX)) as i32;
            let handle = LOOP.with(|l| {
                window.set_timeout_with_callback_and_timeout_and_arguments_0(
                    l.on_timeout.as_ref().unchecked_ref(),
                    ms,
                )
            });
            LOOP.with(|l| l.timeout.set(handle.ok()));
        }
        ControlFlow::Wait => {}
        ControlFlow::Exit => {
            LOOP.with(|l| l.redraw.set(false));
            if let Some(handle) = LOOP.with(|l| l.frame.take()) {
                let _ = window.cancel_animation_frame(handle);
            }
        }
    }
}

/// Ask for a frame at the next animation frame.
fn request_frame() {
    let pending = LOOP.with(|l| {
        l.redraw.set(true);
        l.frame.get().is_some()
    });
    if pending {
        return;
    }
    let handle =
        LOOP.with(|l| dom_window().request_animation_frame(l.on_frame.as_ref().unchecked_ref()));
    LOOP.with(|l| l.frame.set(handle.ok()));
}

fn animation_frame() {
    LOOP.with(|l| l.frame.set(None));
    if LOOP.with(|l| l.redraw.replace(false)) && current_page().is_some() {
        push(RawEvent::RedrawRequested { window: WINDOW });
        drive();
    }
}

fn wake() {
    LOOP.with(|l| l.timeout.set(None));
    push(RawEvent::Wakeup);
    drive();
}

/// An event listener, removed when dropped. Registered non-passive, so
/// handlers may cancel the default action (scrolling, focus moves, text
/// edits).
struct Listener {
    target: EventTarget,
    kind: &'static str,
    callback: Closure<dyn FnMut(Event)>,
}

impl Listener {
    fn new(target: &EventTarget, kind: &'static str, f: impl FnMut(Event) + 'static) -> Self {
        let callback = Closure::<dyn FnMut(Event)>::new(f);
        let options = AddEventListenerOptions::new();
        options.set_passive(false);
        let _ = target.add_event_listener_with_callback_and_add_event_listener_options(
            kind,
            callback.as_ref().unchecked_ref(),
            &options,
        );
        Self {
            target: target.clone(),
            kind,
            callback,
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self
            .target
            .remove_event_listener_with_callback(self.kind, self.callback.as_ref().unchecked_ref());
    }
}

/// The canvas's resize observer and the callback it calls.
type Observer = (ResizeObserver, Closure<dyn FnMut(Array)>);

/// The window's DOM and the state its callbacks share.
struct Page {
    canvas: HtmlCanvasElement,
    input: HtmlTextAreaElement,
    /// Present when the canvas fills the page, so the safe area applies.
    probe: Option<HtmlElement>,
    /// The resize observer reports device-pixel content boxes.
    exact_boxes: bool,
    scale: Cell<f64>,
    /// The canvas in device pixels.
    size: Cell<(u32, u32)>,
    /// The buttons last reported for each pointer id.
    buttons: RefCell<Vec<(i32, PointerButtons)>>,
    /// An IME composition is in progress in the input element.
    composing: Cell<bool>,
    /// The last key press did not name its key (a soft keyboard or IME
    /// sent `229`), so edits arriving as input events stand in for it.
    unidentified: Cell<bool>,
    keyboard: Cell<f64>,
    insets: Cell<Insets>,
    soft_keyboard: Cell<bool>,
    cursor: Cell<CursorIcon>,
    listeners: RefCell<Vec<Listener>>,
    observer: RefCell<Option<Observer>>,
    /// Fires when the device pixel ratio leaves its current value.
    scale_watch: RefCell<Option<Listener>>,
}

impl Page {
    /// The canvas's top-left corner in client coordinates.
    fn origin(&self) -> (f64, f64) {
        let rect = self.canvas.get_bounding_client_rect();
        (rect.left(), rect.top())
    }

    /// The canvas in logical points.
    fn logical_size(&self) -> (f64, f64) {
        let (w, h) = self.size.get();
        let scale = self.scale.get();
        (f64::from(w) / scale, f64::from(h) / scale)
    }

    /// Focus the input element without scrolling the page to it.
    fn focus_input(&self) {
        let options = FocusOptions::new();
        options.set_prevent_scroll(true);
        let _ = self.input.focus_with_options(&options);
    }
}

/// Whether `ResizeObserver` reports device-pixel content boxes (Safari does
/// not; observing one there throws).
fn device_pixel_boxes() -> bool {
    Reflect::get(&js_sys::global(), &"ResizeObserverEntry".into())
        .and_then(|class| Reflect::get(&class, &"prototype".into()))
        .and_then(|proto| Reflect::has(&proto, &"devicePixelContentBoxSize".into()))
        .unwrap_or(false)
}

/// The first `ResizeObserverSize` of a box-size list, as whole pixels.
fn box_size(sizes: &Array) -> Option<(u32, u32)> {
    if sizes.is_undefined() || sizes.length() == 0 {
        return None;
    }
    let size: ResizeObserverSize = sizes.get(0).unchecked_into();
    Some((size.inline_size() as u32, size.block_size() as u32))
}

/// Report a new scale and/or device-pixel size.
fn update_geometry(page: &Page, scale: f64, size: (u32, u32)) {
    let scaled = page.scale.replace(scale) != scale;
    let resized = page.size.replace(size) != size;
    let (width, height) = size;
    if scaled {
        push(RawEvent::ScaleFactorChanged {
            window: WINDOW,
            scale,
            width,
            height,
        });
    } else if resized {
        push(RawEvent::Resized {
            window: WINDOW,
            width,
            height,
        });
    }
    report_safe_area(page);
    report_keyboard(page);
    drive();
}

/// Re-measure the canvas from its layout box.
fn remeasure(page: &Page) {
    let scale = dom_window().device_pixel_ratio();
    let rect = page.canvas.get_bounding_client_rect();
    update_geometry(
        page,
        scale,
        (pixels(rect.width(), scale), pixels(rect.height(), scale)),
    );
}

fn on_resize(entries: Array) {
    let Some(page) = current_page() else { return };
    let Some(entry) = entries
        .iter()
        .next()
        .map(|e| e.unchecked_into::<ResizeObserverEntry>())
    else {
        return;
    };
    let scale = dom_window().device_pixel_ratio();
    let exact = page
        .exact_boxes
        .then(|| box_size(&entry.device_pixel_content_box_size()))
        .flatten();
    let size = exact.unwrap_or_else(|| {
        let rect = entry.content_rect();
        (pixels(rect.width(), scale), pixels(rect.height(), scale))
    });
    update_geometry(&page, scale, size);
}

/// Watch for the device pixel ratio leaving its current value (zoom, a
/// move to another display), re-arming for the new value each time.
fn watch_scale(page: &Page) {
    let scale = dom_window().device_pixel_ratio();
    let query = format!("(resolution: {scale}dppx)");
    let Ok(Some(list)) = dom_window().match_media(&query) else {
        return;
    };
    let listener = Listener::new(&list, "change", |_| {
        let Some(page) = current_page() else { return };
        watch_scale(&page);
        remeasure(&page);
    });
    // Dropping the listener that is running is safe: wasm-bindgen frees a
    // closure only once its last invocation returns.
    *page.scale_watch.borrow_mut() = Some(listener);
}

fn report_safe_area(page: &Page) {
    let Some(probe) = &page.probe else { return };
    let Ok(Some(style)) = dom_window().get_computed_style(probe) else {
        return;
    };
    let side = |name: &str| css_px(&style.get_property_value(name).unwrap_or_default());
    let insets = Insets {
        top: side("padding-top"),
        left: side("padding-left"),
        bottom: side("padding-bottom"),
        right: side("padding-right"),
    };
    if page.insets.replace(insets) != insets {
        push(RawEvent::SafeAreaChanged {
            window: WINDOW,
            insets,
        });
    }
}

/// The on-screen keyboard's cover of the canvas: what the visual viewport
/// lost below the canvas's bottom edge. Pinch-zoomed pages report none.
fn report_keyboard(page: &Page) {
    let Some(viewport) = dom_window().visual_viewport() else {
        return;
    };
    let height = if (viewport.scale() - 1.0).abs() > 0.01 {
        0.0
    } else {
        let bottom = page.canvas.get_bounding_client_rect().bottom();
        keyboard_inset(bottom, viewport.height(), viewport.offset_top())
    };
    if page.keyboard.replace(height) != height {
        push(RawEvent::KeyboardInsetChanged {
            window: WINDOW,
            height,
        });
    }
}

/// The media queries behind [`Appearance`].
const DARK: &str = "(prefers-color-scheme: dark)";
const CONTRAST: [&str; 2] = ["(prefers-contrast: more)", "(forced-colors: active)"];
const REDUCED_MOTION: &str = "(prefers-reduced-motion: reduce)";

fn media(query: &str) -> Option<MediaQueryList> {
    dom_window().match_media(query).ok().flatten()
}

fn matches(query: &str) -> bool {
    media(query).is_some_and(|list| list.matches())
}

fn current_appearance() -> Appearance {
    Appearance {
        color_scheme: if matches(DARK) {
            ColorScheme::Dark
        } else {
            ColorScheme::Light
        },
        high_contrast: CONTRAST.iter().any(|q| matches(q)),
        reduce_motion: matches(REDUCED_MOTION),
    }
}

fn report_appearance() {
    let now = current_appearance();
    if LOOP.with(|l| l.appearance.replace(now)) != now {
        push(RawEvent::AppearanceChanged(now));
        drive();
    }
}

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
    if LOOP.with(|l| l.focused.replace(focused)) != focused && current_page().is_some() {
        push(RawEvent::WindowFocused {
            window: WINDOW,
            focused,
        });
        drive();
    }
}

fn report_fullscreen() {
    if current_page().is_some() {
        push(RawEvent::FullscreenChanged {
            window: WINDOW,
            fullscreen: document().fullscreen_element().is_some(),
        });
        drive();
    }
}

/// The platform string that decides the primary modifier:
/// `navigator.userAgentData.platform` where the browser has it, else
/// `navigator.platform`.
fn platform_name() -> String {
    let navigator = dom_window().navigator();
    Reflect::get(&navigator, &"userAgentData".into())
        .ok()
        .filter(|data| data.is_object())
        .and_then(|data| Reflect::get(&data, &"platform".into()).ok())
        .and_then(|p| p.as_string())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| navigator.platform().unwrap_or_default())
}

/// Write `text` to the system clipboard through the async clipboard API,
/// where the page may use it.
fn write_clipboard(text: String) {
    let clipboard = dom_window().navigator().clipboard();
    if clipboard.is_undefined() {
        return;
    }
    wasm_bindgen_futures::spawn_local(async move {
        let _ = wasm_bindgen_futures::JsFuture::from(clipboard.write_text(&text)).await;
    });
}

/// The native web application.
pub struct WebApp {
    window: Option<WebWindow>,
    listeners: Vec<Listener>,
}

impl WebApp {
    pub fn new() -> Result<Self, PlatformError> {
        let Some(window) = web_sys::window() else {
            return Err(PlatformError::Backend(
                "the web backend needs a browser window".into(),
            ));
        };
        if window.document().is_none() {
            return Err(PlatformError::Backend("the page has no document".into()));
        }
        std::panic::set_hook(Box::new(|info| {
            web_sys::console::error_1(&JsValue::from_str(&info.to_string()));
        }));
        crate::event::WEB_PRIMARY_IS_LOGO.store(
            is_apple(&platform_name()),
            std::sync::atomic::Ordering::Relaxed,
        );

        let doc = document();
        let mut listeners = vec![
            Listener::new(&doc, "visibilitychange", |_| {
                set_suspended(document().hidden())
            }),
            Listener::new(&doc, "fullscreenchange", |_| report_fullscreen()),
            Listener::new(&window, "focus", |_| set_focused(true)),
            Listener::new(&window, "blur", |_| set_focused(false)),
        ];
        for query in [DARK, CONTRAST[0], CONTRAST[1], REDUCED_MOTION] {
            if let Some(list) = media(query) {
                listeners.push(Listener::new(&list, "change", |_| report_appearance()));
            }
        }
        if let Some(viewport) = window.visual_viewport() {
            for kind in ["resize", "scroll"] {
                listeners.push(Listener::new(&viewport, kind, |_| {
                    if let Some(page) = current_page() {
                        report_keyboard(&page);
                        drive();
                    }
                }));
            }
        }
        LOOP.with(|l| {
            l.appearance.set(current_appearance());
            l.suspended.set(doc.hidden());
            l.focused.set(doc.has_focus().unwrap_or(true));
        });
        Ok(Self {
            window: None,
            listeners,
        })
    }
}

impl PlatformApp for WebApp {
    fn create_window(&mut self, config: WindowConfig) -> Result<WindowId, PlatformError> {
        if self.window.is_some() {
            return Err(PlatformError::WindowCreation(
                "a web page has a single window".into(),
            ));
        }
        let failed = |e: JsValue| PlatformError::WindowCreation(format!("{e:?}"));
        let document = document();
        let body = document
            .body()
            .ok_or_else(|| PlatformError::WindowCreation("the page has no body".into()))?;
        let root = document
            .query_selector("[data-viso-root]")
            .ok()
            .flatten()
            .and_then(|e| e.dyn_into::<HtmlElement>().ok());
        let fills_page = root.is_none();
        let root = root.unwrap_or_else(|| body.clone());

        let canvas: HtmlCanvasElement = document
            .create_element("canvas")
            .map_err(failed)?
            .unchecked_into();
        canvas
            .set_attribute("data-viso-canvas", &CANVAS_ID.to_string())
            .map_err(failed)?;
        canvas
            .set_attribute("style", CANVAS_STYLE)
            .map_err(failed)?;
        root.append_child(&canvas).map_err(failed)?;

        let input: HtmlTextAreaElement = document
            .create_element("textarea")
            .map_err(failed)?
            .unchecked_into();
        for (name, value) in [
            ("style", INPUT_STYLE),
            ("inputmode", "none"),
            ("autocapitalize", "off"),
            ("autocomplete", "off"),
            ("autocorrect", "off"),
            ("spellcheck", "false"),
            ("aria-hidden", "true"),
            ("tabindex", "-1"),
        ] {
            input.set_attribute(name, value).map_err(failed)?;
        }
        root.append_child(&input).map_err(failed)?;

        let probe = if fills_page {
            let probe: HtmlElement = document
                .create_element("div")
                .map_err(failed)?
                .unchecked_into();
            probe.set_attribute("style", PROBE_STYLE).map_err(failed)?;
            body.append_child(&probe).map_err(failed)?;
            document.set_title(&config.title);
            Some(probe)
        } else {
            None
        };

        let scale = dom_window().device_pixel_ratio();
        let rect = canvas.get_bounding_client_rect();
        let page = Rc::new(Page {
            canvas,
            input,
            probe,
            exact_boxes: device_pixel_boxes(),
            scale: Cell::new(scale),
            size: Cell::new((pixels(rect.width(), scale), pixels(rect.height(), scale))),
            buttons: RefCell::new(Vec::new()),
            composing: Cell::new(false),
            unidentified: Cell::new(false),
            keyboard: Cell::new(0.0),
            insets: Cell::new(Insets::default()),
            soft_keyboard: Cell::new(false),
            cursor: Cell::new(CursorIcon::Default),
            listeners: RefCell::new(Vec::new()),
            observer: RefCell::new(None),
            scale_watch: RefCell::new(None),
        });
        LOOP.with(|l| *l.page.borrow_mut() = Some(page.clone()));

        *page.listeners.borrow_mut() = input::listen(&page);
        let callback = Closure::<dyn FnMut(Array)>::new(on_resize);
        let observer = ResizeObserver::new(callback.as_ref().unchecked_ref()).map_err(failed)?;
        let options = ResizeObserverOptions::new();
        options.set_box(if page.exact_boxes {
            ResizeObserverBoxOptions::DevicePixelContentBox
        } else {
            ResizeObserverBoxOptions::ContentBox
        });
        observer.observe_with_options(&page.canvas, &options);
        *page.observer.borrow_mut() = Some((observer, callback));
        watch_scale(&page);
        page.focus_input();
        report_safe_area(&page);
        report_keyboard(&page);

        self.window = Some(WebWindow { page });
        Ok(WINDOW)
    }

    fn run(&mut self, handler: &mut dyn AppHandler) {
        // SAFETY: only the trait object's lifetime is erased. The browser
        // loop outlives `run`, and the caller keeps `handler` alive for the
        // rest of the page (the contract of `PlatformApp::run` on a target
        // whose loop cannot block), so every use `drive` makes of the pointer
        // is within the borrow.
        let handler: NonNull<dyn AppHandler> = unsafe {
            std::mem::transmute::<NonNull<dyn AppHandler + '_>, NonNull<dyn AppHandler + 'static>>(
                NonNull::from(handler),
            )
        };
        LOOP.with(|l| l.handler.set(Some(handler)));
        push(RawEvent::AppLaunched);
        drive();
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

    fn set_fullscreen(&mut self, window: WindowId, fullscreen: bool) {
        let Some(w) = self.window.as_ref().filter(|_| window == WINDOW) else {
            return;
        };
        let document = document();
        if fullscreen {
            // The whole page when the canvas fills it, else the canvas's
            // container.
            let target = match (&w.page.probe, document.document_element()) {
                (Some(_), Some(root)) => root,
                _ => w
                    .page
                    .canvas
                    .parent_element()
                    .unwrap_or_else(|| w.page.canvas.clone().unchecked_into()),
            };
            let _ = target.request_fullscreen();
        } else if document.fullscreen_element().is_some() {
            document.exit_fullscreen();
        }
    }

    fn close_window(&mut self, window: WindowId) {
        if window != WINDOW {
            return;
        }
        let Some(w) = self.window.take() else { return };
        let page = w.page;
        LOOP.with(|l| l.page.borrow_mut().take());
        let listeners = std::mem::take(&mut *page.listeners.borrow_mut());
        drop(listeners);
        page.scale_watch.borrow_mut().take();
        if let Some((observer, _callback)) = page.observer.borrow_mut().take() {
            observer.disconnect();
        }
        page.canvas.remove();
        page.input.remove();
        if let Some(probe) = &page.probe {
            probe.remove();
        }
        push(RawEvent::WindowClosed { window: WINDOW });
        drive();
    }

    fn set_clipboard_text(&mut self, text: &str) {
        write_clipboard(text.to_owned());
    }

    fn request_paste(&mut self, window: WindowId) {
        let clipboard = dom_window().navigator().clipboard();
        if clipboard.is_undefined() {
            return;
        }
        wasm_bindgen_futures::spawn_local(async move {
            let read = wasm_bindgen_futures::JsFuture::from(clipboard.read_text()).await;
            if let Some(text) = read.ok().and_then(|t| t.as_string()) {
                push(RawEvent::Paste { window, text });
                drive();
            }
        });
    }

    fn set_cursor(&mut self, window: WindowId, icon: CursorIcon) {
        if let Some(w) = self.window.as_ref().filter(|_| window == WINDOW)
            && w.page.cursor.replace(icon) != icon
        {
            let _ = w
                .page
                .canvas
                .style()
                .set_property("cursor", css_cursor(icon));
        }
    }

    fn set_ime_area(&mut self, window: WindowId, caret: Option<LogicalRect>) {
        if let Some(w) = self.window.as_ref().filter(|_| window == WINDOW) {
            input::set_ime_area(&w.page, caret);
        }
    }

    fn show_soft_keyboard(&mut self, window: WindowId, show: bool) {
        if let Some(w) = self.window.as_ref().filter(|_| window == WINDOW) {
            input::show_soft_keyboard(&w.page, show);
        }
    }

    fn appearance(&self) -> Appearance {
        LOOP.with(|l| l.appearance.get())
    }

    fn framed_windows(&self) -> bool {
        false
    }
}

impl Drop for WebApp {
    fn drop(&mut self) {
        self.listeners.clear();
    }
}

pub struct WebWindow {
    page: Rc<Page>,
}

impl Window for WebWindow {
    fn id(&self) -> WindowId {
        WINDOW
    }

    fn request_redraw(&self) {
        request_frame();
    }

    fn set_title(&mut self, title: &str) {
        if self.page.probe.is_some() {
            document().set_title(title);
        }
    }

    fn scale_factor(&self) -> f64 {
        self.page.scale.get()
    }

    fn inner_size(&self) -> (u32, u32) {
        self.page.size.get()
    }

    fn raw_handle(&self) -> RawWindowHandle {
        RawWindowHandle::WebCanvas {
            canvas_id: CANVAS_ID,
        }
    }
}
