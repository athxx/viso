//! The audio block an `AudioProcess` hook fills on the audio thread: its
//! input and output samples by channel and frame, read and written in place
//! so a hook never allocates.

use std::cell::Cell;

use crate::native::{NativeError, NativeFunction, NativeId, NativeObject, Obj};

/// The identity of the `AudioProcess.audio_process` hook.
pub const AUDIO_PROCESS: NativeId = NativeId::of("viso::game::AudioProcess::audio_process");

/// One block of audio behind a `viso::game::AudioBlock` handle: `frames`
/// frames of `channels` channels at `sample_rate`, the input the hook reads
/// and the output it writes, each channel-major.
#[derive(Debug)]
pub struct AudioBlock {
    channels: usize,
    frames: usize,
    sample_rate: f64,
    input: Box<[Cell<f32>]>,
    output: Box<[Cell<f32>]>,
}

impl NativeObject for AudioBlock {
    const PATH: &'static str = "viso::game::AudioBlock";
}

impl AudioBlock {
    /// A silent block of `frames` frames of `channels` channels at
    /// `sample_rate`; the audio host allocates it once and reuses it.
    pub fn new(channels: usize, frames: usize, sample_rate: f64) -> AudioBlock {
        let silence = || (0..channels * frames).map(|_| Cell::new(0.0)).collect();
        AudioBlock {
            channels,
            frames,
            sample_rate,
            input: silence(),
            output: silence(),
        }
    }

    /// Sets the input sample of `channel` at `frame`.
    ///
    /// # Panics
    ///
    /// If either is out of range.
    pub fn set_input(&self, channel: usize, frame: usize, sample: f32) {
        self.input[self.at(channel, frame).expect("in range")].set(sample);
    }

    /// The output sample of `channel` at `frame`.
    ///
    /// # Panics
    ///
    /// If either is out of range.
    pub fn output(&self, channel: usize, frame: usize) -> f32 {
        self.output[self.at(channel, frame).expect("in range")].get()
    }

    /// Silences the output for the next block.
    pub fn clear_output(&self) {
        for sample in &self.output {
            sample.set(0.0);
        }
    }

    fn at(&self, channel: usize, frame: usize) -> Option<usize> {
        (channel < self.channels && frame < self.frames).then(|| channel * self.frames + frame)
    }

    fn index(&self, channel: i64, frame: i64) -> Result<usize, NativeError> {
        let (Ok(channel), Ok(frame)) = (usize::try_from(channel), usize::try_from(frame)) else {
            return Err(out_of_range());
        };
        self.at(channel, frame).ok_or_else(out_of_range)
    }
}

/// The fault of a sample index outside the block; only the faulting path
/// builds its message.
fn out_of_range() -> NativeError {
    NativeError::new("the channel or frame is outside the audio block")
}

/// A count as an `I64`.
fn count(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

pub(super) static AUDIO_BLOCK_METHODS: [NativeFunction; 5] = [
    crate::native!(fn "channels" |_cx, this: Obj<AudioBlock>| -> i64 { Ok(count(this.channels)) })
        .deterministic()
        .realtime_safe(),
    crate::native!(fn "frames" |_cx, this: Obj<AudioBlock>| -> i64 { Ok(count(this.frames)) })
        .deterministic()
        .realtime_safe(),
    crate::native!(fn "sample_rate" |_cx, this: Obj<AudioBlock>| -> f64 { Ok(this.sample_rate) })
        .deterministic()
        .realtime_safe(),
    crate::native!(fn "input" |_cx, this: Obj<AudioBlock>, channel: i64, frame: i64| -> f32 {
        Ok(this.input[this.index(channel, frame)?].get())
    })
    .deterministic()
    .realtime_safe(),
    crate::native!(action "write" |_cx, this: Obj<AudioBlock>, channel: i64, frame: i64, sample: f32| -> () {
        this.output[this.index(channel, frame)?].set(sample);
        Ok(())
    })
    .deterministic()
    .realtime_safe(),
];
