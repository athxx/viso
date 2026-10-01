component TodoItem {
    input id: I64;
    event toggled(id: I64);
    view {
        Row {
            width: 400dp;
            height: 20dp;
            Text { width: 20dp; height: 20dp; on click { emit toggled(id); } }
        }
    }
}

export component TodoApp {
    state items = [1, 2, 3];
    state done = 0;
    state show = true;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Row {
                width: 400dp;
                height: 20dp;
                Text { width: 20dp; height: 20dp; on click { items = [3, 1, 2]; } }
                Text { width: 20dp; height: 20dp; on click { items = [3, 1, 2, 4]; } }
                Text { width: 20dp; height: 20dp; on click { items = [2, 4]; } }
                Text { width: 20dp; height: 20dp; on click { show = !show; } }
            }
            Column {
                width: 400dp;
                height: 80dp;
                for item in items key item {
                    TodoItem { id: item; on toggled(event) { done = event.id; } }
                }
            }
            Column {
                width: 400dp;
                height: 20dp;
                if show {
                    Text { width: 30dp; height: 20dp; }
                } else {
                    Text { width: 10dp; height: 10dp; }
                    Text { width: 10dp; height: 10dp; }
                }
            }
            Column {
                width: 400dp;
                height: 20dp;
                match done {
                    0 => { },
                    2 => { Text { width: 50dp; height: 20dp; } },
                    _ => { Text { width: 5dp; height: 5dp; } },
                }
            }
        }
    }
}
