//! The monotonic frame clock's time point.
//!
//! Native targets use [`std::time::Instant`] itself. `wasm32-unknown-unknown`
//! has no OS clock behind `std` (`Instant::now` panics there), so on the web
//! [`Instant`] is a same-shaped value read from `performance.now()` — the
//! page's monotonic, sub-millisecond clock. Every crate names time points
//! through this alias, so frame timing, timers and deadlines work unchanged on
//! every target.

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub use std::time::Instant;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use web::Instant;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod web {
    use std::ops::{Add, AddAssign, Sub, SubAssign};
    use std::time::Duration;

    use wasm_bindgen::prelude::wasm_bindgen;

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = performance, js_name = now)]
        fn performance_now() -> f64;
    }

    /// A point on the page's monotonic clock, as time since its origin.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct Instant(Duration);

    impl Instant {
        /// The current time point (`performance.now()`, milliseconds → exact
        /// nanoseconds; the clock never goes backwards).
        pub fn now() -> Self {
            let ms = performance_now().max(0.0);
            Self(Duration::from_nanos((ms * 1_000_000.0) as u64))
        }

        pub fn duration_since(&self, earlier: Self) -> Duration {
            self.saturating_duration_since(earlier)
        }

        pub fn checked_duration_since(&self, earlier: Self) -> Option<Duration> {
            self.0.checked_sub(earlier.0)
        }

        pub fn saturating_duration_since(&self, earlier: Self) -> Duration {
            self.0.saturating_sub(earlier.0)
        }

        pub fn elapsed(&self) -> Duration {
            Self::now().saturating_duration_since(*self)
        }

        pub fn checked_add(&self, d: Duration) -> Option<Self> {
            self.0.checked_add(d).map(Self)
        }

        pub fn checked_sub(&self, d: Duration) -> Option<Self> {
            self.0.checked_sub(d).map(Self)
        }
    }

    impl Add<Duration> for Instant {
        type Output = Self;
        fn add(self, d: Duration) -> Self {
            self.checked_add(d)
                .expect("overflow when adding duration to instant")
        }
    }

    impl AddAssign<Duration> for Instant {
        fn add_assign(&mut self, d: Duration) {
            *self = *self + d;
        }
    }

    impl Sub<Duration> for Instant {
        type Output = Self;
        fn sub(self, d: Duration) -> Self {
            self.checked_sub(d)
                .expect("overflow when subtracting duration from instant")
        }
    }

    impl SubAssign<Duration> for Instant {
        fn sub_assign(&mut self, d: Duration) {
            *self = *self - d;
        }
    }

    impl Sub<Instant> for Instant {
        type Output = Duration;
        fn sub(self, earlier: Instant) -> Duration {
            self.duration_since(earlier)
        }
    }
}
