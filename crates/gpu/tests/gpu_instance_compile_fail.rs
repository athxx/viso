//! Compile-fail coverage for `#[derive(GpuPod)]`: unsupported field types,
//! missing `#[repr(C)]`, non-struct inputs, and non-`Copy` structs must all be
//! rejected at compile time (§7.3: the GpuPod contract is enforced at the
//! derive, not at runtime).
//!
//! Run with `TRYBUILD=overwrite` to regenerate the expected `.stderr` files.

#[test]
fn compile_fail_cases() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/*.rs");
}
