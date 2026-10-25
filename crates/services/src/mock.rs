//! A scripted, recording implementation of every protocol, for headless
//! tests.

use std::cell::RefCell;
use std::rc::Rc;

use crate::haptics::{Haptic, Haptics};
use crate::launcher::{Launcher, launch};
use crate::registry::Services;
use crate::reply::{Reply, ServiceResult};

/// One call a [`Mock`] received.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Call {
    Haptic(Haptic),
    OpenUrl(String),
}

/// Records every call: haptics succeed, and so does opening a URL that
/// opens anywhere.
///
/// Clones share one record, so a test keeps a clone to inspect what the code
/// under test did with [`services`](Self::services).
#[derive(Clone, Default)]
pub struct Mock {
    calls: Rc<RefCell<Vec<Call>>>,
}

impl Mock {
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry whose every service is this mock.
    pub fn services(&self) -> Services {
        let mock = Rc::new(self.clone());
        Services::from_parts(mock.clone(), mock)
    }

    /// The calls received so far, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }

    fn record(&self, call: Call) {
        self.calls.borrow_mut().push(call);
    }
}

impl Launcher for Mock {
    fn open_url(&self, url: &str) -> Reply<()> {
        self.record(Call::OpenUrl(url.to_owned()));
        launch(url, |_| Reply::ready(Ok(())))
    }
}

impl Haptics for Mock {
    fn play(&self, haptic: Haptic) -> ServiceResult<()> {
        self.record(Call::Haptic(haptic));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reply::ServiceError;

    fn now<T>(mut reply: Reply<T>) -> ServiceResult<T> {
        reply.try_take().expect("the mock answers at once")
    }

    #[test]
    fn the_mock_records_what_it_is_asked() {
        let mock = Mock::new();
        let services = mock.services();
        assert_eq!(now(services.launcher().open_url("https://a.b/")), Ok(()));
        assert_eq!(
            now(services.launcher().open_url("javascript:x")),
            Err(ServiceError::Denied)
        );
        assert_eq!(services.haptics().play(Haptic::Light), Ok(()));
        assert_eq!(
            mock.calls(),
            [
                Call::OpenUrl("https://a.b/".into()),
                Call::OpenUrl("javascript:x".into()),
                Call::Haptic(Haptic::Light)
            ]
        );
    }

    #[test]
    fn a_single_service_can_be_replaced() {
        let mock = Mock::new();
        let services = Services::unsupported().with_haptics(mock.clone());
        assert_eq!(services.haptics().play(Haptic::Success), Ok(()));
        assert_eq!(
            now(services.launcher().open_url("https://a.b/")),
            Err(ServiceError::Unsupported)
        );
        assert_eq!(mock.calls(), vec![Call::Haptic(Haptic::Success)]);
    }
}
