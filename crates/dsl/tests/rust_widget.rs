//! A Rust widget a native library declares: a view names it by its type
//! name, its own properties are typed against its schema and reach its
//! implementation as entries, its own events run handlers, and the box
//! properties and standard events every node takes come with it.

use std::sync::Arc;

use viso_behavior::native::{NativeLibrary, NativeWidget, Natives, WidgetEvent, WidgetProperty};
use viso_dsl::frontend::{Compiled, Origin, compile_file_in};
use viso_dsl::view_behavior::view_behavior;
use viso_schema::view::ControlKind;

static GAUGE_PROPS: [WidgetProperty; 3] = [
    WidgetProperty::new("value", "F32"),
    WidgetProperty::new("label", "String").localizable(),
    WidgetProperty::new("level", "F32").two_way(),
];

static GAUGE_EVENTS: [WidgetEvent; 2] = [
    WidgetEvent::new("picked", "F32"),
    WidgetEvent::new("level_changed", "F32"),
];

static DIALS: NativeLibrary = NativeLibrary {
    path: "dials",
    version: 1,
    functions: &[],
    types: &[],
    traits: &[],
    derives: &[],
    widgets: &[NativeWidget::rust("Gauge")
        .properties(&[&GAUGE_PROPS])
        .events(&[&GAUGE_EVENTS])],
};

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        ..Default::default()
    }
}

fn compile(source: &str) -> Compiled {
    let natives = Arc::new(Natives::with(&[&DIALS]).unwrap());
    compile_file_in(source, &origin(), natives)
}

const PANEL: &str = r#"
export component Panel {
    state level: F32 = 0.25;
    state picks = 0;
    view {
        Column {
            Gauge {
                value: level * 2.0;
                label: "Speed";
                bind level <=> level;
                width: 120dp;
                background: #1a1a1a;
                on picked(at) { picks += 1; }
                on click { picks = 0; }
            }
        }
    }
}
"#;

#[test]
fn a_rust_widget_takes_its_own_properties_and_events_and_the_box() {
    let compiled = compile(PANEL);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let behavior = view_behavior(&compiled)
        .unwrap_or_else(|e| panic!("{e:?}"))
        .expect("a view with behavior");
    let (key, control) = behavior
        .controls()
        .find(|(_, c)| c.kind == ControlKind::Rust)
        .expect("the gauge's control");
    let rust = control.rust.as_deref().expect("its Rust entries");
    assert_eq!(&*rust.widget, "Gauge");
    let places: Vec<u16> = rust.properties.iter().map(|&(p, _)| p).collect();
    assert_eq!(places, [0, 1, 2], "value, label and the bound level");
    // `picked`, and the write-back of `level`; `click` is the box's.
    let mut events: Vec<u16> = rust.events.iter().map(|&(p, _)| p).collect();
    events.sort_unstable();
    assert_eq!(events, [0, 1]);
    assert!(control.look.background.is_some(), "the box's look");
    assert_eq!(behavior.routes(key).len(), 1, "the box's `click`");
}

#[test]
fn a_rust_widget_types_its_properties_and_takes_no_children() {
    let wrong = compile("export component P { view { Gauge { value: \"fast\"; } } }");
    let codes: Vec<&str> = wrong.errors().map(|d| d.code).collect();
    assert!(codes.contains(&"E2103"), "{codes:?}");

    let unknown = compile("export component P { view { Gauge { speed: 1.0; } } }");
    assert!(
        unknown.errors().next().is_some(),
        "an undeclared property is an error"
    );

    let children = compile("export component P { view { Gauge { Text { text: \"x\"; } } } }");
    assert!(
        children.errors().next().is_some(),
        "a Rust widget takes no children"
    );
}
