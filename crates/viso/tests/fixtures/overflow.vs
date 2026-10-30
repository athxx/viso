component Overflow {
    state down = 0
    view {
        Row {
            Column {
                width: 120dp
                height: 50dp
                Button { width: 120dp; height: 40dp; text: "Up" }
                Button { width: 120dp; height: 40dp; text: "Down"; on click { down += 1 } }
                Text { width: 120dp; height: 20dp; text: "pressed {down}" }
            }
            Scroll {
                width: 120dp
                height: 40dp
                Column {
                    Button { width: 120dp; height: 40dp; text: "Top" }
                    Button { width: 120dp; height: 40dp; text: "Under" }
                }
            }
        }
    }
}
