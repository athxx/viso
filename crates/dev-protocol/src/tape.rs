//! UI input tapes: the input a window took, frame by frame, with the frame
//! clock and the code revisions the run crossed, so a headless run replays
//! the same run.
//!
//! A runtime records the first window it opens while [`RECORD_ENV`] names
//! where the tape goes, and writes it there as the run ends; a headless run
//! handed a tape in [`TAPE_ENV`] plays it back against its first window. A
//! tape keeps every frame the window ran, in order, each with the time the
//! frame clock advanced by and the input that reached the window before it,
//! in logical points, so a replay on the same surface delivers the same
//! input at the same frame and the same time. A live patch is kept as the
//! frame it landed in and the dev revision it moved the app to: a tape
//! holds no code, so a replay runs the build it is handed and reports the
//! revisions the recording crossed.

use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder, ProtocolTag};

pub use crate::wire::BuildId;

/// The environment variable the path a runtime records its first window's
/// input to is passed in.
pub const RECORD_ENV: &str = "VISO_RECORD_TAPE";

/// The environment variable the path of the tape a headless run replays is
/// passed in.
pub const TAPE_ENV: &str = "VISO_HEADLESS_TAPE";

/// The most frames a tape keeps: past them a recording stops.
pub const MAX_FRAMES: usize = 1 << 22;

/// The most inputs one frame keeps.
pub const MAX_INPUTS: usize = 1 << 12;

/// A recorded run of a window's input.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UiTape {
    /// The build the run started on; 0 when unknown.
    pub build: BuildId,
    /// The dev revision the run started at; 0 off a dev session.
    pub revision: u64,
    /// The window's logical size and scale as the run started.
    pub surface: TapeSurface,
    /// Every frame the window ran, in order.
    pub frames: Vec<TapeFrame>,
    /// The live patches the run took, by ascending frame.
    pub patches: Vec<TapePatch>,
}

/// A window's logical size and scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TapeSurface {
    pub width: f32,
    pub height: f32,
    pub scale: f32,
}

impl Default for TapeSurface {
    fn default() -> Self {
        TapeSurface {
            width: 1280.0,
            height: 720.0,
            scale: 1.0,
        }
    }
}

/// One frame: how far the frame clock advanced since the frame before, and
/// the input that reached the window ahead of it, in arrival order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TapeFrame {
    pub delta_us: u32,
    pub inputs: Vec<TapeInput>,
}

/// A live patch the run took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TapePatch {
    /// The frame it landed in, ahead of the frame's flush: the input of that
    /// frame ran on the code before it.
    pub frame: u32,
    /// The dev revision it moved the app to.
    pub revision: u64,
}

/// A pointer's device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TapePointerKind {
    Mouse = 0,
    Touch = 1,
    Pen = 2,
}

/// A pointer sample's phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TapePointerPhase {
    Moved = 0,
    Down = 1,
    Up = 2,
    Left = 3,
    Cancel = 4,
}

/// A key: its place among the named keys, or a key no name covers, by the
/// code the platform reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapeKey {
    Named(u16),
    Other(u32),
}

/// The modifier keys held, one bit each: shift, control, alt, logo.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TapeModifiers(pub u8);

impl TapeModifiers {
    pub const SHIFT: u8 = 1;
    pub const CONTROL: u8 = 1 << 1;
    pub const ALT: u8 = 1 << 2;
    pub const LOGO: u8 = 1 << 3;

    /// The modifiers of the four held states.
    pub fn of(shift: bool, control: bool, alt: bool, logo: bool) -> TapeModifiers {
        let bit = |held: bool, bit: u8| if held { bit } else { 0 };
        TapeModifiers(
            bit(shift, Self::SHIFT)
                | bit(control, Self::CONTROL)
                | bit(alt, Self::ALT)
                | bit(logo, Self::LOGO),
        )
    }

    /// Whether `bit` is held.
    pub fn holds(self, bit: u8) -> bool {
        self.0 & bit != 0
    }
}

/// One input a window took.
#[derive(Debug, Clone, PartialEq)]
pub enum TapeInput {
    Pointer {
        pointer: u64,
        kind: TapePointerKind,
        phase: TapePointerPhase,
        x: f32,
        y: f32,
        pressure: f32,
        buttons: u8,
        modifiers: TapeModifiers,
    },
    Scroll {
        x: f32,
        y: f32,
        delta_x: f32,
        delta_y: f32,
        modifiers: TapeModifiers,
    },
    Key {
        key: TapeKey,
        pressed: bool,
        repeat: bool,
        modifiers: TapeModifiers,
    },
    /// Committed text.
    Text(String),
    /// An IME composition in progress, its caret a byte offset.
    Preedit { text: String, caret: u32 },
    /// Clipboard text pasted.
    Paste(String),
    /// A copy, or a cut, of the selection.
    Copy { cut: bool },
    /// The window gained or lost the focus.
    Focus(bool),
    /// The window took a new logical size and scale.
    Resize(TapeSurface),
    /// The edit menu's Select All.
    SelectAll,
    /// The edit menu closed on its own.
    EditMenuClosed,
}

