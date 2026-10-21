//! A HUD reads the game its view's root runs (ADR 0045): a binding on a
//! system's `@query` shows its value from the first frame and updates only
//! when the value changes, a region on `game.is_paused()` switches as the
//! view pauses and resumes the game, under hot reload and from the release
//! package; a misused `@query` or `game` is reported.

mod support;

use support::{Rt, origin};
use viso_dsl::frontend::compile_file;
use viso_view::game_session;

const TICK: f64 = 1.0 / 60.0;

const SOURCE: &str = r#"
import viso::game::{FixedUpdate, FixedFrame};

system Score implements FixedUpdate {
    state points = 0;
    @query fn score() -> I64 { points / 2 }
    action fixed_update(frame: FixedFrame) { points += 1; }
}

export component Hud {
    view {
        Column {
            width: 200dp;
            height: 100dp;
            Text {
                width: 100dp;
                height: 20dp;
                text: "toggle";
                on click {
                    if game.is_paused() { game.resume(); } else { game.pause(); }
                }
            }
            Text { width: 100dp; height: 20dp; text: format("Score: {}", game.score()); }
            Column {
                width: 100dp;
                height: 20dp;
                if game.is_paused() { Text { width: 10dp; height: 10dp; text: "Paused"; } }
            }
        }
    }
}
"#;

/// The text of the root's `index`th child.
fn text(rt: &Rt, index: usize) -> String {
    let root = rt.root.expect("mounted");
    let node = rt.children(root)[index];
    rt.store
        .text_request(node)
        .map(|r| r.text.clone())
        .unwrap_or_default()
}

/// One frame of the game, then the view's.
fn tick(rt: &mut Rt) {
    rt.store.run_frame_clients(TICK);
    rt.frame();
}

fn plays(mut rt: Rt) {
    assert!(viso_view::take_game_faults().is_empty());
    assert_eq!(text(&rt, 1), "Score: 0", "the first frame reads the game");
    tick(&mut rt);
    assert_eq!(text(&rt, 1), "Score: 0", "1 / 2");
    tick(&mut rt);
    assert_eq!(text(&rt, 1), "Score: 1");
    assert!(rt.region(2).is_empty());

    rt.click(0);
    tick(&mut rt);
    let root = rt.root.expect("mounted");
    let session = game_session(&rt.store, root).expect("the root runs the game");
    assert!(session.borrow().scheduler().clock().paused());
    assert_eq!(rt.region(2).len(), 1, "the paused banner shows");
    for _ in 0..4 {
        tick(&mut rt);
    }
    assert_eq!(text(&rt, 1), "Score: 1", "a paused game keeps its score");

    rt.click(0);
    tick(&mut rt);
    tick(&mut rt);
    assert!(rt.region(2).is_empty());
    assert_eq!(text(&rt, 1), "Score: 2");
    assert!(viso_view::take_game_faults().is_empty());
}

#[test]
fn a_hud_reads_and_pauses_its_game_under_hot_reload() {
    plays(Rt::mount(SOURCE));
}

#[test]
fn a_packaged_hud_reads_and_pauses_its_game() {
    plays(Rt::packaged(SOURCE));
}

#[test]
fn a_reload_keeps_the_hud_on_the_running_game() {
    let mut rt = Rt::mount(SOURCE);
    for _ in 0..4 {
        tick(&mut rt);
    }
    assert_eq!(text(&rt, 1), "Score: 2");
    rt.reload(&SOURCE.replace("Score: {}", "Points: {}"));
    assert_eq!(text(&rt, 1), "Points: 2", "the same game, read at once");
    tick(&mut rt);
    tick(&mut rt);
    assert_eq!(text(&rt, 1), "Points: 3");
}

fn codes(source: &str) -> Vec<String> {
    compile_file(source, &origin())
        .errors()
        .map(|d| d.code.to_string())
        .collect()
}

#[test]
fn a_misused_query_or_game_is_reported() {
    let with = |system: &str, view: &str| {
        format!(
            "import viso::game::{{FixedUpdate, FixedFrame}};\n{system}\n\
             export component Hud {{ view {{ Text {{ text: {view}; }} }} }}"
        )
    };
    let base = "system S implements FixedUpdate { state n = 0; \
                @query fn count() -> I64 { n } action fixed_update(frame: FixedFrame) {} }";
    assert!(codes(&with(base, r#"format("{}", game.count())"#)).is_empty());
    assert!(
        codes(&with(base, r#"format("{}", game.m())"#)).contains(&"E2001".to_string()),
        "no such query"
    );
    assert!(codes(&with(base, r#"format("{}", game)"#)).contains(&"E2001".to_string()));
    let params = "system S implements FixedUpdate { state n = 0; \
                  @query fn count(k: I64) -> I64 { n + k } action fixed_update(frame: FixedFrame) {} }";
    assert!(codes(&with(params, r#""x""#)).contains(&"E9112".to_string()));
    let unit = "system S implements FixedUpdate { state n = 0; \
                @query fn count() { } action fixed_update(frame: FixedFrame) {} }";
    assert!(codes(&with(unit, r#""x""#)).contains(&"E9112".to_string()));
    let twice = "system S implements FixedUpdate { state n = 0; \
                 @query fn count() -> I64 { n } action fixed_update(frame: FixedFrame) {} }\n\
                 system T implements FixedUpdate { state n = 0; \
                 @query fn count() -> I64 { n } action fixed_update(frame: FixedFrame) {} }";
    assert!(codes(&with(twice, r#""x""#)).contains(&"E9112".to_string()));
    let on_state = "system S implements FixedUpdate { @query state n = 0; \
                    action fixed_update(frame: FixedFrame) {} }";
    assert!(codes(&with(on_state, r#""x""#)).contains(&"E9112".to_string()));
    let no_game = r#"export component Hud { view { Text { text: format("{}", game.tick()); } } }"#;
    assert!(codes(no_game).contains(&"E2001".to_string()), "no system");
    let in_binding = with(base, r#"format("{}", { game.pause(); 1 })"#);
    assert!(!codes(&in_binding).is_empty(), "an action in a binding");
}
