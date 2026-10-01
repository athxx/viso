component Panel {
    view {
        Column {
            width: 100dp;
            height: 40dp;
            if env.size_class == SizeClass::Compact {
                Text { width: 10dp; height: 10dp; }
            } else {
                Text { width: 10dp; height: 10dp; }
                Text { width: 10dp; height: 10dp; }
            }
        }
    }
}

export component Shelf {
    state shown = false;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Text { width: 20dp; height: 20dp; on click { shown = !shown; } }
            Column {
                width: 400dp;
                height: 100dp;
                if shown {
                    Panel {}
                }
            }
        }
    }
}
