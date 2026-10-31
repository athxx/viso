// The pieces every page is built from: cards, sections, tags, an empty
// state, facts and a page header, each a component whose callers fill its
// slots.

import viso::i18n::tr

// A raised surface holding whatever its caller puts in it.
export component Card {
    input padded: Bool = true

    view {
        Column {
            width: fill
            gap: 8dp
            padding: if padded { 16dp } else { 0dp }
            corner_radius: 14dp
            background: theme.colors.surface
            border: Border { width: 1dp, color: theme.colors.outline }
            shadow: Shadow { y: 2dp, blur: 8dp, color: #0000001f }
            clip: true
            slot
        }
    }
}

// A titled group of rows, with room at its end for actions.
export component Section {
    input title: String
    input caption: String = ""

    view {
        Column {
            width: fill
            gap: 10dp
            Row {
                width: fill
                align: Align::center
                gap: 8dp
                Column {
                    width: fill
                    gap: 2dp
                    Text(title) {
                        font_size: 17sp
                        font_weight: FontWeight::semibold
                        semantics.role: heading
                        semantics.heading_level: 2
                    }
                    if caption != "" {
                        Text(caption) { font_size: 13sp; color: theme.colors.muted }
                    }
                }
                slot actions
            }
            slot
        }
    }
}

// A page's title over its content, with tools at its end.
export component PageHeader {
    input title: String
    input subtitle: String = ""

    view {
        Row {
            width: fill
            align: Align::center
            gap: 12dp
            padding: EdgeInsets { top: 8dp, bottom: 8dp }
            Column {
                width: fill
                gap: 4dp
                Text(title) {
                    font_size: 26sp
                    font_weight: FontWeight::bold
                    semantics.role: heading
                    semantics.heading_level: 1
                }
                if subtitle != "" {
                    Text(subtitle) { font_size: 14sp; color: theme.colors.muted }
                }
            }
            slot tools
        }
    }
}

// A tag on a recipe: quieter than a chip, not pressable.
export component Tag {
    input label: String

    view {
        Row {
            padding: EdgeInsets { start: 8dp, end: 8dp, top: 3dp, bottom: 3dp }
            corner_radius: 8dp
            background: theme.colors.background
            Text(label) { font_size: 12sp; color: theme.colors.muted }
        }
    }
}

// What a page shows when it has nothing to list, and what to do about it.
export component EmptyState {
    input title: String
    input message: String

    view {
        Column {
            width: fill
            padding: 32dp
            gap: 8dp
            align: Align::center
            Text(title) { font_size: 18sp; font_weight: FontWeight::semibold }
            Text(message) {
                font_size: 14sp
                color: theme.colors.muted
                soft_wrap: true
                align: TextAlign::center
            }
            slot
        }
    }
}

// A key and its value on one line, the value at the end.
export component Fact {
    input label: String
    input value: String

    view {
        Row {
            width: fill
            gap: 8dp
            Text(label) { width: fill; color: theme.colors.muted; font_size: 13sp }
            Text(value) { font_size: 13sp; font_weight: FontWeight::semibold }
        }
    }
}

// A thin rule between rows.
export component Divider {
    view {
        Column { width: fill; height: 1dp; background: theme.colors.outline }
    }
}

// A confirmation bar: a message and one action, and a way to dismiss it.
export component Notice {
    input message: String
    input action_label: String = ""
    event acted()
    event dismissed()

    view {
        Row {
            width: fill
            gap: 12dp
            align: Align::center
            padding: EdgeInsets { start: 16dp, end: 8dp, top: 8dp, bottom: 8dp }
            corner_radius: 12dp
            background: theme.colors.foreground
            semantics.live: polite
            Text(message) { width: fill; color: theme.colors.background }
            if action_label != "" {
                Button(action_label) { on click { emit acted() } }
            }
            Button(tr("common.dismiss")) { on click { emit dismissed() } }
        }
    }
}

// A field the user types into: a well on the card, ringed while it has
// the focus.
export style Field for TextInput {
    background: theme.colors.background
    corner_radius: 8dp
    padding: EdgeInsets { start: 10dp, end: 10dp, top: 8dp, bottom: 8dp }
    border: Border { width: 1dp, color: theme.colors.outline }

    when focused {
        border: Border { width: 2dp, color: theme.colors.primary }
    }
}

// A number field, as a `Field` is.
export style NumberWell for NumberField {
    background: theme.colors.background
    corner_radius: 8dp
    padding: EdgeInsets { start: 10dp, end: 10dp, top: 8dp, bottom: 8dp }
    border: Border { width: 1dp, color: theme.colors.outline }

    when focused {
        border: Border { width: 2dp, color: theme.colors.primary }
    }
}
