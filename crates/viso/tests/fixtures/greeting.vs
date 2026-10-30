component Greeting {
    input name: String = "world"
    input count: I32 = 1

    view {
        Column {
            node line: Text { text: "{count} x hello, {name}" }
        }
    }
}
