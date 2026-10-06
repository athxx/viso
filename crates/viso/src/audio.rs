//! A game's audio on the device: the audio systems of its module — those
//! implementing `AudioProcess` or `AudioCommands` — run on the default output
//! device's callback, and the [`AudioLink`] they hand back attaches to the
//! game's scheduler (`Scheduler::attach_audio`) for `send_audio` and the
//! `AudioListener`s.

pub use viso_behavior::game::{AudioFault, AudioLink, AudioStatus};
pub use viso_platform::audio::{AudioError, AudioFormat, AudioOutput};

use viso_behavior::Module;
use viso_behavior::game::AudioHost;
use viso_behavior::native::Natives;

/// A game's audio systems rendering on the default output device; dropping
/// it stops them.
#[derive(Debug)]
pub struct GameAudio {
    output: AudioOutput,
}

impl GameAudio {
    /// Starts the audio systems of `module`, linked against `natives`, on
    /// the default output device, with the link the rest of the game talks
    /// to them through; `None` when the module has none.
    ///
    /// # Errors
    ///
    /// When the target has no audio output, the device does not open or
    /// start, or the audio systems do not build.
    pub fn start(
        module: &Module,
        natives: &Natives,
    ) -> Result<Option<(GameAudio, AudioLink)>, AudioError> {
        let mut output = AudioOutput::open()?;
        let format = output.format();
        let built = AudioHost::new(
            &module.encode(),
            natives,
            format.channels,
            format.max_frames,
            format.sample_rate,
        )
        .map_err(AudioError::Device)?;
        let Some((mut host, link)) = built else {
            return Ok(None);
        };
        output.start(move |block| host.render(block))?;
        Ok(Some((GameAudio { output }, link)))
    }

    /// What the device renders.
    pub fn format(&self) -> AudioFormat {
        self.output.format()
    }
}
