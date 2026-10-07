record Todo {
    id: I64;
    title: String;
    done: Bool = false;
}

export component TodoList {
    state todos: List<Todo> = [];
    state draft = "";
    state next_id = 1;

    action add() {
        if draft != "" {
            todos.push(Todo { id: next_id, title: draft });
            next_id += 1;
            draft = "";
        }
    }

    view {
        Column {
            gap: 8dp;
            Row {
                gap: 8dp;
                TextInput { bind value <=> draft; }
                Button {
                    text: "Add";
                    on click { add(); }
                }
            }
            for todo in todos key todo.id {
                Text { text: todo.title; }
            }
        }
    }
}
