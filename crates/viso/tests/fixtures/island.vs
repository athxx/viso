component Chart {
    input title: String
    input scale: I64 = 1
    event picked(at: I64)
    state hits = 0
    view {
        Column {
            width: 200dp
            height: 100dp
            Text {
                width: 100dp
                height: 50dp
                text: format("{} x{}", title, scale)
                on click { hits += 1; emit picked(hits * scale) }
            }
        }
    }
}

export component App {
    state name = "one"
    state last = 0
    view {
        Column {
            Text { text: format("picked {}", last) }
            Island {
                width: 200dp
                height: 100dp
                Chart { title: name; on picked(event) { last = event.at } }
            }
        }
    }
}