/// What follows the stream tag of a tape file, then its version.
const TAPE_KIND: &[u8; 6] = b"uitape";
const TAPE_VERSION: u16 = 1;

impl UiTape {
    /// The live patches the run took up to frame `frame`, inclusive.
    pub fn patches_until(&self, frame: u32) -> impl Iterator<Item = &TapePatch> {
        self.patches.iter().take_while(move |p| p.frame <= frame)
    }

    /// The revisions the run crossed: the one it started at, then each live
    /// patch's.
    pub fn revisions(&self) -> impl Iterator<Item = u64> + '_ {
        core::iter::once(self.revision).chain(self.patches.iter().map(|p| p.revision))
    }

    /// The tape as its file holds it.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut enc = Encoder::with_capacity(64 + self.frames.len() * 4);
        ProtocolTag::current().encode(&mut enc);
        enc.write_raw(TAPE_KIND);
        enc.write_u16(TAPE_VERSION);
        self.build.encode(&mut enc);
        enc.write_varint(self.revision);
        write_surface(&mut enc, self.surface);
        enc.write_varint(self.frames.len() as u64);
        for frame in &self.frames {
            enc.write_varint(u64::from(frame.delta_us));
            enc.write_varint(frame.inputs.len() as u64);
            for input in &frame.inputs {
                input.encode(&mut enc);
            }
        }
        enc.write_varint(self.patches.len() as u64);
        for patch in &self.patches {
            enc.write_varint(u64::from(patch.frame));
            enc.write_varint(patch.revision);
        }
        enc.into_bytes()
    }

    /// The tape `bytes` spell.
    ///
    /// # Errors
    ///
    /// Bytes that are not a tape of this version, or a tape past its bounds:
    /// more than [`MAX_FRAMES`] frames, more than [`MAX_INPUTS`] inputs a
    /// frame, or patches out of order or past its frames.
    pub fn from_bytes(bytes: &[u8]) -> Result<UiTape, DecodeError> {
        let mut dec = Decoder::new(bytes);
        let tag = ProtocolTag::decode(&mut dec)?;
        let at = dec.position();
        if !tag.is_compatible()
            || dec.read_raw(TAPE_KIND.len())? != TAPE_KIND
            || dec.read_u16()? != TAPE_VERSION
        {
            return Err(DecodeError::Malformed { offset: at });
        }
        let build = BuildId::decode(&mut dec)?;
        let revision = dec.read_varint()?;
        let surface = read_surface(&mut dec)?;
        let count = bounded(&mut dec, MAX_FRAMES)?;
        let mut frames = Vec::with_capacity(count.min(1 << 16));
        for _ in 0..count {
            let at = dec.position();
            let delta_us = u32::try_from(dec.read_varint()?)
                .map_err(|_| DecodeError::Malformed { offset: at })?;
            let inputs = bounded(&mut dec, MAX_INPUTS)?;
            let inputs = (0..inputs)
                .map(|_| TapeInput::decode(&mut dec))
                .collect::<Result<_, _>>()?;
            frames.push(TapeFrame { delta_us, inputs });
        }
        let count = bounded(&mut dec, MAX_FRAMES)?;
        let mut patches: Vec<TapePatch> = Vec::with_capacity(count.min(1 << 10));
        for _ in 0..count {
            let at = dec.position();
            let frame = u32::try_from(dec.read_varint()?)
                .ok()
                .filter(|&f| (f as usize) < frames.len())
                .filter(|&f| patches.last().is_none_or(|p| p.frame <= f))
                .ok_or(DecodeError::Malformed { offset: at })?;
            let revision = dec.read_varint()?;
            patches.push(TapePatch { frame, revision });
        }
        if !dec.is_empty() {
            return Err(DecodeError::Malformed {
                offset: dec.position(),
            });
        }
        Ok(UiTape {
            build,
            revision,
            surface,
            frames,
            patches,
        })
    }
}

/// A count read and held to `max`.
fn bounded(dec: &mut Decoder<'_>, max: usize) -> Result<usize, DecodeError> {
    let at = dec.position();
    usize::try_from(dec.read_varint()?)
        .ok()
        .filter(|&n| n <= max)
        .ok_or(DecodeError::Malformed { offset: at })
}

fn write_surface(enc: &mut Encoder, surface: TapeSurface) {
    enc.write_f32(surface.width);
    enc.write_f32(surface.height);
    enc.write_f32(surface.scale);
}

