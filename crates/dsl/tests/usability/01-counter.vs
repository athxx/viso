export component Counter {
    state count = 0;

    view {
        Column {
            gap: 8dp;
            Row {
                gap: 4dp;
                Text { text: "Count"; }
                Text { text: format("{}", count); }
            }
            Button {
                text: "Add";
                on click { count += 1; }
            }
        }
    }
}
