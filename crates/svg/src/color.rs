//! CSS color parsing: hex, `rgb()/rgba()`, `hsl()/hsla()`, the 147 CSS named
//! colors and `transparent`.

/// A straight (non-premultiplied) 8-bit sRGB color with alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const WHITE: Color = Color::rgb(255, 255, 255);
    pub const TRANSPARENT: Color = Color::new(0, 0, 0, 0);

    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Color { r, g, b, a }
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color { r, g, b, a: 255 }
    }
}

/// Parse a CSS color value. `currentColor` is not handled here (it is a
/// property reference, resolved by the style layer).
pub(crate) fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        return parse_hex(hex);
    }
    let lower = s.to_ascii_lowercase();
    if let Some(open) = lower.find('(') {
        let func = lower[..open].trim();
        let inner = lower[open + 1..].strip_suffix(')')?;
        return match func {
            "rgb" | "rgba" => parse_rgb_func(inner),
            "hsl" | "hsla" => parse_hsl_func(inner),
            _ => None,
        };
    }
    named(&lower)
}

fn parse_hex(hex: &str) -> Option<Color> {
    if !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let d = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).ok().map(|v| v * 17);
    let dd = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    match hex.len() {
        3 => Some(Color::rgb(d(0)?, d(1)?, d(2)?)),
        4 => Some(Color::new(d(0)?, d(1)?, d(2)?, d(3)?)),
        6 => Some(Color::rgb(dd(0)?, dd(2)?, dd(4)?)),
        8 => Some(Color::new(dd(0)?, dd(2)?, dd(4)?, dd(6)?)),
        _ => None,
    }
}

/// Split function arguments on commas, or on whitespace with an optional
/// `/ alpha` (CSS Color 4 space syntax).
fn split_args(inner: &str) -> Vec<&str> {
    if inner.contains(',') {
        inner.split(',').map(str::trim).collect()
    } else {
        inner
            .split(|c: char| c.is_ascii_whitespace() || c == '/')
            .filter(|p| !p.is_empty())
            .collect()
    }
}

fn channel(s: &str) -> Option<u8> {
    if let Some(p) = s.strip_suffix('%') {
        let v: f32 = p.trim().parse().ok()?;
        Some((v.clamp(0.0, 100.0) * 2.55).round() as u8)
    } else {
        let v: f32 = s.parse().ok()?;
        Some(v.clamp(0.0, 255.0).round() as u8)
    }
}

