component Counter {
    state count = 0;
    state enabled = true;
    view {
        Column {
            width: 120px;
            Text { text: count; }
            Leaf { visible: enabled; }
        }
    }
}
