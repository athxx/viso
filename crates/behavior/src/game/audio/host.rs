//! The audio thread's host: a VM of its own running a game's audio systems
//! on the platform's audio callback, and the [`AudioLink`] the rest of the
//! game reaches it through.
//!
//! The host decodes its module from bytes, so every value and module it
//! holds is its own and the whole host moves to the audio thread once. A
//! block then runs without allocating, locking or blocking: the commands
//! queued since the last block are decoded in place and handed to every
//! `AudioCommands` hook, and every `AudioProcess` hook fills the block in
//! system order. A hook that faults is silenced until the host is rebuilt,
//! and its fault is left for the app to report.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::message::{AudioMessage, MessageSlot};
use super::queue::{RealtimeReceiver, RealtimeSender, realtime_queue};
use super::{AUDIO_COMMAND, AUDIO_PROCESS, AudioBlock};
use crate::Module;
use crate::native::{NativeValue, Natives, Obj};
use crate::value::Value;
use crate::vm::{Budget, Fault, Instance, Vm};
use crate::wire::LoadError;

/// The commands queued for the audio thread at most, beyond which a send
/// fails.
pub const COMMAND_CAPACITY: usize = 256;

/// The events queued from the audio thread at most, beyond which a send
/// fails.
pub const EVENT_CAPACITY: usize = 256;

/// The game's end of an [`AudioHost`]: it sends commands and receives
/// events, and reads what the audio thread did. Installed as a service of
/// the game's VM, it is what `send_audio` sends through.
#[derive(Debug)]
pub struct AudioLink {
    commands: RealtimeSender<AudioMessage>,
    events: RealtimeReceiver<i64>,
    status: Arc<AudioStatus>,
}

impl AudioLink {
    /// Queues `command`; false when it is not plain data or the queue is
    /// full.
    pub fn send(&self, command: &Value) -> bool {
        AudioMessage::encode(command).is_some_and(|message| self.commands.push(message))
    }

    /// The oldest event the audio thread sent, a variant index of the
    /// package's `AudioEvent` enum.
    pub fn receive(&self) -> Option<i64> {
        self.events.pop()
    }

    /// The commands dropped because the queue was full.
    pub fn dropped_commands(&self) -> u64 {
        self.commands.dropped()
    }

    /// The events dropped because the queue was full.
    pub fn dropped_events(&self) -> u64 {
        self.events.dropped()
    }

    /// What the audio thread did.
    pub fn status(&self) -> &AudioStatus {
        &self.status
    }
}

/// What the audio thread did, read from any thread.
#[derive(Debug, Default)]
pub struct AudioStatus {
    blocks: AtomicU64,
    faults: AtomicU64,
    fault: Mutex<Option<AudioFault>>,
}

impl AudioStatus {
    /// The blocks rendered.
    pub fn blocks(&self) -> u64 {
        self.blocks.load(Ordering::Relaxed)
    }

    /// The hooks that faulted, each silencing its system.
    pub fn faults(&self) -> u64 {
        self.faults.load(Ordering::Relaxed)
    }

