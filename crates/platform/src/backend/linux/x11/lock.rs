//! Pointer lock: the master pointer grabbed by the window with no cursor,
//! held in the window's middle, its motion read from raw XInput events.

use x11rb::protocol::xinput::{self, XIEventMask};
use x11rb::protocol::xproto::{self, ConnectionExt as _, GrabMode, GrabStatus};

use super::X11;
use crate::control::WindowId;
use crate::event::RawEvent;

/// The lock held on one window.
pub(super) struct Lock {
    pub(super) window: WindowId,
    /// The master pointer grabbed.
    device: xinput::DeviceId,
    /// The last raw motion's time and sequence: the server hands a raw
    /// event to a client that grabs the device twice, once for the grab.
    last: Option<(xproto::Timestamp, u16)>,
}

/// A 32.32 fixed-point value.
fn fp3232(v: xinput::Fp3232) -> f64 {
    f64::from(v.integral) + f64::from(v.frac) / 4_294_967_296.0
}

impl X11 {
    /// Locks the pointer to `window`, or lets it go, reporting the change.
    /// A lock is refused unless the window has the keyboard's focus and
    /// the server grants the grab.
    pub(super) fn set_pointer_lock(&mut self, window: WindowId, lock: bool) {
        if !lock {
            if self.lock.as_ref().is_some_and(|l| l.window == window) {
                self.end_lock();
            }
            return;
        }
        if self.lock.as_ref().is_some_and(|l| l.window == window) {
            return;
        }
        self.end_lock();
        let engaged = self.focus == Some(window) && self.grab(window);
        if !engaged {
            self.pump.push(RawEvent::PointerLockChanged {
                window,
                locked: false,
            });
        }
    }

    /// Grabs the master pointer for `window` with no cursor, starts raw
    /// motion and pins the pointer. Returns whether the grab took.
    fn grab(&mut self, window: WindowId) -> bool {
        let Some(i) = self.index(window) else {
            return false;
        };
        let xid = self.windows[i].xid;
        let Some(device) = xinput::xi_get_client_pointer(&*self.conn, x11rb::NONE)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.deviceid)
        else {
            return false;
        };
        let Some(cursor) = self.cursor(crate::event::CursorIcon::Hidden) else {
            return false;
        };
        let mask = XIEventMask::BUTTON_PRESS
            | XIEventMask::BUTTON_RELEASE
            | XIEventMask::MOTION
            | XIEventMask::ENTER
            | XIEventMask::LEAVE;
        let granted = xinput::xi_grab_device(
            &*self.conn,
            xid,
            x11rb::CURRENT_TIME,
            cursor,
            device,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            xinput::GrabOwner::OWNER,
            &[u32::from(mask)],
        )
        .ok()
        .and_then(|c| c.reply().ok())
        .is_some_and(|r| r.status == GrabStatus::SUCCESS);
        if !granted {
            return false;
        }
        let _ = self.select_raw_motion(true);
        self.lock = Some(Lock {
            window,
            device,
            last: None,
        });
        self.pin();
        self.pump.push(RawEvent::PointerLockChanged {
            window,
            locked: true,
        });
        true
    }

    /// Ends the lock, if one is held, reporting it.
    pub(super) fn end_lock(&mut self) {
        let Some(lock) = self.lock.take() else {
            return;
        };
        let _ = xinput::xi_ungrab_device(&*self.conn, x11rb::CURRENT_TIME, lock.device);
        let _ = self.select_raw_motion(false);
        self.pump.push(RawEvent::PointerLockChanged {
            window: lock.window,
            locked: false,
        });
    }

    /// Hears the master pointers' raw motion on the root window, or stops;
    /// the devices' own changes are heard all along.
    fn select_raw_motion(&self, on: bool) -> Result<(), x11rb::errors::ConnectionError> {
        let raw = if on {
            XIEventMask::RAW_MOTION
        } else {
            XIEventMask::from(0u32)
        };
        xinput::xi_select_events(
            &*self.conn,
            self.root,
            &[
                xinput::EventMask {
                    deviceid: xinput::Device::ALL.into(),
                    mask: vec![XIEventMask::HIERARCHY | XIEventMask::DEVICE_CHANGED],
                },
                xinput::EventMask {
                    deviceid: xinput::Device::ALL_MASTER.into(),
                    mask: vec![raw],
                },
            ],
        )?;
        Ok(())
    }

    /// Moves the locked pointer back to the middle of its window.
    fn pin(&self) {
        let Some(lock) = &self.lock else { return };
        let Some(i) = self.index(lock.window) else {
            return;
        };
        let win = &self.windows[i];
        let (w, h) = win.size;
        let _ = self.conn.warp_pointer(
            x11rb::NONE,
            win.xid,
            0,
            0,
            0,
            0,
            (w / 2).min(i16::MAX as u32) as i16,
            (h / 2).min(i16::MAX as u32) as i16,
        );
    }

    /// A raw motion of a mouse: queued as the locked pointer's motion, the
    /// pointer pinned again.
    pub(super) fn on_raw_motion(&mut self, e: &xinput::RawMotionEvent) {
        self.last_time = e.time;
        let Some(lock) = &mut self.lock else { return };
        if lock.last.replace((e.time, e.sequence)) == Some((e.time, e.sequence)) {
            return;
        }
        let window = lock.window;
        let Some(i) = self.index(window) else { return };
        let scale = self.windows[i].scale;
        // The values listed are the valuators the mask sets, in order: the
        // first two a mouse's x and y.
        let mut values = e.axisvalues.iter().copied();
        let mut delta = [0.0f64; 2];
        for axis in 0..2u32 {
            let word = e.valuator_mask.first().copied().unwrap_or(0);
            if word & (1 << axis) != 0
                && let Some(v) = values.next()
            {
                delta[axis as usize] = fp3232(v) / scale;
            }
        }
        if delta != [0.0, 0.0] {
            self.pump.motion(window, delta[0], delta[1]);
            self.pin();
        }
    }

    /// Whether the pointer is locked to the window `xid`, whose own motion
    /// then only moves it back to its pin.
    pub(super) fn locked_to(&self, xid: xproto::Window) -> bool {
        self.lock
            .as_ref()
            .and_then(|l| self.index(l.window))
            .is_some_and(|i| self.windows[i].xid == xid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_point_values_read_as_fractions() {
        let v = |integral, frac| xinput::Fp3232 { integral, frac };
        assert_eq!(fp3232(v(3, 0)), 3.0);
        assert_eq!(fp3232(v(-2, 1 << 31)), -1.5);
    }
}
