// A message, by its id.
component Message {
    input id: I64

    view {
        Column {
            padding: 16dp
            Text("Message #{id}")
        }
    }
}
