//! What crosses between a `web-hybrid` page and its island: the input values
//! the page pushes to the component the renderer draws there, and the events
//! that component emits, which the page's handlers run.
//!
//! Both sides run on the page's thread and never call into each other: the
//! page queues each value it pushes, and the renderer takes them at its next
//! frame ([`poll_pushed`]); the renderer queues each event, and the page runs
//! them once the frame that emitted them is over ([`emit`]).

use std::cell::RefCell;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use viso_behavior::Value;

#[derive(Default)]
struct Crossing {
    /// The latest value pushed for each island input, by `(island, slot)`.
    pushed: Vec<(u32, u32, Value)>,
    /// What to wake once a value is pushed.
    waker: Option<Waker>,
    /// The events emitted and not yet run: each handler entry and its
    /// payload.
    emitted: Vec<(u32, Value)>,
    listener: Option<Rc<Listener>>,
}

/// The page's side of the events: what runs them and what schedules that.
struct Listener {
    run: Box<dyn Fn(u32, Value)>,
    schedule: Box<dyn Fn()>,
}

thread_local! {
    static CROSSING: RefCell<Crossing> = RefCell::new(Crossing::default());
}

/// Pushes `value` to input slot `slot` of island `island`, replacing one
/// pushed before it that the renderer has not taken.
pub fn push(island: u32, slot: u32, value: Value) {
    let waker = CROSSING.with(|crossing| {
        let mut crossing = crossing.borrow_mut();
        match crossing
            .pushed
            .iter_mut()
            .find(|(i, s, _)| (*i, *s) == (island, slot))
        {
            Some(held) => held.2 = value,
            None => crossing.pushed.push((island, slot, value)),
        }
        crossing.waker.take()
    });
    if let Some(waker) = waker {
        waker.wake();
    }
}

/// Moves the values pushed to island `island` into `out` as `(slot, value)`,
/// ready when there were any; pending, with `cx`'s waker woken by the next
/// push, when there were none.
pub fn poll_pushed(island: u32, cx: &mut Context<'_>, out: &mut Vec<(u32, Value)>) -> Poll<()> {
    CROSSING.with(|crossing| {
        let mut crossing = crossing.borrow_mut();
        let before = out.len();
        crossing.pushed.retain(|(i, slot, value)| {
            if *i != island {
                return true;
            }
            out.push((*slot, value.clone()));
            false
        });
        if out.len() > before {
            Poll::Ready(())
        } else {
            crossing.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    })
}

/// The values the island `island` of the page of `component` mounts with:
/// what each of the input entries `entries` gives against the page's states
/// now, `Nil` for one that faults. The values pushed to it before are
/// forgotten, these being as new.
///
/// # Errors
///
/// What keeps the page from running.
pub fn mount(island: u32, component: &str, entries: &[u32]) -> Result<Vec<Value>, String> {
    let page = crate::shared(component)?;
    CROSSING.with(|crossing| crossing.borrow_mut().pushed.retain(|(i, ..)| *i != island));
    let mut page = page.borrow_mut();
    Ok(entries
        .iter()
        .map(|&entry| page.evaluate(entry, &[]).unwrap_or_default())
        .collect())
}

/// Queues the run of handler entry `entry` with `payload`, an event an island
/// emitted: the page runs it once the frame is over.
pub fn emit(entry: u32, payload: Value) {
    let listener = CROSSING.with(|crossing| {
        let mut crossing = crossing.borrow_mut();
        crossing.emitted.push((entry, payload));
        crossing
            .listener
            .clone()
            .filter(|_| crossing.emitted.len() == 1)
    });
    if let Some(listener) = listener {
        (listener.schedule)();
    }
}

/// Has the page run each event an island emits with `run`, given its handler
/// entry and payload, in a call [`deliver`] makes, which `schedule` arranges
/// for once the frame is over. The events emitted before are scheduled now.
pub fn listen(run: impl Fn(u32, Value) + 'static, schedule: impl Fn() + 'static) {
    let listener = Rc::new(Listener {
        run: Box::new(run),
        schedule: Box::new(schedule),
    });
    let pending = CROSSING.with(|crossing| {
        let mut crossing = crossing.borrow_mut();
        crossing.listener = Some(Rc::clone(&listener));
        !crossing.emitted.is_empty()
    });
    if pending {
        (listener.schedule)();
    }
}

/// Runs each event emitted since the last call, in emit order.
pub fn deliver() {
    // An event emitted while these run is scheduled for the next delivery.
    let Some((listener, emitted)) = CROSSING.with(|crossing| {
        let mut crossing = crossing.borrow_mut();
        let listener = crossing.listener.clone()?;
        Some((listener, std::mem::take(&mut crossing.emitted)))
    }) else {
        return;
    };
    for (entry, payload) in emitted {
        (listener.run)(entry, payload);
    }
}
