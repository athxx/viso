//! The Web services, on the browser's own APIs: `navigator.vibrate` and
//! `window.open` for URLs.

use std::rc::Rc;

use js_sys::Array;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{DomException, Navigator};

use crate::haptics::{Haptic, Haptics};
use crate::launcher::{Launcher, launch};
use crate::registry::Services;
use crate::reply::{Reply, ServiceError, ServiceResult};

pub(crate) fn services(_app: &str) -> Services {
    let web = Rc::new(Web);
    Services::from_parts(web.clone(), web)
}

struct Web;

impl Launcher for Web {
    /// A new browsing context, cut off from the page: the browser's popup
    /// blocker may still refuse it outside a click's handler, which the page
    /// cannot see.
    fn open_url(&self, url: &str) -> Reply<()> {
        launch(url, |url| {
            Reply::ready(window().and_then(|w| {
                w.open_with_url_and_target_and_features(url, "_blank", "noopener,noreferrer")
                    .map(|_| ())
                    .map_err(failure)
            }))
        })
    }
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

fn has(object: &JsValue, name: &str) -> bool {
    js_sys::Reflect::has(object, &JsValue::from_str(name)).unwrap_or(false)
}

fn window() -> ServiceResult<web_sys::Window> {
    web_sys::window().ok_or(ServiceError::Unsupported)
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
