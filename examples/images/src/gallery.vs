export component Gallery {
    state night = false

    view {
        Column {
            gap: 8dp
            Image(if night { asset("photos/night.jpg") } else { asset("photos/day.png") }) {
                fit: cover
                width: 320dp
                height: 160dp
                semantics.label: "Today's photo"
            }
            Row {
                gap: 8dp
                align: center
                Svg(asset("icons/star.svg")) { tint: #f5b301; width: 24dp; height: 24dp }
                Button(if night { "Day" } else { "Night" }) {
                    min_width: 64dp; min_height: 32dp
                    on click { night = !night }
                }
            }
        }
    }
}
