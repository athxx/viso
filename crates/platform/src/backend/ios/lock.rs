//! Pointer lock on iPadOS: the view controller prefers the pointer locked,
//! the scene's pointer lock state says when the system locked it, and the
//! connected mice's GameController input reports its motion meanwhile.

use block2::{Block, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{msg_send, sel};
use objc2_foundation::NSArray;
use objc2_ui_kit::UIViewController;

use super::{LOOP, WINDOW, drive, push};
use crate::event::RawEvent;

#[link(name = "GameController", kind = "framework")]
unsafe extern "C" {}

/// What a mouse's `mouseMovedHandler` is called with.
type Moved = dyn Fn(*mut AnyObject, f32, f32);

/// Asks for the pointer locked, or let go, through the view controller's
/// preference; the system answers through the scene's lock state. Refused
/// at once where no mouse is connected or the system has no pointer lock.
pub(super) fn set(controller: &UIViewController, lock: bool) {
    if LOOP.with(|l| l.lock_wanted.replace(lock)) == lock {
        return;
    }
    // SAFETY: asking whether the controller answers a selector.
    let can: bool = unsafe {
        msg_send![controller, respondsToSelector: sel!(setNeedsUpdateOfPrefersPointerLocked)]
    };
    if lock && (!can || mice().is_none_or(|m| m.count() == 0)) {
        LOOP.with(|l| l.lock_wanted.set(false));
        push(RawEvent::PointerLockChanged {
            window: WINDOW,
            locked: false,
        });
        return;
    }
    if can {
        controller.setNeedsUpdateOfPrefersPointerLocked();
    }
}

/// The scene's pointer lock changed: report it, and hear the mice's motion
/// while it holds.
pub(super) fn changed() {
    let locked = LOOP
        .with(|l| l.scene.borrow().clone())
        .and_then(|scene| scene.pointerLockState())
        .is_some_and(|state| state.isLocked());
    if LOOP.with(|l| l.pointer_locked.replace(locked)) == locked {
        return;
    }
    hear_mice(locked);
    if !locked {
        LOOP.with(|l| l.lock_wanted.set(false));
    }
    push(RawEvent::PointerLockChanged {
        window: WINDOW,
        locked,
    });
    drive();
}

/// The connected mice, where GameController knows them.
fn mice() -> Option<Retained<NSArray<AnyObject>>> {
    let class = AnyClass::get(c"GCMouse")?;
    // SAFETY: `+[GCMouse mice]` takes nothing and returns an array of mice.
    let mice: Option<Retained<NSArray<AnyObject>>> = unsafe { msg_send![class, mice] };
    mice
}

/// Sets every connected mouse's motion handler, or (`on` false) clears it.
fn hear_mice(on: bool) {
    let Some(mice) = mice() else { return };
    let handler: Option<RcBlock<Moved>> = on.then(|| {
        RcBlock::new(|_: *mut AnyObject, dx: f32, dy: f32| {
            // GameController's y grows up.
            LOOP.with(|l| {
                crate::event::queue_motion(
                    &mut l.events.borrow_mut(),
                    WINDOW,
                    f64::from(dx),
                    -f64::from(dy),
                );
            });
            drive();
        })
    });
    let block: Option<&Block<Moved>> = handler.as_deref();
    for mouse in mice.iter() {
        // SAFETY: a `GCMouse`'s `mouseInput` is a `GCMouseInput` or nil.
        let input: Option<Retained<AnyObject>> = unsafe { msg_send![&*mouse, mouseInput] };
        let Some(input) = input else { continue };
        // SAFETY: `mouseMovedHandler` takes a block of the
        // `(GCMouseInput *, float, float)` shape `Moved` has, or nil; the
        // input copies the block, which keeps it alive.
        unsafe {
            let _: () = msg_send![&*input, setMouseMovedHandler: block];
        }
    }
}

/// Whether the view controller prefers the pointer locked.
pub(super) fn wanted() -> bool {
    LOOP.with(|l| l.lock_wanted.get())
}
