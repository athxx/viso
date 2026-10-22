//! CSS for an export: the theme as custom properties, a rule per
//! distinct set of declarations, and the values a view's lengths, colors and
//! timings take in CSS.

use std::collections::HashMap;
use std::fmt::Write as _;

use viso_behavior::Value;

use viso_math::{CubicBezier, Easing, Spring};

use crate::ir::ui_ir::{LengthIr, TermsIr};

/// The `Theme` record a view reads as `theme`, as the prelude declares it:
/// each group and its fields, in declaration order.
pub(super) const THEME: &[(&str, &[(&str, Token)])] = &[
    (
        "colors",
        &[
            ("background", Token::Color),
            ("foreground", Token::Color),
            ("surface", Token::Color),
            ("on_surface", Token::Color),
            ("primary", Token::Color),
            ("on_primary", Token::Color),
            ("primary_hover", Token::Color),
            ("accent", Token::Color),
            ("muted", Token::Color),
            ("outline", Token::Color),
            ("error", Token::Color),
            ("on_error", Token::Color),
            ("focus_ring", Token::Color),
            ("scrim", Token::Color),
        ],
    ),
    (
        "typography",
        &[
            ("base_size", Token::Sp),
            ("caption_size", Token::Em),
            ("title_size", Token::Em),
            ("headline_size", Token::Em),
            ("family", Token::Family),
        ],
    ),
    (
        "spacing",
        &[
            ("xsmall", Token::Dp),
            ("small", Token::Dp),
            ("medium", Token::Dp),
            ("large", Token::Dp),
            ("xlarge", Token::Dp),
        ],
    ),
    (
        "radius",
        &[
            ("small", Token::Dp),
            ("medium", Token::Dp),
            ("large", Token::Dp),
        ],
    ),
    (
        "elevation",
        &[
            ("low", Token::Shadow),
            ("medium", Token::Shadow),
            ("high", Token::Shadow),
        ],
    ),
    (
        "motion",
        &[
            ("short", Token::Duration),
            ("medium", Token::Duration),
            ("long", Token::Duration),
            ("standard", Token::Easing),
            ("emphasized", Token::Easing),
        ],
    ),
];

/// What a theme field holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Token {
    Color,
    Dp,
    Sp,
    Em,
    Shadow,
    Duration,
    Easing,
    Family,
}

/// The custom property of the theme field at `path` (`--colors-primary`),
/// and what it holds; `None` for a path the schema does not have.
pub(super) fn theme_var(path: &[u32]) -> Option<(String, Token)> {
    let [group, field] = *path else {
        return None;
    };
    let (group, fields) = THEME.get(group as usize)?;
    let (field, token) = fields.get(field as usize)?;
    Some((format!("--{group}-{}", field.replace('_', "-")), *token))
}

/// The `:root` rule declaring every theme field of `theme`, a `Theme` value.
pub(super) fn theme_rule(theme: &Value) -> String {
    let mut out = String::from(":root {\n");
    for (at, (group, fields)) in THEME.iter().enumerate() {
        let Some(values) = field(theme, at) else {
            continue;
        };
        for (index, (name, token)) in fields.iter().enumerate() {
            let Some(value) = field(values, index).and_then(|v| token_value(v, *token)) else {
                continue;
            };
            let _ = writeln!(out, "  --{group}-{}: {value};", name.replace('_', "-"));
        }
    }
    out.push_str("}\n");
    out
}

fn field(value: &Value, at: usize) -> Option<&Value> {
    match value {
        Value::Agg(agg) => agg.fields.get(at),
        _ => None,
    }
}

/// A theme field's value as CSS.
fn token_value(value: &Value, token: Token) -> Option<String> {
    Some(match token {
        Token::Color => color(value.as_int()? as u32),
        Token::Dp => px(value.as_float()?),
        Token::Sp => sp(value.as_float()?),
        Token::Em => format!("{}em", number(value.as_float()?)),
        Token::Duration => seconds(value.as_float()?),
        Token::Easing => easing(value),
        Token::Family => family(value),
        Token::Shadow => shadow(value)?,
    })
}

