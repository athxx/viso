//! Easing curves: how a move's progress follows its time.
//!
//! An [`Easing`] maps a move's normalized time `t ∈ [0, 1]` onto its
//! progress: `0` at the start and `1` at the end. Between them a curve may
//! leave `[0, 1]` — `Back` overshoots and `Elastic` rings — so a value moved
//! by one passes its target and comes back; what cannot overshoot (an
//! opacity, a color) clamps where it is written.
//!
//! The curves are Robert Penner's ten families, each accelerating from rest
//! ([`Easing::In`]), decelerating to rest ([`Easing::Out`]) or both
//! ([`Easing::InOut`]), and the CSS [`CubicBezier`]. `EASE_IN`, `EASE_OUT`
//! and `EASE_IN_OUT` are the cubic family, the standard picks.

use crate::float::{powf, sin_cos, sqrt};

/// A family of easing curves, each in three shapes (see [`Easing`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Curve {
    /// `t²`.
    Quad,
    /// `t³`.
    Cubic,
    /// `t⁴`.
    Quart,
    /// `t⁵`.
    Quint,
    /// A quarter of a sine wave.
    Sine,
    /// `2^(10t − 10)`.
    Expo,
    /// A quarter of a circle.
    Circ,
    /// Pulls back past the start before it moves (overshoots its end
    /// decelerating).
    Back,
    /// Rings about the start like a plucked spring, about its end
    /// decelerating.
    Elastic,
    /// Bounces off the start like a dropped ball, off its end decelerating.
    Bounce,
}

impl Curve {
    /// Every family, in order.
    pub const ALL: [Curve; 10] = [
        Curve::Quad,
        Curve::Cubic,
        Curve::Quart,
        Curve::Quint,
        Curve::Sine,
        Curve::Expo,
        Curve::Circ,
        Curve::Back,
        Curve::Elastic,
        Curve::Bounce,
    ];

    /// The family accelerating from rest at time `t ∈ [0, 1]`.
    fn ease_in(self, t: f32) -> f32 {
        match self {
            Curve::Quad => t * t,
            Curve::Cubic => t * t * t,
            Curve::Quart => t * t * t * t,
            Curve::Quint => t * t * t * t * t,
            Curve::Sine => 1.0 - sin_cos(t * core::f32::consts::FRAC_PI_2).1,
            Curve::Expo => {
                if t <= 0.0 {
                    0.0
                } else {
                    powf(2.0, 10.0 * t - 10.0)
                }
            }
            Curve::Circ => 1.0 - sqrt((1.0 - t * t).max(0.0)),
            Curve::Back => BACK_C3 * t * t * t - BACK_C1 * t * t,
            Curve::Elastic => {
                if t <= 0.0 || t >= 1.0 {
                    t.clamp(0.0, 1.0)
                } else {
                    let ring = sin_cos((t * 10.0 - 10.75) * ELASTIC_C4).0;
                    -powf(2.0, 10.0 * t - 10.0) * ring
                }
            }
            Curve::Bounce => 1.0 - bounce_out(1.0 - t),
        }
    }

    /// The family accelerating then decelerating at time `t ∈ [0, 1]`.
    fn ease_in_out(self, t: f32) -> f32 {
        match self {
            // Penner's back and elastic in-out take their own constants.
            Curve::Back => {
                let u = 2.0 * t;
                if t < 0.5 {
                    u * u * ((BACK_C2 + 1.0) * u - BACK_C2) / 2.0
                } else {
                    let u = u - 2.0;
                    (u * u * ((BACK_C2 + 1.0) * u + BACK_C2) + 2.0) / 2.0
                }
            }
            Curve::Elastic => {
                if t <= 0.0 || t >= 1.0 {
                    return t.clamp(0.0, 1.0);
                }
                let ring = sin_cos((20.0 * t - 11.125) * ELASTIC_C5).0;
                if t < 0.5 {
                    -(powf(2.0, 20.0 * t - 10.0) * ring) / 2.0
                } else {
                    powf(2.0, -20.0 * t + 10.0) * ring / 2.0 + 1.0
                }
            }
            _ => {
                if t < 0.5 {
                    self.ease_in(2.0 * t) / 2.0
                } else {
                    1.0 - self.ease_in(2.0 - 2.0 * t) / 2.0
                }
            }
        }
    }
}

const BACK_C1: f32 = 1.70158;
const BACK_C2: f32 = BACK_C1 * 1.525;
const BACK_C3: f32 = BACK_C1 + 1.0;
const ELASTIC_C4: f32 = 2.0 * core::f32::consts::PI / 3.0;
const ELASTIC_C5: f32 = 2.0 * core::f32::consts::PI / 4.5;

/// A ball dropped onto its end at time `t`, bouncing thrice.
fn bounce_out(t: f32) -> f32 {
    const N: f32 = 7.5625;
    const D: f32 = 2.75;
    if t < 1.0 / D {
        N * t * t
    } else if t < 2.0 / D {
        let t = t - 1.5 / D;
        N * t * t + 0.75
    } else if t < 2.5 / D {
        let t = t - 2.25 / D;
        N * t * t + 0.9375
    } else {
        let t = t - 2.625 / D;
        N * t * t + 0.984375
    }
}

