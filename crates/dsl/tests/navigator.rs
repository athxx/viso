//! `Navigator<R>`: a standard generic native type. Its methods take and
//! return routes of its type argument, reads answer from the host's stack and
//! navigations edit it; a route of another type is `E2103`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use viso_behavior::native::{
    NativeLibrary, NativeType, NativeValue, Natives, Obj, Param, SchemaTy, VariantFields,
};
use viso_behavior::nav::{Navigation, NavigatorRef, RouteStack};
use viso_behavior::{Budget, Value, Vm};
use viso_dsl::frontend::{Origin, compile_file_in};

static ROUTE: NativeType = NativeType::enumeration_of(
    "Route",
    &["inbox", "message"],
    &[
        VariantFields::UNIT,
        VariantFields::record(&[Param {
            name: "id",
            ty: SchemaTy::I64,
        }]),
    ],
);

static MAIL: NativeLibrary = NativeLibrary {
    path: "mail",
    version: 1,
    functions: &[],
    types: &[ROUTE],
    traits: &[],
    derives: &[],
    widgets: &[],
};

/// A stack of route values.
#[derive(Default)]
struct Stack(RefCell<Vec<Value>>);

impl RouteStack for Stack {
    fn current(&self) -> Option<Value> {
        self.0.borrow().last().cloned()
    }

    fn depth(&self) -> usize {
        self.0.borrow().len()
    }

    fn navigate(&self, navigation: Navigation) -> bool {
        let mut stack = self.0.borrow_mut();
        match navigation {
            Navigation::Push(route) => stack.push(route),
            Navigation::Replace(route) => *stack.last_mut().unwrap() = route,
            Navigation::Pop if stack.len() > 1 => drop(stack.pop()),
            Navigation::Pop => return false,
            Navigation::Reset(route) => *stack = vec![route],
        }
        true
    }
}

const SOURCE: &str = r#"
import mail::Route;

component Shell {
    input nav: Navigator<Route>;
    state seen = 0;

    computed open: I64 = match nav.current() {
        Route::inbox => 0,
        Route::message { id } => id,
    };

    action read(id: I64) {
        nav.push(Route::message { id: id });
        seen = open;
    }

    action back() {
        if nav.can_pop() { let _ = nav.pop(); }
        seen = nav.depth();
    }

    view { Text {} }
}
"#;

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        ..Default::default()
    }
}

#[test]
fn a_navigator_types_its_routes_and_drives_the_host_stack() {
    let natives = Arc::new(Natives::with(&[&MAIL]).unwrap());
    let compiled = compile_file_in(SOURCE, &origin(), natives.clone());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let module = Rc::new(compiled.behavior.bytecode().expect("bytecode"));
    let index = module.component("Shell").expect("component");
    let stack = Rc::new(Stack::default());
    stack.0.borrow_mut().push(Value::Int(0));
    let nav = Obj::new(NavigatorRef(stack.clone())).into_value();
    let layout = module.layout(index);
    let input = layout.input("nav").expect("nav");
    let (read, back) = (
        layout.member("read").expect("read"),
        layout.member("back").expect("back"),
    );
    let mut vm = Vm::new(module, Budget::default());
    let mut shell = vm.instantiate(index, [(input, nav)]).expect("instantiate");
    vm.link(&natives, &[]).expect("link");

    vm.call(&mut shell, read, &[Value::Int(7)]).expect("read");
    assert_eq!(
        shell.states()[0],
        Value::Int(7),
        "the computed reads the pushed route"
    );
    assert_eq!(
        *stack.0.borrow(),
        [Value::Int(0), Value::variant(1, [Value::Int(7)])]
    );
    vm.call(&mut shell, back, &[]).expect("back");
    assert_eq!(shell.states()[0], Value::Int(1));
    vm.call(&mut shell, back, &[]).expect("back at the root");
    assert_eq!(*stack.0.borrow(), [Value::Int(0)], "the root page stays");
}

#[test]
fn a_route_of_another_type_is_e2103() {
    let natives = Arc::new(Natives::with(&[&MAIL]).unwrap());
    let source = "import mail::Route;\n\
        component C { input nav: Navigator<Route>; action go() { nav.push(3); } view { Text {} } }";
    let compiled = compile_file_in(source, &origin(), natives);
    let errors: Vec<_> = compiled.errors().collect();
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert_eq!(errors[0].code, "E2103", "{:#?}", errors[0]);
}
