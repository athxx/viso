component Counter {
    state count = 0
    state enabled = true
    view {
        Column {
            width: 120dp
            Text { text: "{count}" }
            Text { visible: enabled }
        }
    }
}
