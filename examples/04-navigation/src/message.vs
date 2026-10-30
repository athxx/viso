// A message, by its id.
component Message {
    input id: I64

    view {
        Column {
            padding: 16dp
            Text { width: 200dp; height: 24dp; text: format("Message #{id}", id: id) }
        }
    }
}
