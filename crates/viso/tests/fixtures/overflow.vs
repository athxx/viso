component Overflow {
    state down = 0
    view {
        Row {
            Column {
                width: 120dp
                height: 50dp
                Button("Up") { width: 120dp; height: 40dp }
                Button("Down") { width: 120dp; height: 40dp; on click { down += 1 } }
                Text("pressed {down}") { width: 120dp; height: 20dp }
            }
            Scroll {
                width: 120dp
                height: 40dp
                Column {
                    Button("Top") { width: 120dp; height: 40dp }
                    Button("Under") { width: 120dp; height: 40dp }
                }
            }
        }
    }
}
