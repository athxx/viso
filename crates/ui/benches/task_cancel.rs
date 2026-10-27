//! The `tasks` benchmark category: free 1000 rows, one at a time as a list
//! unmounts them, while 100 tasks are live. Every freed node drops the tasks
//! it owns; the cost under test is finding them.
//!
//! Run release (`cargo bench -p viso-ui --bench task_cancel`); debug timing is
//! not a performance result.

use std::hint::black_box;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use viso_ui::{Axis, BuildCx, EffectStore, FlexStyle, LeafStyle, NodeId, NodeStore, Sizing};

const ROWS: usize = 1000;
const TASKS: usize = 100;

/// Where the live tasks hang.
#[derive(Clone, Copy)]
enum Owners {
    /// On nodes outside the freed rows: every free finds none.
    Elsewhere,
    /// On every tenth row's label: the frees drop them all.
    OnRows,
}

/// A root holding a column of `ROWS` rows (a row, an icon and a label) and a
/// column of `TASKS` leaves, with `TASKS` pending tasks spawned on `owners`.
/// Returns the store and the rows.
fn build(owners: Owners) -> (NodeStore, Vec<NodeId>) {
    let mut store = NodeStore::new();
    let mut rows = Vec::with_capacity(ROWS);
    let mut labels = Vec::with_capacity(ROWS);
    let mut keepers = Vec::with_capacity(TASKS);
    {
        let mut cx = BuildCx::new(&mut store);
        cx.flex(FlexStyle::default(), |cx| {
            cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    ..FlexStyle::default()
                },
                |cx| {
                    for _ in 0..ROWS {
                        let row = cx.flex(FlexStyle::default(), |cx| {
                            cx.leaf(LeafStyle {
                                size: Sizing::fixed(16.0, 16.0),
                                ..LeafStyle::default()
                            });
                            labels.push(cx.leaf(LeafStyle::default()).id());
                        });
                        rows.push(row.id());
                    }
                },
            );
            cx.flex(FlexStyle::default(), |cx| {
                for _ in 0..TASKS {
                    keepers.push(cx.leaf(LeafStyle::default()).id());
                }
            });
        });
    }
    for n in 0..TASKS {
        let owner = match owners {
            Owners::Elsewhere => keepers[n],
            Owners::OnRows => labels[n * (ROWS / TASKS)],
        };
        store
            .spawn_task(owner, Box::pin(std::future::pending()))
            .expect("the owner is live");
    }
    (store, rows)
}

fn bench(c: &mut Criterion) {
    for (name, owners) in [
        ("free_1k_rows_100_tasks_elsewhere", Owners::Elsewhere),
        ("free_1k_rows_100_tasks_on_rows", Owners::OnRows),
    ] {
        c.bench_function(name, |b| {
            let mut effects = EffectStore::new();
            let mut scratch = Vec::new();
            b.iter_batched(
                || build(owners),
                |(mut store, rows)| {
                    let mut freed = 0;
                    for row in rows {
                        freed += store.free_tree(row, &mut effects, &mut scratch);
                    }
                    black_box((freed, store.task_count()));
                    store
                },
                BatchSize::LargeInput,
            );
        });
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
