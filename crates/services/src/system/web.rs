//! The Web services, on the browser's own APIs: a file `<input>` and a
//! download link, `navigator.share`, the Notification API and its
//! permission, and `navigator.vibrate`. Pages have no secure storage.
//!
//! Browsers show the file chooser, the share sheet and the permission
//! prompt only while the page has transient user activation, so these
//! calls belong in the handler of the click or key that asked for them.
//! A file chooser dismissed in a browser without the input's `cancel`
//! event never answers.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use js_sys::{Array, Uint8Array};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    Blob, BlobPropertyBag, Document, DomException, Event, EventTarget, FileList, HtmlAnchorElement,
    HtmlInputElement, Navigator, NotificationOptions, NotificationPermission, ShareData, Url,
};

use crate::files::{FileDialogs, OpenOptions, PickedFile, SaveOptions};
use crate::haptics::{Haptic, Haptics};
use crate::notifications::{Notification, Notifications};
use crate::permissions::{Permission, PermissionState, Permissions};
use crate::registry::Services;
use crate::reply::{Completer, Reply, ServiceError, ServiceResult, reply};
use crate::share::{Share, ShareItem};
use crate::unsupported::Unsupported;

/// How long a download's object URL outlives the click that starts it.
const DOWNLOAD_URL_MS: i32 = 60_000;

pub(crate) fn services(_app: &str) -> Services {
    let web = Rc::new(Web::default());
    Services::from_parts(
        web.clone(),
        web.clone(),
        web.clone(),
        web.clone(),
        Rc::new(Unsupported),
        web,
    )
}

#[derive(Default)]
struct Web {
    /// The notifications on screen, one per id, kept to withdraw them.
    shown: RefCell<HashMap<String, web_sys::Notification>>,
}

impl FileDialogs for Web {
    fn open(&self, options: OpenOptions) -> Reply<Vec<PickedFile>> {
        let (completer, reply) = reply();
        if let Err(error) = choose(&options, completer) {
            return Reply::err(error);
        }
        reply
    }

    fn save(&self, options: SaveOptions, contents: Vec<u8>) -> Reply<Option<PathBuf>> {
        Reply::ready(download(&options.suggested_name, &contents).map(|()| None))
    }
}

/// Show a file chooser for `options`; `completer` answers with the files
/// read in full.
fn choose(options: &OpenOptions, completer: Completer<Vec<PickedFile>>) -> ServiceResult<()> {
    let document = document()?;
    let input: HtmlInputElement = document
        .create_element("input")
        .map_err(failure)?
        .unchecked_into();
    input.set_type("file");
    input.set_multiple(options.multiple);
    // A filter without extensions admits any file, and so the chooser.
    if !options.filters.iter().any(|f| f.extensions.is_empty()) {
        let accept: Vec<String> = options
            .filters
            .iter()
            .flat_map(|f| f.extensions.iter().map(|e| format!(".{e}")))
            .collect();
        input.set_accept(&accept.join(","));
    }
    // Some browsers deliver `change` only to an input in the document.
    input.set_hidden(true);
    let body = document
        .body()
        .ok_or_else(|| ServiceError::Failed("the page has no body".to_owned()))?;
    body.append_child(&input).map_err(failure)?;
    let picked = input.clone();
    first_of(&input, &["change", "cancel"], move |event| {
        picked.remove();
        let files = picked.files().filter(|files| files.length() > 0);
        match files {
            Some(files) if event.type_() == "change" => {
                spawn_local(async move { completer.complete(read(files).await) });
            }
            _ => completer.complete(Err(ServiceError::Cancelled)),
        }
    })
    .map_err(failure)?;
    input.click();
    Ok(())
}

async fn read(files: FileList) -> ServiceResult<Vec<PickedFile>> {
    let mut picked = Vec::with_capacity(files.length() as usize);
    for index in 0..files.length() {
        let Some(file) = files.item(index) else {
            continue;
        };
        let buffer = JsFuture::from(file.array_buffer()).await.map_err(failure)?;
        picked.push(PickedFile {
            name: file.name(),
            path: None,
            contents: Uint8Array::new(&buffer).to_vec(),
        });
    }
    Ok(picked)
}

