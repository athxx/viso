fn main() {
    // A fragment is stateless: it reads no Rust variable.
    let label = "Save";
    let _build = viso::ui! {
        Text { text: label; }
    };
}
