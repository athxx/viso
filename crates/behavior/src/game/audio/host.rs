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
//!
//! A reload swaps the systems without a gap and without touching the
//! link: the game builds the new [`AudioProgram`] — VM and instances — off
//! the audio thread and publishes it through the host's [`AudioSwap`], a
//! pair of atomic pointers; the next block installs it in place of the
//! running one, which goes back through the other pointer to be dropped on
//! the game's thread. A program owns every value its VM can reach — its
//! block and the slot commands decode into — so the one dropped there
//! shares nothing with the audio thread; the queues and the status belong
//! to the host and carry across, the event queue moving from the old block
//! to the new. The audio thread only ever exchanges pointers: it neither
//! allocates nor frees.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::message::{AudioMessage, MessageSlot};
use super::queue::{RealtimeReceiver, RealtimeSender, realtime_queue};
use super::{AUDIO_COMMAND, AUDIO_PROCESS, AudioBlock};
use crate::Module;
use crate::aot::NativeCode;
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

/// A module's audio systems on a VM of their own: what a host runs, and
/// what a reload hands it in place of what it runs.
pub struct AudioProgram {
    vm: Vm,
    systems: Box<[AudioSystem]>,
    block: Obj<AudioBlock>,
    /// The block as the value each `AudioProcess` hook takes.
    arg: Value,
    slot: MessageSlot,
}

// SAFETY: a program is built from the module's bytes (`AudioProgram::new`),
// so the `Rc`s it holds — its module, VM, instances, block, the block's value
// and the values its slot decodes — reach only one another, and every value
// its hooks see is one of them; moving it moves every handle to them at once.
// Its VM has no services, and the natives it links are `Send + Sync`
// statics. It runs on one thread at a time.
unsafe impl Send for AudioProgram {}

impl AudioProgram {
    /// The audio systems of the module encoded in `module`, linked against
    /// `natives` and running `native` when given (a release build's module
    /// compiled to Rust), filling blocks of `channels` channels of up to
    /// `max_frames` frames at `sample_rate`; none when the module has no
    /// audio-thread hook.
    ///
    /// # Errors
    ///
    /// When the module does not decode or link, `native` was lowered from
    /// another build, or an audio system's instance cannot be created.
    pub fn new(
        module: &[u8],
        natives: &Natives,
        native: Option<&'static NativeCode>,
        channels: usize,
        max_frames: usize,
        sample_rate: f64,
    ) -> Result<AudioProgram, String> {
        let module = Module::decode(module).map_err(|e: LoadError| e.to_string())?;
        let mut systems = Vec::new();
        let mut vm = Vm::new(
            alloc::rc::Rc::new(module),
            block_budget(channels, max_frames),
        );
        vm.link(natives, &[]).map_err(|e| e.to_string())?;
        if let Some(native) = native {
            vm.install_native(native).map_err(|e| e.to_string())?;
        }
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
        let block = Obj::new(AudioBlock::new(channels, max_frames, sample_rate));
        let mut program = AudioProgram {
            vm,
            systems: systems.into(),
            arg: block.clone().into_value(),
            block,
            slot: MessageSlot::default(),
        };
        program.warm();
        Ok(program)
    }

    /// Runs each `AudioProcess` hook once over a full block here and puts
    /// its instance back as it was, so the VM's registers and frames have
    /// grown to what the hooks need before the program reaches the audio
    /// thread: its first block there allocates nothing. A command's first
    /// decode of each variant still sizes the slot.
    fn warm(&mut self) {
        self.block.set_frames(self.block.capacity());
        for system in &mut self.systems {
            let Some(chunk) = system.process else {
                continue;
            };
            let kept = system.instance.clone();
            let _ = self.vm.call(
                &mut system.instance,
                chunk,
                core::slice::from_ref(&self.arg),
            );
            system.instance = kept;
        }
        self.block.clear_output();
    }

    /// The audio systems it runs.
    pub fn systems(&self) -> usize {
        self.systems.len()
    }
}

/// Where a reload's program waits for the audio thread, and where the one
/// it replaced waits for the game's thread.
#[derive(Default)]
struct HandOff {
    /// Published by the game, taken by the audio thread.
    next: AtomicPtr<AudioProgram>,
    /// Put back by the audio thread, taken by the game.
    retired: AtomicPtr<AudioProgram>,
}

impl HandOff {
    /// Takes the program `slot` holds.
    fn take(slot: &AtomicPtr<AudioProgram>) -> Option<Box<AudioProgram>> {
        let program = slot.swap(ptr::null_mut(), Ordering::Acquire);
        // SAFETY: a non-null pointer in a slot came from `Box::into_raw` and
        // the swap took it out, so this is its one owner.
        (!program.is_null()).then(|| unsafe { Box::from_raw(program) })
    }
}

impl Drop for HandOff {
    fn drop(&mut self) {
        drop(HandOff::take(&self.next));
        drop(HandOff::take(&self.retired));
    }
}

/// The game's end of a host's hand-off: swaps the systems the host runs.
pub struct AudioSwap {
    handoff: Arc<HandOff>,
    channels: usize,
    max_frames: usize,
    sample_rate: f64,
}

