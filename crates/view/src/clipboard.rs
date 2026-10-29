//! The clipboard the `viso::clipboard` natives reach.
//!
//! The app installs a [`HostClipboard`]; every view host built after gets a
//! clipboard service forwarding into it. A write waits there until the app
//! hands it to the platform at the end of the frame, as a handler's copy
//! does; a read waits until the app asks the platform and answers it with
//! what the platform returned, empty when it holds no text or refused.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use viso_behavior::native::Clipboard;

thread_local! {
    static INSTALLED: RefCell<Option<Rc<HostClipboard>>> = const { RefCell::new(None) };
}

/// Installs `clipboard` as the one the views of this thread built from now on
/// read and write; `None` leaves them none, so their clipboard natives fail.
pub fn install_clipboard(clipboard: Option<Rc<HostClipboard>>) {
    INSTALLED.with(|installed| *installed.borrow_mut() = clipboard);
}

/// The clipboard service of a view host built now, when one is installed.
pub(crate) fn service() -> Option<Box<dyn Clipboard>> {
    INSTALLED.with(|installed| {
        let clipboard = installed.borrow().clone()?;
        Some(Box::new(Shared(clipboard)) as Box<dyn Clipboard>)
    })
}

/// What views wrote to the clipboard and asked of it, between the app's
/// frames.
#[derive(Debug, Default)]
pub struct HostClipboard {
    copied: RefCell<Option<String>>,
    reads: RefCell<Vec<Rc<ClipboardRead>>>,
}

impl HostClipboard {
    /// The text written since the last call, the last write winning.
    pub fn take_copied(&self) -> Option<String> {
        self.copied.borrow_mut().take()
    }

    /// Moves the reads asked since the last call into `out`.
    pub fn take_reads(&self, out: &mut Vec<Rc<ClipboardRead>>) {
        out.append(&mut self.reads.borrow_mut());
    }

    /// Whether a write or a read waits for the app.
    pub fn is_pending(&self) -> bool {
        self.copied.borrow().is_some() || !self.reads.borrow().is_empty()
    }
}

/// A read of the clipboard's text, waiting for its answer.
#[derive(Debug, Default)]
pub struct ClipboardRead {
    answer: RefCell<Option<Option<String>>>,
    waker: RefCell<Option<Waker>>,
}

impl ClipboardRead {
    /// Answers the read with `text`, waking whatever waits on it; a second
    /// answer is ignored.
    pub fn answer(&self, text: Option<String>) {
        {
            let mut answer = self.answer.borrow_mut();
            if answer.is_some() {
                return;
            }
            *answer = Some(text);
        }
        if let Some(waker) = self.waker.borrow_mut().take() {
            waker.wake();
        }
    }
}

/// A view host's clipboard service.
struct Shared(Rc<HostClipboard>);

impl Clipboard for Shared {
    fn read_text(&mut self) -> Pin<Box<dyn Future<Output = Option<String>>>> {
        let read = Rc::new(ClipboardRead::default());
        self.0.reads.borrow_mut().push(Rc::clone(&read));
        Box::pin(Answer(read))
    }

    fn write_text(&mut self, text: &str) {
        *self.0.copied.borrow_mut() = Some(text.to_owned());
    }
}

/// A read's answer, as a future.
struct Answer(Rc<ClipboardRead>);

impl Future for Answer {
    type Output = Option<String>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<String>> {
        if let Some(text) = self.0.answer.borrow_mut().take() {
            return Poll::Ready(text);
        }
        *self.0.waker.borrow_mut() = Some(cx.waker().clone());
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::task::Wake;

    struct Count(AtomicU32);

    impl Wake for Count {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn writes_wait_for_the_app_and_reads_finish_on_its_answer() {
        let host = Rc::new(HostClipboard::default());
        let mut service = Shared(Rc::clone(&host));
        service.write_text("a");
        service.write_text("b");
        assert!(host.is_pending());
        assert_eq!(host.take_copied().as_deref(), Some("b"));
        assert_eq!(host.take_copied(), None);

        let count = Arc::new(Count(AtomicU32::new(0)));
        let waker = Waker::from(Arc::clone(&count));
        let mut cx = Context::from_waker(&waker);
        let mut read = service.read_text();
        assert_eq!(read.as_mut().poll(&mut cx), Poll::Pending);
        let mut reads = Vec::new();
        host.take_reads(&mut reads);
        assert_eq!(reads.len(), 1);
        assert!(!host.is_pending());
        reads[0].answer(Some("pasted".into()));
        reads[0].answer(None);
        assert_eq!(count.0.load(Ordering::Relaxed), 1);
        assert_eq!(
            read.as_mut().poll(&mut cx),
            Poll::Ready(Some("pasted".into()))
        );
    }
}
