//! Springs: a move that follows a mass pulled toward its target.
//!
//! A [`Spring`] moves a value as a mass on a damped spring whose rest point
//! is the target: released from where the value is, at the speed it already
//! has, it accelerates toward the target and, lightly damped, rings past it
//! before it settles. It takes no duration: its stiffness, damping and mass
//! say how long. Each part of a value moves on its own spring, solved in
//! closed form, so a move retargeted mid-flight carries its velocity into
//! the next one.

use crate::float::{exp, sin_cos, sqrt};

/// A damped spring a value moves on toward its target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spring {
    /// How hard it pulls toward its target, per unit of distance.
    pub stiffness: f32,
    /// How hard it resists its velocity.
    pub damping: f32,
    /// The mass it pulls.
    pub mass: f32,
}

impl Default for Spring {
    fn default() -> Self {
        Spring::SMOOTH
    }
}

/// The least damping ratio a spring takes: one with none would ring forever.
const LEAST_RATIO: f32 = 0.01;

/// The response half a second and the given damping ratio, mass one:
/// `ω₀ = 4π`.
const fn half_second(ratio: f32) -> Spring {
    const OMEGA: f32 = 4.0 * core::f32::consts::PI;
    Spring {
        stiffness: OMEGA * OMEGA,
        damping: 2.0 * ratio * OMEGA,
        mass: 1.0,
    }
}

impl Spring {
    /// Critically damped: arrives as fast as it can without passing its
    /// target.
    pub const SMOOTH: Spring = half_second(1.0);
    /// Damped a little under critically: a hint of overshoot.
    pub const SNAPPY: Spring = half_second(0.85);
    /// Rings visibly past its target before it settles.
    pub const BOUNCY: Spring = half_second(0.7);

    /// A spring of `stiffness`, `damping` and `mass`: each not finite or not
    /// above zero is the smooth spring's, and damping is held at a hundredth
    /// of critical or more.
    pub fn new(stiffness: f32, damping: f32, mass: f32) -> Self {
        let positive = |v: f32, or: f32| if v.is_finite() && v > 0.0 { v } else { or };
        let stiffness = positive(stiffness, Spring::SMOOTH.stiffness);
        let mass = positive(mass, Spring::SMOOTH.mass);
        let least = 2.0 * LEAST_RATIO * sqrt(stiffness * mass);
        let damping = if damping.is_finite() {
            damping.max(least)
        } else {
            least
        };
        Spring {
            stiffness,
            damping,
            mass,
        }
    }

    /// The spring of mass one that undamped would swing once in `response`
    /// seconds, damped at `ratio` of critical: `1` arrives without
    /// overshoot, less rings, more creeps.
    pub fn with_response(response: f32, ratio: f32) -> Self {
        let response = if response.is_finite() && response > 0.0 {
            response
        } else {
            0.5
        };
        let omega = 2.0 * core::f32::consts::PI / response;
        Spring::new(omega * omega, 2.0 * ratio * omega, 1.0)
    }

    /// Its undamped angular frequency `ω₀ = √(k/m)`, and its damping ratio
    /// `ζ = c / 2√(km)`, held at a hundredth or more.
    fn parameters(&self) -> (f32, f32) {
        let k = if self.stiffness > 0.0 {
            self.stiffness
        } else {
            Spring::SMOOTH.stiffness
        };
        let m = if self.mass > 0.0 { self.mass } else { 1.0 };
        let omega = sqrt(k / m);
        let ratio = (self.damping / (2.0 * sqrt(k * m))).max(LEAST_RATIO);
        (omega, if ratio.is_finite() { ratio } else { 1.0 })
    }

    /// Its damping ratio: `1` critically damped, under it ringing.
    pub fn damping_ratio(&self) -> f32 {
        self.parameters().1
    }

    /// Where a mass released `x0` from its rest point at velocity `v0`
    /// stands `t` seconds on, and its velocity then.
    pub fn at(&self, x0: f32, v0: f32, t: f32) -> (f32, f32) {
        let (omega, ratio) = self.parameters();
        let t = t.max(0.0);
        if (ratio - 1.0).abs() < 1e-3 {
            // Critically damped: `x = (x₀ + (v₀ + ω₀x₀)t)·e^(−ω₀t)`.
            let c = v0 + omega * x0;
            let decay = exp(-omega * t);
            ((x0 + c * t) * decay, (v0 - omega * c * t) * decay)
        } else if ratio < 1.0 {
            // Ringing at `ω_d = ω₀√(1 − ζ²)` inside a decaying envelope.
            let a = ratio * omega;
            let wd = omega * sqrt(1.0 - ratio * ratio);
            let b = (v0 + a * x0) / wd;
            let (s, c) = sin_cos(wd * t);
            let decay = exp(-a * t);
            (
                (x0 * c + b * s) * decay,
                (v0 * c - (x0 * wd + a * b) * s) * decay,
            )
        } else {
            // Two decaying modes, the slow one `r₁` and the fast one `r₂`.
            let root = omega * sqrt(ratio * ratio - 1.0);
            let r1 = -ratio * omega + root;
            let r2 = -ratio * omega - root;
            let c2 = (v0 - r1 * x0) / (r2 - r1);
            let c1 = x0 - c2;
            let (e1, e2) = (exp(r1 * t), exp(r2 * t));
            (c1 * e1 + c2 * e2, r1 * c1 * e1 + r2 * c2 * e2)
        }
    }