/// A `Shadow` as a `box-shadow`, `None` for a value that is not one.
pub(super) fn shadow(value: &Value) -> Option<String> {
    let Value::Agg(agg) = value else {
        return None;
    };
    let [x, y, blur, spread, tint, inset] = &agg.fields[..] else {
        return None;
    };
    let inset = if inset.as_int().is_some_and(|v| v != 0) {
        "inset "
    } else {
        ""
    };
    let tint = tint
        .as_int()
        .map_or("transparent".to_owned(), |c| color(c as u32));
    Some(format!(
        "{inset}{} {} {} {} {tint}",
        px(dp(x)),
        px(dp(y)),
        px(dp(blur).max(0.0)),
        px(dp(spread)),
    ))
}

/// A `Dp` value in dp, zero for none.
pub(super) fn dp(value: &Value) -> f64 {
    match value {
        Value::Agg(terms) => terms.fields.first().and_then(Value::as_float),
        value => value.as_float(),
    }
    .unwrap_or(0.0)
}

/// The interface face's font stack.
const SYSTEM_UI: &str = "system-ui, -apple-system, \"Segoe UI\", Roboto, sans-serif";

/// A `FontFamily` as a font stack: a role its generic family, a named one
/// before the interface face it falls back to.
fn family(value: &Value) -> String {
    match value {
        Value::Int(1) => "serif".to_owned(),
        Value::Int(2) => "ui-monospace, monospace".to_owned(),
        Value::Agg(agg) => match agg.fields.first().and_then(Value::as_str) {
            Some(name) => {
                let name = name.replace('\\', "\\\\").replace('"', "\\\"");
                format!("\"{name}\", {SYSTEM_UI}")
            }
            None => SYSTEM_UI.to_owned(),
        },
        _ => SYSTEM_UI.to_owned(),
    }
}

/// `v` in its shortest form: `12`, `0.5`, `-3.25`.
pub(super) fn number(v: f64) -> String {
    if v == 0.0 {
        return "0".to_owned();
    }
    let rounded = (v * 10_000.0).round() / 10_000.0;
    format!("{rounded}")
}

/// A length in dp as CSS pixels: one dp is one CSS pixel.
pub(super) fn px(v: f64) -> String {
    if v == 0.0 {
        "0".to_owned()
    } else {
        format!("{}px", number(v))
    }
}

/// A length in sp: a sixteenth of the reader's root font size, so it follows
/// their text size setting.
pub(super) fn sp(v: f64) -> String {
    format!("{}rem", number(v / 16.0))
}

/// A time in seconds.
pub(super) fn seconds(v: f64) -> String {
    format!("{}s", number(v.max(0.0)))
}

/// A packed sRGB `0xRRGGBBAA` color.
pub(super) fn color(rgba: u32) -> String {
    if rgba & 0xff == 0xff {
        format!("#{:06x}", rgba >> 8)
    } else {
        format!("#{rgba:08x}")
    }
}

/// An `Easing` value as a CSS timing function: a named curve by its tag, a
/// `cubic_bezier` by its control points, a `spring` by its move from rest
/// over the time [`duration`] gives it.
pub(super) fn easing(value: &Value) -> String {
    match value {
        Value::Agg(spring) if spring.tag == SPRING => spring_function(spring_of(spring)).0,
        Value::Agg(bezier) if bezier.tag == Easing::NAMED => {
            let at = |i: usize| agg_float(bezier, i);
            let b = CubicBezier::new(at(0), at(1), at(2), at(3));
            timing_function(Easing::CubicBezier(b))
        }
        _ => timing_function(
            value
                .as_int()
                .and_then(|tag| u32::try_from(tag).ok())
                .and_then(Easing::named)
                .unwrap_or_default(),
        ),
    }
}

/// The tag of an `Easing`'s `spring`, past its named curves and its
/// `cubic_bezier`.
const SPRING: u32 = Easing::NAMED + 1;

fn agg_float(agg: &viso_behavior::Aggregate, i: usize) -> f32 {
    agg.fields.get(i).and_then(Value::as_float).unwrap_or(0.0) as f32
}

