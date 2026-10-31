//! The sleeps the views' tasks wait on (`viso::time::sleep`), timed by the
//! frame clock: each is due its duration after the frame that asks for it,
//! wakes on the first frame at or past that instant, and keeps the loop
//! waiting for it as a UI timer does, so a headless drive's fixed-step clock
//! times it as it times everything else.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use viso_behavior::native::Timers;
use viso_runtime::Instant;

/// The sleeps of a driver's views, and the instant of its last frame.
#[derive(Default)]
pub(crate) struct Sleeps {
    now: Cell<Option<Instant>>,
    alarms: RefCell<Vec<Weak<Alarm>>>,
}

/// One sleep: when it is due, whether it has rung, and who waits on it.
struct Alarm {
    due: Instant,
    rung: Cell<bool>,
    waker: RefCell<Option<Waker>>,
}

impl Sleeps {
    /// What makes each view host a handle on these sleeps.
    pub(crate) fn maker(self: &Rc<Self>) -> viso_view::MakeTimers {
        let sleeps = Rc::clone(self);
        Rc::new(move || Box::new(Handle(Rc::clone(&sleeps))) as Box<dyn Timers>)
    }

    /// Rings every sleep due by `now`, the frame's instant, waking its task.
    pub(crate) fn ring(&self, now: Instant) {
        self.now.set(Some(now));
        let mut alarms = self.alarms.borrow_mut();
        if alarms.is_empty() {
            return;
        }
        alarms.retain(|alarm| {
            let Some(alarm) = alarm.upgrade() else {
                return false;
            };
            if alarm.due > now {
                return true;
            }
            alarm.rung.set(true);
            if let Some(waker) = alarm.waker.borrow_mut().take() {
                waker.wake();
            }
            false
        });
    }

    /// The instant the soonest sleep is due, if one waits.
    pub(crate) fn earliest(&self) -> Option<Instant> {
        self.alarms
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .map(|alarm| alarm.due)
            .min()
    }
}

/// A view host's handle on its driver's sleeps.
struct Handle(Rc<Sleeps>);

impl Timers for Handle {
    fn sleep(&mut self, duration: Duration) -> Pin<Box<dyn Future<Output = ()>>> {
        let now = self.0.now.get().unwrap_or_else(Instant::now);
        let alarm = Rc::new(Alarm {
            due: now + duration,
            rung: Cell::new(duration.is_zero()),
            waker: RefCell::new(None),
        });
        if !duration.is_zero() {
            self.0.alarms.borrow_mut().push(Rc::downgrade(&alarm));
        }
        Box::pin(Sleep(alarm))
    }
}

/// A task's wait on one sleep.
struct Sleep(Rc<Alarm>);

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0.rung.get() {
            return Poll::Ready(());
        }
        *self.0.waker.borrow_mut() = Some(cx.waker().clone());
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Wake;

    struct Count(AtomicUsize);

    impl Wake for Count {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn a_sleep_rings_on_the_first_frame_past_its_duration() {
        let sleeps = Rc::new(Sleeps::default());
        let start = Instant::now();
        sleeps.ring(start);
        let mut timers = (sleeps.maker())();
        let mut sleep = timers.sleep(Duration::from_millis(100));
        let woken = Arc::new(Count(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&woken));
        let mut cx = Context::from_waker(&waker);
        assert!(sleep.as_mut().poll(&mut cx).is_pending());
        assert_eq!(sleeps.earliest(), Some(start + Duration::from_millis(100)));
        sleeps.ring(start + Duration::from_millis(99));
        assert_eq!(woken.0.load(Ordering::SeqCst), 0, "not due yet");
        sleeps.ring(start + Duration::from_millis(116));
        assert_eq!(woken.0.load(Ordering::SeqCst), 1);
        assert!(sleep.as_mut().poll(&mut cx).is_ready());
        assert_eq!(sleeps.earliest(), None, "nothing keeps the loop waiting");
    }

    #[test]
    fn a_dropped_sleep_keeps_nothing_waiting() {
        let sleeps = Rc::new(Sleeps::default());
        sleeps.ring(Instant::now());
        let mut timers = (sleeps.maker())();
        drop(timers.sleep(Duration::from_secs(5)));
        assert_eq!(sleeps.earliest(), None);
        assert!(
            timers
                .sleep(Duration::ZERO)
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_ready()
        );
    }
}
