//! The colors a window's controls take from its theme.
//!
//! A control names the colors it paints with by [`ColorToken`] instead of by
//! value: [`ColorToken::rgba`] (or [`ColorToken::alpha`]) is an [`Rgba`] any
//! style, text or content color takes, which the store resolves against its
//! [`Palette`] as it writes the node and remembers. A new palette
//! ([`NodeStore::set_palette`](crate::NodeStore::set_palette)) re-resolves
//! exactly the colors that named a token and marks those nodes PAINT, nothing
//! else; a color given by value is the node's own and no theme moves it.

use viso_math::Srgb;
use viso_render::Rgba;

/// A color of the theme, by its role. The first fourteen are the theme's own
/// palette, in the order of the prelude's `ColorPalette`; the rest follow from
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum ColorToken {
    /// The window's background.
    Background,
    /// Text and icons on the background.
    Foreground,
    /// A panel, field or menu over the background.
    Surface,
    /// Text and icons on a surface.
    OnSurface,
    /// The emphasized control: a button, a selection, a filled track.
    Primary,
    /// Text and icons on the primary color.
    OnPrimary,
    /// The primary color under the pointer.
    PrimaryHover,
    /// A second emphasis.
    Accent,
    /// Secondary text: hints, help, placeholders.
    Muted,
    /// Lines: borders, dividers, empty tracks.
    Outline,
    /// An error.
    Error,
    /// Text and icons on the error color.
    OnError,
    /// The focus ring.
    FocusRing,
    /// What dims the window behind a modal.
    Scrim,
    /// The primary color pressed: darker than at rest.
    PrimaryPressed,
}

impl ColorToken {
    /// How many tokens there are.
    pub const COUNT: usize = ColorToken::PrimaryPressed as usize + 1;

    /// Every token, in order.
    pub const ALL: [ColorToken; ColorToken::COUNT] = [
        ColorToken::Background,
        ColorToken::Foreground,
        ColorToken::Surface,
        ColorToken::OnSurface,
        ColorToken::Primary,
        ColorToken::OnPrimary,
        ColorToken::PrimaryHover,
        ColorToken::Accent,
        ColorToken::Muted,
        ColorToken::Outline,
        ColorToken::Error,
        ColorToken::OnError,
        ColorToken::FocusRing,
        ColorToken::Scrim,
        ColorToken::PrimaryPressed,
    ];

    /// The color naming this token, for any color a node takes.
    pub const fn rgba(self) -> Rgba {
        self.alpha(1.0)
    }

    /// The color naming this token with its alpha scaled by `alpha`: a
    /// tint of it, such as an outline at a third.
    pub const fn alpha(self, alpha: f32) -> Rgba {
        // A color's alpha is never negative, so -1 marks a token: its index
        // rides in green and the alpha factor in red.
        Rgba {
            r: alpha,
            g: self as u8 as f32,
            b: 0.0,
            a: TOKEN,
        }
    }

    /// The token `color` names, with its alpha factor; `None` for a color
    /// given by value.
    pub fn of(color: Rgba) -> Option<(ColorToken, f32)> {
        if color.a != TOKEN {
            return None;
        }
        let token = *ColorToken::ALL.get(color.g as usize)?;
        Some((token, color.r))
    }
}

/// Which of a node's colors names a token: its box's fill or border, one of
/// its interaction boxes' (resting, hover, pressed; fill then border), its
/// text's, or its content's by position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ColorSlot {
    Fill,
    Border,
    Box(u8),
    Text,
    Content(u8),
}

impl ColorSlot {
    /// The first slot in order, and the last.
    pub(crate) const FIRST: ColorSlot = ColorSlot::Fill;
    pub(crate) const LAST: ColorSlot = ColorSlot::Content(u8::MAX);
}

/// The alpha marking a color that names a token.
const TOKEN: f32 = -1.0;

/// Whether `color` names a token rather than giving a color.
#[inline(always)]
pub fn is_token(color: Rgba) -> bool {
    color.a == TOKEN
}

/// A theme's colors, one a token, as straight linear RGBA.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    colors: [Rgba; ColorToken::COUNT],
}

impl Palette {
    /// The light palette, the default theme's.
    pub fn light() -> Palette {
        Palette::from_srgb(LIGHT)
    }

    /// The dark palette, the default dark theme's.
    pub fn dark() -> Palette {
        Palette::from_srgb(DARK)
    }

    /// A palette from a theme's fourteen colors, as the prelude's
    /// `ColorPalette` holds them: sRGB `0xRRGGBBAA`, in token order.
    pub fn from_srgb(colors: [u32; 14]) -> Palette {
        Palette::from_base(colors.map(|c| Srgb::from_rgba32(c).into_linear_straight()))
    }

    /// A palette from a theme's fourteen colors, straight linear RGBA in
    /// token order; the rest follow from them.
    pub fn from_base(base: [Rgba; 14]) -> Palette {
        let mut colors = [Rgba::TRANSPARENT; ColorToken::COUNT];
        colors[..14].copy_from_slice(&base);
        let primary = base[ColorToken::Primary as usize];
        colors[ColorToken::PrimaryPressed as usize] = Rgba {
            r: primary.r * PRESSED,
            g: primary.g * PRESSED,
            b: primary.b * PRESSED,
            a: primary.a,
        };
        Palette { colors }
    }

    /// The color of `token`.
    #[inline]
    pub fn get(&self, token: ColorToken) -> Rgba {
        self.colors[token as usize]
    }

    /// `color` as this palette paints it: the color of the token it names,
    /// its alpha scaled, or `color` itself when it names none.
    #[inline]
    pub fn resolve(&self, color: Rgba) -> Rgba {
        match ColorToken::of(color) {
            Some((token, alpha)) => {
                let c = self.get(token);
                Rgba {
                    a: c.a * alpha,
                    ..c
                }
            }
            None => color,
        }
    }
}

