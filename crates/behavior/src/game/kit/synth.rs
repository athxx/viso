//! Synthesized sound: the [`Sfx`] bank and plain tones, rendered by a small
//! additive synthesizer.
//!
//! A game's sound commands land on the [`Stage`] as [`Cue`]s; the host drains
//! them to a [`Synth`] on its audio thread, which renders them into the
//! output buffer. The synthesizer is realtime-safe: a fixed set of voices, no
//! allocation, no lock, no host call. Square and saw waves are band-limited
//! (PolyBLEP), so a high note does not alias into a buzz; every voice has a
//! short attack and a squared decay, and the mix is soft-clipped.
//!
//! [`Stage`]: super::Stage

use super::{Sfx, Wave};

/// The most voices sounding at once; another steals the one nearest its end.
pub const MAX_VOICES: usize = 24;
/// Seconds a voice takes to reach full level.
const ATTACK: f32 = 0.004;

/// A sound to start: a bank sound or a tone, at a gain and stereo pan.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cue {
    pub sound: Sound,
    /// In `[0, 1]`.
    pub gain: f32,
    /// In `[-1, 1]`, left to right.
    pub pan: f32,
}

/// What a cue plays.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Sound {
    Sfx(Sfx),
    /// A tone of `wave` gliding from `from` to `to` hertz over `seconds`.
    Tone {
        wave: Wave,
        from: f32,
        to: f32,
        seconds: f32,
    },
}

/// One note of a bank sound.
#[derive(Debug, Clone, Copy)]
struct Note {
    wave: Wave,
    from: f32,
    to: f32,
    seconds: f32,
    gain: f32,
    delay: f32,
}

const fn note(wave: Wave, from: f32, to: f32, seconds: f32, gain: f32, delay: f32) -> Note {
    Note {
        wave,
        from,
        to,
        seconds,
        gain,
        delay,
    }
}

/// A sequence of equal notes `step` seconds apart, each sounding 90% of it.
macro_rules! jingle {
    ($wave:expr, $step:expr, $gain:expr; $($hz:expr),*) => {{
        const NOTES: &[f32] = &[$($hz),*];
        const N: usize = NOTES.len();
        const fn build() -> [Note; N] {
            let mut out = [note($wave, 0.0, 0.0, 0.0, 0.0, 0.0); N];
            let mut i = 0;
            while i < N {
                out[i] = note($wave, NOTES[i], NOTES[i], $step * 0.9, $gain, $step * i as f32);
                i += 1;
            }
            out
        }
        const OUT: [Note; N] = build();
        &OUT
    }};
}

/// Notes of a bank sound, as a constant.
macro_rules! notes {
    ($($note:expr),* $(,)?) => {{
        const NOTES: &[Note] = &[$($note),*];
        NOTES
    }};
}

// Equal-tempered note frequencies, A4 = 440 Hz.
const A3: f32 = 220.0;
const C4: f32 = 261.626;
const E4: f32 = 329.628;
const C5: f32 = 523.251;
const E5: f32 = 659.255;
const G5: f32 = 783.991;
const B5: f32 = 987.767;
const C6: f32 = 1046.502;
const E6: f32 = 1318.51;