fn spring_of(agg: &viso_behavior::Aggregate) -> Spring {
    Spring::new(agg_float(agg, 0), agg_float(agg, 1), agg_float(agg, 2))
}

/// The seconds a move with `easing` takes in place of `duration`: a
/// spring's, from rest until it settles within a thousandth of its way.
pub(super) fn duration(easing: &Value, duration: f64) -> f64 {
    match easing {
        Value::Agg(spring) if spring.tag == SPRING => spring_function(spring_of(spring)).1,
        _ => duration,
    }
}

/// A move on `spring` from rest as a `linear()` over the time it takes to
/// settle, and that time in seconds.
fn spring_function(spring: Spring) -> (String, f64) {
    let settle = spring.settle_time(1e-3);
    let at = |u: f32| {
        if u >= 1.0 {
            1.0
        } else {
            spring.step(u * settle)
        }
    };
    (linear_function(&at), f64::from(settle))
}

/// The CSS timing function that follows `easing`: `linear`, a
/// `cubic-bezier()`, or a `linear()` through stops sampled along the curve,
/// as many as keep it within 0.2% of the curve.
pub(super) fn timing_function(easing: Easing) -> String {
    match easing {
        Easing::Linear => "linear".to_owned(),
        Easing::CubicBezier(b) => format!(
            "cubic-bezier({}, {}, {}, {})",
            number(f64::from(b.x1)),
            number(f64::from(b.y1)),
            number(f64::from(b.x2)),
            number(f64::from(b.y2))
        ),
        _ => linear_function(&|t: f32| easing.apply(t)),
    }
}

/// A `linear()` through stops sampled along `at` over `[0, 1]`, as many as
/// keep it within 0.2% of it.
fn linear_function(at: &impl Fn(f32) -> f32) -> String {
    let mut stops = vec![0.0f32];
    sample(at, 0.0, 1.0, 0, &mut stops);
    let mut out = String::from("linear(");
    for (i, &t) in stops.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&number(f64::from(at(t))));
        if i > 0 && i + 1 < stops.len() {
            let _ = write!(out, " {}%", number(f64::from(t) * 100.0));
        }
    }
    out.push(')');
    out
}

/// Push onto `stops` the times past `a` up to and including `b` at which a
/// line through `f` stays within tolerance of it, halving where it does not.
fn sample(f: &impl Fn(f32) -> f32, a: f32, b: f32, depth: u32, stops: &mut Vec<f32>) {
    const TOLERANCE: f32 = 0.002;
    let (fa, fb) = (f(a), f(b));
    let off = |t: f32| (f(t) - (fa + (fb - fa) * (t - a) / (b - a))).abs();
    let straight = [0.25, 0.5, 0.75]
        .iter()
        .all(|&k| off(a + (b - a) * k) <= TOLERANCE);
    if depth >= 8 || (depth >= 2 && straight) {
        stops.push(b);
        return;
    }
    let mid = (a + b) * 0.5;
    sample(f, a, mid, depth + 1, stops);
    sample(f, mid, b, depth + 1, stops);
}

/// A folded length that sets an extent: a fixed one, a percentage of the
/// parent's content box with a fixed part, or a content size.
pub(super) fn extent(length: LengthIr) -> Option<String> {
    match length {
        LengthIr::Fixed(dp) => Some(px(f64::from(dp))),
        LengthIr::Relative { fixed, pct } => {
            let pct = number(f64::from(pct) * 100.0);
            Some(if fixed == 0.0 {
                format!("{pct}%")
            } else if fixed < 0.0 {
                format!("calc({pct}% - {})", px(f64::from(-fixed)))
            } else {
                format!("calc({pct}% + {})", px(f64::from(fixed)))
            })
        }
        LengthIr::MinContent => Some("min-content".into()),
        LengthIr::MaxContent => Some("max-content".into()),
        LengthIr::Fill { .. } | LengthIr::Fit => None,
    }
}

