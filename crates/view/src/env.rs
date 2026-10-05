//! The adaptive environment as the behavior VM sees it: each field of the
//! prelude's `Environment` record built from the runtime's [`AdaptiveEnv`].

use std::cell::RefCell;
use std::rc::Rc;

use viso_behavior::{Aggregate, Value};
use viso_ui::adaptive::{
    AdaptiveEnv, AnchorId, DisplayFeatureKind, EnvField, LayoutDirection, LocalConstraints,
    Orientation, PointerPrecision, SizeClass,
};
use viso_ui::layout::Inset;
use viso_ui::{BuildCx, NodeId, Rect};

/// The value of `field` for a reader anchored at `anchor`, as the prelude
/// types it. An anchored field without a live anchor reads the window's.
pub fn env_value(field: EnvField, env: &AdaptiveEnv, anchor: Option<AnchorId>) -> Value {
    let e = env.environment();
    match field {
        EnvField::Window => record([
            record([dp(e.window.width), dp(e.window.height)]),
            dp(e.window.scale_factor),
        ]),
        EnvField::Constraints => {
            let c: LocalConstraints = anchor
                .and_then(|anchor| env.constraints(anchor))
                .unwrap_or_else(|| e.content_constraints());
            record([
                dp(c.min_width),
                c.max_width.map_or(Value::Nil, dp),
                dp(c.min_height),
                c.max_height.map_or(Value::Nil, dp),
            ])
        }
        EnvField::SizeClass => {
            let class = anchor
                .and_then(|anchor| env.size_class(anchor))
                .unwrap_or_else(|| env.window_class());
            Value::Int(match class {
                SizeClass::Compact => 0,
                SizeClass::Medium => 1,
                SizeClass::Expanded => 2,
            })
        }
        EnvField::SafeArea => insets(e.safe_area),
        EnvField::KeyboardInset => record([dp(e.keyboard_inset)]),
        EnvField::DisplayFeatures => Value::List(Rc::new(
            e.display_features
                .iter()
                .map(|feature| {
                    let tag = match feature.kind {
                        DisplayFeatureKind::Hinge => 0,
                        DisplayFeatureKind::Fold => 1,
                        DisplayFeatureKind::Cutout => 2,
                    };
                    aggregate(tag, [rect(feature.bounds)])
                })
                .collect(),
        )),
        EnvField::Input => record([
            Value::Int(match e.input.primary_pointer_precision {
                PointerPrecision::Fine => 0,
                PointerPrecision::Coarse => 1,
                PointerPrecision::Unavailable => 2,
            }),
            Value::bool(e.input.hover_available),
            Value::bool(e.input.keyboard_available),
            Value::bool(e.input.touch_available),
            Value::bool(e.input.pen_available),
            Value::bool(e.input.gamepad_available),
        ]),
        EnvField::TextScale => Value::Float(f64::from(e.text_scale)),
        EnvField::ReducedMotion => Value::bool(e.reduced_motion),
        EnvField::Orientation => Value::Int(match e.orientation() {
            Orientation::Portrait => 0,
            Orientation::Landscape => 1,
        }),
        EnvField::LayoutDirection => Value::Int(match e.layout_direction {
            LayoutDirection::Ltr => 0,
            LayoutDirection::Rtl => 1,
        }),
        EnvField::Locale => record([Value::str(e.locale.as_str())]),
        EnvField::Theme => crate::theme::current_theme(env),
    }
}

/// Links the `env` slots of an embedded view's host once its tree is built:
/// each read is a slot, a field tag and the static ordinal of the node its
/// anchored field resolves at.
#[doc(hidden)]
pub fn __link_env(
    cx: &mut BuildCx<'_>,
    host: &Rc<RefCell<crate::ViewHost>>,
    nodes: &[Option<NodeId>],
    reads: &[(usize, u8, Option<usize>)],
) {
    let mut host = host.borrow_mut();
    cx.structure(|cx| {
        for &(slot, field, anchor) in reads {
            let Some(field) = EnvField::from_tag(field) else {
                continue;
            };
            let anchor = anchor.and_then(|ordinal| nodes.get(ordinal).copied().flatten());
            host.link_env(slot, field, anchor, cx.states);
        }
    });
}

fn dp(value: f32) -> Value {
    Value::Float(f64::from(value))
}

fn rect(r: Rect) -> Value {
    record([dp(r.x), dp(r.y), dp(r.w), dp(r.h)])
}

fn insets(inset: Inset) -> Value {
    record([
        dp(inset.top),
        dp(inset.right),
        dp(inset.bottom),
        dp(inset.left),
    ])
}

fn record<const N: usize>(fields: [Value; N]) -> Value {
    aggregate(0, fields)
}

fn aggregate<const N: usize>(tag: u32, fields: [Value; N]) -> Value {
    Value::Agg(Rc::new(Aggregate {
        tag,
        fields: Box::new(fields),
    }))
}
