component Shell {
    input count: I32;
    view { Text { text: format("{n}", n: count); } }
}
