// The inbox: its draft count lives as long as the page, under every page
// pushed over it.
component Inbox {
    state drafts = 0

    view {
        Column {
            padding: 16dp
            gap: 8dp
            Text("{drafts} drafts") { width: 200dp; height: 24dp }
            Button("New draft") { width: 120dp; height: 32dp; on click { drafts += 1 } }
        }
    }
}
