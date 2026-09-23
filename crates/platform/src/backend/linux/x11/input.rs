//! Pointer, pen and touch input through XInput 2.2: per-device scroll axes
//! for smooth scrolling, pressure for pens, and touch sequences as separate
//! pointers.

use std::collections::HashMap;

use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::xinput::{self, DeviceClass, DeviceClassData, DeviceType, ScrollType};
use x11rb::protocol::xproto::Atom;

use super::super::translate;
use crate::event::{Modifiers, PointerButtons, PointerId, PointerKind};

/// Touch contacts get pointer ids above every mouse and pen id.
const TOUCH_BIT: u64 = 1 << 32;
const PEN_BIT: u64 = 1 << 33;

/// What one physical (slave) device reports besides position.
#[derive(Debug, Default, Clone, PartialEq)]
pub(super) struct Device {
    /// Lowercased, for telling touchpads from tablets.
    name: String,
    pub(super) kind: PointerKind,
    scroll: Vec<ScrollAxis>,
    pressure: Option<Valuator>,
}

#[derive(Debug, Clone, PartialEq)]
struct ScrollAxis {
    number: u16,
    vertical: bool,
    /// Valuator units per wheel notch.
    increment: f64,
    /// The last value seen; the first sample after (re)entry only sets it.
    last: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Valuator {
    number: u16,
    min: f64,
    max: f64,
}

/// What an XI2 motion's valuators mean.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Motion {
    /// Scroll in logical points (positive = towards the user / right).
    pub(super) scroll: (f64, f64),
    /// Normalized pen pressure, when the device senses it.
    pub(super) pressure: Option<f32>,
}

pub(super) fn fp1616(v: i32) -> f64 {
    f64::from(v) / 65536.0
}

fn fp3232(v: xinput::Fp3232) -> f64 {
    f64::from(v.integral) + f64::from(v.frac) / 4_294_967_296.0
}

impl Device {
    fn from_classes(name: String, classes: &[DeviceClass], abs_pressure: Atom) -> Self {
        let mut valuators = HashMap::new();
        let mut scroll = Vec::new();
        let mut has_touch = false;
        for class in classes {
            match &class.data {
                DeviceClassData::Valuator(v) => {
                    valuators.insert(v.number, (v.label, fp3232(v.min), fp3232(v.max)));
                }
                DeviceClassData::Scroll(s) => {
                    let increment = fp3232(s.increment);
                    if increment != 0.0 {
                        scroll.push(ScrollAxis {
                            number: s.number,
                            vertical: s.scroll_type == ScrollType::VERTICAL,
                            increment,
                            last: None,
                        });
                    }
                }
                DeviceClassData::Touch(_) => has_touch = true,
                _ => {}
            }
        }
        let pressure = valuators
            .iter()
            .find(|(_, (label, min, max))| *label == abs_pressure && max > min)
            .map(|(&number, &(_, min, max))| Valuator { number, min, max });
        // Touchpads and touchscreens report pressure too; only a stylus is
        // a pen.
        let pen = pressure.is_some() && !has_touch && !name.contains("touchpad");
        Self {
            name,
            kind: if pen {
                PointerKind::Pen
            } else {
                PointerKind::Mouse
            },
            scroll,
            pressure: pressure.filter(|_| pen),
        }
    }

    pub(super) fn has_scroll(&self) -> bool {
        !self.scroll.is_empty()
    }

    fn reset_scroll(&mut self) {
        for axis in &mut self.scroll {
            axis.last = None;
        }
    }

    /// Read `mask`/`values` (the valuators an event carries, in number
    /// order) as scroll and pressure.
    fn motion(&mut self, mask: &[u32], values: &[xinput::Fp3232]) -> Motion {
        let mut motion = Motion::default();
        for (number, value) in set_bits(mask).zip(values.iter().copied().map(fp3232)) {
            if let Some(axis) = self.scroll.iter_mut().find(|a| a.number == number) {
                if let Some(last) = axis.last {
                    let delta = translate::wheel_notches((value - last) / axis.increment);
                    if axis.vertical {
                        motion.scroll.1 += delta;
                    } else {
                        motion.scroll.0 += delta;
                    }
                }
                axis.last = Some(value);
            } else if let Some(p) = self.pressure.filter(|p| p.number == number) {
                motion.pressure = Some(((value - p.min) / (p.max - p.min)).clamp(0.0, 1.0) as f32);
            }
        }
        motion
    }
}

/// The valuator numbers set in an XI2 mask, lowest first.
fn set_bits(mask: &[u32]) -> impl Iterator<Item = u16> + '_ {
    mask.iter().enumerate().flat_map(|(word, bits)| {
        (0..32u16)
            .filter(move |bit| bits & (1 << bit) != 0)
            .map(move |bit| u16::try_from(word).unwrap_or(u16::MAX).saturating_mul(32) + bit)
    })
}

/// Every pointing device, by XI2 device id.
#[derive(Default)]
pub(super) struct Devices {
    devices: HashMap<u16, Device>,
}

impl Devices {
    /// Re-read every slave pointer.
    pub(super) fn refresh(
        &mut self,
        conn: &impl Connection,
        abs_pressure: Atom,
    ) -> Result<(), ReplyError> {
        let reply = xinput::xi_query_device(conn, xinput::Device::ALL)?.reply()?;
        self.devices = reply
            .infos
            .iter()
            .filter(|i| {
                i.type_ == DeviceType::SLAVE_POINTER || i.type_ == DeviceType::FLOATING_SLAVE
            })
            .map(|i| {
                let name = String::from_utf8_lossy(&i.name).to_ascii_lowercase();
                (
                    i.deviceid,
                    Device::from_classes(name, &i.classes, abs_pressure),
                )
            })
            .collect();
        Ok(())
    }

