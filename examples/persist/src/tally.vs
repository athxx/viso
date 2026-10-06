export component Tally {
    @persist("tally.count")
    state count = 0;
    state taps = 0;

    view {
        Column {
            width: 300dp;
            height: 80dp;
            Text { width: 300dp; height: 40dp; text: format("{c} in all, {t} this run", c: count, t: taps); }
            Button {
                width: 300dp;
                height: 40dp;
                text: "Count";
                on click { count += 1; taps += 1; }
            }
        }
    }
}
