component Scroller {
    state count = 0;
    view {
        Scroll {
            width: 100dp;
            height: 50dp;
            Text { width: 100dp; height: 400dp; text: format("{}", count); }
        }
    }
}
