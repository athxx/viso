//! A game's audio systems on the real default output device: the host
//! renders blocks on the device's callback and the link reports them. Built
//! for the targets with an audio backend, and runs where there is a device;
//! elsewhere there is nothing to render to.

#![cfg(any(
    target_vendor = "apple",
    target_os = "windows",
    target_os = "linux",
    target_os = "android"
))]

use std::rc::Rc;
use std::time::{Duration, Instant};

use viso::audio::{AudioError, GameAudio};
use viso_behavior::native::Natives;
use viso_dsl::frontend::{Origin, compile_file_for};
use viso_dsl::hir::TargetProfile;

const SYNTH: &str = "
import viso::game::{AudioProcess, AudioBlock};

export system Hush implements AudioProcess {
    action audio_process(block: AudioBlock) {
        for c in 0..block.channels() {
            for f in 0..block.frames() {
                // Silence: the test must not make a sound.
                block.write(c, f, 0.0f32);
            }
        }
    }
}
";

#[test]
fn the_audio_systems_render_on_the_device() {
    let compiled = compile_file_for(
        SYNTH,
        &Origin {
            package: "app".into(),
            module: vec!["hush".into()],
            language: None,
        },
        Natives::standard(),
        TargetProfile::default(),
    );
    assert!(!compiled.has_errors(), "{:#?}", compiled.diagnostics);
    let module = Rc::new(compiled.behavior.bytecode().expect("bytecode"));
    let (audio, link) = match GameAudio::start(&module, &Natives::standard()) {
        Ok(started) => started.expect("an audio system"),
        Err(AudioError::Unsupported) => return,
        // A machine without an output device has nothing to render to.
        Err(AudioError::Device(why)) => {
            eprintln!("no audio device: {why}");
            return;
        }
    };
    assert!(audio.format().sample_rate > 0.0);
    let deadline = Instant::now() + Duration::from_secs(2);
    while link.status().blocks() < 3 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(link.status().blocks() >= 3, "{}", link.status().blocks());
    assert_eq!(
        link.status().faults(),
        0,
        "{:?}",
        link.status().take_fault()
    );
    drop(audio);
    let stopped = link.status().blocks();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        link.status().blocks(),
        stopped,
        "dropping it stops the device"
    );
}
