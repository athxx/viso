//! A gauge built in Rust that `.vs` views name: a track filled to its
//! `level`, which a press on the track sets, named by its `label`.

use viso::native::{
    NativeLibrary, NativeWidget, RustWidget, Value, WidgetEvent, WidgetEvents, WidgetImpl,
    WidgetProperty,
};
use viso::ui::{
    Axis, BoxStyle, BuildCx, DispatchPhase, FlexStyle, LeafStyle, Length, NodeId, PointerPhase,
    Rgba, Role, Semantics, Sizing, StructureCx,
};

/// The package's native libraries: the gauge's schema.
pub static NATIVES: &[&NativeLibrary] = &[&DIALS];

/// The package's Rust widgets.
pub static WIDGETS: &[WidgetImpl] = &[WidgetImpl {
    name: "Gauge",
    new: || Box::<Gauge>::default(),
}];

static DIALS: NativeLibrary = NativeLibrary {
    path: "dials",
    version: 1,
    functions: &[],
    types: &[],
    traits: &[],
    derives: &[],
    widgets: &[NativeWidget::rust("Gauge")
        .properties(&[&PROPERTIES])
        .events(&[&EVENTS])],
};

/// Its own properties, by place.
static PROPERTIES: [WidgetProperty; 2] = [
    WidgetProperty::new("level", "F32").two_way(),
    WidgetProperty::new("label", "String").localizable(),
];
const LEVEL: usize = 0;
const LABEL: usize = 1;

/// Its own events, by place: a press sets the level, and reports where.
static EVENTS: [WidgetEvent; 2] = [
    WidgetEvent::new("level_changed", "F32"),
    WidgetEvent::new("pressed", "F32"),
];
const LEVEL_CHANGED: usize = 0;
const PRESSED: usize = 1;

const TRACK: Rgba = Rgba {
    r: 0.25,
    g: 0.27,
    b: 0.32,
    a: 1.0,
};
const FILL: Rgba = Rgba {
    r: 0.35,
    g: 0.65,
    b: 1.0,
    a: 1.0,
};

/// A gauge's nodes, once built.
#[derive(Default)]
pub struct Gauge {
    track: Option<NodeId>,
    fill: Option<NodeId>,
    rest: Option<NodeId>,
    label: String,
    level: f32,
}

impl Gauge {
    /// Names the track by its label and level.
    fn describe(&self, cx: &mut StructureCx<'_>) {
        let Some(track) = self.track else { return };
        let semantics = Semantics::role(Role::Slider)
            .with_label(self.label.clone())
            .with_value(format!("{:.0}%", self.level * 100.0));
        cx.store.set_semantics(track, semantics);
    }
}

impl RustWidget for Gauge {
    fn build(&mut self, cx: &mut BuildCx<'_>, _node: NodeId, events: &WidgetEvents) {
        let filled = |weight: f32, color: Rgba| LeafStyle {
            size: Sizing {
                width: Length::Fill { weight },
                height: Length::Fill { weight: 1.0 },
            },
            style: BoxStyle::solid(color),
        };
        let (mut fill, mut rest) = (None, None);
        let track = cx.flex(
            FlexStyle {
                axis: Axis::Row,
                size: Sizing {
                    width: Length::Fill { weight: 1.0 },
                    height: Length::Fixed(12.0),
                },
                style: BoxStyle::solid(TRACK),
                ..FlexStyle::default()
            },
            |cx| {
                fill = Some(cx.leaf(filled(0.0, FILL)).id());
                rest = Some(cx.leaf(filled(1.0, TRACK)).id());
            },
        );
        let id = track.id();
        let events = events.clone();
        cx.on_pointer(track, move |ev| {
            if ev.phase() == DispatchPhase::Capture {
                return;
            }
            let (Some(p), Some(rect)) = (ev.pointer().copied(), ev.rect_of(id)) else {
                return;
            };
            if p.phase != PointerPhase::Down || rect.w <= 0.0 {
                return;
            }
            let level = ((p.x - rect.x) / rect.w).clamp(0.0, 1.0);
            events.fire(ev, LEVEL_CHANGED, Value::Float(f64::from(level)));
            events.fire(ev, PRESSED, Value::Float(f64::from(level)));
        });
        cx.semantics(track, Semantics::role(Role::Slider));
        (self.track, self.fill, self.rest) = (Some(id), fill, rest);
    }

    fn set(&mut self, cx: &mut StructureCx<'_>, property: usize, value: &Value) {
        match property {
            LEVEL => {
                self.level = value.as_float().unwrap_or_default().clamp(0.0, 1.0) as f32;
                if let (Some(fill), Some(rest)) = (self.fill, self.rest) {
                    cx.store.set_flex_child_weight(fill, Axis::Row, self.level);
                    cx.store
                        .set_flex_child_weight(rest, Axis::Row, 1.0 - self.level);
                }
            }
            LABEL => self.label = value.as_str().unwrap_or_default().to_owned(),
            _ => return,
        }
        self.describe(cx);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn schema() {
        viso::native::check(super::NATIVES, "natives.json");
    }

    #[test]
    fn widgets() {
        viso::native::check_widgets(super::NATIVES, super::WIDGETS);
    }
}
