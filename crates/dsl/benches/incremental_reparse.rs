//! The editor keystroke path: one character typed inside one member of a large
//! component, reparsed in place against a full lex and parse of the same edited
//! source. Both give the same tree; the in-place path reuses every untouched
//! green node.
//!
//! Run release (`cargo bench -p viso-dsl`); criterion defaults to a release
//! profile. Debug timing is not a perf result.

use std::hint::black_box;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use viso_dsl::syntax::grammar::{Entry, IncrementalParse};
use viso_dsl::syntax::{Edit, TextRange, TextSize};

/// A component with `members` actions and a view, about 6 lines per member.
fn source(members: usize) -> String {
    let mut text = String::from("component Big {\n    state count: I64 = 0;\n");
    for i in 0..members {
        text.push_str(&format!(
            "    action step{i}(by: I64) {{\n        let next = count + by * {i};\n        \
             if next > 100 {{ count = 0; }} else {{ count = next; }}\n    }}\n\n"
        ));
    }
    text.push_str("    view {\n        Text { text: format(\"{}\", count); }\n    }\n}\n");
    text
}

fn keystroke(c: &mut Criterion) {
    for members in [10, 1000] {
        let old = source(members);
        // Type a digit into the `by * N` of the middle action.
        let anchor = format!("by * {}", members / 2);
        let at = old.find(&anchor).expect("the middle action") + anchor.len();
        let edit = Edit::new(TextRange::empty(TextSize::new(at as u32)), "7");
        let new = edit.apply(&old);
        let base = IncrementalParse::new(&old, Entry::CompilationUnit);
        let mut probe = base.clone();
        assert!(probe.edit(&edit, &new), "the edit reparses in place");

        let mut group = c.benchmark_group(format!("keystroke/{members}_members"));
        group.bench_function("full", |b| {
            b.iter(|| IncrementalParse::new(black_box(&new), Entry::CompilationUnit));
        });
        group.bench_function("in_place", |b| {
            b.iter_batched(
                || base.clone(),
                |mut parse| {
                    parse.edit(black_box(&edit), black_box(&new));
                    parse
                },
                BatchSize::SmallInput,
            );
        });
        group.finish();
    }
}

criterion_group!(benches, keystroke);
criterion_main!(benches);
