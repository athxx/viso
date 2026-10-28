//! Where each state cell a build allocates comes from, for the development
//! session that carries the cells across a Rust warm restart.
//!
//! A cell has no name of its own in Rust: [`BuildCx::state`] keys it by its
//! call site, file, line and column, and a `component!` or `view!` state by
//! the compiler's symbol. The records queue on the UI thread until the
//! session [takes](take_allocations) them with the window's build. The key is
//! the same in the processes before and after a restart while the call site
//! does not move; a cell whose call site moved gets another key and starts
//! from its initializer, never from another cell's value.
//!
//! [`BuildCx::state`]: crate::BuildCx::state

use std::cell::RefCell;
use std::panic::Location;

use crate::state::{StateId, StateKey};

thread_local! {
    static ALLOCATED: RefCell<Vec<(StateId, StateKey)>> = const { RefCell::new(Vec::new()) };
    static SITES: RefCell<Vec<(StateId, &'static Location<'static>)>> =
        const { RefCell::new(Vec::new()) };
}

/// Queues the Rust call site that allocated the cell `id`, which a report
/// about the cell names it by.
pub(crate) fn allocated_at(id: StateId, site: &'static Location<'static>) {
    SITES.with(|sites| sites.borrow_mut().push((id, site)));
}

/// Moves the call site of every cell allocated in Rust on this thread since
/// the last call into `out`, in allocation order.
pub fn take_sites(out: &mut Vec<(StateId, &'static Location<'static>)>) {
    SITES.with(|sites| out.append(&mut sites.borrow_mut()));
}

/// Queues the cell `id` under `key`.
pub(crate) fn allocated(id: StateId, key: StateKey) {
    ALLOCATED.with(|cells| cells.borrow_mut().push((id, key)));
}

/// The key of a cell allocated at `site`.
pub(crate) fn site_key(site: &Location<'_>) -> StateKey {
    let mut hi = Fnv::new(0x6c62_272e_07bb_0142);
    let mut lo = Fnv::new(0xcbf2_9ce4_8422_2325);
    for hash in [&mut hi, &mut lo] {
        hash.write(b"viso.state.site");
        hash.write(site.file().as_bytes());
        hash.write(&site.line().to_le_bytes());
        hash.write(&site.column().to_le_bytes());
    }
    StateKey::from_parts(hi.0, lo.0)
}

/// Moves every cell allocated on this thread since the last call into
/// `out`, in allocation order.
pub fn take_allocations(out: &mut Vec<(StateId, StateKey)>) {
    ALLOCATED.with(|cells| out.append(&mut cells.borrow_mut()));
}

/// Drops the cells allocated since the last take: those of no window's
/// build.
pub fn clear_allocations() {
    ALLOCATED.with(|cells| cells.borrow_mut().clear());
    SITES.with(|sites| sites.borrow_mut().clear());
}

/// FNV-1a, 64-bit: the same in every process and build.
struct Fnv(u64);

impl Fnv {
    fn new(basis: u64) -> Self {
        Fnv(basis)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[track_caller]
    fn here() -> &'static Location<'static> {
        Location::caller()
    }

    #[test]
    fn a_call_site_keys_the_same_each_time_and_another_differently() {
        let (a, b) = (here(), here());
        assert_eq!(site_key(a), site_key(a));
        assert_ne!(site_key(a), site_key(b));
    }
}
