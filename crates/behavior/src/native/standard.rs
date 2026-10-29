//! The standard native libraries every registry holds: `viso::text`,
//! `viso::math`, `viso::time`, `viso::clipboard`, `viso::i18n`, the event methods of
//! `viso::event`, the navigator of `viso::nav`, the scheduler traits of
//! `viso::game` and `viso::game::quick`, and the widgets of `viso::widgets`.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use core::future::Future;
use core::pin::Pin;
use core::time::Duration;

use super::{
    NativeCx, NativeError, NativeFunction, NativeLibrary, NativeObject, NativeType, Obj, Timers,
};
#[cfg(not(feature = "std"))]
use crate::float::FloatMath as _;

/// The Rust name of each standard library under
/// [`libraries`](super::libraries), by its path.
pub const STANDARD_NAMES: &[(&str, &str)] = &[
    ("viso::text", "TEXT"),
    ("viso::math", "MATH"),
    ("viso::time", "TIME"),
    ("viso::clipboard", "CLIPBOARD"),
    ("viso::i18n", "I18N"),
    ("viso::game", "GAME"),
    ("viso::game::session", "SESSION"),
    ("viso::event", "EVENT"),
    ("viso::nav", "NAV"),
    ("viso::game::quick", "QUICK"),
    ("viso::game::kit", "KIT"),
    ("viso::widgets", "WIDGETS"),
];

/// The standard library holding the native at `path`, by its Rust name: the
/// one whose path is the longest prefix of it.
pub fn standard_name(path: &str) -> Option<&'static str> {
    STANDARD_NAMES
        .iter()
        .filter(|(library, _)| {
            path.strip_prefix(library)
                .is_some_and(|rest| rest.starts_with("::"))
        })
        .max_by_key(|(library, _)| library.len())
        .map(|&(_, name)| name)
}

/// Every standard library.
pub static STANDARD: &[&NativeLibrary] = &[
    &TEXT,
    &MATH,
    &TIME,
    &CLIPBOARD,
    &crate::i18n::I18N,
    &crate::game::GAME,
    &crate::game::SESSION,
    &crate::event::EVENT,
    &crate::nav::NAV,
    &crate::game::quick::QUICK,
    &crate::game::kit::KIT,
    &super::widgets::WIDGETS,
];

/// Text functions; `len` counts Unicode scalar values.
pub static TEXT: NativeLibrary = NativeLibrary {
    path: "viso::text",
    version: 1,
    functions: &[
        crate::native!(fn "upper" |_cx, text: String| -> String { Ok(text.to_uppercase()) })
            .deterministic(),
        crate::native!(fn "lower" |_cx, text: String| -> String { Ok(text.to_lowercase()) })
            .deterministic(),
        crate::native!(fn "trim" |_cx, text: String| -> String { Ok(text.trim().to_owned()) })
            .deterministic(),
        crate::native!(fn "len" |_cx, text: String| -> i64 {
            Ok(text.chars().count() as i64)
        })
        .deterministic(),
        crate::native!(fn "contains" |_cx, text: String, part: String| -> bool {
            Ok(text.contains(&part))
        })
        .deterministic(),
    ],
    types: &[],
    traits: &[],
    derives: &[],
    widgets: &[],
};

/// `F64` math and the `F32` vectors. Every function is `cross_platform`:
/// the basic operations and `sqrt` round as IEEE 754 says, and the
/// transcendentals are computed by [`libm`]'s software implementations, never
/// the host's, so each Tier-1 target returns the same bits.
pub static MATH: NativeLibrary = NativeLibrary {
    path: "viso::math",
    version: 1,
    functions: &[
        crate::native!(fn "sqrt" |_cx, x: f64| -> f64 { Ok(x.sqrt()) })
            .deterministic()
            .realtime_safe(),
        crate::native!(fn "abs" |_cx, x: f64| -> f64 { Ok(x.abs()) })
            .deterministic()
            .realtime_safe(),
        crate::native!(fn "floor" |_cx, x: f64| -> f64 { Ok(x.floor()) })
            .deterministic()
            .realtime_safe(),
        crate::native!(fn "round" |_cx, x: f64| -> f64 { Ok(x.round()) })
            .deterministic()
            .realtime_safe(),
        crate::native!(fn "min" |_cx, a: f64, b: f64| -> f64 { Ok(a.min(b)) })
            .deterministic()
            .realtime_safe(),
        crate::native!(fn "max" |_cx, a: f64, b: f64| -> f64 { Ok(a.max(b)) })
            .deterministic()
            .realtime_safe(),
        crate::native!(fn "clamp" |_cx, x: f64, low: f64, high: f64| -> f64 {
            if low <= high {
                Ok(x.clamp(low, high))
            } else {
                Err(NativeError::new(format!("the range {low}..={high} is empty")))
            }
        })
        .deterministic()
        .realtime_safe(),
        crate::native!(fn "sin" |_cx, x: f64| -> f64 { Ok(libm::sin(x)) })
            .deterministic()
            .realtime_safe(),
        crate::native!(fn "cos" |_cx, x: f64| -> f64 { Ok(libm::cos(x)) })
            .deterministic()
            .realtime_safe(),
        crate::native!(fn "tan" |_cx, x: f64| -> f64 { Ok(libm::tan(x)) })
            .deterministic()
            .realtime_safe(),
        crate::native!(fn "atan" |_cx, x: f64| -> f64 { Ok(libm::atan(x)) })
            .deterministic()
            .realtime_safe(),
        crate::native!(fn "atan2" |_cx, y: f64, x: f64| -> f64 { Ok(libm::atan2(y, x)) })
            .deterministic()
            .realtime_safe(),
        crate::native!(fn "exp" |_cx, x: f64| -> f64 { Ok(libm::exp(x)) })
            .deterministic()
            .realtime_safe(),
        crate::native!(fn "log" |_cx, x: f64| -> f64 { Ok(libm::log(x)) })
            .deterministic()
            .realtime_safe(),
        crate::native!(fn "pow" |_cx, x: f64, y: f64| -> f64 { Ok(libm::pow(x, y)) })
            .deterministic()
            .realtime_safe(),
        crate::native!(fn "hypot" |_cx, x: f64, y: f64| -> f64 { Ok(libm::hypot(x, y)) })
            .deterministic()
            .realtime_safe(),
    ],
    types: &[
        NativeType::value("Vec2F32", &super::vector::VEC2_METHODS),
        NativeType::value("Vec3F32", &super::vector::VEC3_METHODS),
    ],
    traits: &[],
    derives: &[],
    widgets: &[],
};

