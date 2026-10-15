import viso::i18n::tr;

export component Counter {
    state count = 0;

    view {
        Row {
            width: 320dp;
            height: 40dp;
            gap: 16dp;
            Text { width: 152dp; height: 40dp; text: tr("counter.count", count: count); }
            Button {
                width: 152dp;
                height: 40dp;
                text: tr("counter.add");
                on click { count += 1; }
            }
        }
    }
}