/// A CSS `cubic-bezier(x1, y1, x2, y2)`: the curve from `(0, 0)` to `(1, 1)`
/// pulled toward its two control points. Time runs along x, so each `x` is
/// held in `[0, 1]`; a `y` outside it overshoots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CubicBezier {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

impl CubicBezier {
    /// The curve through control points `(x1, y1)` and `(x2, y2)`, each `x`
    /// held in `[0, 1]`.
    pub fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        let x = |v: f32| {
            if v.is_finite() {
                v.clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        let y = |v: f32| if v.is_finite() { v } else { 0.0 };
        CubicBezier {
            x1: x(x1),
            y1: y(y1),
            x2: x(x2),
            y2: y(y2),
        }
    }

    /// Its progress at time `t ∈ [0, 1]`: the curve's y where its x is `t`.
    pub fn at(&self, t: f32) -> f32 {
        // The polynomial coefficients of each coordinate in the curve's
        // parameter `s`: `((a·s + b)·s + c)·s`.
        let coefficients = |p1: f32, p2: f32| {
            let c = 3.0 * p1;
            let b = 3.0 * (p2 - p1) - c;
            (1.0 - c - b, b, c)
        };
        let (ax, bx, cx) = coefficients(self.x1, self.x2);
        let (ay, by, cy) = coefficients(self.y1, self.y2);
        let x_at = |s: f32| ((ax * s + bx) * s + cx) * s;
        // Newton's method from `t`, which converges for all but the
        // steepest curves; bisection where it does not.
        let mut s = t;
        let mut solved = false;
        for _ in 0..8 {
            let error = x_at(s) - t;
            if error.abs() < 1e-6 {
                solved = true;
                break;
            }
            let slope = (3.0 * ax * s + 2.0 * bx) * s + cx;
            if slope.abs() < 1e-6 {
                break;
            }
            s -= error / slope;
        }
        if !solved || !(0.0..=1.0).contains(&s) {
            let (mut lo, mut hi) = (0.0f32, 1.0f32);
            s = t;
            for _ in 0..32 {
                let x = x_at(s);
                if (x - t).abs() < 1e-6 {
                    break;
                }
                if x < t {
                    lo = s;
                } else {
                    hi = s;
                }
                s = (lo + hi) * 0.5;
            }
        }
        ((ay * s + by) * s + cy) * s
    }
}

/// How a move's progress follows its time (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Easing {
    /// Constant rate.
    Linear,
    /// The family accelerating from rest.
    In(Curve),
    /// The family decelerating to rest: what arrives and settles.
    Out(Curve),
    /// The family accelerating, then decelerating: slow at both ends.
    InOut(Curve),
    /// A CSS cubic Bézier.
    CubicBezier(CubicBezier),
}

impl Default for Easing {
    fn default() -> Self {
        Easing::EASE_OUT
    }
}

impl Easing {
    /// Accelerate from rest: the cubic family.
    pub const EASE_IN: Easing = Easing::In(Curve::Cubic);
    /// Decelerate to rest: the cubic family, the natural choice for what
    /// arrives.
    pub const EASE_OUT: Easing = Easing::Out(Curve::Cubic);
    /// Accelerate, then decelerate: the cubic family.
    pub const EASE_IN_OUT: Easing = Easing::InOut(Curve::Cubic);

    /// How many curves have a name (see [`named`](Self::named)).
    pub const NAMED: u32 = 4 + 3 * Curve::ALL.len() as u32;

    /// The curve named `index`th: `Linear`, `EASE_IN`, `EASE_OUT`,
    /// `EASE_IN_OUT`, then each family of [`Curve::ALL`] in, out and in-out.
    /// A view's `Easing` declares its variants in this order.
    pub fn named(index: u32) -> Option<Easing> {
        Some(match index {
            0 => Easing::Linear,
            1 => Easing::EASE_IN,
            2 => Easing::EASE_OUT,
            3 => Easing::EASE_IN_OUT,
            _ => {
                let at = index.checked_sub(4)? as usize;
                let curve = *Curve::ALL.get(at / 3)?;
                match at % 3 {
                    0 => Easing::In(curve),
                    1 => Easing::Out(curve),
                    _ => Easing::InOut(curve),
                }
            }
        })
    }

    /// Its progress at time `t`, held in `[0, 1]`: `0` at `0` and `1` at
    /// `1`, and between them possibly outside `[0, 1]`.
    #[inline]
    pub fn apply(&self, t: f32) -> f32 {
        let t = if t.is_nan() { 1.0 } else { t.clamp(0.0, 1.0) };
        match *self {
            Easing::Linear => t,
            Easing::In(curve) => curve.ease_in(t),
            Easing::Out(curve) => 1.0 - curve.ease_in(1.0 - t),
            Easing::InOut(curve) => curve.ease_in_out(t),
            Easing::CubicBezier(bezier) => {
                if t <= 0.0 || t >= 1.0 {
                    t
                } else {
                    bezier.at(t)
                }
            }
        }
    }

