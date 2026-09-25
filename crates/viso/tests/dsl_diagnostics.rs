//! The DSL macros report diagnostics where the author wrote the error: `ui!` and
//! `component!` at the offending token of the macro body, `view!` at its path
//! literal with the `.vs` file's line and column.
//!
//! Run with `TRYBUILD=overwrite` to regenerate the expected `.stderr` files.

#[test]
fn dsl_diagnostics_point_at_the_source() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/compile_fail/*.rs");
}
