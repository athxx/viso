component Card {
    input title: String

    view {
        Column {
            gap: 8dp
            Text(title)
            slot
            Row {
                gap: 8dp
                slot actions
            }
        }
    }
}

export component Profile {
    state likes = 0

    view {
        Card {
            title: "Profile"
            Text("{likes}")
            actions {
                Button("Like") {
                    on click { likes += 1 }
                }
            }
        }
    }
}
