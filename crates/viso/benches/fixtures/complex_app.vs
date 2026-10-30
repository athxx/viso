component TaskRow {
    input id: I64
    state done = false
    view {
        Row {
            width: 100%
            height: 28dp
            gap: 8dp
            CheckBox { width: 20dp; height: 20dp; bind checked <=> done }
            Text("Task {id}") { width: 200dp; height: 20dp }
            Button("Edit") { width: 64dp; height: 24dp }
        }
    }
}

export component Workspace {
    state items = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
        21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40]
    state level: F32 = 0.5
    state compact = false
    state query = ""
    view {
        Column {
            width: 100%
            height: 100%
            padding: 8dp
            gap: 8dp
            Row {
                height: 32dp
                gap: 8dp
                Text("Tasks") { width: 160dp; height: 24dp }
                TextInput {
                    width: 200dp
                    height: 24dp
                    placeholder: "Search"
                    bind value <=> query
                }
                Toggle("Compact") { bind checked <=> compact }
                Slider { width: 160dp; bind value <=> level }
                Button("Add") {
                    width: 80dp
                    height: 24dp
                    on click { items = [41, 1, 2, 3] }
                }
            }
            Scroll {
                width: 100%
                height: 480dp
                Column {
                    gap: 2dp
                    for item in items key item {
                        TaskRow { id: item }
                    }
                }
            }
            Text("Level {level}") { width: 400dp; height: 20dp }
        }
    }
}
