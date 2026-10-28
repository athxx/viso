component Feedback {
    state count = 0;
    view {
        Text { width: 120dp; height: 40dp; on click { count += 1; } }
    }
    effect echo when (count) run EffectRun::change {
        transaction { count += 1; }
    }
}
