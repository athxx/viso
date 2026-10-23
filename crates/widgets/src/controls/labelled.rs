//! [`Labelled`]: a caption beside the field it names, its access key taking
//! the focus to the field.
//!
//! The caption is one text leaf before the field (above it with
//! [`Labelled::above`]). An `&` in it marks its access key: `labelled("&Name:",
//! text_input(..))` shows "Name:", its N underlined while Alt is held, and
//! Alt+N focuses the field (off Apple systems). The caption is the field's
//! accessible name.

use viso_ui::{
    Access, Align, Axis, BoxStyle, BuildCx, Component, FlexStyle, Inset, Justify, Length, Rgba,
    Size,
};

use crate::controls::mnemonic::Mnemonic;

/// A caption naming the field beside it. See the [module docs](self).
pub struct Labelled<F> {
    caption: String,
    field: F,
    above: bool,
    color: Option<Rgba>,
    gap: f32,
}

/// A [`Labelled`] of `caption` before `field`; an `&` in the caption marks
/// its access key.
pub fn labelled<F: Component>(caption: impl Into<String>, field: F) -> Labelled<F> {
    Labelled {
        caption: caption.into(),
        field,
        above: false,
        color: None,
        gap: 8.0,
    }
}

impl<F: Component> Labelled<F> {
    /// Put the caption above the field instead of before it.
    pub fn above(mut self) -> Self {
        self.above = true;
        self.gap = 4.0;
        self
    }

    /// Set the caption's color.
    pub fn color(mut self, color: Rgba) -> Self {
        self.color = Some(color);
        self
    }

    /// Set the space between the caption and the field.
    pub fn gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }
}

impl<F: Component> Component for Labelled<F> {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let caption = Mnemonic::of(&self.caption);
        let mut text = None;
        let row = cx.flex(
            FlexStyle {
                axis: if self.above { Axis::Column } else { Axis::Row },
                gap: self.gap,
                padding: Inset::all(0.0),
                align: if self.above {
                    Align::Start
                } else {
                    Align::Center
                },
                justify: Justify::Start,
                size: Size {
                    width: Length::Fit,
                    height: Length::Fit,
                },
                style: BoxStyle::NONE,
            },
            |cx| {
                let mut label = caption.label();
                if let Some(color) = self.color {
                    label = label.color(color);
                }
                text = Some(label.build_node(cx));
                self.field.build(cx);
            },
        );
        let (Some(text), Some(target)) = (text, cx.focusable_within(row)) else {
            return;
        };
        cx.accessible_name(target, &caption.text);
        let Some(key) = caption.char() else { return };
        cx.access_key(text, key);
        let target = target.id();
        cx.on_key(text, move |ev| {
            if ev.access() == Some(Access::Key) {
                ev.stop_propagation();
                ev.request_focus(target);
            }
        });
    }
}
