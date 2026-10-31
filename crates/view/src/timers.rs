//! The timers the views of a thread built from now on wait on, when the app
//! installs them ([`install_timers`]): `viso::time::sleep` in a task then
//! follows the app's frame clock instead of the process's timer thread.

use std::cell::RefCell;
use std::rc::Rc;

use viso_behavior::native::Timers;

/// What makes each view host its timers.
pub type MakeTimers = Rc<dyn Fn() -> Box<dyn Timers>>;

thread_local! {
    static INSTALLED: RefCell<Option<MakeTimers>> = const { RefCell::new(None) };
}

/// Installs `make` as what gives the views of this thread built from now on
/// their timers; `None` leaves them the process's timer thread.
pub fn install_timers(make: Option<MakeTimers>) {
    INSTALLED.with(|installed| *installed.borrow_mut() = make);
}

/// The timers of a view host built now, when some are installed.
pub(crate) fn service() -> Option<Box<dyn Timers>> {
    INSTALLED.with(|installed| installed.borrow().as_ref().map(|make| make()))
}
