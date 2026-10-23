//! The [`Avatar`] — a person shown in a circle: their picture, or their
//! initials on a color their name picks.
//!
//! An avatar is one leaf, 40 across unless [`side`](Avatar::side) says. Given
//! a picture ([`image`](Avatar::image)) it shows it cropped to the circle,
//! over the name's color until the picture is decoded; without one it shows
//! the name's initials, white and centred, sized to the circle. The same
//! name always gets the same color, so a list of people tells them apart at
//! a glance.
//!
//! Its role is [`Role::Image`], named by the person's name. It takes no
//! focus and no input of its own.
//!
//! ```
//! use viso_widgets::avatar;
//! use viso_ui::{BuildCx, Component, NodeStore};
//!
//! let mut store = NodeStore::new();
//! let mut cx = BuildCx::new(&mut store);
//! avatar("Ada Lovelace").side(56.0).build(&mut cx);
//! ```

use viso_ui::plate::{avatar_fill, initials};
use viso_ui::{
    Asset, AssetFit, BoxStyle, BuildCx, CheckMark, CheckShape, Component, FontSize, FontWeight,
    LeafStyle, Rgba, Role, Semantics, Size, TextRequest, TextStyle,
};

/// An avatar's side unless set.
const SIDE: f32 = 40.0;

/// Its initials' size, as a share of its side.
const INITIALS: f32 = 0.4;

/// The initials' ink.
const INK: Rgba = Rgba {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};

/// A person's picture or initials in a circle.
///
/// Construct one with [`avatar`]. It binds nothing: a different person is a
/// different avatar.
#[derive(Debug, Clone, PartialEq)]
pub struct Avatar {
    name: String,
    image: Option<Asset>,
    side: f32,
    fill: Option<Rgba>,
}

/// An avatar of the person named `name`.
pub fn avatar(name: impl Into<String>) -> Avatar {
    Avatar {
        name: name.into(),
        image: None,
        side: SIDE,
        fill: None,
    }
}

impl Avatar {
    /// Show the picture `image`, cropped to the circle, in place of the
    /// initials.
    pub fn image(mut self, image: Asset) -> Self {
        self.image = Some(image);
        self
    }

    /// Set its side (40 unless set); the initials grow with it.
    pub fn side(mut self, side: f32) -> Self {
        self.side = side;
        self
    }

    /// Fill the circle `fill` in place of the color the name picks.
    pub fn tint(mut self, fill: Rgba) -> Self {
        self.fill = Some(fill);
        self
    }
}

impl Component for Avatar {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let handle = cx.leaf(LeafStyle {
            size: Size::fixed(self.side, self.side),
            style: BoxStyle {
                fill: self.fill.unwrap_or_else(|| avatar_fill(&self.name)),
                radius_pct: 0.5,
                ..BoxStyle::NONE
            },
        });
        cx.semantics(
            handle,
            Semantics::role(Role::Image).with_label(self.name.as_str()),
        );
        match &self.image {
            Some(image) => {
                cx.asset(handle, image, AssetFit::Cover, None);
            }
            None => {
                let request = TextRequest {
                    text: initials(&self.name),
                    font_size: FontSize::Fixed(self.side * INITIALS),
                    color: INK,
                    soft_wrap: false,
                    locale: None,
                    style: TextStyle {
                        weight: FontWeight(500),
                        ..TextStyle::default()
                    },
                    spans: Vec::new(),
                };
                cx.text_request(handle, request);
                cx.check_mark(
                    handle,
                    CheckMark {
                        shape: CheckShape::Plate,
                        checked: false,
                    },
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ui::NodeStore;

    #[test]
    fn an_avatar_without_a_picture_shows_the_initials_on_the_name_s_color() {
        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        avatar("Ada Lovelace").side(50.0).build(&mut cx);
        let node = cx.root().unwrap();
        let request = store.text_request(node).unwrap();
        assert_eq!(request.text, "AL");
        assert_eq!(request.font_size, FontSize::Fixed(20.0));
        assert_eq!(store.style(node).fill, avatar_fill("Ada Lovelace"));
        assert_eq!(store.style(node).radius_pct, 0.5);
        let semantics = store.semantics(node).unwrap();
        assert_eq!(semantics.role, Role::Image);
        assert_eq!(semantics.label.as_deref(), Some("Ada Lovelace"));
    }
}
