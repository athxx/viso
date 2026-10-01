component Logger {
    state count = 0;
    state log = "";
    view {
        Column {
            width: 200dp;
            height: 100dp;
            Text { width: 100dp; height: 50dp; text: log; on click { count += 1; log = "one"; } }
        }
    }
}
