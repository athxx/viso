// The inbox: its draft count lives as long as the page, under every page
// pushed over it.
component Inbox {
    state drafts = 0

    view {
        Column {
            padding: 16dp
            gap: 8dp
            Text("{drafts} drafts")
            Button("New draft") { on click { drafts += 1 } }
        }
    }
}
