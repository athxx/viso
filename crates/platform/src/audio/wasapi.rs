//! Audio output through WASAPI: the default render endpoint in shared mode,
//! event-driven, float32 stereo at the mix rate (the engine converts to the
//! device's channels), filled from a thread of its own that owns every COM
//! object it uses.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioClient,
    IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX,
    WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0, eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::GUID;

use super::{AudioError, AudioFormat, AudioRender};

const CHANNELS: usize = 2;
/// The engine's buffer: 20 ms, in 100 ns units.
const BUFFER: i64 = 200_000;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
/// `KSDATAFORMAT_SUBTYPE_IEEE_FLOAT`.
const SUBTYPE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
const SPEAKERS_STEREO: u32 = 0x3;

fn failed(what: &str) -> impl Fn(windows::core::Error) -> AudioError + '_ {
    move |e| AudioError::Device(format!("{what}: {e}"))
}

/// The default endpoint's client, initialized for float stereo at the mix
/// rate with event-driven buffering, on this thread's COM apartment.
///
/// # Safety
///
/// COM is initialized on this thread.
unsafe fn client(event: Option<HANDLE>) -> Result<(IAudioClient, u32, u32), AudioError> {
    // SAFETY: COM is initialized (the caller's contract); the mix format is
    // read, copied and freed with the allocator COM gave it with.
    unsafe {
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(failed("listing the audio devices"))?;
        let device = devices
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(failed("finding the default output"))?;
        let client: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .map_err(failed("activating the output"))?;
        let mix = client
            .GetMixFormat()
            .map_err(failed("reading the mix format"))?;
        let rate = (*mix).nSamplesPerSec;
        CoTaskMemFree(Some(mix.cast()));
        let block_align = (CHANNELS * 4) as u16;
        let format = WAVEFORMATEXTENSIBLE {
            Format: WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_EXTENSIBLE,
                nChannels: CHANNELS as u16,
                nSamplesPerSec: rate,
                nAvgBytesPerSec: rate * u32::from(block_align),
                nBlockAlign: block_align,
                wBitsPerSample: 32,
                cbSize: (size_of::<WAVEFORMATEXTENSIBLE>() - size_of::<WAVEFORMATEX>()) as u16,
            },
            Samples: WAVEFORMATEXTENSIBLE_0 {
                wValidBitsPerSample: 32,
            },
            dwChannelMask: SPEAKERS_STEREO,
            SubFormat: SUBTYPE_FLOAT,
        };
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                BUFFER,
                0,
                (&raw const format).cast(),
                None,
            )
            .map_err(failed("initializing the output"))?;
        if let Some(event) = event {
            client
                .SetEventHandle(event)
                .map_err(failed("setting the buffer event"))?;
        }
        let frames = client
            .GetBufferSize()
            .map_err(failed("reading the buffer size"))?;
        Ok((client, rate, frames))
    }
}

/// Runs `f` on a fresh thread inside a COM multithreaded apartment.
fn in_apartment<R: Send + 'static>(
    f: impl FnOnce() -> Result<R, AudioError> + Send + 'static,
) -> Result<R, AudioError> {
    std::thread::spawn(move || {
        // SAFETY: initializes COM on this new thread, undone below.
        let init = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if init.is_err() {
            return Err(AudioError::Device(format!("initializing COM: {init:?}")));
        }
        let result = f();
        // SAFETY: balances the initialization above.
        unsafe { CoUninitialize() };
        result
    })
    .join()
    .unwrap_or_else(|_| Err(AudioError::Device("the audio probe panicked".into())))
}

pub(super) struct Output {
    running: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
}

impl Output {
    pub(super) fn open() -> Result<(Output, AudioFormat), AudioError> {
        let (rate, frames) = in_apartment(|| {
            // SAFETY: COM is initialized on this thread.
            let (_client, rate, frames) = unsafe { client(None)? };
            Ok((rate, frames))
        })?;
        let format = AudioFormat {
            sample_rate: f64::from(rate),
            channels: CHANNELS,
            max_frames: (frames as usize).max(1),
        };
        Ok((Output { running: None }, format))
    }

    pub(super) fn start(
        &mut self,
        mut render: AudioRender,
        format: AudioFormat,
    ) -> Result<(), AudioError> {
        self.stop();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let (started, outcome) = mpsc::channel();
        let writer = std::thread::Builder::new()
            .name("viso-audio".into())
            .spawn(move || {
                // SAFETY: initializes COM on this thread, undone at its end;
                // every COM object below lives and dies on it.
                unsafe {
                    if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
                        let _ = started.send(Err(AudioError::Device("initializing COM".into())));
                        return;
                    }
                    let mut run = || -> Result<(), AudioError> {
                        let event = CreateEventW(None, false, false, None)
                            .map_err(failed("creating the buffer event"))?;
                        let (client, _, frames) = match client(Some(event)) {
                            Ok(found) => found,
                            Err(error) => {
                                let _ = CloseHandle(event);
                                return Err(error);
                            }
                        };
                        let frames = frames as usize;
                        let renderer: IAudioRenderClient = client
                            .GetService()
                            .map_err(failed("opening the render client"))?;
                        let capacity = format.max_frames.max(frames);
                        let mut block = vec![0.0f32; CHANNELS * capacity];
                        client.Start().map_err(failed("starting the output"))?;
                        let _ = started.send(Ok(()));
                        while !stopping.load(Ordering::Relaxed) {
                            if WaitForSingleObject(event, 200) != WAIT_OBJECT_0 {
                                continue;
                            }
                            let Ok(padding) = client.GetCurrentPadding() else {
                                break;
                            };
                            let free = frames.saturating_sub(padding as usize);
                            if free == 0 {
                                continue;
                            }
                            let Ok(data) = renderer.GetBuffer(free as u32) else {
                                break;
                            };
                            let samples = &mut block[..CHANNELS * free];
                            render(samples);
                            let out =
                                std::slice::from_raw_parts_mut(data.cast::<f32>(), CHANNELS * free);
                            for frame in 0..free {
                                for channel in 0..CHANNELS {
                                    out[frame * CHANNELS + channel] =
                                        samples[channel * free + frame];
                                }
                            }
                            let _ = renderer.ReleaseBuffer(free as u32, 0);
                        }
                        let _ = client.Stop();
                        let _ = CloseHandle(event);
                        Ok(())
                    };
                    if let Err(error) = run() {
                        let _ = started.send(Err(error));
                    }
                    CoUninitialize();
                }
            })
            .map_err(|e| AudioError::Device(format!("starting the audio thread: {e}")))?;
        match outcome.recv() {
            Ok(Ok(())) => {
                self.running = Some((stop, writer));
                Ok(())
            }
            Ok(Err(error)) => {
                let _ = writer.join();
                Err(error)
            }
            Err(_) => {
                let _ = writer.join();
                Err(AudioError::Device("the audio thread ended".into()))
            }
        }
    }

    pub(super) fn stop(&mut self) {
        if let Some((stop, writer)) = self.running.take() {
            stop.store(true, Ordering::Relaxed);
            let _ = writer.join();
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.stop();
    }
}
