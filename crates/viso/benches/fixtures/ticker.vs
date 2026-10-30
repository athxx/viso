export component Ticker {
    state count = 1000
    view {
        Column {
            width: 100%
            height: 100%
            padding: 8dp
            gap: 8dp
            Text("Tick") { width: 100dp; height: 24dp; on click { count += 1 } }
            Text("{count}") { width: 200dp; height: 24dp }
            Text("A steady page around one changing label") { width: 400dp; height: 24dp }
            Row {
                gap: 8dp
                Text("Inbox") { width: 120dp; height: 24dp }
                Text("Drafts") { width: 120dp; height: 24dp }
                Text("Archive") { width: 120dp; height: 24dp }
            }
            Text("Nothing else on it changes") { width: 400dp; height: 24dp }
        }
    }
}
