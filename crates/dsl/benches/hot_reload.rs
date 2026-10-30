//! The compiler side of a hot reload: an edited `.vs` file recompiled into a
//! candidate and the patch from the last good planned against it, for one small
//! component and for a module of hundreds of components its page inlines. The
//! edit inserts one node, so the plan carries one structural insert and keeps
//! every state.
//!
//! Run release (`cargo bench -p viso-dsl --bench hot_reload`); criterion
//! defaults to a release profile. Debug timing is not a perf result.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_dev_protocol::wire::{FileId, NodeRef, StateAction, StructuralOp, ViewPatch};
use viso_dsl::aot::ShaderLang;
use viso_dsl::frontend::Origin;
use viso_dsl::hotreload::{CandidatePlan, plan_view, view_patch};

/// Cards in the large module, and the one the edit lands in.
const CARDS: usize = 300;
const EDITED: usize = CARDS / 2;

/// The inserted node.
const INSERT: &str = "            Text { text: \"new\"; }\n";

/// A counter with a label and a handler, `insert` appended to its column.
fn small(insert: &str) -> String {
    format!(
        r#"export component Counter {{
    state count = 0;
    state label = "taps";
    action bump() {{ count += 1; }}
    view {{
        Column {{
            Text {{ text: "{{label}}: {{count}}"; on click {{ bump(); }} }}
            Text {{ text: "hint"; }}
{insert}        }}
    }}
}}
"#
    )
}

/// `CARDS` stateful components and a page inlining each, `insert` appended to
/// the row of card `edited`.
fn large(edited: Option<usize>) -> String {
    let mut text = String::new();
    for i in 0..CARDS {
        let insert = if Some(i) == edited { INSERT } else { "" };
        text.push_str(&format!(
            r#"component Card{i} {{
    state taps = 0;
    action tap() {{ taps += 1; }}
    view {{
        Row {{
            Text {{ text: "card {i}: {{taps}}"; on click {{ tap(); }} }}
{insert}        }}
    }}
}}

"#
        ));
    }
    text.push_str(
        "export component Page {\n    state selected = 0;\n    view {\n        Column {\n",
    );
    for i in 0..CARDS {
        text.push_str(&format!("            Card{i} {{ }}\n"));
    }
    text.push_str("        }\n    }\n}\n");
    text
}

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
        ..Default::default()
    }
}

/// Recompiles `source` and plans its patch from `last_good`.
fn reload(last_good: &CandidatePlan, source: &str, origin: &Origin) -> ViewPatch {
    let candidate = plan_view(source, origin).expect("the edit compiles");
    view_patch(FileId(0), Some(last_good), &candidate, &[ShaderLang::None]).expect("the patch")
}

/// Plans the reload once and checks it is the one insert under `parent` with
/// every state kept, then times it.
fn bench(c: &mut Criterion, name: &str, old: &str, new: &str, parent: u32) {
    let origin = origin();
    let last_good = plan_view(old, &origin).expect("the last good compiles");
    let patch = reload(&last_good, new, &origin);
    let plan = &patch.plan;
    assert!(!plan.preserving, "an insert is a structural edit");
    match plan.structural.as_slice() {
        [
            StructuralOp::Insert {
                parent: at,
                subtree,
                ..
            },
        ] => {
            assert_eq!(
                *at,
                NodeRef::Static(parent),
                "the insert lands in the edited container"
            );
            assert_eq!(subtree.len(), 1, "one node is built");
        }
        other => panic!("expected one insert, got {other:?}"),
    }
    assert_eq!(plan.states.len(), last_good.sources.len());
    assert!(
        plan.states.iter().all(|s| s.action == StateAction::Keep),
        "every state keeps its value"
    );
    eprintln!(
        "{name}: {} source bytes, {} inlined instances, recompiled and planned to 1 insert, {} states kept, {} node carries",
        new.len(),
        last_good.tree.instances.len(),
        plan.states.len(),
        plan.nodes.len()
    );

    c.bench_function(name, |b| {
        b.iter(|| reload(&last_good, black_box(new), &origin));
    });
}

fn small_component(c: &mut Criterion) {
    bench(
        c,
        "hot_reload_small_component",
        &small(""),
        &small(INSERT),
        0,
    );
}

fn large_module(c: &mut Criterion) {
    // Pre-order: the page's column, then each card's row and its text.
    let row = 1 + 2 * EDITED as u32;
    bench(
        c,
        "hot_reload_large_module",
        &large(None),
        &large(Some(EDITED)),
        row,
    );
}

criterion_group!(small_benches, small_component);
criterion_group! {
    name = large_benches;
    // A large recompile takes a few hundred milliseconds.
    config = Criterion::default().sample_size(10);
    targets = large_module
}
criterion_main!(small_benches, large_benches);
