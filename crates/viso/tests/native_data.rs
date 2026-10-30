//! A native record: a Rust struct deriving `NativeData`, which a native
//! returns inside a `Result` and `.vs` reads field by field. A native enum: a
//! Rust enum deriving it, which `.vs` builds and matches as any enum.

use std::rc::Rc;
use std::sync::Arc;

use viso::__private::viso_behavior::native::schema;
use viso::__private::viso_behavior::{Budget, Vm};
use viso::native::{NativeData, NativeLibrary, NativeType, NativeValue, Value, native};
use viso_dsl::frontend::{Origin, compile_file_in};

#[derive(Debug, Clone, PartialEq, NativeData)]
#[native(path = "api::Friend")]
struct Friend {
    name: String,
}

#[derive(Debug, Clone, PartialEq, NativeData)]
#[native(path = "api::Profile")]
struct Profile {
    name: String,
    followers: i64,
    tags: Vec<String>,
    best: Option<Friend>,
}

#[derive(Debug, Clone, PartialEq, NativeData)]
#[native(path = "api::Route")]
enum Route {
    Inbox,
    Message(i64, String),
    UserProfile {
        name: String,
        tags: Vec<String>,
    },
    #[native(name = "prefs")]
    Settings,
}

static API: NativeLibrary = NativeLibrary {
    path: "api",
    version: 1,
    functions: &[
        native!(fn "profile" |_cx, id: i64| -> Result<Profile, String> {
            Ok(match id {
                1 => Ok(Profile {
                    name: "Ada".into(),
                    followers: 7,
                    tags: vec!["math".into()],
                    best: Some(Friend { name: "Charles".into() }),
                }),
                _ => Err(format!("no profile {id}")),
            })
        }),
        native!(fn "followed" |_cx, p: Profile| -> Profile {
            Ok(Profile { followers: p.followers + 1, ..p })
        }),
        native!(fn "deeper" |_cx, r: Route| -> Route {
            Ok(match r {
                Route::Inbox => Route::Message(1, "hi".into()),
                Route::Message(id, _) => Route::UserProfile { name: format!("#{id}"), tags: vec![] },
                Route::UserProfile { .. } => Route::Settings,
                Route::Settings => Route::Inbox,
            })
        }),
    ],
    types: &[Profile::TYPE, Friend::TYPE, Route::TYPE],
    traits: &[],
    derives: &[],
    widgets: &[],
};

const SOURCE: &str = r#"
import api::{profile, followed, Profile};

component Card {
    state name = "";
    state followers = 0;
    state friend = "";
    state why = "";

    action load(id: I64) {
        match profile(id) {
            Result::Ok(found) => {
                let more = followed(found);
                name = more.name;
                followers = more.followers + more.tags.len();
                friend = more.best?.name ?? "";
            },
            Result::Err(e) => { why = e; },
        }
    }

    view { Text { text: name; } }
}
"#;

#[test]
fn a_native_record_is_read_field_by_field() {
    let natives = Arc::new(viso::__private::viso_behavior::native::Natives::with(&[&API]).unwrap());
    let origin = Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        ..Default::default()
    };
    let compiled = compile_file_in(SOURCE, &origin, natives.clone());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let module = Rc::new(compiled.behavior.bytecode().expect("bytecode"));
    let index = module.component("Card").expect("component");
    let mut vm = Vm::new(module, Budget::default());
    let mut card = vm.instantiate(index, []).expect("instantiate");
    vm.link(&natives, &[]).expect("link");
    let load = vm.module().layout(index).member("load").expect("load");

    vm.call(&mut card, load, &[Value::Int(1)]).expect("load 1");
    assert_eq!(card.states()[0], Value::str("Ada"));
    assert_eq!(card.states()[1], Value::Int(9));
    assert_eq!(card.states()[2], Value::str("Charles"));
    vm.call(&mut card, load, &[Value::Int(2)]).expect("load 2");
    assert_eq!(card.states()[3], Value::str("no profile 2"));
}

#[test]
fn a_native_record_round_trips_and_carries_its_fields_in_the_schema() {
    let ada = Profile {
        name: "Ada".into(),
        followers: 1,
        tags: vec![],
        best: None,
    };
    assert_eq!(Profile::from_value(&ada.clone().into_value()), Some(ada));
    assert_eq!(Profile::from_value(&Value::Int(1)), None);
    assert_eq!(Profile::TYPE.field("best"), Some(3));
    const { assert!(Profile::TYPE.value) };

    let text = schema::write(&[&API]).unwrap();
    let back = schema::read(&text).unwrap();
    let ty: &NativeType = &back[0].types[0];
    assert_eq!(ty.fields, Profile::FIELDS);
    assert_eq!(schema::write(&back).unwrap(), text);
}

