component Card {
    input title: String
    @default slot content: Slot<Node>
    slot actions: SlotList<Node> = empty

    view {
        Column {
            gap: 8dp
            Text(title)
            SlotOutlet { slot: content }
            Row {
                gap: 8dp
                SlotOutlet { slot: actions }
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
            fill actions {
                Button("Like") {
                    on click { likes += 1 }
                }
            }
        }
    }
}
