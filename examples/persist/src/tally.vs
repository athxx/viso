export component Tally {
    @persist("tally.count")
    state count = 0
    state taps = 0

    view {
        Column {
            padding: 16dp
            gap: 8dp
            Text("{count} in all, {taps} this run")
            Button("Count") {
                min_width: 96dp; min_height: 32dp
                on click { count += 1; taps += 1 }
            }
        }
    }
}
