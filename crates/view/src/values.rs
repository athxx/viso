//! Delivering the values a view's nodes show: a label's text and a text
//! field's seeded buffer.
//!
//! A node's value is a pure entry of the view's handler table. It is evaluated
//! when the node mounts and again only when a cell it reads changes, and a value
//! equal to the one the node shows delivers nothing, so a frame that changes
//! nothing it reads never touches the node.

use std::cell::RefCell;
use std::rc::Rc;

use viso_behavior::Value;
use viso_ui::{NodeId, Rgba, StateId, StructureCx, TextRequest};

use crate::control::{Control, ControlKind};
use crate::host::ViewHost;
use crate::scope::Scope;

/// The font size of the text a view shows, in logical pixels.
const FONT_SIZE: f32 = 14.0;

/// The color of the text a view shows.
const COLOR: Rgba = Rgba {
    r: 0.05,
    g: 0.05,
    b: 0.06,
    a: 1.0,
};

/// Delivers the value each node of `nodes` shows, then registers one structure
/// hook that re-delivers, on a frame changing cells they read, the values of
/// exactly the nodes reading them. `nodes` are static nodes of the view `host`
/// runs, whose entries run in the empty scope.
pub fn mount_values(
    cx: &mut StructureCx<'_>,
    host: &Rc<RefCell<ViewHost>>,
    nodes: &[(NodeId, Control)],
) {
    let Ok(mut view) = host.try_borrow_mut() else {
        return;
    };
    let mut shown: Vec<Shown> = nodes
        .iter()
        .map(|&(node, control)| Shown::new(node, control, Scope::EMPTY, &view))
        .collect();
    for node in &mut shown {
        node.deliver(cx, &mut view);
    }
    drop(view);
    shown.retain(|node| !node.deps.is_empty());
    if shown.is_empty() {
        return;
    }
    let deps: Vec<StateId> = shown
        .iter()
        .flat_map(|node| node.deps.iter().copied())
        .collect();
    let host = Rc::clone(host);
    cx.store.add_structure_hook(deps, move |cx, changed| {
        let Ok(mut view) = host.try_borrow_mut() else {
            return;
        };
        for node in &mut shown {
            if node.reads_any(changed) {
                node.deliver(cx, &mut view);
            }
        }
    });
}

/// A node showing a value of the view.
pub(crate) struct Shown {
    node: NodeId,
    control: Control,
    /// The scope its entries run in.
    scope: Scope,
    /// The cells its entries read, ascending.
    deps: Box<[StateId]>,
    /// The value it shows, `None` before the first delivery.
    value: Option<Value>,
}

impl Shown {
    /// Node `node`, showing the values of `control`'s entries in `scope`.
    pub(crate) fn new(node: NodeId, control: Control, scope: Scope, host: &ViewHost) -> Shown {
        let mut deps = Vec::new();
        for entry in [control.value, control.min, control.max, control.step]
            .into_iter()
            .flatten()
        {
            host.entry_cells(entry, &scope, &mut deps);
        }
        deps.sort_unstable_by_key(|id| (id.index(), id.generation()));
        deps.dedup();
        Shown {
            node,
            control,
            scope,
            deps: deps.into(),
            value: None,
        }
    }

    /// Whether a cell of `changed` is one its entries read.
    pub(crate) fn reads_any(&self, changed: &[StateId]) -> bool {
        changed.iter().any(|id| {
            self.deps
                .binary_search_by_key(&(id.index(), id.generation()), |d| {
                    (d.index(), d.generation())
                })
                .is_ok()
        })
    }

    /// Evaluates its value against the current states and delivers it unless
    /// the node already shows it. A fault is kept as the host's
    /// [`last_fault`](ViewHost::last_fault) and leaves the node as it is.
    pub(crate) fn deliver(&mut self, cx: &mut StructureCx<'_>, host: &mut ViewHost) {
        let value = match self.control.value {
            Some(entry) => match host.evaluate(entry, &self.scope, None, &*cx.states) {
                Ok(value) => value,
                Err(fault) => {
                    host.record_fault(fault);
                    return;
                }
            },
            None => Value::Nil,
        };
        if self.value.as_ref() == Some(&value) {
            return;
        }
        let text = || TextRequest {
            text: value.as_str().unwrap_or_default().to_owned(),
            font_size: FONT_SIZE,
            color: COLOR,
            soft_wrap: false,
            locale: None,
        };
        match self.control.kind {
            ControlKind::Label => cx.store.set_text_request(self.node, text()),
            ControlKind::TextInput => cx.store.seed_text(self.node, text()),
            ControlKind::Toggle | ControlKind::Slider | ControlKind::Select => {}
        }
        self.value = Some(value);
    }
}
