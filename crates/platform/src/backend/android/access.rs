//! The Android accessibility bridge (ADR 0030): an AccessKit injecting
//! adapter on the activity's host view.
//!
//! The adapter installs its delegate on the UI thread and calls the handlers
//! there, so the handlers only queue the request in the inbox, which wakes the
//! loop. The activation handler answers with no tree; the facade's next frame
//! sends the full one through [`update`]. A recreated activity brings a new
//! view, so the loop attaches a fresh adapter to it.

use accesskit::{ActionHandler, ActionRequest, ActivationHandler, TreeUpdate};
use accesskit_android::InjectingAdapter;
use accesskit_android::jni::JNIEnv;
use accesskit_android::jni::objects::JObject;

use super::{Msg, jni, send};
use crate::accessibility::AccessRequest;

/// An adapter on the current activity's host view, or `None` when there is
/// no activity to attach to.
pub(super) fn attach() -> Option<InjectingAdapter> {
    jni::with_host_view(|env, view| {
        // SAFETY: `env` is this (attached) thread's environment and `view` a
        // local reference valid until the closure returns; the adapter keeps
        // only a weak global reference of its own to the view.
        let (env, host) = unsafe { (JNIEnv::from_raw(env.cast()), JObject::from_raw(view.cast())) };
        let mut env = env.ok()?;
        Some(InjectingAdapter::new(&mut env, &host, Requests, Requests))
    })
    .flatten()
}

/// Apply `update` if an assistive technology has asked for the tree.
pub(super) fn update(adapter: &mut InjectingAdapter, update: TreeUpdate) {
    adapter.update_if_active(|| update);
}

/// Both handlers: each request becomes a [`Msg::Access`] for the loop.
struct Requests;

impl ActivationHandler for Requests {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        send(Msg::Access(AccessRequest::Activated));
        None
    }
}

impl ActionHandler for Requests {
    fn do_action(&mut self, request: ActionRequest) {
        if let Some(request) = AccessRequest::from_accesskit(&request) {
            send(Msg::Access(request));
        }
    }
}