/// A length of five terms as one CSS value: `px` is a device pixel, a CSS
/// pixel over the device pixel ratio, which `--viso-px` holds.
pub(super) fn terms(t: TermsIr, device: &mut bool) -> String {
    let parts: Vec<(f32, String)> = [
        (t.dp, "1px".to_owned()),
        (t.px, "var(--viso-px)".to_owned()),
        (t.sp, "0.0625rem".to_owned()),
        (t.em, "1em".to_owned()),
        (t.pct, "100%".to_owned()),
    ]
    .into_iter()
    .filter(|(v, _)| *v != 0.0)
    .collect();
    if t.px != 0.0 {
        *device = true;
    }
    match parts.as_slice() {
        [] => "0".to_owned(),
        [(v, unit)] if unit == "1px" => px(f64::from(*v)),
        [(v, unit)] if unit == "1em" => format!("{}em", number(f64::from(*v))),
        [(v, unit)] if unit == "100%" => format!("{}%", number(f64::from(*v) * 100.0)),
        _ => {
            let mut out = String::from("calc(");
            for (at, (v, unit)) in parts.iter().enumerate() {
                let v = f64::from(*v);
                if at > 0 {
                    out.push_str(if v < 0.0 { " - " } else { " + " });
                } else if v < 0.0 {
                    out.push('-');
                }
                let _ = write!(out, "{} * {unit}", number(v.abs()));
            }
            out.push(')');
            out
        }
    }
}

/// The page's rules: each distinct set of declarations once, under a class
/// named for when it first appeared, and the rules a node owns under its own.
#[derive(Debug, Default)]
pub(super) struct Sheet {
    shared: HashMap<String, usize>,
    /// Each rule: the media query it holds under, its selector and body.
    rules: Vec<(Option<&'static str>, String, String)>,
}

impl Sheet {
    /// The class holding `declarations`, made the first time they are asked
    /// for; `None` for none.
    pub(super) fn class(&mut self, declarations: &[(&str, String)]) -> Option<String> {
        if declarations.is_empty() {
            return None;
        }
        let body = body(declarations);
        let next = self.shared.len();
        let index = *self.shared.entry(body.clone()).or_insert(next);
        let name = format!("s{index}");
        if index == next {
            self.rules.push((None, format!(".{name}"), body));
        }
        Some(name)
    }

    /// A rule of its own for `selector`, under `media` when it names a
    /// query.
    pub(super) fn rule(
        &mut self,
        media: Option<&'static str>,
        selector: String,
        declarations: &[(&str, String)],
    ) {
        if !declarations.is_empty() {
            self.rules.push((media, selector, body(declarations)));
        }
    }