fn read_surface(dec: &mut Decoder<'_>) -> Result<TapeSurface, DecodeError> {
    let at = dec.position();
    let surface = TapeSurface {
        width: dec.read_f32()?,
        height: dec.read_f32()?,
        scale: dec.read_f32()?,
    };
    let edge = |v: f32| v.is_finite() && v > 0.0;
    if !(edge(surface.width) && edge(surface.height) && edge(surface.scale)) {
        return Err(DecodeError::Malformed { offset: at });
    }
    Ok(surface)
}

impl Encode for TapeInput {
    fn encode(&self, enc: &mut Encoder) {
        match self {
            TapeInput::Pointer {
                pointer,
                kind,
                phase,
                x,
                y,
                pressure,
                buttons,
                modifiers,
            } => {
                enc.write_u8(0);
                enc.write_varint(*pointer);
                enc.write_u8(*kind as u8);
                enc.write_u8(*phase as u8);
                enc.write_f32(*x);
                enc.write_f32(*y);
                enc.write_f32(*pressure);
                enc.write_u8(*buttons);
                enc.write_u8(modifiers.0);
            }
            TapeInput::Scroll {
                x,
                y,
                delta_x,
                delta_y,
                modifiers,
            } => {
                enc.write_u8(1);
                enc.write_f32(*x);
                enc.write_f32(*y);
                enc.write_f32(*delta_x);
                enc.write_f32(*delta_y);
                enc.write_u8(modifiers.0);
            }
            TapeInput::Key {
                key,
                pressed,
                repeat,
                modifiers,
            } => {
                enc.write_u8(2);
                match key {
                    TapeKey::Named(at) => {
                        enc.write_u8(0);
                        enc.write_u16(*at);
                    }
                    TapeKey::Other(code) => {
                        enc.write_u8(1);
                        enc.write_u32(*code);
                    }
                }
                enc.write_u8(u8::from(*pressed) | (u8::from(*repeat) << 1));
                enc.write_u8(modifiers.0);
            }
            TapeInput::Text(text) => {
                enc.write_u8(3);
                enc.write_str(text);
            }
            TapeInput::Preedit { text, caret } => {
                enc.write_u8(4);
                enc.write_str(text);
                enc.write_varint(u64::from(*caret));
            }
            TapeInput::Paste(text) => {
                enc.write_u8(5);
                enc.write_str(text);
            }
            TapeInput::Copy { cut } => {
                enc.write_u8(6);
                enc.write_u8(u8::from(*cut));
            }
            TapeInput::Focus(focused) => {
                enc.write_u8(7);
                enc.write_u8(u8::from(*focused));
            }
            TapeInput::Resize(surface) => {
                enc.write_u8(8);
                write_surface(enc, *surface);
            }
            TapeInput::SelectAll => enc.write_u8(9),
            TapeInput::EditMenuClosed => enc.write_u8(10),
        }
    }
}