    /// Whether a mass `x` from its rest point at velocity `v` has settled
    /// within `rest`: whether its energy could carry it no further than
    /// `rest` from there. A spring loses energy as it moves, so a settled
    /// mass stays within `rest` from then on.
    pub fn settled(&self, x: f32, v: f32, rest: f32) -> bool {
        let (omega, _) = self.parameters();
        let reach = v / omega;
        x * x + reach * reach <= rest * rest
    }

    /// The progress of a move from rest at `t` seconds: `0` at the start,
    /// toward `1` at the target, past it while it rings.
    pub fn step(&self, t: f32) -> f32 {
        1.0 - self.at(1.0, 0.0, t).0
    }

    /// How many seconds a move from rest takes to settle within `rest` of
    /// a distance of one.
    pub fn settle_time(&self, rest: f32) -> f32 {
        let rest = rest.max(1e-6);
        let settled = |t: f32| {
            let (x, v) = self.at(1.0, 0.0, t);
            self.settled(x, v, rest)
        };
        let mut hi = 0.05f32;
        while !settled(hi) && hi < 600.0 {
            hi *= 2.0;
        }
        let mut lo = 0.0f32;
        for _ in 0..32 {
            let mid = (lo + hi) * 0.5;
            if settled(mid) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        hi
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same spring integrated in small steps.
    fn integrated(spring: &Spring, x0: f32, v0: f32, t: f32) -> (f32, f32) {
        let (mut x, mut v) = (x0 as f64, v0 as f64);
        let (k, c, m) = (
            spring.stiffness as f64,
            spring.damping as f64,
            spring.mass as f64,
        );
        let steps = (t as f64 / 1e-5) as usize;
        for _ in 0..steps {
            let a = (-k * x - c * v) / m;
            v += a * 1e-5;
            x += v * 1e-5;
        }
        (x as f32, v as f32)
    }

    #[test]
    fn the_closed_form_follows_the_spring_in_every_regime() {
        for spring in [
            Spring::BOUNCY,
            Spring::SMOOTH,
            Spring::new(100.0, 40.0, 1.0),
            Spring::new(300.0, 10.0, 2.0),
        ] {
            assert_eq!(spring.at(1.5, -4.0, 0.0), (1.5, -4.0));
            for t in [0.05, 0.2, 0.6] {
                let (x, v) = spring.at(1.5, -4.0, t);
                let (xi, vi) = integrated(&spring, 1.5, -4.0, t);
                assert!((x - xi).abs() < 2e-3, "{spring:?} x at {t}: {x} vs {xi}");
                assert!((v - vi).abs() < 2e-2, "{spring:?} v at {t}: {v} vs {vi}");
            }
        }
    }

    #[test]
    fn a_light_spring_rings_past_its_target_and_a_critical_one_does_not() {
        let peak = |s: Spring| {
            (0..200)
                .map(|i| s.step(i as f32 / 100.0))
                .fold(0.0, f32::max)
        };
        assert!(peak(Spring::BOUNCY) > 1.03);
        assert!(peak(Spring::SMOOTH) <= 1.0);
        assert!(peak(Spring::new(100.0, 40.0, 1.0)) <= 1.0);
        assert!(
            (Spring::with_response(0.5, 1.0).stiffness - Spring::SMOOTH.stiffness).abs() < 1e-2
        );
        assert!((Spring::SNAPPY.damping_ratio() - 0.85).abs() < 1e-5);
    }

    #[test]
    fn a_settled_spring_stays_settled() {
        for spring in [
            Spring::BOUNCY,
            Spring::SMOOTH,
            Spring::new(100.0, 40.0, 1.0),
        ] {
            let t = spring.settle_time(1e-3);
            assert!(t > 0.1 && t < 3.0, "{spring:?}: {t}");
            for i in 0..200 {
                let (x, v) = spring.at(1.0, 0.0, t + i as f32 / 50.0);
                assert!(spring.settled(x, v, 1.0001e-3), "{spring:?} at {i}");
                assert!(x.abs() <= 1.0001e-3);
            }
            let (x, v) = spring.at(1.0, 0.0, t * 0.9);
            assert!(!spring.settled(x, v, 1e-3));
        }
    }

    #[test]
    fn an_undamped_or_degenerate_spring_still_settles() {
        let spring = Spring::new(f32::NAN, 0.0, -1.0);
        assert_eq!(spring.stiffness, Spring::SMOOTH.stiffness);
        assert_eq!(spring.mass, 1.0);
        assert!((spring.damping_ratio() - LEAST_RATIO).abs() < 1e-6);
        let raw = Spring {
            stiffness: 100.0,
            damping: 0.0,
            mass: 1.0,
        };
        assert!(raw.settle_time(1e-3) < 600.0);
    }
}
