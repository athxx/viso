export component Editor {
    state text = "";

    view {
        Column {
            gap: 8dp;
            Text { text: "Notes"; }
            TextInput { bind value <=> text; }
        }
    }
}