/// A running clock, the object behind a `viso::time::Stopwatch` handle.
/// Starting one needs the monotonic clock of `std`; without it `start`
/// fails.
#[derive(Debug)]
pub struct Stopwatch(#[cfg(feature = "std")] std::time::Instant);

impl NativeObject for Stopwatch {
    const PATH: &'static str = "viso::time::Stopwatch";
}

static STOPWATCH_METHODS: [NativeFunction; 2] = [
    crate::native!(action "start" |_cx| -> Obj<Stopwatch> {
        start_stopwatch().map(Obj::new)
    }),
    crate::native!(action "elapsed_ms" |_cx, this: Obj<Stopwatch>| -> f64 {
        Ok(elapsed_ms(&this))
    }),
];

/// A stopwatch started now.
#[cfg(feature = "std")]
fn start_stopwatch() -> Result<Stopwatch, NativeError> {
    Ok(Stopwatch(std::time::Instant::now()))
}

/// An error: without `std` there is no clock to start one on.
#[cfg(not(feature = "std"))]
fn start_stopwatch() -> Result<Stopwatch, NativeError> {
    Err(NativeError::new(
        "`viso::time::Stopwatch` needs a monotonic clock, which a build without `std` lacks",
    ))
}

/// The milliseconds since `stopwatch` started.
#[cfg(feature = "std")]
fn elapsed_ms(stopwatch: &Stopwatch) -> f64 {
    stopwatch.0.elapsed().as_secs_f64() * 1000.0
}

/// Without `std` a stopwatch measures nothing.
#[cfg(not(feature = "std"))]
fn elapsed_ms(_stopwatch: &Stopwatch) -> f64 {
    0.0
}

/// Clocks and waits. Reading a clock is an action: two reads differ.
pub static TIME: NativeLibrary = NativeLibrary {
    path: "viso::time",
    version: 1,
    functions: &[
        crate::native!(task "sleep" async |cx, duration: Duration| -> () {
            let wait = wait_for(cx, duration)?;
            Ok(async move {
                wait.await;
                Ok(())
            })
        }),
    ],
    types: &[NativeType::new("Stopwatch", &STOPWATCH_METHODS)],
    traits: &[],
    derives: &[],
    widgets: &[],
};

/// Work that finishes once `duration` has passed, on the host's [`Timers`],
/// else on the process's timer thread.
#[cfg(feature = "std")]
fn wait_for(
    cx: &mut NativeCx<'_>,
    duration: Duration,
) -> Result<Pin<Box<dyn Future<Output = ()>>>, NativeError> {
    Ok(match cx.service::<Box<dyn Timers>>() {
        Ok(timers) => timers.sleep(duration),
        Err(_) => super::timers::sleep_on_thread(duration),
    })
}

/// Work that finishes once `duration` has passed, on the host's [`Timers`]:
/// without `std` there is no timer thread to fall back to.
#[cfg(not(feature = "std"))]
fn wait_for(
    cx: &mut NativeCx<'_>,
    duration: Duration,
) -> Result<Pin<Box<dyn Future<Output = ()>>>, NativeError> {
    Ok(cx.service::<Box<dyn Timers>>()?.sleep(duration))
}

/// The host clipboard service natives read and write, installed on a
/// [`Vm`](crate::Vm) as a `Box<dyn Clipboard>`.
pub trait Clipboard {
    /// The clipboard text, if it holds text, once the platform answers: a
    /// read may wait on a permission prompt, so it never blocks the loop.
    fn read_text(&mut self) -> Pin<Box<dyn Future<Output = Option<String>>>>;

    /// Replaces the clipboard contents with `text`.
    fn write_text(&mut self, text: &str);
}

/// The clipboard, behind the `clipboard.read` and `clipboard.write`
/// capabilities.
pub static CLIPBOARD: NativeLibrary = NativeLibrary {
    path: "viso::clipboard",
    version: 1,
    functions: &[
        crate::native!(task "read_text" async |cx| -> Option<String> {
            let read = cx.service::<Box<dyn Clipboard>>()?.read_text();
            Ok(async move { Ok(read.await) })
        })
        .requires(&["clipboard.read"]),
        crate::native!(action "write_text" |cx, text: String| -> () {
            cx.service::<Box<dyn Clipboard>>()?.write_text(&text);
            Ok(())
        })
        .requires(&["clipboard.write"]),
    ],
    types: &[],
    traits: &[],
    derives: &[],
    widgets: &[],
};
