//! A view's capability grant at run time: the package's `[package]
//! capabilities` travel in the module, through its wire form, to the host that
//! links it; a handler calling a native outside the grant faults with `E6103`,
//! its writes roll back and the view keeps handling; a preview host runs with
//! the preview grant whatever the package asks for.

use std::cell::RefCell;
use std::rc::Rc;

use viso_behavior::native::{Clipboard, PREVIEW_CAPABILITIES};
use viso_behavior::{FaultKind, Value};
use viso_dsl::frontend::{Origin, compile_file_for};
use viso_dsl::hir::{CapabilitySet, TargetProfile};
use viso_dsl::schema::Natives;
use viso_dsl::view_behavior::{ViewBehavior, view_behavior};
use viso_ui::StateStore;
use viso_view::{Scope, ViewHost};

const SOURCE: &str = r#"
import viso::clipboard;

export component Notes {
    state copies = 0;
    state note = "hi";
    view {
        Column {
            Text { text: note; on click { copies += 1; clipboard::write_text(note); } }
            Text { text: "plain"; on click { copies += 100; } }
        }
    }
}
"#;

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["notes".to_owned()],
        language: None,
    }
}

/// The view of [`SOURCE`] built for a package granted `granted`.
fn view(granted: &[&str]) -> ViewBehavior {
    let mut capabilities = CapabilitySet::new();
    for &capability in granted {
        capabilities.insert(capability);
    }
    let profile = TargetProfile {
        capabilities,
        ..TargetProfile::default()
    };
    let compiled = compile_file_for(SOURCE, &origin(), Natives::standard(), profile);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    view_behavior(&compiled)
        .expect("mounts")
        .expect("has behavior")
}

/// A clipboard the test reads back.
struct Board(Rc<RefCell<Option<String>>>);

impl Clipboard for Board {
    fn read_text(&mut self) -> Option<String> {
        self.0.borrow().clone()
    }

    fn write_text(&mut self, text: &str) {
        *self.0.borrow_mut() = Some(text.to_owned());
    }
}

/// `host` with a clipboard installed, and what it holds.
fn with_board(mut host: ViewHost) -> (ViewHost, Rc<RefCell<Option<String>>>) {
    let board = Rc::new(RefCell::new(None));
    host.services_mut()
        .insert::<Box<dyn Clipboard>>(Box::new(Board(Rc::clone(&board))));
    (host, board)
}

fn copies(host: &ViewHost) -> Value {
    host.state(host.state_slot("copies").unwrap())
        .unwrap()
        .clone()
}

/// The handler entries of the copying and the plain node, in view order.
fn handlers(view: &ViewBehavior) -> [u32; 2] {
    let entries: Vec<u32> = view
        .nodes()
        .flat_map(|(_, routes)| routes.iter().map(|&(_, entry)| entry))
        .collect();
    entries.try_into().expect("two handlers")
}

fn click(host: &mut ViewHost, handler: u32, states: &mut StateStore) -> bool {
    host.dispatch(handler, Value::Nil, &Scope::EMPTY, states)
}

#[test]
fn the_package_grant_travels_in_the_module_and_its_wire_form() {
    let view = view(&["clipboard.write"]);
    let [copy, _] = handlers(&view);
    assert_eq!(view.module.capabilities(), [Box::from("clipboard.write")]);
    let mut states = StateStore::default();
    for host in [
        ViewHost::new(Rc::clone(&view.module), &view.component).expect("mounts"),
        ViewHost::from_bytes(&view.bytes, &view.component).expect("loads"),
    ] {
        assert_eq!(host.capabilities(), [Box::from("clipboard.write")]);
        let (mut host, board) = with_board(host);
        assert!(
            click(&mut host, copy, &mut states),
            "{:?}",
            host.last_fault()
        );
        assert_eq!(copies(&host), Value::Int(1));
        assert_eq!(board.borrow().as_deref(), Some("hi"));
    }
}

#[test]
fn a_denied_native_faults_rolls_back_and_the_view_keeps_running() {
    let view = view(&[]);
    let [copy, plain] = handlers(&view);
    assert!(view.module.capabilities().is_empty());
    let host = ViewHost::new(Rc::clone(&view.module), &view.component).expect("mounts");
    let (mut host, board) = with_board(host);
    let mut states = StateStore::default();

    assert!(!click(&mut host, copy, &mut states));
    let fault = host.take_fault().expect("a fault");
    assert_eq!(
        (fault.kind, fault.kind.code()),
        (FaultKind::CapabilityDenied, "E6103")
    );
    assert!(
        fault.message.contains("clipboard.write"),
        "{}",
        fault.message
    );
    assert_eq!(copies(&host), Value::Int(0), "the write rolled back");
    assert_eq!(*board.borrow(), None);

    assert!(
        click(&mut host, plain, &mut states),
        "{:?}",
        host.last_fault()
    );
    assert_eq!(copies(&host), Value::Int(100));
    assert!(!click(&mut host, copy, &mut states));
    assert_eq!(copies(&host), Value::Int(100));
}

#[test]
fn a_preview_runs_with_the_preview_grant_whatever_the_package_asks() {
    let view = view(&["clipboard.write"]);
    let [copy, plain] = handlers(&view);
    let host = ViewHost::with_capabilities(
        Rc::clone(&view.module),
        &view.component,
        PREVIEW_CAPABILITIES,
    )
    .expect("mounts");
    assert_eq!(
        host.capabilities(),
        [
            Box::from("asset.read.package"),
            Box::from("gpu.draw.sandboxed"),
            Box::from("ui.basic")
        ]
    );
    let (mut host, board) = with_board(host);
    let mut states = StateStore::default();
    assert!(!click(&mut host, copy, &mut states));
    assert_eq!(host.take_fault().map(|f| f.kind.code()), Some("E6103"));
    assert_eq!(*board.borrow(), None);
    assert!(click(&mut host, plain, &mut states));
}