/// The notes of a bank sound.
fn bank(sfx: Sfx) -> &'static [Note] {
    use Wave::{Noise, Saw, Sine, Square, Triangle};
    match sfx {
        Sfx::Jump => notes![note(Square, 260.0, 540.0, 0.12, 0.22, 0.0)],
        Sfx::Shoot => notes![note(Square, 880.0, 180.0, 0.09, 0.20, 0.0)],
        Sfx::Zap => notes![
            note(Saw, 1200.0, 90.0, 0.18, 0.22, 0.0),
            note(Noise, 600.0, 600.0, 0.10, 0.12, 0.0),
        ],
        Sfx::Grab => notes![note(Sine, 320.0, 180.0, 0.12, 0.25, 0.0)],
        Sfx::Angry => notes![note(Square, 150.0, 90.0, 0.25, 0.22, 0.0)],
        Sfx::Calm => notes![note(Sine, 390.0, 520.0, 0.20, 0.20, 0.0)],
        Sfx::Rescue => jingle!(Triangle, 0.09, 0.22; E5, G5),
        Sfx::Shove => notes![note(Noise, 200.0, 200.0, 0.06, 0.30, 0.0)],
        Sfx::Board => notes![note(Sine, 220.0, 330.0, 0.11, 0.22, 0.0)],
        Sfx::Coin => jingle!(Triangle, 0.07, 0.20; B5, E6),
        Sfx::Hurt => notes![note(Saw, 300.0, 120.0, 0.15, 0.22, 0.0)],
        Sfx::Win => jingle!(Triangle, 0.10, 0.22; C5, E5, G5, C6),
        Sfx::Lose => jingle!(Square, 0.14, 0.20; E4, C4, A3),
        Sfx::Powerup => jingle!(Square, 0.06, 0.16; C5, E5, G5, C6, E6),
        Sfx::Explode => notes![
            note(Noise, 200.0, 200.0, 0.50, 0.35, 0.0),
            note(Saw, 120.0, 40.0, 0.40, 0.25, 0.0),
        ],
        Sfx::Click => notes![note(Sine, 1400.0, 1400.0, 0.03, 0.18, 0.0)],
        Sfx::Step => notes![note(Noise, 300.0, 300.0, 0.04, 0.12, 0.0)],
        Sfx::Squeak => notes![note(Sine, 900.0, 1400.0, 0.08, 0.18, 0.0)],
        Sfx::Roar => notes![
            note(Saw, 220.0, 60.0, 0.5, 0.28, 0.0),
            note(Noise, 300.0, 300.0, 0.35, 0.14, 0.0),
        ],
        Sfx::Bark => notes![
            note(Square, 520.0, 520.0, 0.06, 0.30, 0.0),
            note(Square, 340.0, 340.0, 0.06, 0.30, 0.06),
        ],
        Sfx::Moo => notes![note(Square, 200.0, 150.0, 0.35, 0.18, 0.0)],
        Sfx::Clank => notes![
            note(Square, 980.0, 980.0, 0.07, 0.30, 0.0),
            note(Square, 300.0, 300.0, 0.07, 0.30, 0.07),
        ],
        Sfx::Whip => notes![note(Square, 420.0, 1500.0, 0.09, 0.25, 0.0)],
    }
}

#[derive(Debug, Clone, Copy)]
struct Voice {
    wave: Wave,
    from: f32,
    to: f32,
    length: f32,
    left: f32,
    right: f32,
    delay: f32,
    t: f32,
    phase: f32,
    noise: u32,
    active: bool,
}

const SILENT: Voice = Voice {
    wave: Wave::Sine,
    from: 0.0,
    to: 0.0,
    length: 0.0,
    left: 0.0,
    right: 0.0,
    delay: 0.0,
    t: 0.0,
    phase: 0.0,
    noise: 0x02f6_e2b1,
    active: false,
};

/// A realtime additive synthesizer.
#[derive(Debug, Clone)]
pub struct Synth {
    rate: f32,
    voices: [Voice; MAX_VOICES],
}

impl Synth {
    /// A silent synthesizer rendering at `sample_rate` hertz.
    pub fn new(sample_rate: f32) -> Synth {
        Synth {
            rate: if sample_rate.is_finite() {
                sample_rate.max(8000.0)
            } else {
                48000.0
            },
            voices: [SILENT; MAX_VOICES],
        }
    }