fn alpha(s: &str) -> Option<u8> {
    let v = if let Some(p) = s.strip_suffix('%') {
        p.trim().parse::<f32>().ok()? / 100.0
    } else {
        s.parse::<f32>().ok()?
    };
    Some((v.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn parse_rgb_func(inner: &str) -> Option<Color> {
    let a = split_args(inner);
    match a.len() {
        3 => Some(Color::rgb(channel(a[0])?, channel(a[1])?, channel(a[2])?)),
        4 => Some(Color::new(
            channel(a[0])?,
            channel(a[1])?,
            channel(a[2])?,
            alpha(a[3])?,
        )),
        _ => None,
    }
}

fn parse_hsl_func(inner: &str) -> Option<Color> {
    let a = split_args(inner);
    if a.len() != 3 && a.len() != 4 {
        return None;
    }
    let h: f32 = a[0].trim_end_matches("deg").parse().ok()?;
    let pct = |s: &str| -> Option<f32> {
        Some(
            s.trim_end_matches('%')
                .parse::<f32>()
                .ok()?
                .clamp(0.0, 100.0)
                / 100.0,
        )
    };
    let s = pct(a[1])?;
    let l = pct(a[2])?;
    let al = if a.len() == 4 { alpha(a[3])? } else { 255 };
    let h = h.rem_euclid(360.0) / 360.0;
    let (r, g, b) = if s == 0.0 {
        (l, l, l)
    } else {
        let q = if l < 0.5 {
            l * (1.0 + s)
        } else {
            l + s - l * s
        };
        let p = 2.0 * l - q;
        let hue = |mut t: f32| {
            if t < 0.0 {
                t += 1.0;
            }
            if t > 1.0 {
                t -= 1.0;
            }
            if t < 1.0 / 6.0 {
                p + (q - p) * 6.0 * t
            } else if t < 0.5 {
                q
            } else if t < 2.0 / 3.0 {
                p + (q - p) * (2.0 / 3.0 - t) * 6.0
            } else {
                p
            }
        };
        (hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0))
    };
    let u = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    Some(Color::new(u(r), u(g), u(b), al))
}

fn named(name: &str) -> Option<Color> {
    let rgb = match name {
        "transparent" => return Some(Color::TRANSPARENT),
        "aliceblue" => 0xf0f8ff,
        "antiquewhite" => 0xfaebd7,
        "aqua" => 0x00ffff,
        "aquamarine" => 0x7fffd4,
        "azure" => 0xf0ffff,
        "beige" => 0xf5f5dc,
        "bisque" => 0xffe4c4,
        "black" => 0x000000,
        "blanchedalmond" => 0xffebcd,
        "blue" => 0x0000ff,
        "blueviolet" => 0x8a2be2,
        "brown" => 0xa52a2a,
        "burlywood" => 0xdeb887,
        "cadetblue" => 0x5f9ea0,
        "chartreuse" => 0x7fff00,
        "chocolate" => 0xd2691e,
        "coral" => 0xff7f50,
        "cornflowerblue" => 0x6495ed,
        "cornsilk" => 0xfff8dc,
        "crimson" => 0xdc143c,
        "cyan" => 0x00ffff,
        "darkblue" => 0x00008b,
        "darkcyan" => 0x008b8b,
        "darkgoldenrod" => 0xb8860b,
        "darkgray" | "darkgrey" => 0xa9a9a9,
        "darkgreen" => 0x006400,
        "darkkhaki" => 0xbdb76b,
        "darkmagenta" => 0x8b008b,
        "darkolivegreen" => 0x556b2f,
        "darkorange" => 0xff8c00,
        "darkorchid" => 0x9932cc,
        "darkred" => 0x8b0000,
        "darksalmon" => 0xe9967a,
        "darkseagreen" => 0x8fbc8f,
        "darkslateblue" => 0x483d8b,
        "darkslategray" | "darkslategrey" => 0x2f4f4f,
        "darkturquoise" => 0x00ced1,
        "darkviolet" => 0x9400d3,
        "deeppink" => 0xff1493,
        "deepskyblue" => 0x00bfff,
        "dimgray" | "dimgrey" => 0x696969,
        "dodgerblue" => 0x1e90ff,
        "firebrick" => 0xb22222,
        "floralwhite" => 0xfffaf0,
        "forestgreen" => 0x228b22,
        "fuchsia" => 0xff00ff,
        "gainsboro" => 0xdcdcdc,
        "ghostwhite" => 0xf8f8ff,
        "gold" => 0xffd700,
        "goldenrod" => 0xdaa520,
        "gray" | "grey" => 0x808080,
        "green" => 0x008000,
        "greenyellow" => 0xadff2f,
        "honeydew" => 0xf0fff0,
        "hotpink" => 0xff69b4,
        "indianred" => 0xcd5c5c,
        "indigo" => 0x4b0082,
        "ivory" => 0xfffff0,
        "khaki" => 0xf0e68c,
        "lavender" => 0xe6e6fa,
        "lavenderblush" => 0xfff0f5,
        "lawngreen" => 0x7cfc00,
        "lemonchiffon" => 0xfffacd,
        "lightblue" => 0xadd8e6,
        "lightcoral" => 0xf08080,
        "lightcyan" => 0xe0ffff,
        "lightgoldenrodyellow" => 0xfafad2,
        "lightgray" | "lightgrey" => 0xd3d3d3,
        "lightgreen" => 0x90ee90,
        "lightpink" => 0xffb6c1,
        "lightsalmon" => 0xffa07a,
        "lightseagreen" => 0x20b2aa,
        "lightskyblue" => 0x87cefa,
        "lightslategray" | "lightslategrey" => 0x778899,
        "lightsteelblue" => 0xb0c4de,
        "lightyellow" => 0xffffe0,
        "lime" => 0x00ff00,
        "limegreen" => 0x32cd32,
        "linen" => 0xfaf0e6,
        "magenta" => 0xff00ff,
        "maroon" => 0x800000,
        "mediumaquamarine" => 0x66cdaa,
        "mediumblue" => 0x0000cd,
        "mediumorchid" => 0xba55d3,
        "mediumpurple" => 0x9370db,
        "mediumseagreen" => 0x3cb371,
        "mediumslateblue" => 0x7b68ee,
        "mediumspringgreen" => 0x00fa9a,
        "mediumturquoise" => 0x48d1cc,
        "mediumvioletred" => 0xc71585,
        "midnightblue" => 0x191970,
        "mintcream" => 0xf5fffa,
        "mistyrose" => 0xffe4e1,
        "moccasin" => 0xffe4b5,
        "navajowhite" => 0xffdead,
        "navy" => 0x000080,
        "oldlace" => 0xfdf5e6,
        "olive" => 0x808000,
        "olivedrab" => 0x6b8e23,
        "orange" => 0xffa500,
        "orangered" => 0xff4500,
        "orchid" => 0xda70d6,
        "palegoldenrod" => 0xeee8aa,
        "palegreen" => 0x98fb98,
        "paleturquoise" => 0xafeeee,
        "palevioletred" => 0xdb7093,
        "papayawhip" => 0xffefd5,
        "peachpuff" => 0xffdab9,
        "peru" => 0xcd853f,
        "pink" => 0xffc0cb,
        "plum" => 0xdda0dd,
        "powderblue" => 0xb0e0e6,
        "purple" => 0x800080,
        "rebeccapurple" => 0x663399,
        "red" => 0xff0000,
        "rosybrown" => 0xbc8f8f,
        "royalblue" => 0x4169e1,
        "saddlebrown" => 0x8b4513,
        "salmon" => 0xfa8072,
        "sandybrown" => 0xf4a460,
        "seagreen" => 0x2e8b57,
        "seashell" => 0xfff5ee,
        "sienna" => 0xa0522d,
        "silver" => 0xc0c0c0,
        "skyblue" => 0x87ceeb,
        "slateblue" => 0x6a5acd,
        "slategray" | "slategrey" => 0x708090,
        "snow" => 0xfffafa,
        "springgreen" => 0x00ff7f,
        "steelblue" => 0x4682b4,
        "tan" => 0xd2b48c,
        "teal" => 0x008080,
        "thistle" => 0xd8bfd8,
        "tomato" => 0xff6347,
        "turquoise" => 0x40e0d0,
        "violet" => 0xee82ee,
        "wheat" => 0xf5deb3,
        "white" => 0xffffff,
        "whitesmoke" => 0xf5f5f5,
        "yellow" => 0xffff00,
        "yellowgreen" => 0x9acd32,
        _ => return None,
    };
    Some(Color::rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_forms() {
        assert_eq!(parse_color("#f00"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(parse_color("#00ff0080"), Some(Color::new(0, 255, 0, 128)));
        assert_eq!(
            parse_color("rgb(10%, 20, 30)"),
            Some(Color::rgb(26, 20, 30))
        );
        assert_eq!(
            parse_color("rgba(1,2,3,0.5)"),
            Some(Color::new(1, 2, 3, 128))
        );
        assert_eq!(
            parse_color("rgb(1 2 3 / 50%)"),
            Some(Color::new(1, 2, 3, 128))
        );
        assert_eq!(
            parse_color("hsl(120, 100%, 50%)"),
            Some(Color::rgb(0, 255, 0))
        );
        assert_eq!(
            parse_color("CornflowerBlue"),
            Some(Color::rgb(100, 149, 237))
        );
        assert_eq!(parse_color("nope"), None);
    }
}