/// Have the browser download `contents` as `name`.
fn download(name: &str, contents: &[u8]) -> ServiceResult<()> {
    let document = document()?;
    let parts = Array::of1(&Uint8Array::from(contents));
    let properties = BlobPropertyBag::new();
    properties.set_type("application/octet-stream");
    let blob =
        Blob::new_with_u8_array_sequence_and_options(&parts, &properties).map_err(failure)?;
    let url = Url::create_object_url_with_blob(&blob).map_err(failure)?;
    let link: HtmlAnchorElement = document
        .create_element("a")
        .map_err(failure)?
        .unchecked_into();
    link.set_href(&url);
    link.set_download(name);
    link.click();
    // The browser fetches the URL after the click returns.
    let revoke = Closure::once_into_js(move || {
        let _ = Url::revoke_object_url(&url);
    });
    window()?
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            revoke.unchecked_ref(),
            DOWNLOAD_URL_MS,
        )
        .map_err(failure)?;
    Ok(())
}

impl Share for Web {
    fn share(&self, item: ShareItem) -> Reply<()> {
        let result = (|| {
            let navigator = navigator()?;
            if !has(&navigator, "share") {
                return Err(ServiceError::Unsupported);
            }
            let data = ShareData::new();
            match &item {
                ShareItem::Text(text) => data.set_text(text),
                ShareItem::Url(url) => data.set_url(url),
            }
            if has(&navigator, "canShare") && !navigator.can_share_with_data(&data) {
                return Err(ServiceError::Unsupported);
            }
            Ok(navigator.share_with_data(&data))
        })();
        match result {
            Ok(promise) => settle(promise, |_| Ok(())),
            Err(error) => Reply::err(error),
        }
    }
}

impl Notifications for Web {
    fn notify(&self, notification: Notification) -> Reply<()> {
        if !has(&js_sys::global(), "Notification") {
            return Reply::err(ServiceError::Unsupported);
        }
        if web_sys::Notification::permission() != NotificationPermission::Granted {
            return Reply::err(ServiceError::Denied);
        }
        let options = NotificationOptions::new();
        options.set_body(&notification.body);
        options.set_tag(&notification.id);
        match web_sys::Notification::new_with_options(&notification.title, &options) {
            Ok(shown) => {
                self.shown.borrow_mut().insert(notification.id, shown);
                Reply::ready(Ok(()))
            }
            // Browsers that show notifications only from a service worker
            // (Chrome on Android) refuse the constructor with a TypeError.
            Err(error) if error.is_instance_of::<js_sys::TypeError>() => {
                Reply::err(ServiceError::Unsupported)
            }
            Err(error) => Reply::err(failure(error)),
        }
    }

    fn withdraw(&self, id: &str) {
        if let Some(shown) = self.shown.borrow_mut().remove(id) {
            shown.close();
        }
    }
}

impl Permissions for Web {
    fn status(&self, permission: Permission) -> Reply<PermissionState> {
        let Permission::Notifications = permission;
        Reply::ready(notification_state())
    }

    fn request(&self, permission: Permission) -> Reply<PermissionState> {
        let Permission::Notifications = permission;
        match notification_state() {
            Ok(PermissionState::Prompt) => {}
            settled => return Reply::ready(settled),
        }
        match web_sys::Notification::request_permission() {
            Ok(promise) => settle(promise, |answer| {
                Ok(match answer.as_string().as_deref() {
                    Some("granted") => PermissionState::Granted,
                    Some("denied") => PermissionState::Denied,
                    _ => PermissionState::Prompt,
                })
            }),
            Err(error) => Reply::err(failure(error)),
        }
    }
}

fn notification_state() -> ServiceResult<PermissionState> {
    if !has(&js_sys::global(), "Notification") {
        return Err(ServiceError::Unsupported);
    }
    Ok(match web_sys::Notification::permission() {
        NotificationPermission::Granted => PermissionState::Granted,
        NotificationPermission::Denied => PermissionState::Denied,
        _ => PermissionState::Prompt,
    })
}

