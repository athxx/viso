//! A bounded single-producer single-consumer queue the audio thread and the
//! rest of the app talk through without locks: a fixed ring of slots and two
//! counters, so a push or a pop never allocates or blocks, and a push to a
//! full queue drops the value and counts it.

use std::cell::{Cell, UnsafeCell};
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Keeps the two counters on separate cache lines, so the producer's writes
/// do not evict the consumer's.
#[repr(align(128))]
struct Padded<T>(T);

struct Ring<T> {
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
    /// The next slot to pop, counting every pop ever.
    head: Padded<AtomicUsize>,
    /// The next slot to push, counting every push ever.
    tail: Padded<AtomicUsize>,
    dropped: AtomicU64,
}

// SAFETY: a slot is written only by the one producer while it is outside
// `head..tail`, and read only by the one consumer while it is inside; the
// release store of `tail` (after the write) and the acquire load of it
// (before the read) order the two, and likewise `head` for reuse. `T: Copy`
// leaves nothing to drop in an abandoned slot.
unsafe impl<T: Copy + Send> Sync for Ring<T> {}
// SAFETY: as above; the ring owns plain `Copy` values.
unsafe impl<T: Copy + Send> Send for Ring<T> {}

/// The pushing end of a [`realtime_queue`]; one thread owns it.
pub struct RealtimeSender<T> {
    ring: Arc<Ring<T>>,
    /// Not `Sync`: pushes from two threads at once would race on `tail`.
    _one_thread: PhantomData<Cell<()>>,
}

/// The popping end of a [`realtime_queue`]; one thread owns it.
pub struct RealtimeReceiver<T> {
    ring: Arc<Ring<T>>,
    _one_thread: PhantomData<Cell<()>>,
}

/// A queue holding up to `capacity` values, rounded up to a power of two.
///
/// # Panics
///
/// If `capacity` is zero.
pub fn realtime_queue<T: Copy + Send>(capacity: usize) -> (RealtimeSender<T>, RealtimeReceiver<T>) {
    assert!(capacity > 0, "a queue holds at least one value");
    let capacity = capacity.next_power_of_two();
    let ring = Arc::new(Ring {
        slots: (0..capacity)
            .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
            .collect(),
        head: Padded(AtomicUsize::new(0)),
        tail: Padded(AtomicUsize::new(0)),
        dropped: AtomicU64::new(0),
    });
    (
        RealtimeSender {
            ring: Arc::clone(&ring),
            _one_thread: PhantomData,
        },
        RealtimeReceiver {
            ring,
            _one_thread: PhantomData,
        },
    )
}

impl<T: Copy + Send> RealtimeSender<T> {
    /// Queues `value`; false, counting it dropped, when the queue is full.
    pub fn push(&self, value: T) -> bool {
        let ring = &*self.ring;
        let tail = ring.tail.0.load(Ordering::Relaxed);
        let head = ring.head.0.load(Ordering::Acquire);
        if tail.wrapping_sub(head) == ring.slots.len() {
            ring.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let slot = &ring.slots[tail & (ring.slots.len() - 1)];
        // SAFETY: the slot is outside `head..tail`, so the consumer does not
        // read it, and this is the one producer (`Self` is not `Sync`).
        unsafe { (*slot.get()).write(value) };
        ring.tail.0.store(tail.wrapping_add(1), Ordering::Release);
        true
    }

    /// The values dropped because the queue was full.
    pub fn dropped(&self) -> u64 {
        self.ring.dropped.load(Ordering::Relaxed)
    }
}

impl<T: Copy + Send> RealtimeReceiver<T> {
    /// The oldest queued value.
    pub fn pop(&self) -> Option<T> {
        let ring = &*self.ring;
        let head = ring.head.0.load(Ordering::Relaxed);
        let tail = ring.tail.0.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        let slot = &ring.slots[head & (ring.slots.len() - 1)];
        // SAFETY: the slot is inside `head..tail`: the producer wrote it
        // before its release store of `tail`, which the acquire load above
        // saw, and does not write it again until `head` passes it.
        let value = unsafe { (*slot.get()).assume_init() };
        ring.head.0.store(head.wrapping_add(1), Ordering::Release);
        Some(value)
    }

    /// The values dropped because the queue was full.
    pub fn dropped(&self) -> u64 {
        self.ring.dropped.load(Ordering::Relaxed)
    }
}

impl<T> std::fmt::Debug for RealtimeSender<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RealtimeSender")
    }
}

impl<T> std::fmt::Debug for RealtimeReceiver<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RealtimeReceiver")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_pop_in_order_and_a_full_queue_drops() {
        let (tx, rx) = realtime_queue::<u32>(3);
        for n in 0..4 {
            assert!(tx.push(n), "rounded up to four slots");
        }
        assert!(!tx.push(4));
        assert_eq!(tx.dropped(), 1);
        assert_eq!(rx.pop(), Some(0));
        assert!(tx.push(5), "a popped slot is reused");
        let rest: Vec<u32> = std::iter::from_fn(|| rx.pop()).collect();
        assert_eq!(rest, [1, 2, 3, 5]);
        assert_eq!(rx.pop(), None);
    }

    #[test]
    fn the_ends_work_from_two_threads() {
        let (tx, rx) = realtime_queue::<u64>(64);
        let producer = std::thread::spawn(move || {
            let mut n = 0;
            while n < 100_000 {
                if tx.push(n) {
                    n += 1;
                }
            }
        });
        let mut next = 0;
        while next < 100_000 {
            if let Some(n) = rx.pop() {
                assert_eq!(n, next, "nothing lost, nothing reordered");
                next += 1;
            }
        }
        producer.join().expect("the producer finished");
    }
}
