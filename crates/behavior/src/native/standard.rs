//! The standard native libraries every registry holds: `viso::text`,
//! `viso::math`, `viso::time`, `viso::clipboard`, the scheduler traits of
//! `viso::game` and `viso::game::quick`, and the widgets of `viso::widgets`.

use std::time::{Duration, Instant};

use super::{NativeError, NativeFunction, NativeLibrary, NativeObject, NativeType, Obj, Timers};

/// Every standard library.
pub static STANDARD: &[&NativeLibrary] = &[
    &TEXT,
    &MATH,
    &TIME,
    &CLIPBOARD,
    &crate::game::GAME,
    &crate::game::quick::QUICK,
    &crate::game::kit::KIT,
    &super::widgets::WIDGETS,
];

/// Text functions; `len` counts Unicode scalar values.
static TEXT: NativeLibrary = NativeLibrary {
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

/// `F64` math and the `F32` vectors.
static MATH: NativeLibrary = NativeLibrary {
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
#[derive(Debug)]
pub struct Stopwatch(Instant);

impl NativeObject for Stopwatch {
    const PATH: &'static str = "viso::time::Stopwatch";
}

static STOPWATCH_METHODS: [NativeFunction; 2] = [
    crate::native!(action "start" |_cx| -> Obj<Stopwatch> {
        Ok(Obj::new(Stopwatch(Instant::now())))
    }),
    crate::native!(action "elapsed_ms" |_cx, this: Obj<Stopwatch>| -> f64 {
        Ok(this.0.elapsed().as_secs_f64() * 1000.0)
    }),
];

/// Clocks and waits. Reading a clock is an action: two reads differ.
static TIME: NativeLibrary = NativeLibrary {
    path: "viso::time",
    version: 1,
    functions: &[crate::native!(task "sleep" |cx, duration: Duration| -> () {
        let wait = match cx.service::<Box<dyn Timers>>() {
            Ok(timers) => timers.sleep(duration),
            Err(_) => super::timers::sleep_on_thread(duration),
        };
        cx.suspend(async move {
            wait.await;
            Ok(())
        });
        Ok(())
    })],
    types: &[NativeType::new("Stopwatch", &STOPWATCH_METHODS)],
    traits: &[],
    derives: &[],
    widgets: &[],
};

/// The host clipboard service natives read and write, installed on a
/// [`Vm`](crate::Vm) as a `Box<dyn Clipboard>`.
pub trait Clipboard {
    /// The clipboard text, if it holds text.
    fn read_text(&mut self) -> Option<String>;

    /// Replaces the clipboard contents with `text`.
    fn write_text(&mut self, text: &str);
}

/// The clipboard, behind the `clipboard.read` and `clipboard.write`
/// capabilities.
static CLIPBOARD: NativeLibrary = NativeLibrary {
    path: "viso::clipboard",
    version: 1,
    functions: &[
        crate::native!(action "read_text" |cx| -> Option<String> {
            Ok(cx.service::<Box<dyn Clipboard>>()?.read_text())
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
