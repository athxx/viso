//! Scales: where values fall in a chart's plot, and the round ticks along
//! an axis.

/// A run of evenly spaced round values from `lo` to `hi`, `step` apart.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Ticks {
    pub lo: f64,
    pub hi: f64,
    pub step: f64,
    pub values: Vec<f64>,
}

impl Ticks {
    /// Where `v` falls between `lo` (`0`) and `hi` (`1`).
    pub fn unit(&self, v: f64) -> f32 {
        if self.hi > self.lo {
            ((v - self.lo) / (self.hi - self.lo)) as f32
        } else {
            0.5
        }
    }
}

/// A round step about `span / count` long: 1, 2 or 5 of a power of ten.
pub(crate) fn round_step(span: f64, count: usize) -> f64 {
    let raw = span / count.max(1) as f64;
    if !(raw.is_finite() && raw > 0.0) {
        return 1.0;
    }
    let power = 10f64.powf(raw.log10().floor());
    let norm = raw / power;
    let mult = if norm >= 50f64.sqrt() {
        10.0
    } else if norm >= 10f64.sqrt() {
        5.0
    } else if norm >= 2f64.sqrt() {
        2.0
    } else {
        1.0
    };
    mult * power
}

/// The values `lo..=hi` round out to, about `count` steps: the domain
/// widened to the round values round it.
pub(crate) fn nice(lo: f64, hi: f64, count: usize) -> Ticks {
    let (lo, hi) = spread(lo, hi);
    let step = round_step(hi - lo, count);
    let first = (lo / step).floor();
    let last = (hi / step).ceil();
    let values = (0..=(last - first) as i64)
        .map(|k| clean((first + k as f64) * step))
        .collect();
    Ticks {
        lo: clean(first * step),
        hi: clean(last * step),
        step,
        values,
    }
}

/// The round values within `lo..=hi`, about `count` steps, the domain kept
/// as it is: a line across its whole run of `x`.
pub(crate) fn within(lo: f64, hi: f64, count: usize) -> Ticks {
    let (lo, hi) = spread(lo, hi);
    let step = round_step(hi - lo, count);
    let first = (lo / step).ceil();
    let last = (hi / step).floor();
    let values = (0..=((last - first).max(-1.0)) as i64)
        .map(|k| clean((first + k as f64) * step))
        .collect();
    Ticks {
        lo,
        hi,
        step,
        values,
    }
}

/// A domain with room in it: a single value widened around itself.
fn spread(lo: f64, hi: f64) -> (f64, f64) {
    if !(lo.is_finite() && hi.is_finite()) {
        return (0.0, 1.0);
    }
    if hi > lo {
        return (lo, hi);
    }
    let pad = if lo == 0.0 { 1.0 } else { lo.abs() * 0.1 };
    (lo - pad, hi + pad)
}

/// `v` without the float dust a product of steps leaves: `0.30000000000000004`
/// is `0.3`.
fn clean(v: f64) -> f64 {
    let r = (v * 1e9).round() / 1e9;
    if r == 0.0 { 0.0 } else { r }
}

/// How many decimals a value on a grid of `step` needs.
fn decimals(step: f64) -> usize {
    let mut d = 0;
    let mut s = step.abs();
    while d < 6 && (s - s.round()).abs() > 1e-6 * s.max(1.0) {
        s *= 10.0;
        d += 1;
    }
    d
}

/// Write `v` as a tick on a grid of `step` labels it: as many decimals as
/// the step needs, and from a thousand up compact (`12K`, `3.5M`, `2B`).
pub fn format_tick(v: f64, step: f64) -> String {
    let big = v.abs().max(step.abs());
    let (unit, suffix) = if big >= 1e9 {
        (1e9, "B")
    } else if big >= 1e6 {
        (1e6, "M")
    } else if big >= 1e3 {
        (1e3, "K")
    } else {
        (1.0, "")
    };
    let d = decimals(step / unit);
    let text = format!("{:.*}{suffix}", d, v / unit);
    if text.starts_with('-') && text[1..].trim_start_matches(['0', '.']).is_empty() {
        text[1..].to_owned()
    } else {
        text
    }
}

/// Write `v` as a value a tooltip reads: up to three significant decimals,
/// grouped by thousands.
pub fn format_value(v: f64) -> String {
    if !v.is_finite() {
        return "–".to_owned();
    }
    let d = if v.abs() >= 100.0 {
        0
    } else if v.abs() >= 1.0 {
        2
    } else {
        3
    };
    let text = format!("{:.*}", d, v);
    let text = if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text
    };
    let (sign, rest) = text
        .strip_prefix('-')
        .map_or(("", text.as_str()), |r| ("-", r));
    let (int, frac) = rest.split_once('.').map_or((rest, ""), |(i, f)| (i, f));
    let mut grouped = String::with_capacity(int.len() + int.len() / 3);
    for (k, c) in int.chars().enumerate() {
        if k > 0 && (int.len() - k) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    match frac {
        "" => format!("{sign}{grouped}"),
        f => format!("{sign}{grouped}.{f}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_domain_rounds_out_to_round_ticks() {
        let t = nice(3.0, 97.0, 5);
        assert_eq!((t.lo, t.hi, t.step), (0.0, 100.0, 20.0));
        assert_eq!(t.values, [0.0, 20.0, 40.0, 60.0, 80.0, 100.0]);
        let t = nice(0.12, 0.97, 4);
        assert_eq!(t.step, 0.2);
        assert_eq!(t.values, [0.0, 0.2, 0.4, 0.6, 0.8, 1.0]);
        let t = nice(-12.0, 30.0, 5);
        assert_eq!((t.lo, t.hi), (-20.0, 30.0));
        let t = nice(5.0, 5.0, 5);
        assert!(t.lo < 5.0 && t.hi > 5.0, "a lone value gets room");
    }

    #[test]
    fn ticks_within_keep_the_domain() {
        let t = within(1.0, 12.0, 5);
        assert_eq!((t.lo, t.hi), (1.0, 12.0));
        assert_eq!(t.values, [2.0, 4.0, 6.0, 8.0, 10.0, 12.0]);
        assert_eq!(t.unit(6.5), 0.5);
    }

    #[test]
    fn ticks_and_values_read_cleanly() {
        assert_eq!(format_tick(0.4, 0.2), "0.4");
        assert_eq!(format_tick(25.0, 0.25), "25.00");
        assert_eq!(format_tick(2000.0, 1000.0), "2K");
        assert_eq!(format_tick(2500.0, 500.0), "2.5K");
        assert_eq!(format_tick(-3e6, 1e6), "-3M");
        assert_eq!(format_tick(-0.0, 1.0), "0");
        assert_eq!(format_value(1234567.0), "1,234,567");
        assert_eq!(format_value(5.4321), "5.43");
        assert_eq!(format_value(-0.0421), "-0.042");
        assert_eq!(format_value(12.5), "12.5");
    }
}
