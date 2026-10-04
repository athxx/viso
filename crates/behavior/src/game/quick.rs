//! `viso::game::quick`: a game as one `QuickGame` system, with fewer imports
//! and one typed context per hook.
//!
//! The scheduler runs `QuickGame.start` as a `Startup` hook and
//! `QuickGame.fixed` as a `FixedUpdate` one, by hook identity: a quick game
//! steps the same clock, reads the same input and snapshots the same way as
//! the full Game Profile, and splitting it into `Startup` and `FixedUpdate`
//! systems changes nothing it computes. Its contexts are views of the full
//! profile's: [`QuickStart`] of `GameStart`, [`QuickFrame`] of `FixedFrame`.

use super::{FixedFrame, GameStart, InputSnapshot, signed};
use crate::native::{
    HookDomain, NativeFunction, NativeHook, NativeId, NativeLibrary, NativeObject, NativeTrait,
    NativeType, Obj, Param, SchemaTy,
};

/// The identity of the `QuickGame.start` hook.
pub const QUICK_START: NativeId = NativeId::of("viso::game::quick::QuickGame::start");
/// The identity of the `QuickGame.fixed` hook.
pub const QUICK_FIXED: NativeId = NativeId::of("viso::game::quick::QuickGame::fixed");

/// The start a `QuickGame.start` runs in, behind a
/// `viso::game::quick::QuickStart` handle.
#[derive(Debug)]
pub struct QuickStart {
    pub(super) start: Obj<GameStart>,
}

impl NativeObject for QuickStart {
    const PATH: &'static str = "viso::game::quick::QuickStart";
}

/// The fixed tick a `QuickGame.fixed` runs in, behind a
/// `viso::game::quick::QuickFrame` handle.
#[derive(Debug)]
pub struct QuickFrame {
    pub(super) frame: Obj<FixedFrame>,
}

impl NativeObject for QuickFrame {
    const PATH: &'static str = "viso::game::quick::QuickFrame";
}

static QUICK_START_METHODS: [NativeFunction; 1] = [crate::native!(
    fn "tick" |_cx, this: Obj<QuickStart>| -> i64 { Ok(signed(this.start.tick.get())) }
)
.deterministic()
.realtime_safe()];

static QUICK_FRAME_METHODS: [NativeFunction; 4] = [
    crate::native!(fn "tick" |_cx, this: Obj<QuickFrame>| -> i64 {
        Ok(signed(this.frame.tick.get()))
    })
    .deterministic()
    .realtime_safe(),
    crate::native!(fn "dt" |_cx, this: Obj<QuickFrame>| -> f64 { Ok(this.frame.dt) })
        .deterministic()
        .realtime_safe(),
    crate::native!(fn "time" |_cx, this: Obj<QuickFrame>| -> f64 {
        Ok(this.frame.tick.get() as f64 * this.frame.dt)
    })
    .deterministic()
    .realtime_safe(),
    crate::native!(fn "input" |_cx, this: Obj<QuickFrame>| -> Obj<InputSnapshot> {
        Ok(this.frame.input.clone())
    })
    .deterministic()
    .realtime_safe()
    .property(),
];

/// The `QuickGame` trait and the contexts its hooks receive, borrowed for the
/// call like the full profile's.
pub(crate) static QUICK: NativeLibrary = NativeLibrary {
    path: "viso::game::quick",
    version: 1,
    functions: &[],
    types: &[
        NativeType::new("QuickStart", &QUICK_START_METHODS).borrowed(),
        NativeType::new("QuickFrame", &QUICK_FRAME_METHODS).borrowed(),
    ],
    traits: &[NativeTrait {
        name: "QuickGame",
        hooks: &[
            NativeHook {
                name: "start",
                params: &[Param {
                    name: "cx",
                    ty: SchemaTy::Handle(QuickStart::PATH),
                }],
                domain: HookDomain::Simulation,
            },
            NativeHook {
                name: "fixed",
                params: &[Param {
                    name: "frame",
                    ty: SchemaTy::Handle(QuickFrame::PATH),
                }],
                domain: HookDomain::Simulation,
            },
        ],
    }],
    derives: &[],
    widgets: &[],
};
