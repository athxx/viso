//! The realtime rules of an `AudioProcess` hook (`E9108`): it and every
//! callable it reaches build nothing on the heap, start no task, call only
//! realtime-safe natives, do not recurse and loop only over ranges bounded
//! by constants and the block's size; an `AudioProcess` system implements no
//! other trait. The hook fills an `AudioBlock` in place.

use std::rc::Rc;

use viso_behavior::game::{AUDIO_PROCESS, AudioBlock};
use viso_behavior::native::{NativeValue, Natives, Obj};
use viso_behavior::{Budget, Vm};
use viso_dsl::frontend::{Compiled, Origin, compile_file_for};
use viso_dsl::hir::TargetProfile;

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["synth".to_owned()],
        language: None,
    }
}

fn compiled(source: &str) -> Compiled {
    let source = format!(
        "import viso::game::{{AudioProcess, AudioBlock, FrameUpdate, RenderFrame}};\n\
         import viso::text;\n{source}"
    );
    compile_file_for(
        &source,
        &origin(),
        Natives::standard(),
        TargetProfile::default(),
    )
}

/// Each `E9108` compiling `source` reports: its message and notes.
fn realtime(source: &str) -> Vec<(String, Vec<String>)> {
    let compiled = compiled(source);
    let others: Vec<_> = compiled.errors().filter(|d| d.code != "E9108").collect();
    assert!(others.is_empty(), "{others:#?}");
    compiled
        .errors()
        .map(|d| (d.message.clone(), d.notes.clone()))
        .collect()
}

/// A synth whose hook runs `body`, with `fn`s `helpers` beside it.
fn synth(body: &str, helpers: &str) -> String {
    format!(
        "const TAPS: I64 = 4;\n{helpers}\n\
         export system Synth implements AudioProcess {{\n\
             @local state gain: F32 = 0.5f32;\n\
             @local state name = \"a\";\n\
             @local state n = 2;\n\
             action audio_process(block: AudioBlock) {{\n{body}\n}}\n\
         }}\n"
    )
}

const MIX: &str = "
    for c in 0..block.channels() {
        for f in 0..block.frames() {
            block.write(c, f, scale(block.input(c, f), gain));
        }
    }
    for t in 0..(TAPS * 2) {
        gain = gain * 1.0f32;
    }";

const SCALE: &str = "fn scale(x: F32, by: F32) -> F32 { x * by }";

#[test]
fn a_realtime_hook_that_fills_its_block_in_place_compiles() {
    assert_eq!(realtime(&synth(MIX, SCALE)), []);
}

#[test]
fn the_hook_and_what_it_reaches_follow_the_realtime_rules() {
    let one = |body: &str| -> String {
        let found = realtime(&synth(body, ""));
        assert_eq!(found.len(), 1, "{body}: {found:#?}");
        found[0].0.clone()
    };
    assert!(one("let xs = [1, 2];").contains("builds a list"));
    assert!(one("name = name + \"b\";").contains("`String`"));
    assert!(one("let p = (1, 2);").contains("record, tuple"));
    assert!(one("let f = |x: I64| x + 1;").contains("closure"));
    assert!(one("let k = text::len(name);").contains("`len` is not realtime-safe"));
    assert!(one("while gain > 1.0f32 { gain = gain * 0.5f32; }").contains("loops only"));
    assert!(one("loop { break; }").contains("loops only"));
    assert!(one("for i in 0..n { gain = gain * 1.0f32; }").contains("loops only"));

    let reached = realtime(&synth(
        "let x = depth(3);",
        "fn depth(k: I64) -> I64 { if k <= 0 { 0 } else { depth(k - 1) + 1 } }",
    ));
    assert_eq!(reached.len(), 1, "{reached:#?}");
    assert!(reached[0].0.contains("recurses"));
    assert!(
        reached[0].1[0].contains("`depth` runs on the audio thread: `Synth.audio_process`"),
        "{reached:#?}"
    );
}

#[test]
fn an_audio_system_implements_no_other_trait() {
    let found = realtime(
        "export system Synth implements AudioProcess + FrameUpdate {
    action audio_process(block: AudioBlock) {}
    action frame_update(frame: RenderFrame) {}
}",
    );
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(found[0].0.contains("implements no other trait"));
}

#[test]
fn the_rules_hold_only_on_the_audio_thread() {
    let found = realtime(
        "export system Hud implements FrameUpdate {
    @local state xs: List<I64> = [];
    action frame_update(frame: RenderFrame) {
        while xs.len() < 3 { xs = xs + [1]; }
    }
}",
    );
    assert_eq!(found, []);
}

#[test]
fn the_hook_fills_an_audio_block() {
    let compiled = compiled(&synth(MIX, SCALE));
    assert!(!compiled.has_errors(), "{:#?}", compiled.diagnostics);
    let module = Rc::new(compiled.behavior.bytecode().expect("verified bytecode"));
    let system = &module.systems()[0];
    let hook = system
        .hooks
        .iter()
        .find(|(id, _)| *id == AUDIO_PROCESS)
        .map(|&(_, action)| action)
        .expect("the hook is bound");
    let mut vm = Vm::new(Rc::clone(&module), Budget::default());
    vm.link(&Natives::standard(), &[]).expect("links");
    let mut instance = vm.instantiate(system.component, []).expect("instantiates");
    let block = Obj::new(AudioBlock::new(2, 3, 48_000.0));
    for c in 0..2 {
        for f in 0..3 {
            block.set_input(c, f, (c * 3 + f) as f32);
        }
    }
    vm.call(&mut instance, hook, &[block.clone().into_value()])
        .expect("runs");
    for c in 0..2 {
        for f in 0..3 {
            assert_eq!(block.output(c, f), (c * 3 + f) as f32 * 0.5);
        }
    }
}
