component Regions {
    state open = false;
    state mode = 0;
    state picked = 0;
    state items = [1, 2, 3];
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Row {
                width: 400dp;
                height: 20dp;
                Text { width: 20dp; height: 20dp; on click { open = !open; } }
                Text { width: 20dp; height: 20dp; on click { mode += 1; } }
                Text { width: 20dp; height: 20dp; on click { items = [3, 1, 2]; } }
                Text { width: 20dp; height: 20dp; on click { items = [1, 1]; } }
            }
            Column {
                width: 400dp;
                height: 60dp;
                if open preserve "panel" {
                    Text { width: 10dp; height: 10dp; }
                } else {
                    Text { width: 10dp; height: 10dp; }
                    Text { width: 10dp; height: 10dp; }
                }
            }
            Column {
                width: 400dp;
                height: 60dp;
                match mode {
                    0 => { Text { width: 10dp; height: 10dp; } },
                    1 => { Row { width: 10dp; height: 10dp; Text { width: 5dp; height: 5dp; } } },
                    _ => { },
                }
            }
            Row {
                width: 400dp;
                height: 20dp;
                for item in items key item {
                    Text { width: 20dp; height: 20dp; on click { picked = item; } }
                }
            }
        }
    }
}
