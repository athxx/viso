//! Style colors: straight sRGB, as styles are written, blended in sRGB
//! between stops and turned linear for drawing.

use viso_math::srgb_to_linear;

/// A straight (not premultiplied) sRGB color, each channel in `[0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const BLACK: Color = Color::new(0.0, 0.0, 0.0, 1.0);
    pub const WHITE: Color = Color::new(1.0, 1.0, 1.0, 1.0);
    pub const TRANSPARENT: Color = Color::new(0.0, 0.0, 0.0, 0.0);

    /// The color of channels `r`, `g`, `b`, `a` in `[0, 1]`.
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    /// An opaque color of 8-bit channels.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::rgba(r, g, b, 1.0)
    }

    /// A color of 8-bit channels and alpha `a`.
    pub const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Self {
        Self::new(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a)
    }

    /// An opaque color from `0xRRGGBB`.
    pub const fn hex(v: u32) -> Self {
        Self::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
    }

    /// The color at alpha `a`.
    pub const fn alpha(mut self, a: f32) -> Self {
        self.a = a;
        self
    }

    /// Read `#rgb`, `#rrggbb`, `#rrggbbaa`, `rgb(…)`, `rgba(…)`, `hsl(…)`
    /// or `hsla(…)`.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Some(hex) = s.strip_prefix('#') {
            let digit = |i: usize| u8::from_str_radix(hex.get(i..i + 1)?, 16).ok();
            let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
            return match hex.len() {
                3 => Some(Self::rgb(digit(0)? * 17, digit(1)? * 17, digit(2)? * 17)),
                6 => Some(Self::rgb(byte(0)?, byte(2)?, byte(4)?)),
                8 => Some(Self::rgba(
                    byte(0)?,
                    byte(2)?,
                    byte(4)?,
                    byte(6)? as f32 / 255.0,
                )),
                _ => None,
            };
        }
        let (name, args) = s.split_once('(')?;
        let args: Vec<f32> = args
            .strip_suffix(')')?
            .split(',')
            .map(|a| a.trim().trim_end_matches('%').parse().ok())
            .collect::<Option<_>>()?;
        let a = args.get(3).copied().unwrap_or(1.0);
        match (name.trim(), args.len()) {
            ("rgb" | "rgba", 3 | 4) => Some(Self::new(
                args[0] / 255.0,
                args[1] / 255.0,
                args[2] / 255.0,
                a,
            )),
            ("hsl" | "hsla", 3 | 4) => {
                let (h, s, l) = (
                    args[0].rem_euclid(360.0) / 60.0,
                    args[1] / 100.0,
                    args[2] / 100.0,
                );
                let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
                let x = c * (1.0 - (h % 2.0 - 1.0).abs());
                let (r, g, b) = match h as u32 {
                    0 => (c, x, 0.0),
                    1 => (x, c, 0.0),
                    2 => (0.0, c, x),
                    3 => (0.0, x, c),
                    4 => (x, 0.0, c),
                    _ => (c, 0.0, x),
                };
                let m = l - c / 2.0;
                Some(Self::new(r + m, g + m, b + m, a))
            }
            _ => None,
        }
    }

    /// The color as straight linear RGBA, for drawing, at `opacity`.
    pub fn linear(self, opacity: f32) -> [f32; 4] {
        [
            srgb_to_linear(self.r),
            srgb_to_linear(self.g),
            srgb_to_linear(self.b),
            (self.a * opacity).clamp(0.0, 1.0),
        ]
    }

    /// The color blended toward `other` by `t`, channel by channel.
    pub fn mix(self, other: Color, t: f32) -> Color {
        Color::new(
            self.r + (other.r - self.r) * t,
            self.g + (other.g - self.g) * t,
            self.b + (other.b - self.b) * t,
            self.a + (other.a - self.a) * t,
        )
    }

    /// The color's relative luminance.
    pub fn luminance(self) -> f32 {
        let [r, g, b, _] = self.linear(1.0);
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_parse() {
        assert_eq!(Color::parse("#fff"), Some(Color::WHITE));
        assert_eq!(Color::parse("#000000"), Some(Color::BLACK));
        assert_eq!(
            Color::parse("#ff000080").map(|c| (c.r, (c.a * 255.0).round())),
            Some((1.0, 128.0))
        );
        assert_eq!(
            Color::parse("rgba(255, 0, 0, 0.5)"),
            Some(Color::new(1.0, 0.0, 0.0, 0.5))
        );
        let hsl = Color::parse("hsl(120, 100%, 50%)").unwrap();
        assert!((hsl.g - 1.0).abs() < 1e-6 && hsl.r.abs() < 1e-6);
        assert_eq!(Color::parse("blue"), None);
        assert_eq!(Color::hex(0x336699), Color::rgb(0x33, 0x66, 0x99));
        assert!(Color::WHITE.luminance() > 0.99 && Color::BLACK.luminance() == 0.0);
    }
}
