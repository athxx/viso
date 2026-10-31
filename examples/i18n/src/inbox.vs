import viso::i18n::tr

export component Inbox {
    state unread = 3

    view {
        Column {
            padding: 16dp
            gap: 8dp
            Text(tr("inbox.title"))
            Text(tr("inbox.unread", count: unread))
            Button(tr("inbox.mark")) { on click { if unread > 0 { unread -= 1 } } }
        }
    }
}
