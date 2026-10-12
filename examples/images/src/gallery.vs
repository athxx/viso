export component Gallery {
    state night = false;

    view {
        Column {
            width: 320dp;
            height: 240dp;
            gap: 8dp;
            Image {
                source: if night { asset("photos/night.jpg") } else { asset("photos/day.png") };
                fit: ImageFit::cover;
                width: 320dp;
                height: 160dp;
                semantics.label: "Today's photo";
            }
            Row {
                gap: 8dp;
                Svg { source: asset("icons/star.svg"); tint: #f5b301; width: 24dp; height: 24dp; }
                Button {
                    width: 120dp;
                    height: 32dp;
                    text: if night { "Day" } else { "Night" };
                    on click { night = !night; }
                }
            }
        }
    }
}
