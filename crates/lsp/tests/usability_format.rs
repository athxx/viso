//! The human usability samples (§156) are already in canonical form: the
//! formatter leaves each unchanged, so formatting a sample a person just
//! wrote from them moves nothing they did not type.

use viso_lsp::format::format;

const SAMPLES: [(&str, &str); 11] = [
    (
        "01-counter.vs",
        include_str!("../../dsl/tests/usability/01-counter.vs"),
    ),
    (
        "02-form.vs",
        include_str!("../../dsl/tests/usability/02-form.vs"),
    ),
    (
        "03-todo-list.vs",
        include_str!("../../dsl/tests/usability/03-todo-list.vs"),
    ),
    (
        "04-search.vs",
        include_str!("../../dsl/tests/usability/04-search.vs"),
    ),
    (
        "05-slot-component.vs",
        include_str!("../../dsl/tests/usability/05-slot-component.vs"),
    ),
    (
        "06-size-class.vs",
        include_str!("../../dsl/tests/usability/06-size-class.vs"),
    ),
    (
        "07-adaptive-scope.vs",
        include_str!("../../dsl/tests/usability/07-adaptive-scope.vs"),
    ),
    (
        "08-quick-game.vs",
        include_str!("../../dsl/tests/usability/08-quick-game.vs"),
    ),
    (
        "09-split-system.vs",
        include_str!("../../dsl/tests/usability/09-split-system.vs"),
    ),
    (
        "10-fixed-diagnostic.vs",
        include_str!("../../dsl/tests/usability/10-fixed-diagnostic.vs"),
    ),
    (
        "11-hot-reload-focus.vs",
        include_str!("../../dsl/tests/usability/11-hot-reload-focus.vs"),
    ),
];

#[test]
fn every_sample_formats_to_itself() {
    let mut failures = Vec::new();
    for (name, source) in SAMPLES {
        let once = format(source);
        if once != source {
            failures.push(format!("{name}:\n--- formatted ---\n{once}"));
        }
        if format(&once) != once {
            failures.push(format!("{name}: not idempotent"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n====\n"));
}
