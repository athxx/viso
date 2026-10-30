import viso::i18n::tr

export component Inbox {
    state unread = 3

    view {
        Column {
            width: 400dp
            height: 120dp
            Text(tr("inbox.title")) { width: 400dp; height: 40dp }
            Text(tr("inbox.unread", count: unread)) { width: 400dp; height: 40dp }
            Button(tr("inbox.mark")) {
                width: 400dp
                height: 40dp
                on click { if unread > 0 { unread -= 1 } }
            }
        }
    }
}