#[test]
fn a_field_a_native_record_lacks_is_e2001_with_the_nearest() {
    let natives = Arc::new(viso::__private::viso_behavior::native::Natives::with(&[&API]).unwrap());
    let origin = Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        ..Default::default()
    };
    let source = "import api::{profile, Profile};\n\
        fn name_of(p: Profile) -> String { p.nme }\n\
        component C { view { Text {} } }";
    let compiled = compile_file_in(source, &origin, natives);
    let errors: Vec<_> = compiled.errors().collect();
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert_eq!(errors[0].code, "E2001");
    assert!(
        format!("{:?}", errors[0]).contains("name"),
        "{:#?}",
        errors[0]
    );
}

const ROUTES: &str = r#"
import api::{deeper, Route};

fn title(r: Route) -> String {
    match r {
        Route::inbox => "inbox",
        Route::message(id, subject) => "message {id}: {subject}",
        Route::user_profile { name, tags } => "{name} ({tags.len()})",
        Route::prefs => "prefs",
    }
}

component Nav {
    state route: Route = Route::inbox;
    state titles = "";

    action walk() {
        titles = title(route);
        route = deeper(route);
        titles = "{titles}|{title(route)}";
        route = deeper(route);
        titles = "{titles}|{title(route)}";
        route = deeper(route);
        titles = "{titles}|{title(route)}";
        route = Route::user_profile { name: "Ada", tags: ["math"] };
        titles = "{titles}|{title(route)}";
        route = Route::message(7, "late");
    }

    view { Text { text: title(route); } }
}
"#;

#[test]
fn a_native_enum_is_built_matched_and_passed_as_any_enum() {
    let natives = Arc::new(viso::__private::viso_behavior::native::Natives::with(&[&API]).unwrap());
    let origin = Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        ..Default::default()
    };
    let compiled = compile_file_in(ROUTES, &origin, natives.clone());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let module = Rc::new(compiled.behavior.bytecode().expect("bytecode"));
    let index = module.component("Nav").expect("component");
    let mut vm = Vm::new(module, Budget::default());
    let mut nav = vm.instantiate(index, []).expect("instantiate");
    vm.link(&natives, &[]).expect("link");
    assert_eq!(Route::from_value(&nav.states()[0]), Some(Route::Inbox));
    let walk = vm.module().layout(index).member("walk").expect("walk");
    vm.call(&mut nav, walk, &[]).expect("walk");
    assert_eq!(
        Route::from_value(&nav.states()[0]),
        Some(Route::Message(7, "late".into()))
    );
    assert_eq!(
        nav.states()[1],
        Value::str("inbox|message 1: hi|#1 (0)|prefs|Ada (1)")
    );
}

#[test]
fn a_native_enum_round_trips_and_carries_its_variants_in_the_schema() {
    for route in [
        Route::Inbox,
        Route::Message(3, "x".into()),
        Route::UserProfile {
            name: "Ada".into(),
            tags: vec!["a".into()],
        },
        Route::Settings,
    ] {
        assert_eq!(Route::from_value(&route.clone().into_value()), Some(route));
    }
    assert_eq!(
        Route::Settings.into_value(),
        Value::Int(3),
        "a unit variant is its index"
    );
    assert_eq!(Route::from_value(&Value::Int(4)), None);
    assert_eq!(
        Route::TYPE.variants,
        ["inbox", "message", "user_profile", "prefs"]
    );
    assert!(Route::TYPE.payload(0).is_unit());
    assert!(!Route::TYPE.payload(1).named);
    assert_eq!(Route::TYPE.payload(1).fields[1].name, "1");
    assert!(Route::TYPE.payload(2).named);

    let text = schema::write(&[&API]).unwrap();
    let back = schema::read(&text).unwrap();
    let ty: &NativeType = &back[0].types[2];
    assert_eq!(ty.payloads, Route::TYPE.payloads);
    assert_eq!(schema::write(&back).unwrap(), text);
}

#[test]
fn a_match_missing_a_native_variant_is_not_exhaustive() {
    let natives = Arc::new(viso::__private::viso_behavior::native::Natives::with(&[&API]).unwrap());
    let origin = Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        ..Default::default()
    };
    let source = "import api::Route;\n\
        fn f(r: Route) -> I64 { match r { Route::inbox => 0, Route::prefs => 1 } }\n\
        component C { view { Text {} } }";
    let compiled = compile_file_in(source, &origin, natives);
    let errors: Vec<_> = compiled.errors().collect();
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert!(
        format!("{:?}", errors[0]).contains("message"),
        "names a missing variant: {:#?}",
        errors[0]
    );
}
