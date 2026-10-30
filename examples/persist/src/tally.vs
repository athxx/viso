export component Tally {
    @persist("tally.count")
    state count = 0
    state taps = 0

    view {
        Column {
            width: 300dp
            height: 80dp
            Text("{count} in all, {taps} this run") {
                width: 300dp
                height: 40dp
            }
            Button("Count") {
                width: 300dp
                height: 40dp
                on click { count += 1; taps += 1 }
            }
        }
    }
}
