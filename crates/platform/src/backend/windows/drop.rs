//! A window as an OLE drop target: a drag from outside reaches the app step
//! by step, and OLE hears the app's answer to each. And the drags the app
//! carries out of a window, which the shell runs from a data object holding
//! their files, link and text.
//!
//! OLE calls the target from the message loop, inside the pump's OS turn, so
//! each step is driven through the app at once and the effect it answers
//! with is the app's answer to that step.

use std::cell::RefCell;
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use ::windows::Win32::Foundation::{GlobalFree, HGLOBAL, HWND, POINT, POINTL};
use ::windows::Win32::Graphics::Gdi::ScreenToClient;
use ::windows::Win32::System::Com::{
    DVASPECT_CONTENT, FORMATETC, IDataObject, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
};
use ::windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use ::windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use ::windows::Win32::System::Ole::{
    CF_HDROP, CF_UNICODETEXT, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE,
    DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl, RegisterDragDrop, ReleaseStgMedium,
    RevokeDragDrop,
};
use ::windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use ::windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use ::windows::Win32::UI::Shell::{
    DROPFILES, DragQueryFileW, HDROP, SHCreateDataObject, SHDoDragDrop,
};
use ::windows::core::{Ref, implement, w};

use super::WindowState;
use crate::backend::win32_translate as translate;
use crate::drop::{
    DragOut, DropAction, DropActions, DropKinds, DropOffer, DropPhase, DropReply, RawDrop,
};
use crate::event::RawEvent;

/// Makes `state`'s window a drop target. OLE holds the target until
/// [`revoke`].
pub(super) fn register(state: &Rc<WindowState>) {
    let target: IDropTarget = DropTarget {
        state: Rc::downgrade(state),
        arrival: RefCell::new(None),
    }
    .into();
    // SAFETY: a live window of this thread, which initialized OLE.
    let _ = unsafe { RegisterDragDrop(state.hwnd.get(), &target) };
}

/// The window is going: OLE lets go of its target.
pub(super) fn revoke(hwnd: HWND) {
    // SAFETY: a window of ours, still live inside its `WM_DESTROY`.
    let _ = unsafe { RevokeDragDrop(hwnd) };
}

#[implement(IDropTarget)]
struct DropTarget {
    state: Weak<WindowState>,
    /// What the drag over the window offers, read as it came, and the app's
    /// answer.
    arrival: RefCell<Option<(DropOffer, DropReply)>>,
}

impl DropTarget {
    /// Hands step `phase` of the drag at screen point `pt` to the app and
    /// answers OLE through `effect`, which holds what the source allows.
    fn step(
        &self,
        phase: DropPhase,
        pt: &POINTL,
        offer: DropOffer,
        reply: DropReply,
        effect: *mut DROPEFFECT,
    ) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let mut at = POINT { x: pt.x, y: pt.y };
        // SAFETY: a plain conversion against our live window.
        let _ = unsafe { ScreenToClient(state.hwnd.get(), &mut at) };
        let (x, y) = state.logical_point(at.x, at.y);
        state.push(RawEvent::Drop(RawDrop {
            window: state.id,
            phase,
            x,
            y,
            offer,
            reply: reply.clone(),
        }));
        super::drive(&state.shared);
        if effect.is_null() {
            return;
        }
        // SAFETY: OLE passes a valid in/out effect for the call.
        unsafe {
            let allowed = actions(*effect);
            *effect = drop_effect(allowed.settle(reply.get()));
        }
    }
}

impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(
        &self,
        data: Ref<IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> ::windows::core::Result<()> {
        let offer = data.as_ref().map(offer_of).unwrap_or_default();
        let reply = DropReply::new();
        *self.arrival.borrow_mut() = Some((offer.clone(), reply.clone()));
        self.step(DropPhase::Over, pt, offer, reply, effect);
        Ok(())
    }

    fn DragOver(
        &self,
        _keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> ::windows::core::Result<()> {
        let (offer, reply) = self.arrival.borrow().clone().unwrap_or_default();
        self.step(DropPhase::Over, pt, offer, reply, effect);
        Ok(())
    }

    fn DragLeave(&self) -> ::windows::core::Result<()> {
        if let Some((offer, reply)) = self.arrival.borrow_mut().take() {
            let none = std::ptr::null_mut();
            self.step(DropPhase::Leave, &POINTL::default(), offer, reply, none);
        }
        Ok(())
    }

    fn Drop(
        &self,
        data: Ref<IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> ::windows::core::Result<()> {
        let reply = self
            .arrival
            .borrow_mut()
            .take()
            .map(|(_, reply)| reply)
            .unwrap_or_default();
        let offer = data.as_ref().map(offer_of).unwrap_or_default();
        self.step(DropPhase::Drop, pt, offer, reply, effect);
        Ok(())
    }
}

/// The actions OLE effect mask `effect` allows.
fn actions(effect: DROPEFFECT) -> DropActions {
    DropActions {
        moves: effect.0 & DROPEFFECT_MOVE.0 != 0,
        copies: effect.0 & DROPEFFECT_COPY.0 != 0,
        links: effect.0 & DROPEFFECT_LINK.0 != 0,
    }
}

/// The OLE effect of `action`.
fn drop_effect(action: Option<DropAction>) -> DROPEFFECT {
    match action {
        Some(DropAction::Move) => DROPEFFECT_MOVE,
        Some(DropAction::Copy) => DROPEFFECT_COPY,
        Some(DropAction::Link) => DROPEFFECT_LINK,
        None => DROPEFFECT_NONE,
    }
}

/// A global-memory format `format` of the data.
fn format(format: u16) -> FORMATETC {
    FORMATETC {
        cfFormat: format,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

/// What a drag's data offers: its files, its link and its text.
fn offer_of(data: &IDataObject) -> DropOffer {
    // SAFETY: a constant name.
    let url = unsafe { RegisterClipboardFormatW(w!("UniformResourceLocatorW")) } as u16;
    // SAFETY: plain queries of a live data object.
    let has = |f: u16| unsafe { data.QueryGetData(&format(f)) }.is_ok();
    let mut offer = DropOffer {
        kinds: DropKinds {
            text: has(CF_UNICODETEXT.0),
            uri: url != 0 && has(url),
            files: has(CF_HDROP.0),
        },
        ..DropOffer::default()
    };
    if offer.kinds.files {
        offer.files = with_global(data, CF_HDROP.0, files_of).unwrap_or_default();
    }
    if offer.kinds.uri {
        offer.uri = with_global(data, url, text_of).filter(|u| !u.is_empty());
    }
    if offer.kinds.text {
        offer.text = with_global(data, CF_UNICODETEXT.0, text_of);
    }
    offer
}

/// Reads the global memory of format `f` of the data with `read`, then
/// releases it.
fn with_global<T>(data: &IDataObject, f: u16, read: impl FnOnce(HGLOBAL) -> T) -> Option<T> {
    // SAFETY: the medium the data object hands over is ours to read and
    // release; its global handle stays valid until the release.
    unsafe {
        let mut medium = data.GetData(&format(f)).ok()?;
        if medium.tymed != TYMED_HGLOBAL.0 as u32 {
            ReleaseStgMedium(&mut medium);
            return None;
        }
        let out = read(medium.u.hGlobal);
        ReleaseStgMedium(&mut medium);
        Some(out)
    }
}

/// The UTF-16 text a global block holds, up to its nul.
fn text_of(block: HGLOBAL) -> String {
    // SAFETY: the locked block holds `GlobalSize` bytes; the slice stays
    // within them and ends before the unlock.
    unsafe {
        let src = GlobalLock(block).cast::<u16>().cast_const();
        if src.is_null() {
            return String::new();
        }
        let units = std::slice::from_raw_parts(src, GlobalSize(block) / 2);
        let text = translate::clipboard_text(units);
        let _ = GlobalUnlock(block);
        text
    }
}

/// The paths an `HDROP` block lists.
fn files_of(block: HGLOBAL) -> Vec<PathBuf> {
    let drop = HDROP(block.0);
    // SAFETY: queries of a valid drop handle into buffers of the length it
    // reported.
    unsafe {
        let count = DragQueryFileW(drop, u32::MAX, None);
        (0..count)
            .filter_map(|i| {
                let len = DragQueryFileW(drop, i, None) as usize;
                let mut buf = vec![0u16; len + 1];
                let got = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
                (got > 0).then(|| PathBuf::from(OsString::from_wide(&buf[..got])))
            })
            .collect()
    }
}

/// Runs `drag` from `state`'s window, held by the mouse, until it is let go
/// or put back; what the drop did.
pub(super) fn drag_out(state: &WindowState, drag: &DragOut) -> Option<DropAction> {
    let data = data_object(drag)?;
    let mut allowed = DROPEFFECT_NONE;
    for (yes, effect) in [
        (drag.actions.copies, DROPEFFECT_COPY),
        (drag.actions.moves, DROPEFFECT_MOVE),
        (drag.actions.links, DROPEFFECT_LINK),
    ] {
        if yes {
            allowed.0 |= effect.0;
        }
    }
    // SAFETY: a live window of this thread, which initialized OLE; the shell
    // supplies the drop source and the drag image.
    let done = unsafe { SHDoDragDrop(Some(state.hwnd.get()), &data, None, allowed) };
    // OLE kept the button's release, and the capture with it.
    state.buttons.set(crate::event::PointerButtons::NONE);
    // SAFETY: a plain call; it fails harmlessly with no capture held.
    let _ = unsafe { ReleaseCapture() };
    let effect = done.ok()?;
    [
        (DROPEFFECT_MOVE, DropAction::Move),
        (DROPEFFECT_COPY, DropAction::Copy),
        (DROPEFFECT_LINK, DropAction::Link),
    ]
    .into_iter()
    .find(|(e, _)| effect.0 & e.0 != 0)
    .map(|(_, action)| action)
}

/// A shell data object holding `drag`'s files, link and text.
fn data_object(drag: &DragOut) -> Option<IDataObject> {
    // SAFETY: an empty shell data object, which takes any global format set
    // on it.
    let data: IDataObject = unsafe { SHCreateDataObject(None, None, None) }.ok()?;
    if !drag.files.is_empty() {
        let mut list = Vec::new();
        for file in &drag.files {
            list.extend(file.as_os_str().encode_wide());
            list.push(0);
        }
        list.push(0);
        let header = DROPFILES {
            pFiles: std::mem::size_of::<DROPFILES>() as u32,
            fWide: true.into(),
            ..DROPFILES::default()
        };
        // SAFETY: `DROPFILES` is plain data, viewed as its own bytes.
        let head = unsafe {
            std::slice::from_raw_parts(
                (&raw const header).cast::<u8>(),
                std::mem::size_of::<DROPFILES>(),
            )
        };
        let mut bytes = head.to_vec();
        bytes.extend(list.iter().flat_map(|u| u.to_le_bytes()));
        set_global(&data, CF_HDROP.0, &bytes);
    }
    if let Some(uri) = &drag.uri {
        // SAFETY: a constant name.
        let url = unsafe { RegisterClipboardFormatW(w!("UniformResourceLocatorW")) } as u16;
        if url != 0 {
            set_global(&data, url, &wide_bytes(uri));
        }
    }
    let text = drag.plain_text();
    if !text.is_empty() {
        set_global(&data, CF_UNICODETEXT.0, &wide_bytes(&text));
    }
    Some(data)
}

/// `text` as nul-ended UTF-16, in bytes.
fn wide_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// Hands the data object a global block of format `f` holding `bytes`.
fn set_global(data: &IDataObject, f: u16, bytes: &[u8]) {
    // SAFETY: the block is allocated `bytes.len()` long and written within
    // it while locked; the data object owns it once `SetData` takes it, and
    // it is freed here if not.
    unsafe {
        let Ok(block) = GlobalAlloc(GMEM_MOVEABLE, bytes.len()) else {
            return;
        };
        let dst = GlobalLock(block).cast::<u8>();
        if dst.is_null() {
            let _ = GlobalFree(Some(block));
            return;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
        let _ = GlobalUnlock(block);
        let medium = STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: block },
            pUnkForRelease: std::mem::ManuallyDrop::new(None),
        };
        if data.SetData(&format(f), &medium, true).is_err() {
            let _ = GlobalFree(Some(block));
        }
    }
}
