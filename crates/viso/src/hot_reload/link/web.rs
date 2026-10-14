//! The dev channel of a runtime in a browser: a WebSocket to the web dev
//! server that served the page, at [`WEB_DEV_PATH`] on the dev channel's
//! loopback address, carrying the frames the TCP channel carries, one or
//! more per binary message.
//!
//! The browser has no threads to read or write on: the socket's callbacks
//! run on the loop's thread, assemble frames, decode each as the TCP reader
//! does, queue what the loop takes and wake it. What the loop sends before
//! the host accepted the launch waits for the acceptance. The server ends the
//! channel with [`WEB_RELOAD_CLOSE`] once it serves a newer build, and the
//! page loads it.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use js_sys::{ArrayBuffer, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{BinaryType, CloseEvent, MessageEvent, WebSocket};

use viso_platform::LoopWaker;
use viso_view::dev::wire::{
    Blobs, Domains, HostMessage, MAX_FRAME, Message, Reject, RuntimeHello, RuntimeMessage,
    SchemaFingerprint, WEB_DEV_PATH, WEB_RELOAD_CLOSE, WindowGeometry, WireError, accept_host,
    write_frame,
};

use super::{Frame, Incoming, Outgoing, host_frame, identity, launch_hello, write_outgoing};

/// What the socket's callbacks and the loop share.
struct Shared {
    hello: RuntimeHello,
    /// Whether the host accepted the launch.
    accepted: bool,
    /// Whether the channel ended.
    ended: bool,
    /// Frames the loop sent before the acceptance.
    unsent: Vec<Vec<u8>>,
    /// Bytes of a frame still arriving.
    buf: Vec<u8>,
    blobs: Blobs,
    incoming: VecDeque<Incoming>,
}

/// The app's end of the dev channel.
pub(crate) struct DevLink {
    socket: WebSocket,
    shared: Rc<RefCell<Shared>>,
    _on_open: Closure<dyn FnMut()>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_close: Closure<dyn FnMut(CloseEvent)>,
}

impl DevLink {
    /// The link `viso run` asked for through the page's launch variables, if
    /// any, for a runtime compiled against `schema` that applies
    /// `capabilities`; `wake` kicks the loop when a host frame arrives.
    pub(crate) fn from_launch(
        wake: LoopWaker,
        schema: SchemaFingerprint,
        capabilities: Domains,
    ) -> Option<Self> {
        let (addr, hello) = launch_hello(schema, capabilities)?;
        let socket = WebSocket::new(&format!("ws://{addr}{WEB_DEV_PATH}")).ok()?;
        socket.set_binary_type(BinaryType::Arraybuffer);
        let shared = Rc::new(RefCell::new(Shared {
            hello,
            accepted: false,
            ended: false,
            unsent: Vec::new(),
            buf: Vec::new(),
            blobs: Blobs::default(),
            incoming: VecDeque::new(),
        }));

        let on_open = {
            let (socket, shared) = (socket.clone(), Rc::clone(&shared));
            Closure::<dyn FnMut()>::new(move || {
                let hello = RuntimeMessage::Hello(shared.borrow().hello.clone());
                send_frame(&socket, &hello);
            })
        };
        let on_message = {
            let (socket, shared) = (socket.clone(), Rc::clone(&shared));
            Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                let Ok(data) = event.data().dyn_into::<ArrayBuffer>() else {
                    return;
                };
                let delivered = {
                    let mut shared = shared.borrow_mut();
                    shared
                        .buf
                        .extend_from_slice(&Uint8Array::new(&data).to_vec());
                    take_frames(&mut shared, &socket)
                };
                if delivered {
                    wake.wake();
                }
            })
        };
        let on_close = {
            let shared = Rc::clone(&shared);
            Closure::<dyn FnMut(CloseEvent)>::new(move |event: CloseEvent| {
                shared.borrow_mut().ended = true;
                if event.code() == WEB_RELOAD_CLOSE
                    && let Some(window) = web_sys::window()
                {
                    let _ = window.location().reload();
                }
            })
        };
        socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        Some(DevLink {
            socket,
            shared,
            _on_open: on_open,
            _on_message: on_message,
            _on_close: on_close,
        })
    }

    /// Sends `message` to the host, once it accepted the launch.
    pub(crate) fn send(&mut self, message: RuntimeMessage) {
        self.write(Outgoing::Message(message));
    }

    /// Sends the snapshot `bytes` and the app's `windows`, captured in
    /// `capture_us`.
    pub(crate) fn send_snapshot(
        &mut self,
        bytes: Vec<u8>,
        windows: Vec<WindowGeometry>,
        capture_us: u64,
    ) {
        self.write(Outgoing::Snapshot(bytes, windows, capture_us));
    }

    /// The next host frame the socket delivered.
    pub(crate) fn poll(&self) -> Option<Incoming> {
        self.shared.borrow_mut().incoming.pop_front()
    }

    fn write(&self, outgoing: Outgoing) {
        let _ = write_outgoing::<()>(outgoing, |message| {
            self.queue(message);
            Ok(())
        });
    }

    fn queue(&self, message: &RuntimeMessage) {
        let mut shared = self.shared.borrow_mut();
        if shared.ended {
            return;
        }
        if shared.accepted {
            drop(shared);
            send_frame(&self.socket, message);
        } else if let Some(frame) = frame(message) {
            shared.unsent.push(frame);
        }
    }
}

