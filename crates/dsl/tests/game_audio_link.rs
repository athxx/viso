//! The audio thread's host and its typed queues: a game's `send_audio`
//! reaches the `AudioCommands` hook on the audio thread before the next
//! block, the `AudioProcess` hook fills each block there, and the events it
//! sends with `block.send` reach the `AudioListener`s in the next frame. The
//! message types are the package's `@derive(AudioCommand)` and
//! `@derive(AudioEvent)` enums (`E2201`, `E2202`).

use std::rc::Rc;

use viso_behavior::game::{AudioHost, Scheduler};
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Module, Value, Vm};
use viso_dsl::frontend::{Compiled, Origin, compile_file_for};
use viso_dsl::hir::TargetProfile;

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["band".to_owned()],
        language: None,
    }
}

const IMPORTS: &str = "import viso::game::{AudioProcess, AudioCommands, AudioListener, \
                       AudioBlock, FrameUpdate, RenderFrame, send_audio};\n";

fn compiled(source: &str) -> Compiled {
    compile_file_for(
        &format!("{IMPORTS}{source}"),
        &origin(),
        Natives::standard(),
        TargetProfile::default(),
    )
}

fn errors(source: &str) -> Vec<(String, String)> {
    compiled(source)
        .errors()
        .map(|d| (d.code.to_string(), d.message.clone()))
        .collect()
}

const BAND: &str = r#"
@derive(AudioCommand)
enum Cue {
    stop;
    play(F32, I64);
}

@derive(AudioEvent)
enum Heard { tick; }

export system Synth implements AudioProcess + AudioCommands {
    @local state level: F32 = 0.0f32;
    @local state every = 0;
    @local state count = 0;

    action audio_command(command: Cue) {
        match command {
            Cue::stop => { level = 0.0f32; every = 0; },
            Cue::play(l, e) => { level = l; every = e; },
        }
    }

    action audio_process(block: AudioBlock) {
        for f in 0..block.frames() {
            for c in 0..block.channels() {
                block.write(c, f, level);
            }
            if every > 0 {
                count += 1;
                if count >= every {
                    count = 0;
                    let sent = block.send(Heard::tick);
                }
            }
        }
    }
}

export system Conductor implements FrameUpdate + AudioListener {
    @local state started = false;
    @local state heard = 0;

    action frame_update(frame: RenderFrame) {
        if !started {
            started = send_audio(Cue::play(0.25f32, 4));
        }
    }

    action audio_event(event: Heard) {
        heard += 1;
    }
}
"#;

fn module(source: &str) -> Rc<Module> {
    let compiled = compiled(source);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    Rc::new(compiled.behavior.bytecode().expect("verified bytecode"))
}

fn state(game: &Scheduler, system: &str, name: &str) -> Value {
    let module = game.vm().module();
    let component = module.component(system).expect("system");
    let index = module
        .systems()
        .iter()
        .position(|s| s.component == component)
        .expect("a system");
    let slot = module.layout(component).state(name).expect("state");
    game.instance(index).states()[slot].clone()
}

#[test]
fn commands_reach_the_audio_thread_and_events_come_back() {
    let module = module(BAND);
    let (mut host, link) = AudioHost::new(&module.encode(), &Natives::standard(), 2, 16, 48_000.0)
        .expect("builds")
        .expect("has audio systems");
    let mut vm = Vm::new(Rc::clone(&module), Budget::default());
    vm.link(&Natives::standard(), &[]).expect("links");
    let mut game = Scheduler::new(vm).expect("starts");
    game.attach_audio(link);

    let mut block = vec![1.0f32; 2 * 16];
    host.render(&mut block);
    assert!(block.iter().all(|&s| s == 0.0), "silent before a command");

    game.frame(1.0 / 60.0);
    assert_eq!(state(&game, "Conductor", "started"), Value::bool(true));
    // The audio thread is another thread: the host moves there once.
    let (host, block) = std::thread::spawn(move || {
        let mut block = vec![0.0f32; 2 * 8];
        host.render(&mut block);
        (host, block)
    })
    .join()
    .expect("rendered");
    assert!(block.iter().all(|&s| s == 0.25), "{block:?}");
    assert_eq!(host.channels(), 2);

    game.frame(1.0 / 60.0);
    assert_eq!(
        state(&game, "Conductor", "heard"),
        Value::Int(2),
        "one event every four frames, delivered in the next frame"
    );
    let link = game.audio().expect("attached");
    assert_eq!(link.status().blocks(), 2);
    assert_eq!(link.status().faults(), 0);
    assert_eq!(link.dropped_commands() + link.dropped_events(), 0);
}

#[test]
fn a_module_without_audio_systems_has_no_host() {
    let module = module(
        "export system Hud implements FrameUpdate {\n    action frame_update(frame: RenderFrame) {}\n}",
    );
    let host =
        AudioHost::new(&module.encode(), &Natives::standard(), 2, 16, 48_000.0).expect("builds");
    assert!(host.is_none());
}

#[test]
fn a_faulting_hook_is_silenced_and_its_fault_kept() {
    let module = module(
        "export system Synth implements AudioProcess {
    action audio_process(block: AudioBlock) {
        block.write(9, 0, 1.0f32);
    }
}",
    );
    let (mut host, link) = AudioHost::new(&module.encode(), &Natives::standard(), 2, 4, 48_000.0)
        .expect("builds")
        .expect("has audio systems");
    let mut block = vec![0.0f32; 8];
    host.render(&mut block);
    host.render(&mut block);
    assert_eq!(link.status().faults(), 1, "silenced after the first");
    let fault = link.status().take_fault().expect("kept");
    assert_eq!(fault.system, 0);
    assert!(
        fault.fault.message.contains("outside the audio block"),
        "{fault:?}"
    );
}

#[test]
fn the_message_types_are_the_packages_derived_enums() {
    let found = errors(
        "@derive(AudioCommand)\nenum Cue { say { text: String; } }\n\
         export system S implements AudioCommands {\n    action audio_command(command: Cue) {}\n}",
    );
    assert!(
        found
            .iter()
            .any(|(c, m)| c == "E2201" && m.contains("not plain")),
        "{found:?}"
    );
    let found = errors(
        "@derive(AudioCommand)\nenum Cue { wide(I64, I64, I64, I64, I64, I64, I64, I64, I64, I64, \
         I64, I64, I64, I64, I64, I64); }",
    );
    assert!(
        found
            .iter()
            .any(|(c, m)| c == "E2201" && m.contains("at most 16")),
        "{found:?}"
    );
    let found =
        errors("@derive(AudioCommand)\nenum A { a; }\n@derive(AudioCommand)\nenum B { b; }");
    assert!(found.iter().any(|(c, _)| c == "E2202"), "{found:?}");
    let found = errors("@derive(AudioEvent)\nenum Heard { level(F32); }");
    assert!(found.iter().any(|(c, _)| c == "E2201"), "{found:?}");
    let found = errors(
        "@derive(AudioCommand)\nenum Cue { a; }\n\
         export system S implements FrameUpdate {\n    action frame_update(frame: RenderFrame) { \
         let ok = send_audio(1); }\n}",
    );
    assert!(
        found.iter().any(|(c, _)| c == "E2103"),
        "the command type: {found:?}"
    );
    let found = errors(
        "export system S implements AudioProcess {\n    action audio_process(block: AudioBlock) { \
         let ok = send_audio(()); }\n}",
    );
    assert!(
        found
            .iter()
            .any(|(c, m)| c == "E9108" && m.contains("realtime")),
        "send_audio is not for the audio thread: {found:?}"
    );
}
