export component Counter {
    state count = 0

    view {
        Column {
            width: 420dp
            height: 96dp
            Text("Count: {count}") { width: 420dp; height: 40dp }
            Button("Increment") {
                width: 160dp
                height: 40dp
                on click { count += 1 }
            }
        }
    }
}
