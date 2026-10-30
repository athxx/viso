export component Counter {
    state count = 0

    view {
        Column {
            gap: 8dp
            Row {
                gap: 4dp
                Text("Count")
                Text("{count}")
            }
            Button("Add") {
                on click { count += 1 }
            }
        }
    }
}
