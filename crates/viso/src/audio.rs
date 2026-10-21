//! A game's audio on the device: the audio systems of its module — those
//! implementing `AudioProcess` or `AudioCommands` — run on the default output
//! device's callback, and the [`AudioLink`] they hand back attaches to the
//! game's scheduler (`Scheduler::attach_audio`) for `send_audio` and the
//! `AudioListener`s. A game a view runs starts its audio here when its module
//! has audio systems, and a reload swaps them on the running device without
//! a gap ([`GameAudio::swap`]).

pub use viso_behavior::game::{AudioFault, AudioLink, AudioStatus};
pub use viso_platform::audio::{AudioError, AudioFormat, AudioOutput};

use viso_behavior::Module;
use viso_behavior::aot::NativeCode;
use viso_behavior::game::{AUDIO_COMMAND, AUDIO_PROCESS, AudioHost, AudioSwap};
use viso_behavior::native::Natives;
use viso_view::GameAudioDevice;

/// A game's audio systems rendering on the default output device; dropping
/// it stops them.
#[derive(Debug)]
pub struct GameAudio {
    output: AudioOutput,
    swap: AudioSwap,
}

impl GameAudio {
    /// Starts the audio systems of `module`, linked against `natives` and
    /// running `native` when given, on the default output device, with the link the rest of the game talks
    /// to them through; `None` when the module has none, the device left
    /// closed.
    ///
    /// # Errors
    ///
    /// When the target has no audio output, the device does not open or
    /// start, or the audio systems do not build.
    pub fn start(
        module: &Module,
        natives: &Natives,
        native: Option<&'static NativeCode>,
    ) -> Result<Option<(GameAudio, AudioLink)>, AudioError> {
        let audio = |s: &viso_behavior::System| {
            s.hook(AUDIO_PROCESS).is_some() || s.hook(AUDIO_COMMAND).is_some()
        };
        if !module.systems().iter().any(audio) {
            return Ok(None);
        }
        let mut output = AudioOutput::open()?;
        let format = output.format();
        let built = AudioHost::new(
            &module.encode(),
            natives,
            native,
            format.channels,
            format.max_frames,
            format.sample_rate,
        )
        .map_err(AudioError::Device)?;
        let Some((mut host, link)) = built else {
            return Ok(None);
        };
        let swap = host.swapper();
        output.start(move |block| host.render(block))?;
        Ok(Some((GameAudio { output, swap }, link)))
    }

    /// What the device renders.
    pub fn format(&self) -> AudioFormat {
        self.output.format()
    }

    /// Builds the audio systems of `module` on this thread and hands them to
    /// the device, which plays them from its next block on with no gap; the
    /// link stays valid. A module without audio systems silences it.
    ///
    /// # Errors
    ///
    /// When the systems do not build: the device plays on what it played.
    pub fn swap(&mut self, module: &Module, natives: &Natives) -> Result<(), String> {
        self.swap.swap(&module.encode(), natives)
    }
}

impl GameAudioDevice for GameAudio {
    fn reload(&mut self, module: &Module, natives: &Natives) -> Result<(), String> {
        self.swap(module, natives)
    }

    fn collect(&mut self) {
        self.swap.collect();
    }
}

/// Starts the audio of the games this thread's views run on the default
/// output device; a target without one plays them silent.
pub(crate) fn install() {
    viso_view::install_game_audio(Some(|module, natives, native| {
        match GameAudio::start(module, natives, native) {
            Ok(started) => Ok(
                started.map(|(audio, link)| (Box::new(audio) as Box<dyn GameAudioDevice>, link))
            ),
            Err(AudioError::Unsupported) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }));
}
