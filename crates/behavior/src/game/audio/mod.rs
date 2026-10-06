//! The audio thread's side of a game: the block an `AudioProcess` hook fills,
//! its input and output samples by channel and frame, read and written in
//! place so a hook never allocates; the typed messages it trades with the
//! other systems through bounded lock-free queues ([`realtime_queue`]): a
//! value of the package's `@derive(AudioCommand)` enum sent with
//! `send_audio` arrives at `AudioCommands.audio_command` before the next
//! block, and a variant of its `@derive(AudioEvent)` enum sent with
//! `block.send` arrives at `AudioListener.audio_event` in the next frame;
//! and the [`AudioHost`] running the hooks on the audio callback.

mod host;
mod message;
mod queue;

use std::cell::Cell;

pub use host::{AudioFault, AudioHost, AudioLink, AudioStatus, COMMAND_CAPACITY, EVENT_CAPACITY};
pub use message::{AudioMessage, MESSAGE_TOKENS, MessageSlot};
pub use queue::{RealtimeReceiver, RealtimeSender, realtime_queue};

use crate::native::{
    NativeError, NativeFunction, NativeId, NativeObject, NativeValue, Obj, SchemaTy,
};
use crate::value::Value;

/// The identity of the `AudioProcess.audio_process` hook.
pub const AUDIO_PROCESS: NativeId = NativeId::of("viso::game::AudioProcess::audio_process");

/// The identity of the `AudioCommands.audio_command` hook.
pub const AUDIO_COMMAND: NativeId = NativeId::of("viso::game::AudioCommands::audio_command");

/// The identity of the `AudioListener.audio_event` hook.
pub const AUDIO_EVENT: NativeId = NativeId::of("viso::game::AudioListener::audio_event");

/// The derive naming the package's messages to the audio thread: an enum
/// whose payloads are plain data, flattening into at most
/// [`MESSAGE_TOKENS`] tokens.
pub const AUDIO_COMMAND_DERIVE: &str = "AudioCommand";

/// The derive naming the package's messages from the audio thread: an enum
/// without payloads, since building one there would allocate.
pub const AUDIO_EVENT_DERIVE: &str = "AudioEvent";

/// A value of the package's audio command type at the native boundary.
#[derive(Debug, Clone)]
pub struct AudioCommandValue(pub Value);

impl NativeValue for AudioCommandValue {
    const TY: SchemaTy = SchemaTy::AudioCommand;

    fn from_value(value: &Value) -> Option<Self> {
        Some(AudioCommandValue(value.clone()))
    }

    fn into_value(self) -> Value {
        self.0
    }
}

/// A variant of the package's audio event type, by its index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioEventValue(pub i64);

impl NativeValue for AudioEventValue {
    const TY: SchemaTy = SchemaTy::AudioEvent;

    fn from_value(value: &Value) -> Option<Self> {
        value.as_int().map(AudioEventValue)
    }

    fn into_value(self) -> Value {
        Value::Int(self.0)
    }
}

/// One block of audio behind a `viso::game::AudioBlock` handle: `frames`
/// frames of `channels` channels at `sample_rate`, the input the hook reads
/// and the output it writes, each channel-major, and the queue its events
/// leave through.
#[derive(Debug)]
pub struct AudioBlock {
    channels: usize,
    frames: Cell<usize>,
    capacity: usize,
    sample_rate: f64,
    input: Box<[Cell<f32>]>,
    output: Box<[Cell<f32>]>,
    events: Option<RealtimeSender<i64>>,
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
            frames: Cell::new(frames),
            capacity: frames,
            sample_rate,
            input: silence(),
            output: silence(),
            events: None,
        }
    }

    /// Sends the events `block.send` queues through `events`.
    pub fn with_events(mut self, events: RealtimeSender<i64>) -> AudioBlock {
        self.events = Some(events);
        self
    }

    /// The channels.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The frames of this block.
    pub fn frames(&self) -> usize {
        self.frames.get()
    }

    /// Makes the next block `frames` frames long, at most the frames it was
    /// made with; the input and output keep their storage.
    ///
    /// # Panics
    ///
    /// If `frames` is more than that.
    pub fn set_frames(&self, frames: usize) {
        assert!(
            frames <= self.capacity,
            "the block holds {} frames",
            self.capacity
        );
        self.frames.set(frames);
    }

    /// The output, channel-major: each channel's frames in turn.
    pub fn output_samples(&self) -> impl Iterator<Item = f32> + '_ {
        self.output[..self.channels * self.frames()]
            .iter()
            .map(Cell::get)
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
        for sample in &self.output[..self.channels * self.frames()] {
            sample.set(0.0);
        }
    }

    fn at(&self, channel: usize, frame: usize) -> Option<usize> {
        let frames = self.frames();
        (channel < self.channels && frame < frames).then(|| channel * frames + frame)
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

pub(super) static AUDIO_BLOCK_METHODS: [NativeFunction; 6] = [
    crate::native!(fn "channels" |_cx, this: Obj<AudioBlock>| -> i64 { Ok(count(this.channels)) })
        .deterministic()
        .realtime_safe(),
    crate::native!(fn "frames" |_cx, this: Obj<AudioBlock>| -> i64 { Ok(count(this.frames())) })
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
    // Sends an event to the `AudioListener`s; false when the queue is full
    // or the block has none.
    crate::native!(action "send" |_cx, this: Obj<AudioBlock>, event: AudioEventValue| -> bool {
        Ok(this.events.as_ref().is_some_and(|events| events.push(event.0)))
    })
    .realtime_safe(),
];

/// `send_audio(command)`: queues `command` for the `AudioCommands` on the
/// audio thread; false when no audio thread runs or its queue is full. A
/// Presentation command, so a Simulation hook's sends once per tick.
pub(super) static AUDIO_FUNCTIONS: [NativeFunction; 1] = [crate::native!(
    action "send_audio" |cx, command: AudioCommandValue| -> bool {
        let Ok(link) = cx.service::<AudioLink>() else {
            return Ok(false);
        };
        Ok(link.send(&command.0))
    }
)
.presentation()];
