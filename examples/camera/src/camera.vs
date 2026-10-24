export component Camera {
    state playing = true;
    state back = false;
    state shown = false;

    view {
        Column {
            width: 640dp;
            gap: 8dp;
            CameraView {
                facing: if back { CameraFacing::back } else { CameraFacing::front };
                width: 640dp;
                height: 360dp;
                bind playing <=> playing;
                on loaded(size) { shown = true; }
                semantics.label: "The camera's picture";
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
                    text: if back { "Front camera" } else { "Back camera" };
                    on click { back = !back; }
                }
                Text { text: if shown { "Capturing" } else { "Opening the camera" }; }
            }
        }
    }
}
