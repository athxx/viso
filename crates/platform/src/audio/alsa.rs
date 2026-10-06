//! Audio output through ALSA (`libasound`, loaded at runtime): the
//! `default` device — PipeWire's or PulseAudio's ALSA plugin on most
//! desktops — as interleaved float32 stereo at 48 kHz, written a period at
//! a time from a thread of its own.

use std::ffi::{CStr, c_char, c_int, c_long, c_uint, c_ulong, c_void};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use super::{AudioError, AudioFormat, AudioRender};

type Pcm = *mut c_void;

const STREAM_PLAYBACK: c_int = 0;
const FORMAT_FLOAT_LE: c_int = 14;
const ACCESS_RW_INTERLEAVED: c_int = 3;
const RATE: c_uint = 48_000;
const CHANNELS: usize = 2;
/// The latency asked for: a period of about 5 ms, four of them buffered.
const LATENCY_US: c_uint = 20_000;

/// The `libasound` entry points used.
struct Alsa {
    open: unsafe extern "C" fn(*mut Pcm, *const c_char, c_int, c_int) -> c_int,
    set_params: unsafe extern "C" fn(Pcm, c_int, c_int, c_uint, c_uint, c_int, c_uint) -> c_int,
    get_params: unsafe extern "C" fn(Pcm, *mut c_ulong, *mut c_ulong) -> c_int,
    writei: unsafe extern "C" fn(Pcm, *const c_void, c_ulong) -> c_long,
    recover: unsafe extern "C" fn(Pcm, c_int, c_int) -> c_int,
    drop_: unsafe extern "C" fn(Pcm) -> c_int,
    prepare: unsafe extern "C" fn(Pcm) -> c_int,
    close: unsafe extern "C" fn(Pcm) -> c_int,
}

fn alsa() -> Result<&'static Alsa, AudioError> {
    static ALSA: std::sync::OnceLock<Option<Alsa>> = std::sync::OnceLock::new();
    ALSA.get_or_init(load)
        .as_ref()
        .ok_or_else(|| AudioError::Device("libasound.so.2 is not installed".into()))
}

fn load() -> Option<Alsa> {
    // SAFETY: `dlopen` and `dlsym` take NUL-terminated names; each symbol is
    // cast to the signature `alsa/pcm.h` declares for it. The library stays
    // loaded for the process.
    unsafe {
        let library = libc::dlopen(
            c"libasound.so.2".as_ptr(),
            libc::RTLD_NOW | libc::RTLD_LOCAL,
        );
        if library.is_null() {
            return None;
        }
        let symbol = |name: &CStr| {
            let found = libc::dlsym(library, name.as_ptr());
            (!found.is_null()).then_some(found)
        };
        // A symbol's address as the function pointer type of its field.
        fn cast<F: Copy>(found: *mut c_void) -> F {
            assert_eq!(size_of::<F>(), size_of::<*mut c_void>());
            // SAFETY: `F` is a function pointer type of the symbol's
            // signature, the size of an address (checked above).
            unsafe { std::mem::transmute_copy(&found) }
        }
        Some(Alsa {
            open: cast(symbol(c"snd_pcm_open")?),
            set_params: cast(symbol(c"snd_pcm_set_params")?),
            get_params: cast(symbol(c"snd_pcm_get_params")?),
            writei: cast(symbol(c"snd_pcm_writei")?),
            recover: cast(symbol(c"snd_pcm_recover")?),
            drop_: cast(symbol(c"snd_pcm_drop")?),
            prepare: cast(symbol(c"snd_pcm_prepare")?),
            close: cast(symbol(c"snd_pcm_close")?),
        })
    }
}

/// The PCM handle, moved to the writer thread while it runs.
struct Handle(Pcm);

// SAFETY: an ALSA PCM handle may be used from any thread, one at a time:
// the writer thread owns it while it runs and `stop` joins it before the
// handle is used again.
unsafe impl Send for Handle {}

pub(super) struct Output {
    pcm: Pcm,
    period: usize,
    running: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
}

