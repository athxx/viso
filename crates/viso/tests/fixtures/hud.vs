import viso::game::{FixedUpdate, FixedFrame}

system Score implements FixedUpdate {
    state points = 0
    @query fn score() -> I64 { points }
    action fixed_update(frame: FixedFrame) { points += 1 }
}

export component Hud {
    view {
        Column {
            width: 200dp
            height: 40dp
            Text("Score: {game.score()}") { width: 100dp; height: 20dp }
        }
    }
}
