use viso_behavior::Value;
use viso_behavior::aot::build_hash;
use viso_dsl::frontend::{Origin, compile_file};
use viso_dsl::view_behavior::{ViewBehavior, view_behavior};

use super::{Boot, Page, offer};

const COUNTER: &str = r#"
export component Counter {
    state clicks = 0;
    event clicked(count: Int);
    view {
        Column {
            Text("Clicked {clicks} times")
            Button("Click") {
                on click { clicks += 1; emit clicked(clicks); }
            }
        }
    }
}
"#;

/// The behavior of `source`, offered as an app's `view!` offers it.
fn offered(source: &str) -> ViewBehavior {
    let origin = Origin {
        package: "app".into(),
        module: vec!["app".into()],
        language: None,
        ..Default::default()
    };
    let compiled = compile_file(source, &origin);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let vb = view_behavior(&compiled).unwrap().unwrap();
    offer(Boot {
        module: Vec::leak(vb.bytes.clone()),
        component: Box::leak(vb.component.clone().into_boxed_str()),
        native: None,
        libraries: &[],
    });
    vb
}

/// The entries of the counter's text and its button's click.
fn entries(vb: &ViewBehavior) -> (u32, u32) {
    // The text is the first node showing a value.
    let text = vb.controls().find_map(|(_, c)| c.value).unwrap();
    let click = vb
        .nodes()
        .find_map(|(_, routes)| routes.first())
        .unwrap()
        .handler;
    (text, click)
}

#[test]
fn a_handler_commits_and_hands_over_what_it_changed_and_emitted() {
    let vb = offered(COUNTER);
    let (text, click) = entries(&vb);
    let mut page = Page::open("Counter", None, build_hash(&vb.module)).unwrap();
    assert_eq!(
        page.evaluate(text, &[]).unwrap(),
        Value::str("Clicked 0 times")
    );
    page.transact(click, &[Value::Nil]).unwrap();
    let batch = page.take();
    assert_eq!(batch.changed, [0]);
    assert_eq!(batch.events.len(), 1);
    assert_eq!(batch.events[0].args[..], [Value::Int(1)]);
    assert_eq!(
        page.evaluate(text, &[]).unwrap(),
        Value::str("Clicked 1 times")
    );
    // A pure entry is no transaction.
    page.evaluate(text, &[]).unwrap();
    assert!(page.take().changed.is_empty());
}

#[test]
fn a_page_of_another_build_does_not_open_on_the_app_s_module() {
    let vb = offered(COUNTER);
    let stale = build_hash(&vb.module) ^ 1;
    let error = Page::open("Counter", None, stale).err().unwrap();
    assert!(error.contains("different sources"), "{error}");
    // Its own module it opens on.
    let own = Page::open("Counter", Some(&vb.bytes), build_hash(&vb.module));
    assert!(own.is_ok());
}

#[test]
fn a_write_from_the_browser_joins_the_batch_only_when_it_changes() {
    let vb = offered(COUNTER);
    let mut page = Page::open("Counter", None, build_hash(&vb.module)).unwrap();
    page.write([(0, Value::Int(0))]);
    assert!(page.take().changed.is_empty());
    page.write([(0, Value::Int(5))]);
    assert_eq!(page.take().changed, [0]);
    assert_eq!(page.state(0), Value::Int(5));
}
