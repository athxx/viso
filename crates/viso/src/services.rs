//! Platform services from a handler: opening URLs and haptic feedback. File
//! dialogs, share, notifications and secure storage are viso-ext crates.
//!
//! A handler reaches the session's registry through [`ServicesExt::services`]
//! and awaits the reply in a task:
//!
//! ```ignore
//! Button::new("Docs").on_click(move |cx| {
//!     let opened = cx.services().launcher().open_url("https://viso.dev/");
//!     cx.spawn_then(opened, move |cx, opened| {
//!         cx.set(failed, StateValue::Bool(opened.is_err()));
//!     });
//! })
//! ```

pub use viso_services::*;

use viso_ui::EventCx;

/// `cx.services()` on the event context.
pub trait ServicesExt {
    /// The session's service registry. Every call answers
    /// [`ServiceError::Unsupported`] where the session has none.
    fn services(&self) -> &Services;
}

impl ServicesExt for EventCx<'_> {
    fn services(&self) -> &Services {
        self.__services()
            .and_then(|any| any.downcast_ref::<Services>())
            .unwrap_or_else(|| unsupported())
    }
}

/// The registry lent where the driver lent none, created once per thread.
fn unsupported() -> &'static Services {
    thread_local! {
        static NONE: &'static Services = Box::leak(Box::new(Services::unsupported()));
    }
    NONE.with(|none| *none)
}

/// The name the OS knows this app by: the executable's file stem.
pub(crate) fn app_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| {
            exe.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "viso".to_owned())
}
