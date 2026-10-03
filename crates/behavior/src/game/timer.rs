//! Tick timers: `Cooldown` and `TickTimer`, plain values counting whole ticks
//! of the fixed step. The compiler converts their `Duration` to ticks, so at
//! run time they only compare ticks and never accumulate floating-point time;
//! as values they snapshot, replay and migrate like any other state.

use std::rc::Rc;

use crate::native::{NativeFunction, NativeValue, SchemaTy, Ticks};
use crate::value::{Aggregate, Value};

/// A cooldown: ready until fired, then not ready again for its period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cooldown {
    period: i64,
    ready_at: i64,
}

impl Cooldown {
    /// The full path of its native value type.
    pub const PATH: &'static str = "viso::game::Cooldown";

    /// A ready cooldown of `period` ticks.
    pub fn new(period: i64) -> Cooldown {
        Cooldown {
            period: period.max(0),
            ready_at: 0,
        }
    }

    /// Whether it is ready at `tick`.
    pub fn ready(self, tick: i64) -> bool {
        tick >= self.ready_at
    }

    /// Fired at `tick`: ready again `period` ticks later.
    pub fn fire(self, tick: i64) -> Cooldown {
        Cooldown {
            ready_at: tick.saturating_add(self.period),
            ..self
        }
    }

    /// The ticks from `tick` until it is ready, 0 when it is.
    pub fn remaining(self, tick: i64) -> i64 {
        self.ready_at.saturating_sub(tick).max(0)
    }
}

/// A repeating timer: due every `period` ticks, phase-locked to tick 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickTimer {
    period: i64,
    due_at: i64,
}

impl TickTimer {
    /// The full path of its native value type.
    pub const PATH: &'static str = "viso::game::TickTimer";

    /// A timer due every `period` ticks, first at tick `period`; a period
    /// shorter than one tick is one tick.
    pub fn every(period: i64) -> TickTimer {
        let period = period.max(1);
        TickTimer {
            period,
            due_at: period,
        }
    }

    /// Whether it is due at `tick`.
    pub fn due(self, tick: i64) -> bool {
        tick >= self.due_at
    }

    /// Re-armed at `tick`: due at the first multiple of its period after
    /// `tick`, so a late check neither drifts nor fires twice. Not yet due, it
    /// is unchanged.
    pub fn rearm(self, tick: i64) -> TickTimer {
        if tick < self.due_at {
            return self;
        }
        let missed = (tick - self.due_at) / self.period + 1;
        TickTimer {
            due_at: self
                .due_at
                .saturating_add(missed.saturating_mul(self.period)),
            ..self
        }
    }

    /// The ticks from `tick` until it is due, 0 when it is.
    pub fn remaining(self, tick: i64) -> i64 {
        self.due_at.saturating_sub(tick).max(0)
    }
}

/// The two tick counts of a timer value.
fn pair(value: &Value) -> Option<(i64, i64)> {
    match value {
        Value::Agg(agg) => match &agg.fields[..] {
            [first, second] => Some((first.as_int()?, second.as_int()?)),
            _ => None,
        },
        _ => None,
    }
}

fn aggregate(first: i64, second: i64) -> Value {
    Value::Agg(Rc::new(Aggregate {
        tag: 0,
        fields: Box::new([Value::Int(first), Value::Int(second)]),
    }))
}

impl NativeValue for Cooldown {
    const TY: SchemaTy = SchemaTy::Value(Cooldown::PATH);

    fn from_value(value: &Value) -> Option<Cooldown> {
        pair(value).map(|(period, ready_at)| Cooldown { period, ready_at })
    }

    fn into_value(self) -> Value {
        aggregate(self.period, self.ready_at)
    }
}

impl NativeValue for TickTimer {
    const TY: SchemaTy = SchemaTy::Value(TickTimer::PATH);

    fn from_value(value: &Value) -> Option<TickTimer> {
        pair(value)
            .filter(|&(period, _)| period >= 1)
            .map(|(period, due_at)| TickTimer { period, due_at })
    }

    fn into_value(self) -> Value {
        aggregate(self.period, self.due_at)
    }
}

pub(super) static COOLDOWN_METHODS: [NativeFunction; 4] = [
    crate::native!(fn "new" |_cx, period: Ticks| -> Cooldown { Ok(Cooldown::new(period.0)) })
        .constant()
        .realtime_safe(),
    crate::native!(fn "ready" |_cx, this: Cooldown, tick: i64| -> bool { Ok(this.ready(tick)) })
        .deterministic()
        .realtime_safe(),
    crate::native!(fn "fire" |_cx, this: Cooldown, tick: i64| -> Cooldown {
        Ok(this.fire(tick))
    })
    .deterministic(),
    crate::native!(fn "remaining" |_cx, this: Cooldown, tick: i64| -> i64 {
        Ok(this.remaining(tick))
    })
    .deterministic()
    .realtime_safe(),
];

pub(super) static TICK_TIMER_METHODS: [NativeFunction; 4] = [
    crate::native!(fn "every" |_cx, period: Ticks| -> TickTimer {
        Ok(TickTimer::every(period.0))
    })
    .constant()
    .realtime_safe(),
    crate::native!(fn "due" |_cx, this: TickTimer, tick: i64| -> bool { Ok(this.due(tick)) })
        .deterministic()
        .realtime_safe(),
    crate::native!(fn "rearm" |_cx, this: TickTimer, tick: i64| -> TickTimer {
        Ok(this.rearm(tick))
    })
    .deterministic(),
    crate::native!(fn "remaining" |_cx, this: TickTimer, tick: i64| -> i64 {
        Ok(this.remaining(tick))
    })
    .deterministic()
    .realtime_safe(),
];