impl Decode for TapeInput {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let at = dec.position();
        let malformed = DecodeError::Malformed { offset: at };
        let modifiers = |dec: &mut Decoder<'_>| -> Result<TapeModifiers, DecodeError> {
            let at = dec.position();
            let bits = dec.read_u8()?;
            if bits >> 4 != 0 {
                return Err(DecodeError::Malformed { offset: at });
            }
            Ok(TapeModifiers(bits))
        };
        let flag = |dec: &mut Decoder<'_>| -> Result<bool, DecodeError> {
            let at = dec.position();
            match dec.read_u8()? {
                0 => Ok(false),
                1 => Ok(true),
                _ => Err(DecodeError::Malformed { offset: at }),
            }
        };
        Ok(match dec.read_u8()? {
            0 => {
                let pointer = dec.read_varint()?;
                let kind = match dec.read_u8()? {
                    0 => TapePointerKind::Mouse,
                    1 => TapePointerKind::Touch,
                    2 => TapePointerKind::Pen,
                    _ => return Err(malformed),
                };
                let phase = match dec.read_u8()? {
                    0 => TapePointerPhase::Moved,
                    1 => TapePointerPhase::Down,
                    2 => TapePointerPhase::Up,
                    3 => TapePointerPhase::Left,
                    4 => TapePointerPhase::Cancel,
                    _ => return Err(malformed),
                };
                TapeInput::Pointer {
                    pointer,
                    kind,
                    phase,
                    x: dec.read_f32()?,
                    y: dec.read_f32()?,
                    pressure: dec.read_f32()?,
                    buttons: dec.read_u8()?,
                    modifiers: modifiers(dec)?,
                }
            }
            1 => TapeInput::Scroll {
                x: dec.read_f32()?,
                y: dec.read_f32()?,
                delta_x: dec.read_f32()?,
                delta_y: dec.read_f32()?,
                modifiers: modifiers(dec)?,
            },
            2 => {
                let key = match dec.read_u8()? {
                    0 => TapeKey::Named(dec.read_u16()?),
                    1 => TapeKey::Other(dec.read_u32()?),
                    _ => return Err(malformed),
                };
                let at = dec.position();
                let bits = dec.read_u8()?;
                if bits >> 2 != 0 {
                    return Err(DecodeError::Malformed { offset: at });
                }
                TapeInput::Key {
                    key,
                    pressed: bits & 1 != 0,
                    repeat: bits & 2 != 0,
                    modifiers: modifiers(dec)?,
                }
            }
            3 => TapeInput::Text(dec.read_str()?.to_owned()),
            4 => {
                let text = dec.read_str()?.to_owned();
                let at = dec.position();
                let caret = u32::try_from(dec.read_varint()?)
                    .ok()
                    .filter(|&c| text.is_char_boundary(c as usize))
                    .ok_or(DecodeError::Malformed { offset: at })?;
                TapeInput::Preedit { text, caret }
            }
            5 => TapeInput::Paste(dec.read_str()?.to_owned()),
            6 => TapeInput::Copy { cut: flag(dec)? },
            7 => TapeInput::Focus(flag(dec)?),
            8 => TapeInput::Resize(read_surface(dec)?),
            9 => TapeInput::SelectAll,
            10 => TapeInput::EditMenuClosed,
            _ => return Err(malformed),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> UiTape {
        let modifiers = TapeModifiers::of(true, false, false, true);
        UiTape {
            build: BuildId(0xb111d),
            revision: 3,
            surface: TapeSurface {
                width: 400.0,
                height: 300.0,
                scale: 2.0,
            },
            frames: vec![
                TapeFrame {
                    delta_us: 16_667,
                    inputs: vec![
                        TapeInput::Pointer {
                            pointer: 0,
                            kind: TapePointerKind::Mouse,
                            phase: TapePointerPhase::Down,
                            x: 10.5,
                            y: 20.0,
                            pressure: 1.0,
                            buttons: 1,
                            modifiers,
                        },
                        TapeInput::Key {
                            key: TapeKey::Named(2),
                            pressed: true,
                            repeat: false,
                            modifiers,
                        },
                        TapeInput::Key {
                            key: TapeKey::Other(0xe0),
                            pressed: false,
                            repeat: true,
                            modifiers: TapeModifiers::default(),
                        },
                    ],
                },
                TapeFrame::default(),
                TapeFrame {
                    delta_us: 33_000,
                    inputs: vec![
                        TapeInput::Scroll {
                            x: 1.0,
                            y: 2.0,
                            delta_x: 0.0,
                            delta_y: -3.5,
                            modifiers,
                        },
                        TapeInput::Text("é".into()),
                        TapeInput::Preedit {
                            text: "にほ".into(),
                            caret: 3,
                        },
                        TapeInput::Paste("x".into()),
                        TapeInput::Copy { cut: true },
                        TapeInput::Focus(false),
                        TapeInput::Resize(TapeSurface::default()),
                        TapeInput::SelectAll,
                        TapeInput::EditMenuClosed,
                    ],
                },
            ],
            patches: vec![
                TapePatch {
                    frame: 1,
                    revision: 4,
                },
                TapePatch {
                    frame: 2,
                    revision: 5,
                },
            ],
        }
    }

    #[test]
    fn a_tape_round_trips_and_names_the_revisions_it_crossed() {
        let tape = sample();
        let bytes = tape.to_bytes();
        assert_eq!(UiTape::from_bytes(&bytes).as_ref(), Ok(&tape));
        assert_eq!(tape.revisions().collect::<Vec<_>>(), [3, 4, 5]);
        assert_eq!(tape.patches_until(1).count(), 1);
    }

    #[test]
    fn a_damaged_tape_does_not_decode() {
        let bytes = sample().to_bytes();
        for cut in 0..bytes.len() {
            assert!(UiTape::from_bytes(&bytes[..cut]).is_err(), "cut at {cut}");
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(UiTape::from_bytes(&trailing).is_err());

        // A patch past the frames, and patches out of order.
        for patches in [
            vec![TapePatch {
                frame: 9,
                revision: 1,
            }],
            vec![
                TapePatch {
                    frame: 2,
                    revision: 1,
                },
                TapePatch {
                    frame: 1,
                    revision: 2,
                },
            ],
        ] {
            let tape = UiTape {
                patches,
                ..sample()
            };
            assert!(UiTape::from_bytes(&tape.to_bytes()).is_err());
        }
        // A caret inside a character.
        let tape = UiTape {
            frames: vec![TapeFrame {
                delta_us: 0,
                inputs: vec![TapeInput::Preedit {
                    text: "é".into(),
                    caret: 1,
                }],
            }],
            ..sample()
        };
        assert!(UiTape::from_bytes(&tape.to_bytes()).is_err());
    }
}
