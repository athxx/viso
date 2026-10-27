fn main() {
    let _ = |cx: &mut viso::ui::BuildCx<'_>| {
        viso::view!("input_shell.vs", count: "seven")(cx);
    };
}