impl Haptics for Web {
    fn play(&self, haptic: Haptic) -> ServiceResult<()> {
        let navigator = navigator()?;
        if !has(&navigator, "vibrate") {
            return Err(ServiceError::Unsupported);
        }
        // Vibration only: a phone's motor, in milliseconds on and off.
        let pattern: &[u32] = match haptic {
            Haptic::Selection => &[8],
            Haptic::Light => &[10],
            Haptic::Medium => &[20],
            Haptic::Heavy => &[35],
            Haptic::Success => &[10, 60, 20],
            Haptic::Warning => &[20, 80, 20],
            Haptic::Error => &[30, 60, 30, 60, 30],
        };
        let pattern: Array = pattern.iter().map(|ms| JsValue::from(*ms)).collect();
        // The browser refuses before the page's first user activation.
        if navigator.vibrate_with_pattern(&pattern) {
            Ok(())
        } else {
            Err(ServiceError::Denied)
        }
    }
}

/// Run `run` on the first of `events` to reach `target`, then stop
/// listening.
fn first_of(
    target: &EventTarget,
    events: &'static [&'static str],
    run: impl FnOnce(Event) + 'static,
) -> Result<(), JsValue> {
    type Listener = Closure<dyn FnMut(Event)>;
    struct Once {
        run: Option<Box<dyn FnOnce(Event)>>,
        listener: Option<Listener>,
    }
    let state = Rc::new(RefCell::new(Once {
        run: Some(Box::new(run)),
        listener: None,
    }));
    let owner = target.clone();
    let inner = state.clone();
    let listener = Listener::new(move |event: Event| {
        let (run, listener) = {
            let mut state = inner.borrow_mut();
            (state.run.take(), state.listener.take())
        };
        if let Some(listener) = &listener {
            for name in events {
                let _ = owner
                    .remove_event_listener_with_callback(name, listener.as_ref().unchecked_ref());
            }
        }
        // A closure must not be freed while it runs; free it afterwards.
        spawn_local(async move { drop(listener) });
        if let Some(run) = run {
            run(event);
        }
    });
    for name in events {
        target.add_event_listener_with_callback(name, listener.as_ref().unchecked_ref())?;
    }
    state.borrow_mut().listener = Some(listener);
    Ok(())
}

/// A reply `answer` completes from `promise`'s value.
fn settle<T: 'static>(
    promise: js_sys::Promise,
    answer: impl FnOnce(JsValue) -> ServiceResult<T> + 'static,
) -> Reply<T> {
    let (completer, reply) = reply();
    spawn_local(async move {
        let result = JsFuture::from(promise).await.map_err(failure);
        completer.complete(result.and_then(answer));
    });
    reply
}

fn has(object: &JsValue, name: &str) -> bool {
    js_sys::Reflect::has(object, &JsValue::from_str(name)).unwrap_or(false)
}

fn window() -> ServiceResult<web_sys::Window> {
    web_sys::window().ok_or(ServiceError::Unsupported)
}

fn document() -> ServiceResult<Document> {
    window()?.document().ok_or(ServiceError::Unsupported)
}

fn navigator() -> ServiceResult<Navigator> {
    Ok(window()?.navigator())
}

/// The service error a rejected browser call stands for.
fn failure(error: JsValue) -> ServiceError {
    if let Some(exception) = error.dyn_ref::<DomException>() {
        return match exception.name().as_str() {
            "AbortError" => ServiceError::Cancelled,
            "NotAllowedError" | "SecurityError" => ServiceError::Denied,
            "NotSupportedError" => ServiceError::Unsupported,
            _ => ServiceError::Failed(exception.message()),
        };
    }
    if let Some(error) = error.dyn_ref::<js_sys::Error>() {
        return ServiceError::Failed(error.message().into());
    }
    ServiceError::Failed(error.as_string().unwrap_or_else(|| format!("{error:?}")))
}
