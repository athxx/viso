component Logger {
    state count = 0
    state log = ""
    view {
        Column {
            width: 200dp
            height: 100dp
            Text(log) { width: 100dp; height: 50dp; on click { count += 1; log = "one" } }
        }
    }
}