    /// A device's classes changed (a tablet tool switch, a master pointer
    /// taking over a different slave).
    pub(super) fn changed(&mut self, device: u16, classes: &[DeviceClass], abs_pressure: Atom) {
        if let Some(d) = self.devices.get_mut(&device) {
            *d = Device::from_classes(std::mem::take(&mut d.name), classes, abs_pressure);
        }
        self.reset_scroll();
    }

    pub(super) fn reset_scroll(&mut self) {
        for d in self.devices.values_mut() {
            d.reset_scroll();
        }
    }

    pub(super) fn kind(&self, source: u16) -> PointerKind {
        self.devices
            .get(&source)
            .map_or(PointerKind::Mouse, |d| d.kind)
    }

    pub(super) fn has_scroll(&self, source: u16) -> bool {
        self.devices.get(&source).is_some_and(Device::has_scroll)
    }

    pub(super) fn motion(
        &mut self,
        source: u16,
        mask: &[u32],
        values: &[xinput::Fp3232],
    ) -> Motion {
        self.devices
            .get_mut(&source)
            .map(|d| d.motion(mask, values))
            .unwrap_or_default()
    }
}

/// The pointer id a device or touch sequence reports as.
pub(super) fn pointer_id(kind: PointerKind, source: u16, touch: u32) -> PointerId {
    match kind {
        PointerKind::Mouse => PointerId::MOUSE,
        PointerKind::Pen => PointerId(PEN_BIT | u64::from(source)),
        PointerKind::Touch => PointerId(TOUCH_BIT | u64::from(touch)),
    }
}

/// The buttons an X button number stands for.
pub(super) fn button(detail: u32) -> PointerButtons {
    match detail {
        1 => PointerButtons::PRIMARY,
        2 => PointerButtons::MIDDLE,
        3 => PointerButtons::SECONDARY,
        _ => PointerButtons::NONE,
    }
}

/// The scroll a legacy wheel button (4-7) stands for, in logical points.
pub(super) fn wheel_button(detail: u32) -> Option<(f64, f64)> {
    let notch = translate::wheel_notches(1.0);
    match detail {
        4 => Some((0.0, -notch)),
        5 => Some((0.0, notch)),
        6 => Some((-notch, 0.0)),
        7 => Some((notch, 0.0)),
        _ => None,
    }
}

/// Modifiers from an X modifier mask (Shift, Control, Mod1, Mod4).
pub(super) fn modifiers(mask: u32) -> Modifiers {
    Modifiers {
        shift: mask & 1 != 0,
        control: mask & 4 != 0,
        alt: mask & 8 != 0,
        logo: mask & 64 != 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(v: f64) -> xinput::Fp3232 {
        xinput::Fp3232 {
            integral: v.floor() as i32,
            frac: ((v - v.floor()) * 4_294_967_296.0) as u32,
        }
    }

    fn wheel_device() -> Device {
        Device {
            name: String::new(),
            kind: PointerKind::Mouse,
            scroll: vec![
                ScrollAxis {
                    number: 2,
                    vertical: false,
                    increment: 15.0,
                    last: None,
                },
                ScrollAxis {
                    number: 3,
                    vertical: true,
                    increment: 15.0,
                    last: None,
                },
            ],
            pressure: None,
        }
    }

    #[test]
    fn mask_bits_enumerate_in_order() {
        assert_eq!(set_bits(&[0b1010, 1]).collect::<Vec<_>>(), [1, 3, 32]);
    }

    #[test]
    fn smooth_scroll_is_relative_to_the_previous_sample() {
        let mut d = wheel_device();
        // The first sample only records the position.
        assert_eq!(d.motion(&[0b1000], &[fp(100.0)]).scroll, (0.0, 0.0));
        let down = d.motion(&[0b1000], &[fp(115.0)]).scroll;
        assert_eq!(down, (0.0, translate::wheel_notches(1.0)));
        let both = d.motion(&[0b1100], &[fp(-7.5), fp(100.0)]).scroll;
        assert_eq!(both, (0.0, translate::wheel_notches(-1.0)), "x only primed");
        d.reset_scroll();
        assert_eq!(d.motion(&[0b1000], &[fp(0.0)]).scroll, (0.0, 0.0));
    }

    #[test]
    fn wheel_buttons_match_the_wheel_sign() {
        let (_, up) = wheel_button(4).unwrap();
        assert!(
            up < 0.0,
            "wheel away from the user scrolls like the other backends"
        );
        assert_eq!(wheel_button(5).unwrap().1, -up);
        assert_eq!(wheel_button(1), None);
    }

    #[test]
    fn pen_pressure_normalizes() {
        let mut d = Device {
            name: "stylus".into(),
            kind: PointerKind::Pen,
            scroll: Vec::new(),
            pressure: Some(Valuator {
                number: 2,
                min: 0.0,
                max: 2048.0,
            }),
        };
        let m = d.motion(&[0b111], &[fp(10.0), fp(20.0), fp(512.0)]);
        assert_eq!(m.pressure, Some(0.25));
    }

    #[test]
    fn ids_separate_kinds() {
        assert_eq!(pointer_id(PointerKind::Mouse, 9, 0), PointerId::MOUSE);
        assert_ne!(
            pointer_id(PointerKind::Touch, 9, 3),
            pointer_id(PointerKind::Pen, 3, 0)
        );
        assert!(modifiers(1 | 4).shift);
        assert_eq!(button(3), PointerButtons::SECONDARY);
    }
}
