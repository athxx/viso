//! Compile-fail coverage for `#[derive(GpuInstance)]`: unsupported field types
//! and missing `#[repr(C)]` must be rejected at compile time (exit criterion
//! "错误字段类型编译失败").
//!
//! Run with `TRYBUILD=overwrite` to regenerate the expected `.stderr` files.

#[test]
fn compile_fail_cases() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/*.rs");
}