    /// The first fault not taken yet.
    pub fn take_fault(&self) -> Option<AudioFault> {
        self.fault.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// A fault of an audio system's hook.
#[derive(Debug, Clone)]
pub struct AudioFault {
    /// The system's index in the module.
    pub system: usize,
    pub fault: Fault,
}

/// An audio system: its instance on the host's VM and its hooks.
struct AudioSystem {
    index: usize,
    instance: Instance,
    process: Option<u32>,
    command: Option<u32>,
    silenced: bool,
}

/// Runs a game's audio systems on the audio thread.
pub struct AudioHost {
    vm: Vm,
    systems: Box<[AudioSystem]>,
    block: Obj<AudioBlock>,
    /// The block as the value each `AudioProcess` hook takes.
    arg: Value,
    commands: RealtimeReceiver<AudioMessage>,
    slot: MessageSlot,
    status: Arc<AudioStatus>,
}

// SAFETY: a host is built from the module's bytes (`AudioHost::new`), so the
// `Rc`s it holds — its module, VM, instances, block and values — reach only
// one another and none is shared with anything outside it; moving the whole
// host to another thread moves every handle to them at once. Its VM has no
// services, and the natives it links are `Send + Sync` statics. It is used
// from one thread at a time (`render` takes `&mut self`).
unsafe impl Send for AudioHost {}

impl AudioHost {
    /// A host running the audio systems of the module encoded in `module`,
    /// linked against `natives`, filling blocks of `channels` channels of up
    /// to `max_frames` frames at `sample_rate`; `None` when the module has
    /// no system with an audio-thread hook.
    ///
    /// # Errors
    ///
    /// When the module does not decode or link, or an audio system's
    /// instance cannot be created.
    pub fn new(
        module: &[u8],
        natives: &Natives,
        channels: usize,
        max_frames: usize,
        sample_rate: f64,
    ) -> Result<Option<(AudioHost, AudioLink)>, String> {
        let module = Module::decode(module).map_err(|e: LoadError| e.to_string())?;
        let mut systems = Vec::new();
        let mut vm = Vm::new(std::rc::Rc::new(module), block_budget(channels, max_frames));
        vm.link(natives, &[]).map_err(|e| e.to_string())?;
        let module = vm.module().clone();
        for (index, system) in module.systems().iter().enumerate() {
            let (process, command) = (system.hook(AUDIO_PROCESS), system.hook(AUDIO_COMMAND));
            if process.is_none() && command.is_none() {
                continue;
            }
            let instance = vm
                .instantiate(system.component, [])
                .map_err(|fault| fault.to_string())?;
            systems.push(AudioSystem {
                index,
                instance,
                process,
                command,
                silenced: false,
            });
        }
        if systems.is_empty() {
            return Ok(None);
        }
        let (commands, inbox) = realtime_queue(COMMAND_CAPACITY);
        let (outbox, events) = realtime_queue(EVENT_CAPACITY);
        let block =
            Obj::new(AudioBlock::new(channels, max_frames, sample_rate).with_events(outbox));
        let status = Arc::new(AudioStatus::default());
        let host = AudioHost {
            vm,
            systems: systems.into(),
            arg: block.clone().into_value(),
            block,
            commands: inbox,
            slot: MessageSlot::default(),
            status: Arc::clone(&status),
        };
        let link = AudioLink {
            commands,
            events,
            status,
        };
        Ok(Some((host, link)))
    }

    /// The channels of a block.
    pub fn channels(&self) -> usize {
        self.block.channels()
    }

    /// Renders one block into `output`, channel-major: each channel's frames
    /// in turn, `output.len() / channels` frames. The commands queued since
    /// the last block reach the `AudioCommands` hooks first.
    ///
    /// # Panics
    ///
    /// When `output` holds more frames than the host was made for.
    pub fn render(&mut self, output: &mut [f32]) {
        let frames = output.len() / self.block.channels().max(1);
        self.block.set_frames(frames);
        self.block.clear_output();
        while let Some(message) = self.commands.pop() {
            let command = self.slot.decode(&message);
            for system in &mut self.systems {
                if let (Some(chunk), false) = (system.command, system.silenced) {
                    let args = [command.clone()];
                    let result = self.vm.call(&mut system.instance, chunk, &args);
                    settle(system, result, &self.status);
                }
            }
        }
        for system in &mut self.systems {
            if let (Some(chunk), false) = (system.process, system.silenced) {
                let args = [self.arg.clone()];
                let result = self.vm.call(&mut system.instance, chunk, &args);
                settle(system, result, &self.status);
            }
        }
        for (out, sample) in output.iter_mut().zip(self.block.output_samples()) {
            *out = sample;
        }
        self.status.blocks.fetch_add(1, Ordering::Relaxed);
    }
}

/// What one hook may spend on a block: a hook touches each sample through
/// the block's natives, so its native calls scale with the block, sixteen a
/// sample; it builds nothing, so it may allocate no more than a small
/// reserve.
fn block_budget(channels: usize, frames: usize) -> Budget {
    let samples = channels.max(1).saturating_mul(frames.max(1));
    Budget {
        native_calls: u32::try_from(samples.saturating_mul(16)).unwrap_or(u32::MAX),
        memory: 64 << 10,
        ..Budget::default()
    }
}

/// Silences `system` when its hook faulted, keeping the fault for the app
/// unless one waits already; the audio thread never waits for the lock.
fn settle<T>(system: &mut AudioSystem, result: Result<T, Fault>, status: &AudioStatus) {
    let Err(fault) = result else {
        return;
    };
    system.silenced = true;
    status.faults.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut slot) = status.fault.try_lock()
        && slot.is_none()
    {
        *slot = Some(AudioFault {
            system: system.index,
            fault,
        });
    }
}

impl std::fmt::Debug for AudioHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioHost")
            .field("systems", &self.systems.len())
            .field("channels", &self.block.channels())
            .finish()
    }
}