    /// Starts `cue`.
    pub fn play(&mut self, cue: &Cue) {
        let gain = if cue.gain.is_finite() {
            cue.gain.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let pan = if cue.pan.is_finite() {
            cue.pan.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let (left, right) = (((1.0 - pan) * 0.5).sqrt(), ((1.0 + pan) * 0.5).sqrt());
        let mut start = |n: &Note| {
            let slot = self.free_voice();
            let finite = |v: f32, low: f32, high: f32| {
                if v.is_finite() {
                    v.clamp(low, high)
                } else {
                    low
                }
            };
            let level = finite(n.gain, 0.0, 1.0) * gain;
            self.voices[slot] = Voice {
                wave: n.wave,
                from: finite(n.from, 20.0, 8000.0),
                to: finite(n.to, 20.0, 8000.0),
                length: finite(n.seconds, 0.01, 3.0),
                left: left * level,
                right: right * level,
                delay: finite(n.delay, 0.0, 3.0),
                t: 0.0,
                phase: 0.0,
                noise: 0x02f6_e2b1 ^ slot as u32,
                active: true,
            };
        };
        match cue.sound {
            Sound::Sfx(sfx) => bank(sfx).iter().for_each(start),
            Sound::Tone {
                wave,
                from,
                to,
                seconds,
            } => start(&note(wave, from, to, seconds, 1.0, 0.0)),
        }
    }

    /// Adds every sounding voice to `out`, interleaved frames of `channels`
    /// samples: left then right, and the mono mix when `channels` is 1;
    /// channels past the second are left alone.
    pub fn render(&mut self, out: &mut [f32], channels: usize) {
        if channels == 0 || !self.voices.iter().any(|v| v.active) {
            return;
        }
        let dt = 1.0 / self.rate;
        for frame in out.chunks_exact_mut(channels) {
            let (mut l, mut r) = (0.0f32, 0.0f32);
            for v in self.voices.iter_mut().filter(|v| v.active) {
                if v.delay > 0.0 {
                    v.delay -= dt;
                    continue;
                }
                if v.t >= v.length {
                    v.active = false;
                    continue;
                }
                let u = v.t / v.length;
                let hz = v.from + (v.to - v.from) * u;
                let step = (hz * dt).min(0.5);
                let raw = wave(v, step);
                v.phase = (v.phase + step).fract();
                let level = (v.t / ATTACK).min(1.0) * (1.0 - u) * (1.0 - u);
                l += raw * level * v.left;
                r += raw * level * v.right;
                v.t += dt;
            }
            if channels == 1 {
                frame[0] += soft_clip((l + r) * std::f32::consts::FRAC_1_SQRT_2);
            } else {
                frame[0] += soft_clip(l);
                frame[1] += soft_clip(r);
            }
        }
    }

    /// The voices sounding or waiting to.
    pub fn voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    /// A silent voice, or the one nearest its end.
    fn free_voice(&self) -> usize {
        if let Some(i) = self.voices.iter().position(|v| !v.active) {
            return i;
        }
        let progress = |v: &Voice| if v.delay > 0.0 { 0.0 } else { v.t / v.length };
        (0..MAX_VOICES)
            .max_by(|&a, &b| progress(&self.voices[a]).total_cmp(&progress(&self.voices[b])))
            .unwrap_or(0)
    }
}

/// The voice's waveform at its phase, `step` its phase increment.
fn wave(v: &mut Voice, step: f32) -> f32 {
    let t = v.phase;
    match v.wave {
        Wave::Sine => (t * std::f32::consts::TAU).sin(),
        Wave::Square => {
            let square = if t < 0.5 { 1.0 } else { -1.0 };
            square + blep(t, step) - blep((t + 0.5).fract(), step)
        }
        Wave::Saw => 2.0 * t - 1.0 - blep(t, step),
        Wave::Triangle => 1.0 - 4.0 * (t - 0.5).abs(),
        Wave::Noise => {
            v.noise ^= v.noise << 13;
            v.noise ^= v.noise >> 17;
            v.noise ^= v.noise << 5;
            (v.noise as f32 / u32::MAX as f32) * 2.0 - 1.0
        }
    }
}

/// The PolyBLEP correction of a unit step at phase 0, for phase `t` and
/// increment `step`.
fn blep(t: f32, step: f32) -> f32 {
    if step <= 0.0 {
        0.0
    } else if t < step {
        let t = t / step;
        t + t - t * t - 1.0
    } else if t > 1.0 - step {
        let t = (t - 1.0) / step;
        t * t + t + t + 1.0
    } else {
        0.0
    }
}

/// A smooth limit of `x` to `(-1, 1)`: linear near 0, a Padé `tanh` beyond.
fn soft_clip(x: f32) -> f32 {
    let x = x.clamp(-3.0, 3.0);
    x * (27.0 + x * x) / (27.0 + 9.0 * x * x)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(cue: Cue, seconds: f32) -> (Synth, Vec<f32>) {
        let mut synth = Synth::new(48000.0);
        synth.play(&cue);
        let mut out = vec![0.0; (48000.0 * seconds) as usize * 2];
        synth.render(&mut out, 2);
        (synth, out)
    }

    fn energy(samples: impl Iterator<Item = f32>) -> f32 {
        samples.map(|s| s * s).sum()
    }

    #[test]
    fn every_bank_sound_sounds_and_ends() {
        for (i, _) in Sfx::VARIANTS.iter().enumerate() {
            let sfx = Sfx::from_index(i as i64).expect("a variant");
            let cue = Cue {
                sound: Sound::Sfx(sfx),
                gain: 1.0,
                pan: 0.0,
            };
            let (synth, out) = render(cue, 1.2);
            assert!(energy(out.iter().copied()) > 1.0, "{sfx:?} is silent");
            assert!(out.iter().all(|s| s.abs() < 1.0), "{sfx:?} clips");
            assert_eq!(synth.voices(), 0, "{sfx:?} still sounds");
        }
    }

    #[test]
    fn a_tone_sounds_at_its_pitch() {
        let cue = Cue {
            sound: Sound::Tone {
                wave: Wave::Sine,
                from: 440.0,
                to: 440.0,
                seconds: 1.0,
            },
            gain: 1.0,
            pan: 0.0,
        };
        let (_, out) = render(cue, 0.5);
        let left: Vec<f32> = out.iter().step_by(2).copied().collect();
        let rises = left
            .windows(2)
            .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
            .count();
        assert!(
            (219..=221).contains(&rises),
            "{rises} cycles in half a second"
        );
    }

    #[test]
    fn pan_moves_the_sound_between_channels() {
        let cue = |pan| Cue {
            sound: Sound::Sfx(Sfx::Calm),
            gain: 1.0,
            pan,
        };
        let (_, out) = render(cue(-1.0), 0.3);
        let (l, r) = (
            energy(out.iter().step_by(2).copied()),
            energy(out.iter().skip(1).step_by(2).copied()),
        );
        assert!(l > 1.0 && r < 1e-6, "{l} {r}");
    }

    #[test]
    fn band_limiting_cuts_the_aliases_of_a_high_square_wave() {
        // At 48 kHz the 15th and 31st harmonics of 3.1 kHz fold back to
        // 1.5 kHz and 100 Hz, where a square wave has no energy of its own.
        let power = |samples: &[f32], hz: f32| {
            let coeff = 2.0 * (std::f32::consts::TAU * hz / 48000.0).cos();
            let (mut s1, mut s2) = (0.0f32, 0.0f32);
            for &x in samples {
                let s0 = x + coeff * s1 - s2;
                s2 = s1;
                s1 = s0;
            }
            s1 * s1 + s2 * s2 - coeff * s1 * s2
        };
        let mut voice = Voice {
            wave: Wave::Square,
            active: true,
            ..SILENT
        };
        let step = 3100.0 / 48000.0;
        let (mut limited, mut naive) = (Vec::new(), Vec::new());
        for _ in 0..48000 {
            naive.push(if voice.phase < 0.5 { 1.0 } else { -1.0 });
            limited.push(wave(&mut voice, step));
            voice.phase = (voice.phase + step).fract();
        }
        for hz in [1500.0, 100.0] {
            let (l, n) = (power(&limited, hz), power(&naive, hz));
            assert!(l < n * 0.25, "{hz} Hz: {l} vs {n}");
        }
    }

    #[test]
    fn a_full_synth_steals_the_voice_nearest_its_end() {
        let mut synth = Synth::new(48000.0);
        let cue = Cue {
            sound: Sound::Sfx(Sfx::Jump),
            gain: 0.5,
            pan: 0.0,
        };
        for _ in 0..MAX_VOICES + 5 {
            synth.play(&cue);
        }
        assert_eq!(synth.voices(), MAX_VOICES);
    }
}