    /// Whether its progress can leave `[0, 1]`.
    pub fn overshoots(&self) -> bool {
        match *self {
            Easing::In(curve) | Easing::Out(curve) | Easing::InOut(curve) => {
                matches!(curve, Curve::Back | Curve::Elastic)
            }
            Easing::CubicBezier(b) => !(0.0..=1.0).contains(&b.y1) || !(0.0..=1.0).contains(&b.y2),
            Easing::Linear => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every() -> impl Iterator<Item = Easing> {
        (0..Easing::NAMED).map(|i| Easing::named(i).unwrap())
    }

    #[test]
    fn every_curve_starts_at_zero_and_ends_at_one() {
        for easing in every().chain([Easing::CubicBezier(CubicBezier::new(0.3, -0.5, 0.7, 1.5))]) {
            assert_eq!(easing.apply(0.0), 0.0, "{easing:?}");
            assert!((easing.apply(1.0) - 1.0).abs() < 1e-6, "{easing:?}");
            assert_eq!(easing.apply(-1.0), easing.apply(0.0));
            assert_eq!(easing.apply(2.0), easing.apply(1.0));
        }
        assert_eq!(Easing::named(Easing::NAMED), None);
    }

    #[test]
    fn the_curves_follow_penner_s_formulas() {
        let near = |a: f32, b: f32| (a - b).abs() < 1e-4;
        assert!(near(Easing::In(Curve::Quad).apply(0.5), 0.25));
        assert!(near(Easing::Out(Curve::Quad).apply(0.5), 0.75));
        assert!(near(Easing::InOut(Curve::Cubic).apply(0.25), 0.0625));
        assert!(near(
            Easing::InOut(Curve::Quint).apply(0.75),
            1.0 - 0.015625
        ));
        assert!(near(Easing::In(Curve::Sine).apply(0.5), 1.0 - 0.70710677));
        assert!(near(
            Easing::Out(Curve::Expo).apply(0.5),
            1.0 - 2f32.powf(-5.0)
        ));
        assert!(near(Easing::In(Curve::Circ).apply(0.6), 0.2));
        assert!(near(Easing::Out(Curve::Bounce).apply(0.5), 0.765625));
        assert!(near(Easing::InOut(Curve::Bounce).apply(0.5), 0.5));
        // Back pulls back about 10% before it moves.
        let dip = (1..100)
            .map(|i| Easing::In(Curve::Back).apply(i as f32 / 100.0))
            .fold(f32::INFINITY, f32::min);
        assert!((dip + 0.0995).abs() < 2e-3, "{dip}");
        assert!(
            Easing::Out(Curve::Elastic).apply(0.1) > 1.0,
            "rings past its end"
        );
        assert!(Easing::InOut(Curve::Back).apply(0.9) > 1.0);
    }

    #[test]
    fn the_standard_picks_are_the_cubic_family() {
        assert_eq!(Easing::default(), Easing::Out(Curve::Cubic));
        assert_eq!(Easing::named(1), Some(Easing::EASE_IN));
        assert_eq!(Easing::named(4), Some(Easing::In(Curve::Quad)));
        assert_eq!(Easing::named(5), Some(Easing::Out(Curve::Quad)));
        assert_eq!(Easing::named(33), Some(Easing::InOut(Curve::Bounce)));
    }

    #[test]
    fn a_cubic_bezier_solves_for_its_time() {
        // CSS `ease`.
        let ease = CubicBezier::new(0.25, 0.1, 0.25, 1.0);
        assert!((ease.at(0.5) - 0.8024).abs() < 1e-3, "{}", ease.at(0.5));
        // The straight line is linear.
        let line = CubicBezier::new(0.0, 0.0, 1.0, 1.0);
        for i in 0..=10 {
            let t = i as f32 / 10.0;
            assert!((line.at(t) - t).abs() < 1e-4);
        }
        // A steep curve still lands on its time.
        let steep = CubicBezier::new(1.0, 0.0, 1.0, 0.0);
        assert!(steep.at(0.5) < 0.2);
        assert!(Easing::CubicBezier(CubicBezier::new(0.3, -0.5, 0.7, 1.5)).overshoots());
        assert_eq!(CubicBezier::new(2.0, 0.0, -1.0, f32::NAN).x1, 1.0);
    }

    #[test]
    fn overshooting_curves_say_so() {
        assert!(Easing::Out(Curve::Back).overshoots());
        assert!(!Easing::Out(Curve::Bounce).overshoots());
        for easing in every().filter(|e| !e.overshoots()) {
            for i in 0..=100 {
                let p = easing.apply(i as f32 / 100.0);
                assert!((-1e-5..=1.0 + 1e-5).contains(&p), "{easing:?} at {i}: {p}");
            }
        }
    }
}
