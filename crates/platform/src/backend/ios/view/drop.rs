//! Drops onto the window from other apps: a drop interaction on the root
//! view hands the app each move of a drag over it, at once, and proposes the
//! operation the app answers. A drop's text, links and files load in the
//! background and reach the app together once the last has loaded; a file
//! the providing app only lends for the load is copied out first.

use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{ClassType, DefinedClass};
use objc2_foundation::{NSArray, NSError, NSItemProvider, NSItemProviderReading, NSString, NSURL};
use objc2_ui_kit::{UIDragDropSession, UIDropOperation, UIDropSession};

use super::VisoView;
use crate::backend::ios::{LOOP, drive, guarded, push};
use crate::backend::main_queue::post_to_main;
use crate::drop::{DropAction, DropActions, DropKinds, DropOffer, DropPhase, DropReply, RawDrop};
use crate::event::RawEvent;

/// A drop waiting for what it holds to load.
pub(super) struct Landing {
    step: RawDrop,
    /// Which drop the loads that come back belong to.
    id: u64,
    /// Loads not yet back.
    waiting: usize,
}

/// What one load brought back.
enum Arrived {
    Text(Option<String>),
    Links(Vec<String>),
    File(Option<PathBuf>),
}

/// Loads back from any queue, for the main thread to take.
static ARRIVED: Mutex<Vec<(u64, Arrived)>> = Mutex::new(Vec::new());

static DROPS: AtomicU64 = AtomicU64::new(0);

type Session = ProtocolObject<dyn UIDropSession>;

impl VisoView {
    /// Step `phase` of the drag in `session`, handed to the app at once; the
    /// operation the app answered.
    pub(super) fn drop_step(&self, session: &Session, phase: DropPhase) -> UIDropOperation {
        let ivars = self.ivars();
        let (kinds, reply) = if phase == DropPhase::Over {
            ivars
                .arrival
                .borrow_mut()
                .get_or_insert_with(|| (kinds_of(session), DropReply::new()))
                .clone()
        } else {
            ivars.arrival.borrow_mut().take().unwrap_or_default()
        };
        let at = session.locationInView(self);
        push(RawEvent::Drop(RawDrop {
            window: ivars.window,
            phase,
            x: at.x,
            y: at.y,
            offer: DropOffer {
                kinds,
                ..DropOffer::default()
            },
            reply: reply.clone(),
        }));
        drive();
        let actions = DropActions {
            moves: session.allowsMoveOperation(),
            copies: true,
            links: false,
        };
        match actions.settle(reply.get()) {
            Some(DropAction::Move) => UIDropOperation::Move,
            Some(_) => UIDropOperation::Copy,
            None => UIDropOperation::Cancel,
        }
    }

    /// The drag in `session` was let go: what it holds starts loading, and
    /// the drop reaches the app once all of it is back.
    pub(super) fn perform_drop(&self, session: &Session) {
        let ivars = self.ivars();
        let reply = ivars
            .arrival
            .borrow_mut()
            .take()
            .map(|(_, reply)| reply)
            .unwrap_or_default();
        let at = session.locationInView(self);
        let id = DROPS.fetch_add(1, Ordering::Relaxed) + 1;
        let mut waiting = 0;
        // SAFETY: a plain query, of a class that loads from item providers.
        if unsafe { session.canLoadObjectsOfClass(NSString::class()) } {
            waiting += 1;
            let done = RcBlock::new(move |found: NonNull<NSArray<_>>| {
                // SAFETY: the array the load hands over, valid for the call.
                let found: &NSArray<ProtocolObject<dyn NSItemProviderReading>> =
                    unsafe { found.as_ref() };
                let text = found
                    .iter()
                    .find_map(|o| any(&o).downcast_ref::<NSString>().map(NSString::to_string));
                arrive(id, Arrived::Text(text));
            });
            // SAFETY: the block captures only the drop's id, so it may run on
            // any queue.
            let _ = unsafe { session.loadObjectsOfClass_completion(NSString::class(), &done) };
        }
        // SAFETY: as above.
        if unsafe { session.canLoadObjectsOfClass(NSURL::class()) } {
            waiting += 1;
            let done = RcBlock::new(move |found: NonNull<NSArray<_>>| {
                // SAFETY: the array the load hands over, valid for the call.
                let found: &NSArray<ProtocolObject<dyn NSItemProviderReading>> =
                    unsafe { found.as_ref() };
                let links = found
                    .iter()
                    .filter_map(|o| any(&o).downcast_ref::<NSURL>()?.absoluteString())
                    .map(|s| s.to_string())
                    .collect();
                arrive(id, Arrived::Links(links));
            });
            // SAFETY: as for the text.
            let _ = unsafe { session.loadObjectsOfClass_completion(NSURL::class(), &done) };
        }
        for item in session.items() {
            let provider = item.itemProvider();
            if !is_file(&provider) {
                continue;
            }
            let Some(kind) = provider.registeredTypeIdentifiers().firstObject() else {
                continue;
            };
            waiting += 1;
            let done = RcBlock::new(move |url: *mut NSURL, in_place: Bool, _: *mut NSError| {
                // SAFETY: the URL the load hands over, valid for the call.
                let path = unsafe { url.as_ref() }.and_then(|u| keep(u, in_place.as_bool(), id));
                arrive(id, Arrived::File(path));
            });
            // SAFETY: the block captures only the drop's id, so it may run on
            // any queue, which the load calls it on.
            let _ = unsafe {
                provider
                    .loadInPlaceFileRepresentationForTypeIdentifier_completionHandler(&kind, &done)
            };
        }
        *ivars.landing.borrow_mut() = Some(Landing {
            step: RawDrop {
                window: ivars.window,
                phase: DropPhase::Drop,
                x: at.x,
                y: at.y,
                offer: DropOffer::default(),
                reply,
            },
            id,
            waiting,
        });
        if waiting == 0 {
            self.land();
        }
    }

