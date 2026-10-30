import viso::i18n::tr

export component App {
    state clicks = 0

    view {
        Column {
            width: 320dp
            height: 96dp
            gap: 16dp
            Text(tr("app.clicks", count: clicks)) { width: 320dp; height: 40dp }
            Button(tr("app.click")) {
                width: 320dp
                height: 40dp
                on click { clicks += 1 }
            }
        }
    }
}
