import viso::i18n::tr;

export component App {
    state clicks = 0;

    view {
        Column {
            width: 320dp;
            height: 96dp;
            gap: 16dp;
            Text { width: 320dp; height: 40dp; text: tr("app.clicks", count: clicks); }
            Button {
                width: 320dp;
                height: 40dp;
                text: tr("app.click");
                on click { clicks += 1; }
            }
        }
    }
}