    /// Every rule, in the order it was made.
    pub(super) fn write(&self, out: &mut String) {
        for (media, selector, body) in &self.rules {
            match media {
                Some(media) => {
                    let _ = writeln!(out, "{media} {{ {selector} {{{body}}} }}");
                }
                None => {
                    let _ = writeln!(out, "{selector} {{{body}}}");
                }
            }
        }
    }
}

fn body(declarations: &[(&str, String)]) -> String {
    let mut out = String::new();
    for (name, value) in declarations {
        let _ = write!(out, " {name}: {value};");
    }
    out.push(' ');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_read_as_css() {
        assert_eq!(extent(LengthIr::Fixed(12.0)).unwrap(), "12px");
        assert_eq!(
            extent(LengthIr::Relative {
                fixed: -16.0,
                pct: 1.0
            })
            .unwrap(),
            "calc(100% - 16px)"
        );
        let mut device = false;
        assert_eq!(
            terms(
                TermsIr {
                    dp: 4.0,
                    sp: 2.0,
                    ..TermsIr::default()
                },
                &mut device
            ),
            "calc(4 * 1px + 2 * 0.0625rem)"
        );
        assert!(!device);
        assert_eq!(color(0x3b6c_f0ff), "#3b6cf0");
        assert_eq!(color(0x0000_0066), "#00000066");
    }

    #[test]
    fn the_theme_schema_matches_the_default_theme() {
        let theme = viso_behavior::theme::default_theme();
        for (at, (_, fields)) in THEME.iter().enumerate() {
            let Some(Value::Agg(group)) = field(&theme, at) else {
                panic!("group {at}");
            };
            assert_eq!(group.fields.len(), fields.len(), "group {at}");
        }
        let rule = theme_rule(&theme);
        assert!(rule.contains("  --colors-primary: #3b6cf0;\n"), "{rule}");
        assert!(rule.contains("  --typography-base-size: 0.875rem;\n"));
        assert!(rule.contains("  --elevation-low: 0 1px 2px 0 #00000024;\n"));
        assert!(rule.contains("  --motion-standard: linear(0, "), "{rule}");
    }

    #[test]
    fn a_curve_is_a_linear_function_within_tolerance_of_it() {
        for tag in 1..Easing::NAMED {
            let easing = Easing::named(tag).unwrap();
            let css = timing_function(easing);
            let stops: Vec<(f32, f32)> = css
                .strip_prefix("linear(")
                .and_then(|s| s.strip_suffix(')'))
                .unwrap()
                .split(", ")
                .enumerate()
                .map(|(i, stop)| {
                    let mut parts = stop.split(' ');
                    let value: f32 = parts.next().unwrap().parse().unwrap();
                    let at = parts
                        .next()
                        .map(|p| p.trim_end_matches('%').parse::<f32>().unwrap() / 100.0)
                        .unwrap_or(if i == 0 { 0.0 } else { 1.0 });
                    (at, value)
                })
                .collect();
            assert!(stops.len() <= 260, "{easing:?}: {} stops", stops.len());
            for k in 0..=200 {
                let t = k as f32 / 200.0;
                let seg = stops.windows(2).find(|w| w[1].0 >= t).unwrap();
                let (a, b) = (seg[0], seg[1]);
                let line = a.1 + (b.1 - a.1) * (t - a.0) / (b.0 - a.0).max(1e-6);
                assert!((line - easing.apply(t)).abs() < 0.01, "{easing:?} at {t}");
            }
        }
        assert_eq!(timing_function(Easing::Linear), "linear");
        assert_eq!(
            timing_function(Easing::CubicBezier(CubicBezier::new(0.2, 0.0, 0.0, 1.0))),
            "cubic-bezier(0.2, 0, 0, 1)"
        );
    }

    #[test]
    fn a_spring_is_a_linear_function_over_the_time_it_takes_to_settle() {
        let (css, seconds) = spring_function(Spring::BOUNCY);
        assert!((seconds - f64::from(Spring::BOUNCY.settle_time(1e-3))).abs() < 1e-9);
        assert!(
            css.starts_with("linear(0, ") && css.ends_with(", 1)"),
            "{css}"
        );
        let peak = css
            .trim_start_matches("linear(")
            .trim_end_matches(')')
            .split(", ")
            .map(|stop| stop.split(' ').next().unwrap().parse::<f32>().unwrap())
            .fold(0.0, f32::max);
        assert!(peak > 1.03, "it rings past its end: {css}");
        let value = Value::Agg(std::rc::Rc::new(viso_behavior::Aggregate {
            tag: SPRING,
            fields: Box::new([Value::Float(300.0), Value::Float(10.0), Value::Float(1.0)]),
        }));
        assert_eq!(
            duration(&value, 0.2),
            spring_function(Spring::new(300.0, 10.0, 1.0)).1
        );
        assert_eq!(duration(&Value::Int(2), 0.2), 0.2);
    }

    #[test]
    fn a_rule_is_shared_by_the_nodes_it_fits() {
        let mut sheet = Sheet::default();
        let a = sheet.class(&[("gap", "8px".into())]);
        let b = sheet.class(&[("gap", "4px".into())]);
        let c = sheet.class(&[("gap", "8px".into())]);
        assert_eq!((a.as_deref(), b.as_deref()), (Some("s0"), Some("s1")));
        assert_eq!(a, c);
        let mut out = String::new();
        sheet.write(&mut out);
        assert_eq!(out, ".s0 { gap: 8px; }\n.s1 { gap: 4px; }\n");
    }
}
