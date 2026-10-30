export component Ticker {
    state count = 1000
    view {
        Column {
            width: 100%
            height: 100%
            padding: 8dp
            gap: 8dp
            Text { width: 100dp; height: 24dp; text: "Tick"; on click { count += 1 } }
            Text { width: 200dp; height: 24dp; text: format("{}", count) }
            Text { width: 400dp; height: 24dp; text: "A steady page around one changing label" }
            Row {
                gap: 8dp
                Text { width: 120dp; height: 24dp; text: "Inbox" }
                Text { width: 120dp; height: 24dp; text: "Drafts" }
                Text { width: 120dp; height: 24dp; text: "Archive" }
            }
            Text { width: 400dp; height: 24dp; text: "Nothing else on it changes" }
        }
    }
}