impl AudioSwap {
    /// Builds the audio systems of the module encoded in `module` — a
    /// reload's, interpreted — linked against `natives`, here, and hands them to the host, which installs
    /// them before its next block; a module without audio systems silences
    /// it. Drops the program the host gave back from the last swap, and one
    /// published but not installed yet.
    ///
    /// # Errors
    ///
    /// As [`AudioProgram::new`]: the host keeps what it runs.
    pub fn swap(&self, module: &[u8], natives: &Natives) -> Result<(), String> {
        drop(HandOff::take(&self.handoff.retired));
        let program = AudioProgram::new(
            module,
            natives,
            None,
            self.channels,
            self.max_frames,
            self.sample_rate,
        )?;
        let stale = self
            .handoff
            .next
            .swap(Box::into_raw(Box::new(program)), Ordering::AcqRel);
        if !stale.is_null() {
            // SAFETY: as in `HandOff::take`: the swap took it out.
            drop(unsafe { Box::from_raw(stale) });
        }
        Ok(())
    }

    /// Drops the program the host gave back, if it gave one back.
    pub fn collect(&self) {
        drop(HandOff::take(&self.handoff.retired));
    }
}

impl core::fmt::Debug for AudioSwap {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AudioSwap")
            .field("channels", &self.channels)
            .field("max_frames", &self.max_frames)
            .finish_non_exhaustive()
    }
}

/// Runs a game's audio systems on the audio thread.
pub struct AudioHost {
    program: Box<AudioProgram>,
    handoff: Arc<HandOff>,
    commands: RealtimeReceiver<AudioMessage>,
    status: Arc<AudioStatus>,
}

// SAFETY: a host is built from the module's bytes (`AudioHost::new`), so the
// `Rc`s it holds are its program's, which reach only one another; moving the
// whole host to another thread moves every handle to them at once. A program
// the hand-off installs or retires is itself `Send` and shares no `Rc` with
// the host. It is used from one thread at a time (`render` takes `&mut
// self`).
unsafe impl Send for AudioHost {}

impl AudioHost {
    /// A host running the audio systems of the module encoded in `module`,
    /// linked against `natives` and running `native` when given, filling
    /// blocks of `channels` channels of up to `max_frames` frames at
    /// `sample_rate`; `None` when the module has no system with an
    /// audio-thread hook.
    ///
    /// # Errors
    ///
    /// As [`AudioProgram::new`].
    pub fn new(
        module: &[u8],
        natives: &Natives,
        native: Option<&'static NativeCode>,
        channels: usize,
        max_frames: usize,
        sample_rate: f64,
    ) -> Result<Option<(AudioHost, AudioLink)>, String> {
        let program =
            AudioProgram::new(module, natives, native, channels, max_frames, sample_rate)?;
        if program.systems.is_empty() {
            return Ok(None);
        }
        let (commands, inbox) = realtime_queue(COMMAND_CAPACITY);
        let (outbox, events) = realtime_queue(EVENT_CAPACITY);
        program.block.set_events(Some(outbox));
        let status = Arc::new(AudioStatus::default());
        let host = AudioHost {
            program: Box::new(program),
            handoff: Arc::default(),
            commands: inbox,
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
        self.program.block.channels()
    }

    /// The game's end of the hand-off swapping the systems this host runs,
    /// taken before the host moves to the audio thread.
    pub fn swapper(&self) -> AudioSwap {
        let block = &self.program.block;
        AudioSwap {
            handoff: Arc::clone(&self.handoff),
            channels: block.channels(),
            max_frames: block.capacity(),
            sample_rate: block.sample_rate(),
        }
    }

    /// Installs the program a swap published, unless the one it replaced
    /// last has not been taken back yet: pointers exchanged, nothing
    /// allocated or freed.
    fn install(&mut self) {
        if !self.handoff.retired.load(Ordering::Acquire).is_null() {
            return;
        }
        let next = self.handoff.next.swap(ptr::null_mut(), Ordering::Acquire);
        if next.is_null() {
            return;
        }
        // SAFETY: the pointer came from `Box::into_raw` in `AudioSwap::swap`
        // and the swap above took it out, so this is its one owner.
        let next = unsafe { Box::from_raw(next) };
        // The event queue moves to the new block: the link stays valid.
        next.block.set_events(self.program.block.take_events());
        let old = core::mem::replace(&mut self.program, next);
        // Only this thread fills `retired`, and it was empty.
        self.handoff
            .retired
            .store(Box::into_raw(old), Ordering::Release);
    }

    /// Renders one block into `output`, channel-major: each channel's frames
    /// in turn, `output.len() / channels` frames. The commands queued since
    /// the last block reach the `AudioCommands` hooks first.
    ///
    /// # Panics
    ///
    /// When `output` holds more frames than the host was made for.
    pub fn render(&mut self, output: &mut [f32]) {
        self.install();
        let program = &mut *self.program;
        let block = &program.block;
        let frames = output.len() / block.channels().max(1);
        block.set_frames(frames);
        block.clear_output();
        while let Some(message) = self.commands.pop() {
            let command = program.slot.decode(&message);
            for system in &mut program.systems {
                if let (Some(chunk), false) = (system.command, system.silenced) {
                    let args = [command.clone()];
                    let result = program.vm.call(&mut system.instance, chunk, &args);
                    settle(system, result, &self.status);
                }
            }
        }
        for system in &mut program.systems {
            if let (Some(chunk), false) = (system.process, system.silenced) {
                let args = [program.arg.clone()];
                let result = program.vm.call(&mut system.instance, chunk, &args);
                settle(system, result, &self.status);
            }
        }
        for (out, sample) in output.iter_mut().zip(program.block.output_samples()) {
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

impl core::fmt::Debug for AudioHost {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AudioHost")
            .field("systems", &self.program.systems.len())
            .field("channels", &self.program.block.channels())
            .finish()
    }
}