    /// Takes the loads back so far into the drop, which goes to the app with
    /// the last of them.
    fn land(&self) {
        let mut landing = self.ivars().landing.borrow_mut();
        let Some(pending) = landing.as_mut() else {
            return;
        };
        let arrived = std::mem::take(&mut *ARRIVED.lock().unwrap_or_else(|e| e.into_inner()));
        let offer = &mut pending.step.offer;
        for (_, back) in arrived.into_iter().filter(|(id, _)| *id == pending.id) {
            pending.waiting = pending.waiting.saturating_sub(1);
            match back {
                Arrived::Text(text) => offer.text = offer.text.take().or(text),
                Arrived::Links(links) => {
                    for link in links {
                        match crate::drop::file_path(&link) {
                            Some(path) => offer.files.push(path),
                            None if offer.uri.is_none() => offer.uri = Some(link),
                            None => {}
                        }
                    }
                }
                Arrived::File(path) => offer.files.extend(path),
            }
        }
        if pending.waiting > 0 {
            return;
        }
        let Some(Landing { mut step, .. }) = landing.take() else {
            return;
        };
        drop(landing);
        let offer = &mut step.offer;
        let mut seen = Vec::with_capacity(offer.files.len());
        offer.files.retain(|f| {
            let new = !seen.contains(f);
            seen.push(f.clone());
            new
        });
        if offer.text.is_none() {
            offer.text = offer.uri.clone();
        }
        offer.kinds = DropKinds {
            text: offer.text.is_some(),
            uri: offer.uri.is_some(),
            files: !offer.files.is_empty(),
        };
        push(RawEvent::Drop(step));
        drive();
    }
}

/// A loaded object, to downcast to the class its load asked for.
fn any(object: &Retained<ProtocolObject<dyn NSItemProviderReading>>) -> &AnyObject {
    (**object).as_ref()
}

/// Hands load `back` of drop `id` to the main thread.
fn arrive(id: u64, back: Arrived) {
    ARRIVED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((id, back));
    post_to_main(landed);
}

extern "C" fn landed(_: *mut c_void) {
    guarded(|| {
        if let Some(view) = LOOP.with(|l| l.view.borrow().clone()) {
            view.land();
        }
    });
}

/// The kinds a drag in `session` offers.
fn kinds_of(session: &Session) -> DropKinds {
    let links = NSArray::from_retained_slice(&[NSString::from_str("public.url")]);
    DropKinds {
        // SAFETY: a plain query, of a class that loads from item providers.
        text: unsafe { session.canLoadObjectsOfClass(NSString::class()) },
        uri: session.hasItemsConformingToTypeIdentifiers(&links),
        files: session
            .items()
            .iter()
            .any(|item| is_file(&item.itemProvider())),
    }
}

/// Whether the item a provider gives is a file: one it names as one, or one
/// that is neither text nor a link (an image, a document).
fn is_file(provider: &NSItemProvider) -> bool {
    let has = |t: &str| provider.hasItemConformingToTypeIdentifier(&NSString::from_str(t));
    has("public.file-url") || !(has("public.text") || has("public.url"))
}

/// The path of the file a load handed over: its own where it is lent in
/// place, else a copy in the temporary directory, the original going once
/// the load returns.
fn keep(url: &NSURL, in_place: bool, id: u64) -> Option<PathBuf> {
    let path = PathBuf::from(url.path()?.to_string());
    if in_place {
        return Some(path);
    }
    let dir = std::env::temp_dir().join(format!("viso-drop-{id}"));
    std::fs::create_dir_all(&dir).ok()?;
    let copy = dir.join(path.file_name()?);
    std::fs::copy(&path, &copy).ok()?;
    Some(copy)
}
