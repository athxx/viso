import viso::i18n::tr;

export component Inbox {
    state unread = 3;

    view {
        Column {
            width: 400dp;
            height: 120dp;
            Text { width: 400dp; height: 40dp; text: tr("inbox.title"); }
            Text { width: 400dp; height: 40dp; text: tr("inbox.unread", count: unread); }
            Button {
                width: 400dp;
                height: 40dp;
                text: tr("inbox.mark");
                on click { if unread > 0 { unread -= 1; } }
            }
        }
    }
}
