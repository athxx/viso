// Kitchen timers: each counts down a second at a time as a task of the
// page, drawn as a ring of its own brush that reads the clock between
// ticks, so the ring sweeps smoothly while the numbers step.

import viso::i18n::tr
import viso::time
import viso::math::Vec2F32
import model::{KitchenTimer, clock_text}
import kit::{Field, NumberWell, Card, PageHeader, EmptyState}

// A ring of `tint` round the box, `done` of it lit, sweeping on toward the
// next second while `running`; a soft glow pulses once it reaches zero.
shader TimerRing {
    instance tint: ColorLinear
    instance done: F32
    instance pace: F32
    instance running: F32

    fragment () -> ColorLinear {
        let p = uv - Vec2F32(0.5, 0.5)
        let r = length(p) * 2.0
        let band = smoothstep(0.80, 0.82, r) * (1.0 - smoothstep(0.96, 0.98, r))
        let angle = atan2(p.x, -p.y) / 6.2831855 + 0.5
        let lit = done + pace * fract(time) * running
        let on = 1.0 - step(lit, angle)
        let track = 0.12
        let k = band * (track + (1.0 - track) * on)
        let finished = step(0.999, done)
        let glow = finished * (0.5 + 0.5 * sin(time * 6.0)) * (1.0 - smoothstep(0.0, 0.8, r)) * 0.25
        let a = clamp(k + glow, 0.0, 1.0)
        return ColorLinear(tint.r * a, tint.g * a, tint.b * a, a)
    }
}

// One timer: its ring and time left, and what can be done with it.
component TimerCard {
    input timer: KitchenTimer
    event paused(id: I64)
    event resumed(id: I64)
    event reset(id: I64)
    event removed(id: I64)

    computed finished: Bool = timer.left <= 0
    computed fraction: F32 = if timer.total <= 0 { 1.0 } else {
        1.0 - (timer.left as F32) / (timer.total as F32)
    }

    view {
        Card {
            Row {
                width: fill
                gap: 16dp
                align: Align::center
                Stack {
                    width: 96dp
                    height: 96dp
                    content_align: Alignment2D::center
                    ShaderView {
                        width: fill
                        height: fill
                        shader: TimerRing {
                            tint: if finished { #e0603f } else { #3f8fd6 }
                            done: fraction
                            pace: if timer.total <= 0 { 0.0 } else { 1.0 / (timer.total as F32) }
                            running: if timer.running && !finished { 1.0 } else { 0.0 }
                        }
                    }
                    Text(clock_text(timer.left)) {
                        font_size: 18sp
                        font_weight: FontWeight::semibold
                        semantics.live: LiveRegion::off
                    }
                }
                Column {
                    width: fill
                    gap: 6dp
                    Text(timer.label) {
                        font_weight: FontWeight::semibold
                        soft_wrap: true
                        width: fill
                        max_lines: Option::Some(2)
                        overflow: TextOverflow::ellipsis
                    }
                    Text(if finished { tr("timers.done") } else if timer.running {
                        tr("timers.running")
                    } else {
                        tr("timers.paused")
                    }) {
                        font_size: 13sp
                        color: if finished { theme.colors.error } else { theme.colors.muted }
                        semantics.live: LiveRegion::polite
                    }
                    Row {
                        gap: 8dp
                        if !finished {
                            Button(if timer.running { tr("timers.pause") } else { tr("timers.resume") }) {
                                on click {
                                    if timer.running { emit paused(timer.id) } else { emit resumed(timer.id) }
                                }
                            }
                        }
                        Button(tr("timers.reset")) { on click { emit reset(timer.id) } }
                        Button(tr("common.remove")) { on click { emit removed(timer.id) } }
                    }
                }
            }
        }
    }
}

// A form for a timer of its own: a label and how many minutes.
component NewTimer {
    event created(label: String, minutes: I64)

    state label = ""
    state minutes: F64 = 5.0

    view {
        Card {
            Row {
                width: fill
                gap: 8dp
                align: Align::center
                TextInput {
                    styles: [Field]
                    width: fill
                    placeholder: tr("timers.label")
                    semantics.label: tr("timers.label")
                    bind value <=> label
                }
                NumberField {
                    styles: [NumberWell]
                    width: 96dp
                    min: Option::Some(1.0)
                    max: Option::Some(240.0)
                    step: 1.0
                    semantics.label: tr("timers.minutes")
                    bind value <=> minutes
                }
                Text(tr("timers.minutes")) { color: theme.colors.muted }
                Button(tr("timers.start")) {
                    on click {
                        let name = if label.trim() == "" { tr("timers.untitled") } else { label.trim() }
                        emit created(name, minutes as I64)
                        label = ""
                    }
                }
            }
        }
    }
}

// The timers page. The page owns the clock: one task ticks every second
// while any timer runs, and reports each tick for the shell to apply.
export component Timers {
    input timers: List<KitchenTimer>
    input wide: Bool = true
    event created(label: String, minutes: I64)
    event ticked()
    event paused(id: I64)
    event resumed(id: I64)
    event reset(id: I64)
    event removed(id: I64)

    computed running: I64 = count_running(timers)

    task second() -> I64 {
        await time::sleep(1s)
        1
    }

    action tick() {
        start second() as clock {
            policy = [TaskPolicy::drop_new]
            success (n) {
                emit ticked()
                if running > 0 { tick() }
            }
        }
    }

    effect clock when (running) {
        transaction { if running > 0 { tick() } }
    }

    view {
        Scroll {
            width: fill
            height: fill
            Column {
                width: fill
                gap: 16dp
                padding: EdgeInsets { bottom: 32dp }
                PageHeader {
                    title: tr("timers.title")
                    subtitle: tr("timers.count", count: running)
                }
                NewTimer { on created(e) { emit created(e.label, e.minutes) } }
                if timers.is_empty() {
                    EmptyState {
                        title: tr("timers.empty_title")
                        message: tr("timers.empty_message")
                    }
                } else {
                    Grid {
                        width: fill
                        columns: if wide { [Track::fr(1.0), Track::fr(1.0)] } else { [Track::fr(1.0)] }
                        column_gap: 16dp
                        row_gap: 16dp
                        for timer in timers key timer.id {
                            TimerCard {
                                timer: timer
                                on paused(e) { emit paused(e.id) }
                                on resumed(e) { emit resumed(e.id) }
                                on reset(e) { emit reset(e.id) }
                                on removed(e) { emit removed(e.id) }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn count_running(timers: List<KitchenTimer>) -> I64 {
    let mut n = 0
    for timer in timers {
        if timer.running && timer.left > 0 { n += 1 }
    }
    n
}
