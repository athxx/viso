export component Panel {
    state level: F32 = 0.25;
    state presses = 0;

    view {
        Column {
            padding: 16dp;
            gap: 12dp;

            Gauge {
                label: "Volume";
                bind level <=> level;
                width: 200dp;
                corner_radius: 6dp;
                on pressed(at) { presses += 1; }
            }
            Text { width: 200dp; height: 24dp; text: format("{n}", n: presses); }
        }
    }
}
