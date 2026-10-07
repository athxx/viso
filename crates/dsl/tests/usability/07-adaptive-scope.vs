component Panel {
    view {
        Column {
            if env.size_class == SizeClass::Compact {
                Text { text: "Narrow"; }
            } else {
                Text { text: "Wide"; }
            }
        }
    }
}

export component Page {
    view {
        Row {
            Column {
                width: 240dp;
                AdaptiveScope { Panel {} }
            }
            Column {
                width: 900dp;
                AdaptiveScope { Panel {} }
            }
        }
    }
}