impl Default for Palette {
    fn default() -> Self {
        Palette::light()
    }
}

/// How much of the primary color's linear light a press keeps.
const PRESSED: f32 = 0.65;

/// The default theme's colors, sRGB `0xRRGGBBAA` in token order.
pub const LIGHT: [u32; 14] = [
    0xffff_ffff, // background
    0x1b1f_24ff, // foreground
    0xf5f6_f8ff, // surface
    0x1b1f_24ff, // on_surface
    0x3b6c_f0ff, // primary
    0xffff_ffff, // on_primary
    0x2f5c_d6ff, // primary_hover
    0x8a4f_ffff, // accent
    0x6b72_80ff, // muted
    0xd0d5_ddff, // outline
    0xd93a_3aff, // error
    0xffff_ffff, // on_error
    0x3b6c_f099, // focus_ring
    0x0000_0066, // scrim
];

/// The default dark theme's colors, sRGB `0xRRGGBBAA` in token order.
pub const DARK: [u32; 14] = [
    0x1a1c_21ff, // background
    0xe6e8_ebff, // foreground
    0x2a2d_33ff, // surface
    0xe6e8_ebff, // on_surface
    0x82b3_edff, // primary
    0xfcfc_ffff, // on_primary
    0x91bf_f6ff, // primary_hover
    0xb391_ffff, // accent
    0x9aa1_acff, // muted
    0x454a_53ff, // outline
    0xff6b_6bff, // error
    0x1b1f_24ff, // on_error
    0x82b3_ed99, // focus_ring
    0x0000_0099, // scrim
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_survives_as_a_color_and_resolves_with_its_alpha() {
        let half = ColorToken::Outline.alpha(0.5);
        assert!(is_token(half));
        assert_eq!(ColorToken::of(half), Some((ColorToken::Outline, 0.5)));
        let palette = Palette::light();
        let outline = palette.get(ColorToken::Outline);
        assert_eq!(
            palette.resolve(half),
            Rgba {
                a: outline.a * 0.5,
                ..outline
            }
        );
        let plain = Rgba::new(0.2, 0.3, 0.4, 1.0);
        assert!(!is_token(plain));
        assert_eq!(palette.resolve(plain), plain);
    }

    use crate::{
        BoxStyle, BuildCx, DirtyClass, InteractionStyle, LeafStyle, NodeStore, StateStore,
    };
    use crate::{Length, Sizing};

    fn leaf(style: BoxStyle) -> LeafStyle {
        LeafStyle {
            size: Sizing {
                width: Length::Fixed(10.0),
                height: Length::Fixed(10.0),
            },
            style,
        }
    }

    #[test]
    fn a_new_palette_repaints_what_named_tokens_and_marks_paint_only() {
        let mut store = NodeStore::new();
        let given = Rgba::new(0.2, 0.3, 0.4, 1.0);
        let (themed, plain, boxed) = {
            let mut cx = BuildCx::new(&mut store);
            let themed = cx.leaf(leaf(BoxStyle::solid(ColorToken::Primary.rgba())));
            let plain = cx.leaf(leaf(BoxStyle::solid(given)));
            let boxed = cx.leaf(leaf(BoxStyle::NONE));
            (themed.id(), plain.id(), boxed.id())
        };
        store.set_interaction_style(
            boxed,
            InteractionStyle {
                resting: BoxStyle::solid(ColorToken::Surface.rgba()),
                hover: BoxStyle::solid(ColorToken::OnSurface.alpha(0.1)),
                pressed: BoxStyle::solid(ColorToken::Outline.rgba()),
                pressed_cell: None,
                hover_cell: None,
            },
        );
        store.resolve_interaction_styles(&StateStore::new());
        let light = Palette::light();
        assert_eq!(store.style(themed).fill, light.get(ColorToken::Primary));
        assert_eq!(store.style(boxed).fill, light.get(ColorToken::Surface));
        store.clear_dirty();

        let dark = Palette::dark();
        store.set_palette(dark);
        assert_eq!(store.style(themed).fill, dark.get(ColorToken::Primary));
        assert_eq!(store.style(boxed).fill, dark.get(ColorToken::Surface));
        assert_eq!(
            store.style(plain).fill,
            given,
            "a color given by value stays"
        );
        for id in [themed, boxed] {
            assert_eq!(store.dirty(id), DirtyClass::PAINT);
        }
        assert!(store.dirty(plain).is_empty());
        // The boxes the cells choose between follow the palette too.
        store.mark_dirty(boxed, DirtyClass::STYLE);
        store.resolve_interaction_styles(&StateStore::new());
        assert_eq!(store.style(boxed).fill, dark.get(ColorToken::Surface));
    }

    #[test]
    fn a_color_given_over_a_token_keeps_out_of_the_theme() {
        let mut store = NodeStore::new();
        let node = BuildCx::new(&mut store)
            .leaf(leaf(BoxStyle::solid(ColorToken::Primary.rgba())))
            .id();
        // The token's own color, written back, stays the token's.
        let primary = store.palette().get(ColorToken::Primary);
        store.set_fill(node, primary);
        store.set_palette(Palette::dark());
        assert_eq!(
            store.style(node).fill,
            Palette::dark().get(ColorToken::Primary)
        );
        let given = Rgba::new(0.9, 0.1, 0.1, 1.0);
        store.set_fill(node, given);
        store.set_palette(Palette::light());
        assert_eq!(store.style(node).fill, given);
    }

    #[test]
    fn every_token_is_found_by_its_index() {
        for token in ColorToken::ALL {
            assert_eq!(ColorToken::of(token.rgba()), Some((token, 1.0)));
        }
    }
}
