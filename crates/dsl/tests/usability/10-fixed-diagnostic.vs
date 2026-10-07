export component Total {
    state count: I64 = 0;
    state scale: F64 = 1.5;

    computed shown: F64 = count as F64 * scale;

    view {
        Text { text: format("{}", shown); }
    }
}
