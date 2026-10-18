export component Counter {
    state count = 0;

    view {
        Column {
            width: 420dp;
            height: 96dp;
            Text { width: 420dp; height: 40dp; text: format("Count: {c}", c: count); }
            Button {
                width: 160dp;
                height: 40dp;
                text: "Increment";
                on click { count += 1; }
            }
        }
    }
}
