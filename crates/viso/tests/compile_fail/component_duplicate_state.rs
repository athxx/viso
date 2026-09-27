viso::component! {
    Counter {
        state count = 0;
        state count = 1;
        view { Text { text: format("{}", count); } }
    }
}

fn main() {}
