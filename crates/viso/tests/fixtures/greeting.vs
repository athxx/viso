component Greeting {
    input name: String = "world"
    input count: I32 = 1

    view {
        Column {
            node line: Text("{count} x hello, {name}")
        }
    }
}