impl Drop for DevLink {
    fn drop(&mut self) {
        self.socket.set_onopen(None);
        self.socket.set_onmessage(None);
        self.socket.set_onclose(None);
        let _ = self.socket.close();
    }
}

/// `message` as one frame.
fn frame(message: &impl Message) -> Option<Vec<u8>> {
    let mut frame = Vec::new();
    write_frame(&mut frame, message).ok()?;
    Some(frame)
}

fn send_frame(socket: &WebSocket, message: &impl Message) {
    if let Some(frame) = frame(message) {
        let _ = socket.send_with_u8_array(&frame);
    }
}

/// Takes every whole frame off `shared.buf`: the host's answer to the hello
/// first, then the frames for the loop. Whether one was queued for it.
fn take_frames(shared: &mut Shared, socket: &WebSocket) -> bool {
    let mut delivered = false;
    while !shared.ended && shared.buf.len() >= 4 {
        let len = u32::from_le_bytes(shared.buf[..4].try_into().expect("four bytes")) as usize;
        if len > MAX_FRAME {
            shared
                .incoming
                .push_back(Incoming::Undecodable(WireError::FrameTooLarge { len }));
            end(shared, socket);
            return true;
        }
        if shared.buf.len() < 4 + len {
            break;
        }
        let body: Vec<u8> = shared.buf.drain(..4 + len).skip(4).collect();
        if !shared.accepted {
            match HostMessage::from_frame(&body) {
                Ok(HostMessage::Hello(host)) if accept_host(&host, &shared.hello).is_ok() => {
                    shared.accepted = true;
                    let identity = identity(&shared.hello);
                    shared.incoming.push_back(Incoming::Connected(identity));
                    delivered = true;
                    for frame in std::mem::take(&mut shared.unsent) {
                        let _ = socket.send_with_u8_array(&frame);
                    }
                }
                Ok(HostMessage::Hello(host)) => {
                    if let Err(reject) = accept_host(&host, &shared.hello) {
                        refused(reject);
                    }
                    end(shared, socket);
                }
                Ok(HostMessage::Reject(reject)) => {
                    refused(reject);
                    end(shared, socket);
                }
                _ => end(shared, socket),
            }
            continue;
        }
        match host_frame(&body, &mut shared.blobs) {
            Frame::Deliver(incoming) => {
                shared.incoming.push_back(incoming);
                delivered = true;
            }
            Frame::Kept => {}
            Frame::End => end(shared, socket),
        }
    }
    delivered
}

fn end(shared: &mut Shared, socket: &WebSocket) {
    shared.ended = true;
    shared.unsent.clear();
    shared.buf.clear();
    let _ = socket.close();
}

fn refused(reject: Reject) {
    web_sys::console::warn_1(&format!("[viso] dev channel refused: {reject}").into());
}
