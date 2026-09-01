//! Phase-specific contexts.
//!
//! Instead of one omnipotent context, each frame phase gets a purpose-specific
//! one. This is both an architectural constraint and a performance / reasoning
//! tool: a `LayoutCx` cannot launch a network request, create a window, or
//! submit arbitrary GPU work.
//!
//! Phase 0 defines these as marker types so signatures like
//! `fn layout(&mut self, cx: &mut LayoutCx<'_>, ..)` can be written against a
//! stable contract before the real capabilities are filled in.

use core::marker::PhantomData;

macro_rules! phase_cx {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        pub struct $name<'a> {
            _life: PhantomData<&'a mut ()>,
        }
        impl<'a> $name<'a> {
            #[doc(hidden)]
            pub fn __new() -> Self {
                Self { _life: PhantomData }
            }
        }
    };
}

phase_cx!(/// Application-scope context: create windows, register services.
    AppCx);
phase_cx!(/// Input dispatch context.
    EventCx);
phase_cx!(/// State-flush / reactive update context.
    UpdateCx);
phase_cx!(/// Layout context. No network, no window creation, no GPU submit.
    LayoutCx);
phase_cx!(/// Paint-data production context.
    PaintCx);
phase_cx!(/// Render/submit context (internal).
    RenderCx);
phase_cx!(/// Async task context.
    TaskCx);
