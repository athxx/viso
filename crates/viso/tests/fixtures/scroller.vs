component Scroller {
    state count = 0
    view {
        Scroll {
            width: 100dp
            height: 50dp
            Text("{count}") { width: 100dp; height: 400dp }
        }
    }
}
