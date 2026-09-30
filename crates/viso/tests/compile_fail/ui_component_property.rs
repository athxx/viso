viso::component! {
    Tally {
        view { Text { } }
    }
}

fn main() {
    let _build = viso::ui! {
        Column {
            Tally { width: 4dp; }
        }
    };
}
