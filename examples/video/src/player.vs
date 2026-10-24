export component Player {
    state playing = false;
    state looping = true;

    view {
        Column {
            width: 640dp;
            gap: 8dp;
            Video {
                source: asset("clip.mp4");
                width: 640dp;
                height: 360dp;
                controls: true;
                bind playing <=> playing;
                looping: looping;
                semantics.label: "A test pattern with a tone";
            }
            Row {
                gap: 8dp;
                Button {
                    width: 120dp;
                    height: 32dp;
                    text: if playing { "Pause" } else { "Play" };
                    on click { playing = !playing; }
                }
                Button {
                    width: 160dp;
                    height: 32dp;
                    text: if looping { "Play once" } else { "Loop" };
                    on click { looping = !looping; }
                }
            }
        }
    }
}
