//! How a number field reads, shows and steps the number its text holds: the
//! format the runtime edits a field by and an export writes one with.

use alloc::format;
use alloc::string::String;

/// The most decimal places a field shows.
const MOST_PLACES: u8 = 12;

/// How a number field reads, shows and steps its number.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumberFormat {
    /// The least number it holds; `f64::NEG_INFINITY` for no bound.
    pub min: f64,
    /// The greatest number it holds; `f64::INFINITY` for no bound.
    pub max: f64,
    /// How far one step moves it.
    pub step: f64,
    /// The decimal places it shows and rounds to.
    pub precision: u8,
}

// Every field is a number a constructor made finite or infinite, never NaN.
impl Eq for NumberFormat {}

impl Default for NumberFormat {
    fn default() -> Self {
        NumberFormat::new(f64::NEG_INFINITY, f64::INFINITY, 1.0)
    }
}

impl NumberFormat {
    /// A format holding `min..=max` (either bound infinite for none) that
    /// steps by `step` and shows as many decimal places as `step` has. A
    /// bound that is NaN is none, a range given backwards is turned round,
    /// and a step that is not a positive number is `1`.
    pub fn new(min: f64, max: f64, step: f64) -> Self {
        let min = if min.is_nan() { f64::NEG_INFINITY } else { min };
        let max = if max.is_nan() { f64::INFINITY } else { max };
        let (min, max) = if min <= max { (min, max) } else { (max, min) };
        let step = if step.is_finite() && step > 0.0 {
            step
        } else {
            1.0
        };
        NumberFormat {
            min,
            max,
            step,
            precision: places(step),
        }
    }

    /// The format showing `precision` decimal places.
    pub fn with_precision(self, precision: u8) -> Self {
        NumberFormat {
            precision: precision.min(MOST_PLACES),
            ..self
        }
    }

    /// Whether it holds only whole numbers: it shows no decimal places.
    pub fn whole(&self) -> bool {
        self.precision == 0
    }

    /// Whether it holds numbers below zero.
    pub fn signed(&self) -> bool {
        self.min < 0.0
    }

    /// The number `text` reads as, if it reads as one: an optional sign,
    /// digits and a decimal point, around spaces; a comma or a full-width
    /// digit reads as typed.
    pub fn read(&self, text: &str) -> Option<f64> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let mut ascii = String::with_capacity(text.len());
        for c in text.chars().map(number_char) {
            if !(c.is_ascii_digit() || c == '-' || c == '+' || c == '.') {
                return None;
            }
            ascii.push(c);
        }
        ascii.parse::<f64>().ok().filter(|v| v.is_finite())
    }

    /// Whether the field holds `value`: it lies within the range.
    pub fn holds(&self, value: f64) -> bool {
        value >= self.min && value <= self.max
    }

    /// `value` as the field holds it: within the range, rounded to the
    /// precision.
    pub fn settle(&self, value: f64) -> f64 {
        let scale = (0..self.precision).fold(1.0, |scale, _| scale * 10.0);
        let rounded = round(value * scale) / scale;
        let rounded = if rounded.is_finite() { rounded } else { value };
        // Rounding may carry past a bound that is not on the precision.
        let held = rounded.clamp(self.min, self.max);
        if held == 0.0 { 0.0 } else { held }
    }

    /// `value` moved by `steps` steps, as the field holds it.
    pub fn stepped(&self, value: f64, steps: f64) -> f64 {
        self.settle(value + steps * self.step)
    }

    /// The text showing `value` at the precision.
    pub fn show(&self, value: f64) -> String {
        let value = if value == 0.0 { 0.0 } else { value };
        format!("{:.*}", usize::from(self.precision), value)
    }

    /// The number the field steps from when its text reads as none: zero
    /// within the range.
    pub fn origin(&self) -> f64 {
        0f64.clamp(self.min, self.max)
    }
}

/// `x` rounded to the nearest whole number, halves away from zero; one too
/// large to carry a fraction (or NaN) is left as it is.
fn round(x: f64) -> f64 {
    if x.is_nan() || x.abs() >= 4.5e15 {
        return x;
    }
    let whole = x as i64 as f64;
    let fraction = x - whole;
    if fraction >= 0.5 {
        whole + 1.0
    } else if fraction <= -0.5 {
        whole - 1.0
    } else {
        whole
    }
}

/// `c` as a number field reads it: a full-width digit, sign or separator as
/// its ASCII one, and a comma, ideographic full stop or minus sign as the
/// separator or sign it stands for.
pub fn number_char(c: char) -> char {
    match c {
        '\u{FF10}'..='\u{FF19}' => char::from(b'0' + (c as u32 - 0xFF10) as u8),
        '\u{FF0D}' | '\u{2212}' => '-',
        '\u{FF0E}' | '\u{FF0C}' | '\u{3002}' | ',' => '.',
        c => c,
    }
}

/// The decimal places `step` is written with, at most [`MOST_PLACES`].
fn places(step: f64) -> u8 {
    let text = format!("{step}");
    let places = text
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len());
    (places as u8).min(MOST_PLACES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_format_shows_as_many_places_as_its_step() {
        assert_eq!(NumberFormat::new(0.0, 1.0, 0.25).precision, 2);
        assert_eq!(NumberFormat::new(0.0, 10.0, 1.0).precision, 0);
        let f = NumberFormat::new(10.0, 0.0, -3.0);
        assert_eq!((f.min, f.max, f.step), (0.0, 10.0, 1.0), "turned round");
        assert!(f.whole() && !f.signed());
        assert!(NumberFormat::default().signed());
        assert!(!NumberFormat::new(0.0, 1.0, 0.1).whole());
    }

    #[test]
    fn a_format_reads_numbers_only() {
        let f = NumberFormat::default();
        assert_eq!(f.read(" -1.5 "), Some(-1.5));
        assert_eq!(f.read("1,5"), Some(1.5));
        assert_eq!(f.read("\u{FF13}"), Some(3.0));
        assert_eq!(f.read("1."), Some(1.0));
        assert_eq!(f.read("1e3"), None);
        assert_eq!(f.read("inf"), None);
        assert_eq!(f.read("-"), None);
        assert_eq!(f.read(""), None);
    }

    #[test]
    fn a_format_settles_within_its_range_at_its_precision() {
        let f = NumberFormat::new(0.0, 1.0, 0.1);
        assert_eq!(f.settle(0.46), 0.5);
        assert_eq!(f.settle(1.7), 1.0);
        assert_eq!(f.stepped(0.3, 1.0), 0.4);
        assert_eq!(f.stepped(0.95, 1.0), 1.0);
        assert_eq!(f.show(f.settle(-0.01)), "0.0", "no negative zero");
        assert_eq!(NumberFormat::new(5.0, 9.0, 1.0).origin(), 5.0);
    }
}
