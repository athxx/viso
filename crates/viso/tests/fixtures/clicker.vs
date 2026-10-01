component Clicker {
    state count = 0;
    view {
        Column {
            width: 200dp;
            height: 100dp;
            Text {
                width: 100dp;
                height: 50dp;
                on click { count += 1; }
                on key_down(event) { count += 10; }
            }
        }
    }
}
