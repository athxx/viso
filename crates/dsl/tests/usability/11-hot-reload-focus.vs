export component Editor {
    state text = ""

    view {
        Column {
            gap: 8dp
            Text("Notes")
            TextInput { bind value <=> text }
        }
    }
}
