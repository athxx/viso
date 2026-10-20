//! A text field's `placeholder` and `secure` and a check's `label`, from
//! source to what the nodes draw and tell assistive technology: constant or
//! reading state, under hot reload and from the release package.

mod support;

use support::Rt;
use viso_ui::text_edit::reconcile;
use viso_ui::{EditIntent, NodeId, Role, TextRequest};

const LOGIN: &str = r#"
export component Login {
    state hidden = true;
    view {
        Column {
            Text { width: 20dp; height: 20dp; on click { hidden = !hidden; } }
            TextInput { placeholder: "Password"; secure: hidden; }
            TextInput { value: "ada"; placeholder: "Name"; }
            CheckBox { label: if hidden { "Show" } else { "Hide" }; }
        }
    }
}
"#;

/// The nodes of [`LOGIN`]: its two fields and its check.
fn nodes(rt: &Rt) -> [NodeId; 3] {
    let [_, _, password, name, check] = rt.descendants()[..] else {
        panic!("a column of four");
    };
    [password, name, check]
}

/// Seeds the fields, then the text each node of `nodes` draws, if declared
/// since the last call.
fn drawn<const N: usize>(rt: &mut Rt, nodes: [NodeId; N]) -> [Option<TextRequest>; N] {
    reconcile(&mut rt.store, &mut rt.text_edits, None);
    let mut requests = Vec::new();
    rt.store.take_text_requests(&mut requests);
    nodes.map(|node| {
        requests
            .iter()
            .rev()
            .find(|(id, _)| *id == node)
            .map(|(_, request)| (**request).clone())
    })
}

/// What `node` tells assistive technology: its role, name and hint.
fn said(rt: &Rt, node: NodeId) -> (Role, Option<&str>, Option<&str>) {
    let semantics = rt.store.semantics(node).expect("described");
    (
        semantics.role,
        semantics.label.as_deref(),
        semantics.hint.as_deref(),
    )
}

#[test]
fn a_field_draws_its_placeholder_or_masks_and_a_check_its_caption_in_every_target() {
    for mut rt in [Rt::mount(LOGIN), Rt::packaged(LOGIN)] {
        assert_eq!(rt.fault(), None);
        let [password, name, check] = nodes(&rt);
        let [p, n, c] = drawn(&mut rt, [password, name, check]).map(Option::unwrap);
        assert_eq!(p.text, "Password", "an empty field draws its placeholder");
        assert!(p.color.a < n.color.a, "dimmed");
        assert_eq!(n.text, "ada");
        assert_eq!(c.text, "Show");
        assert_eq!(
            said(&rt, password),
            (Role::SecureField, None, Some("Password"))
        );
        assert_eq!(said(&rt, name), (Role::TextField, None, Some("Name")));
        assert_eq!(said(&rt, check), (Role::CheckBox, Some("Show"), None));

        rt.text_edits
            .editing(password)
            .queue(EditIntent::Insert("pw".into()));
        let [p] = drawn(&mut rt, [password]);
        assert_eq!(p.unwrap().text, "\u{2022}\u{2022}", "a bullet per grapheme");
        assert_eq!(rt.text_edits.get(password).unwrap().text, "pw");

        rt.click(0);
        let [p, n, c] = drawn(&mut rt, [password, name, check]);
        assert_eq!(p.unwrap().text, "pw", "unmasked, it draws its text");
        assert_eq!(n, None, "a field that did not change draws nothing new");
        assert_eq!(c.unwrap().text, "Hide");
        assert_eq!(said(&rt, password).0, Role::TextField);
        assert_eq!(said(&rt, check).1, Some("Hide"));
    }
}

#[test]
fn a_reload_restyles_a_field_and_keeps_what_it_holds() {
    let mut rt = Rt::mount(LOGIN);
    let [password, ..] = nodes(&rt);
    drawn(&mut rt, [password]);
    rt.text_edits
        .editing(password)
        .queue(EditIntent::Insert("pw".into()));
    drawn(&mut rt, [password]);
    rt.reload(&LOGIN.replace(
        "placeholder: \"Password\"; secure: hidden;",
        "placeholder: \"Secret\";",
    ));
    let [password, ..] = nodes(&rt);
    let [p] = drawn(&mut rt, [password]);
    assert_eq!(p.unwrap().text, "pw");
    let buffer = rt.text_edits.get(password).unwrap();
    assert_eq!(
        (
            buffer.text.as_str(),
            buffer.look.placeholder.as_str(),
            buffer.look.secure
        ),
        ("pw", "Secret", false)
    );
    assert_eq!(said(&rt, password), (Role::TextField, None, Some("Secret")));
}
