import viso::i18n::tr

export component Counter {
    state count = 0

    view {
        Row {
            width: 320dp
            height: 40dp
            gap: 16dp
            Text(tr("counter.count", count: count)) { width: 152dp; height: 40dp }
            Button(tr("counter.add")) {
                width: 152dp
                height: 40dp
                on click { count += 1 }
            }
        }
    }
}
