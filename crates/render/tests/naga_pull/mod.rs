//! Shared by the tests that run `naga`-translated WGSL on the Metal device.

/// Remove the pulled-vertex bounds checks: they compare the index with a
/// buffer-size table the Metal backend has no reason to bind, and the renderer
/// never draws past the data it wrote.
pub fn strip_pull_bounds(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(start) = rest.find("(_buffer_sizes.buffer_size") {
        let Some(len) = rest[start..].find(')') else {
            break;
        };
        // `index < (_buffer_sizes.buffer_sizeN / stride)`: drop back to the
        // start of the comparison.
        let head = &rest[..start];
        let cut = head.rfind('<').expect("a bounds check is a comparison");
        let lhs = head[..cut].trim_end();
        let lhs_start = lhs
            .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
            .map_or(0, |i| i + 1);
        out.push_str(&lhs[..lhs_start]);
        out.push_str("true");
        rest = &rest[start + len + 1..];
    }
    out.push_str(rest);
    out
}