// SAFETY: as `Handle`.
unsafe impl Send for Output {}

impl Output {
    pub(super) fn open() -> Result<(Output, AudioFormat), AudioError> {
        let alsa = alsa()?;
        let mut pcm: Pcm = ptr::null_mut();
        let check = |status: c_int, what: &str| {
            if status < 0 {
                Err(AudioError::Device(format!("{what}: error {status}")))
            } else {
                Ok(())
            }
        };
        // SAFETY: `pcm` receives the handle; the name is NUL-terminated.
        check(
            unsafe { (alsa.open)(&mut pcm, c"default".as_ptr(), STREAM_PLAYBACK, 0) },
            "opening the default device",
        )?;
        let mut output = Output {
            pcm,
            period: 0,
            running: None,
        };
        // SAFETY: the handle is open.
        check(
            unsafe {
                (alsa.set_params)(
                    pcm,
                    FORMAT_FLOAT_LE,
                    ACCESS_RW_INTERLEAVED,
                    CHANNELS as c_uint,
                    RATE,
                    1,
                    LATENCY_US,
                )
            },
            "configuring the device",
        )?;
        let (mut buffer, mut period): (c_ulong, c_ulong) = (0, 0);
        // SAFETY: as above.
        check(
            unsafe { (alsa.get_params)(pcm, &mut buffer, &mut period) },
            "reading the period",
        )?;
        let period = (period as usize).max(64);
        let format = AudioFormat {
            sample_rate: f64::from(RATE),
            channels: CHANNELS,
            max_frames: period,
        };
        output.period = period;
        Ok((output, format))
    }

    pub(super) fn start(
        &mut self,
        mut render: AudioRender,
        format: AudioFormat,
    ) -> Result<(), AudioError> {
        self.stop();
        let alsa = alsa()?;
        let stop = Arc::new(AtomicBool::new(false));
        let (period, channels) = (self.period, format.channels);
        let handle = Handle(self.pcm);
        let stopping = Arc::clone(&stop);
        let writer = std::thread::Builder::new()
            .name("viso-audio".into())
            .spawn(move || {
                let handle = handle;
                let mut block = vec![0.0f32; channels * period];
                let mut interleaved = vec![0.0f32; channels * period];
                while !stopping.load(Ordering::Relaxed) {
                    render(&mut block);
                    for frame in 0..period {
                        for channel in 0..channels {
                            interleaved[frame * channels + channel] =
                                block[channel * period + frame];
                        }
                    }
                    let mut written = 0;
                    while written < period && !stopping.load(Ordering::Relaxed) {
                        // SAFETY: the handle is open and this thread owns it;
                        // the slice holds `period - written` frames.
                        let n = unsafe {
                            (alsa.writei)(
                                handle.0,
                                interleaved[written * channels..].as_ptr().cast(),
                                (period - written) as c_ulong,
                            )
                        };
                        if n < 0 {
                            // SAFETY: as above; `recover` restarts after an
                            // underrun or a suspend.
                            if unsafe { (alsa.recover)(handle.0, n as c_int, 1) } < 0 {
                                return;
                            }
                        } else {
                            written += n as usize;
                        }
                    }
                }
            })
            .map_err(|e| AudioError::Device(format!("starting the audio thread: {e}")))?;
        self.running = Some((stop, writer));
        Ok(())
    }

    pub(super) fn stop(&mut self) {
        let Some((stop, writer)) = self.running.take() else {
            return;
        };
        stop.store(true, Ordering::Relaxed);
        let _ = writer.join();
        if let Ok(alsa) = alsa() {
            // SAFETY: the writer thread ended, so this thread owns the
            // handle; dropping the queued frames and preparing again lets a
            // later start write at once.
            unsafe {
                (alsa.drop_)(self.pcm);
                (alsa.prepare)(self.pcm);
            }
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.stop();
        if let Ok(alsa) = alsa() {
            // SAFETY: the handle is open and nothing else uses it.
            unsafe {
                (alsa.close)(self.pcm);
            }
        }
    }
}
