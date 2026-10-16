//! The schema of a package's own components: found by path or suffix beside
//! the framework's widgets, each input's type, default and the dirty classes
//! its view actually binds it to, its events, slots and parts, and where it is
//! declared, with a revision that follows its public surface only.

use std::fs;
use std::path::PathBuf;

use viso_behavior::native::SlotCardinality;
use viso_dsl::hir::TargetProfile;
use viso_dsl::package::{PackageManifest, load_package};
use viso_dsl::schema::{
    Catalog, ListKind, Natives, Schema, SchemaKind, list, package_schemas, query, search,
};

const CARD: &str = r#"
export component Card {
    @bindable(changed)
    input value: String;
    input title: String;
    input dense: Bool = false;
    input accent: Color = #3366ff;
    input count: I64 = 0;
    input level: F32 = 0.5f32;
    event changed(value: String);
    slot footer: OptionalSlot<Node>;
    view {
        Column {
            width: 200dp;
            height: 100dp;
            Text { text: title; color: accent; }
            if dense { Text { text: "dense"; } }
            Text { text: value; }
            Slider { value: level; }
        }
    }
}

component Badge {
    input label: String;
    view { Text { text: label; } }
}

export component Frame {
    input caption: String;
    state heading = "Home";
    view { Card { title: caption; value: heading; } }
}
"#;

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("viso-dsl-schema-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        Self(dir)
    }

    fn write(&self, file: &str, text: &str) {
        fs::write(self.0.join("src").join(file), text).unwrap();
    }

    fn components(&self) -> Vec<Schema> {
        let package = load_package(
            &self.0,
            PackageManifest {
                name: "app",
                language: None,
                profile: TargetProfile::default(),
            },
        );
        let errors: Vec<_> = package
            .files
            .iter()
            .flat_map(|f| &f.diagnostics)
            .filter(|d| d.severity == viso_dsl::Severity::Error)
            .collect();
        assert!(errors.is_empty(), "{errors:#?}");
        package_schemas(&package, &self.0, "0.3.0")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn catalog(components: Vec<Schema>) -> Catalog {
    Catalog::framework(Natives::standard()).with_package(components)
}

#[test]
fn a_component_reports_its_inputs_events_slots_and_origin() {
    let s = Scratch::new("card");
    s.write("card.vs", CARD);
    let catalog = catalog(s.components());
    let card = query("Card", &catalog).expect("Card");
    assert_eq!(card.kind, SchemaKind::Component);
    assert_eq!(card.symbol, "app::card::Card");
    assert_eq!(card.version, "0.3.0");
    let input = |name: &str| {
        card.inputs
            .iter()
            .find(|i| i.name == name)
            .unwrap_or_else(|| panic!("{name}"))
    };
    let title = input("title");
    assert_eq!((title.ty.as_str(), title.required), ("String", true));
    assert_eq!(
        title.invalidates,
        ["MEASURE", "LAYOUT", "PAINT", "SEMANTICS"]
    );
    let dense = input("dense");
    assert_eq!(dense.default.as_deref(), Some("false"));
    assert!(!dense.required);
    assert_eq!(dense.invalidates, ["STRUCTURE"], "it decides a region");
    assert_eq!(input("accent").invalidates, ["PAINT"]);
    assert_eq!(input("accent").default.as_deref(), Some("#3366ff"));
    assert!(input("count").invalidates.is_empty(), "never read");
    assert!(input("value").two_way && !input("title").two_way);
    assert_eq!(
        input("level").invalidates,
        query("Slider.value", &catalog).unwrap().inputs[0].invalidates,
        "a control's value, as the widget declares it"
    );

    let [changed] = &card.events[..] else {
        panic!("one event: {:?}", card.events);
    };
    assert_eq!(changed.payload.as_deref(), Some("(value: String)"));
    assert!(!changed.bubbles && !changed.cancelable);
    let [footer] = &card.slots[..] else {
        panic!("one slot: {:?}", card.slots);
    };
    assert_eq!(footer.name, "footer");
    assert_eq!(footer.cardinality, SlotCardinality::Optional);
    assert!(!footer.default);

    let origin = card.origin.as_ref().expect("a package's");
    assert_eq!(origin.location(), "src/card.vs:2:18");
    assert_eq!(origin.visibility(), "public");
    let badge = query("card::Badge", &catalog).expect("Badge");
    assert_eq!(badge.origin.as_ref().unwrap().visibility(), "private");

    let frame = query("app::card::Frame", &catalog).expect("full path");
    assert_eq!(frame.inputs.len(), 1);
    assert!(query("Button", &catalog).unwrap().origin.is_none());
}

#[test]
fn a_component_input_read_through_another_binds_what_that_one_does() {
    let s = Scratch::new("through");
    s.write("card.vs", CARD);
    let catalog = catalog(s.components());
    let frame = query("Frame.caption", &catalog).expect("Frame.caption");
    assert_eq!(
        frame.inputs[0].invalidates,
        ["MEASURE", "LAYOUT", "PAINT", "SEMANTICS"]
    );
}

#[test]
fn the_revision_follows_the_public_surface() {
    let s = Scratch::new("revision");
    s.write("card.vs", CARD);
    let revision = |s: &Scratch| {
        s.components()
            .into_iter()
            .find(|c| c.symbol.ends_with("::Card"))
            .and_then(|c| c.origin)
            .unwrap()
            .revision
    };
    let first = revision(&s);
    s.write("card.vs", &CARD.replace("width: 200dp;", "width: 210dp;"));
    assert_eq!(revision(&s), first, "a layout constant is not the surface");
    s.write(
        "card.vs",
        &CARD.replace("input count: I64 = 0;", "input count: I64 = 1;"),
    );
    assert_ne!(revision(&s), first, "a default is");
}

#[test]
fn a_suffix_two_symbols_share_lists_both_and_lists_name_groups() {
    let s = Scratch::new("ambiguous");
    s.write("card.vs", CARD);
    s.write(
        "text.vs",
        "export component Button { view { Text { text: \"b\"; } } }\n",
    );
    let catalog = catalog(s.components());
    let error = query("Button", &catalog).expect_err("two");
    assert_eq!(error.code, "E2001");
    assert_eq!(
        error.notes,
        [
            "did you mean `viso::widgets::Button`?",
            "did you mean `app::text::Button`?"
        ]
    );
    let components: Vec<_> = list(ListKind::Components, &catalog)
        .into_iter()
        .map(|h| (h.path, h.origin))
        .collect();
    assert_eq!(
        components,
        [
            (
                "app::card::Card".to_owned(),
                Some("src/card.vs:2:18".to_owned())
            ),
            (
                "app::card::Badge".to_owned(),
                Some("src/card.vs:24:11".to_owned())
            ),
            (
                "app::card::Frame".to_owned(),
                Some("src/card.vs:29:18".to_owned())
            ),
            (
                "app::text::Button".to_owned(),
                Some("src/text.vs:1:18".to_owned())
            ),
        ]
    );
    let widgets = list(ListKind::Widgets, &catalog);
    assert!(widgets.iter().any(|h| h.path == "viso::widgets::Button"));
    assert!(widgets.iter().all(|h| h.origin.is_none()));
    assert!(
        list(ListKind::Types, &catalog)
            .iter()
            .all(|h| h.kind == SchemaKind::Type)
    );
    let [children] = &query("Column", &catalog_of_framework()).unwrap().slots[..] else {
        panic!("Column has one slot");
    };
    assert_eq!(children.name, "children");
    assert_eq!(children.cardinality, SlotCardinality::Many);
    assert!(children.default);
    assert!(
        list(ListKind::Natives, &catalog)
            .iter()
            .any(|h| h.path == "viso::text::upper")
    );
    assert!(
        search("card.ti", &catalog)
            .iter()
            .any(|h| h.path == "app::card::Card.title")
    );
}

fn catalog_of_framework() -> Catalog {
    Catalog::framework(Natives::standard())
}

const MODEL: &str = r#"
export record Point {
    x: I64;
    y: I64 = 0;
}

enum Status {
    idle;
    ready(I64, String);
    failed { code: I64; };
}

export component Panel {
    @default slot body: Slot<Node>;
    slot extras: SlotList<Node> = empty;
    view {
        Column {
            SlotOutlet { slot: body; }
            SlotOutlet { slot: extras; }
        }
    }
}
"#;

#[test]
fn a_package_record_and_enum_are_schema_symbols() {
    let s = Scratch::new("model");
    s.write("model.vs", MODEL);
    let catalog = catalog(s.components());
    let point = query("Point", &catalog).expect("Point");
    assert_eq!(point.kind, SchemaKind::Record);
    assert_eq!(point.symbol, "app::model::Point");
    let fields: Vec<_> = point
        .fields
        .iter()
        .map(|f| (f.name.as_str(), f.ty.as_str(), f.default.as_deref()))
        .collect();
    assert_eq!(fields, [("x", "I64", None), ("y", "I64", Some("0"))]);
    let origin = point.origin.as_ref().expect("declared");
    assert_eq!(origin.location(), "src/model.vs:2:15");
    assert_eq!(origin.visibility(), "public");

    let status = query("model::Status", &catalog).expect("Status");
    assert_eq!(status.kind, SchemaKind::Enum);
    let variants: Vec<_> = status
        .variants
        .iter()
        .map(|v| (v.name.as_str(), v.payload.as_deref()))
        .collect();
    assert_eq!(
        variants,
        [
            ("idle", None),
            ("ready", Some("(I64, String)")),
            ("failed", Some("{ code: I64 }"))
        ]
    );
    assert_eq!(status.origin.unwrap().visibility(), "private");

    let y = query("Point.y", &catalog).expect("a field is a member");
    assert_eq!(y.fields.len(), 1);
    let types: Vec<_> = list(ListKind::Types, &catalog)
        .into_iter()
        .filter(|h| h.origin.is_some())
        .map(|h| (h.path, h.kind))
        .collect();
    assert_eq!(
        types,
        [
            ("app::model::Point".to_owned(), SchemaKind::Record),
            ("app::model::Status".to_owned(), SchemaKind::Enum)
        ]
    );
    assert!(
        search("status.rea", &catalog)
            .iter()
            .any(|h| h.path == "app::model::Status.ready" && h.detail == "(I64, String)")
    );
    let panel = query("Panel", &catalog).expect("Panel");
    let slots: Vec<_> = panel
        .slots
        .iter()
        .map(|s| (s.name.as_str(), s.cardinality, s.default))
        .collect();
    assert_eq!(
        slots,
        [
            ("body", SlotCardinality::One, true),
            ("extras", SlotCardinality::Many, false)
        ]
    );
}
