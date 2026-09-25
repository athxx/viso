//! A scripted, recording implementation of every protocol, for headless
//! tests.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;

use crate::files::{FileDialogs, OpenOptions, PickedFile, SaveOptions};
use crate::haptics::{Haptic, Haptics};
use crate::notifications::{Notification, Notifications};
use crate::permissions::{Permission, PermissionState, Permissions};
use crate::registry::Services;
use crate::reply::{Reply, ServiceError, ServiceResult};
use crate::secure_storage::SecureStorage;
use crate::share::{Share, ShareItem};

/// One call a [`Mock`] received.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Call {
    Open(OpenOptions),
    Save {
        options: SaveOptions,
        contents: Vec<u8>,
    },
    Share(ShareItem),
    Notify(Notification),
    Withdraw(String),
    PermissionStatus(Permission),
    RequestPermission(Permission),
    SecureGet(String),
    SecureSet(String),
    SecureRemove(String),
    Haptic(Haptic),
}

#[derive(Default)]
struct MockState {
    calls: Vec<Call>,
    opens: VecDeque<ServiceResult<Vec<PickedFile>>>,
    saves: VecDeque<ServiceResult<Option<PathBuf>>>,
    permissions: HashMap<Permission, PermissionState>,
    answers: HashMap<Permission, PermissionState>,
    secrets: HashMap<String, Vec<u8>>,
}

/// Records every call and answers from a script.
///
/// - Dialogs answer the queued results in order, then
///   [`ServiceError::Cancelled`], as if the user dismissed them.
/// - Every permission starts [`PermissionState::Prompt`]; a request answers
///   [`answer_request`](Self::answer_request)'s state (granted by default)
///   and keeps it.
/// - Notifications need the permission granted and answer
///   [`ServiceError::Denied`] otherwise.
/// - Secure storage is an in-memory map.
/// - Share and haptics succeed.
///
/// Clones share one state, so a test keeps a clone to inspect what the code
/// under test did with [`services`](Self::services).
#[derive(Clone, Default)]
pub struct Mock {
    state: Rc<RefCell<MockState>>,
}

impl Mock {
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry whose every service is this mock.
    pub fn services(&self) -> Services {
        let this = Rc::new(self.clone());
        Services::from_parts(
            this.clone(),
            this.clone(),
            this.clone(),
            this.clone(),
            this.clone(),
            this,
        )
    }

    /// The calls received so far, oldest first.
    pub fn calls(&self) -> Vec<Call> {
        self.state.borrow().calls.clone()
    }

    /// Queue the next open dialog's answer.
    pub fn answer_open(&self, result: ServiceResult<Vec<PickedFile>>) {
        self.state.borrow_mut().opens.push_back(result);
    }

    /// Queue the next save dialog's answer.
    pub fn answer_save(&self, result: ServiceResult<Option<PathBuf>>) {
        self.state.borrow_mut().saves.push_back(result);
    }

    /// Set where `permission` stands now.
    pub fn set_permission(&self, permission: Permission, state: PermissionState) {
        self.state
            .borrow_mut()
            .permissions
            .insert(permission, state);
    }

    /// What the user answers when `permission` is requested.
    pub fn answer_request(&self, permission: Permission, state: PermissionState) {
        self.state.borrow_mut().answers.insert(permission, state);
    }

    fn record(&self, call: Call) -> std::cell::RefMut<'_, MockState> {
        let mut state = self.state.borrow_mut();
        state.calls.push(call);
        state
    }

    fn permission(state: &MockState, permission: Permission) -> PermissionState {
        state
            .permissions
            .get(&permission)
            .copied()
            .unwrap_or(PermissionState::Prompt)
    }
}

impl FileDialogs for Mock {
    fn open(&self, options: OpenOptions) -> Reply<Vec<PickedFile>> {
        let mut state = self.record(Call::Open(options));
        Reply::ready(
            state
                .opens
                .pop_front()
                .unwrap_or(Err(ServiceError::Cancelled)),
        )
    }

    fn save(&self, options: SaveOptions, contents: Vec<u8>) -> Reply<Option<PathBuf>> {
        let mut state = self.record(Call::Save { options, contents });
        Reply::ready(
            state
                .saves
                .pop_front()
                .unwrap_or(Err(ServiceError::Cancelled)),
        )
    }
}

impl Share for Mock {
    fn share(&self, item: ShareItem) -> Reply<()> {
        self.record(Call::Share(item));
        Reply::ready(Ok(()))
    }
}

impl Notifications for Mock {
    fn notify(&self, notification: Notification) -> Reply<()> {
        let state = self.record(Call::Notify(notification));
        match Self::permission(&state, Permission::Notifications) {
            PermissionState::Granted => Reply::ready(Ok(())),
            _ => Reply::err(ServiceError::Denied),
        }
    }

    fn withdraw(&self, id: &str) {
        self.record(Call::Withdraw(id.to_owned()));
    }
}

impl Permissions for Mock {
    fn status(&self, permission: Permission) -> Reply<PermissionState> {
        let state = self.record(Call::PermissionStatus(permission));
        Reply::ready(Ok(Self::permission(&state, permission)))
    }

    fn request(&self, permission: Permission) -> Reply<PermissionState> {
        let mut state = self.record(Call::RequestPermission(permission));
        let now = match Self::permission(&state, permission) {
            PermissionState::Prompt => state
                .answers
                .get(&permission)
                .copied()
                .unwrap_or(PermissionState::Granted),
            decided => decided,
        };
        state.permissions.insert(permission, now);
        Reply::ready(Ok(now))
    }
}

impl SecureStorage for Mock {
    fn get(&self, key: &str) -> Reply<Option<Vec<u8>>> {
        let state = self.record(Call::SecureGet(key.to_owned()));
        Reply::ready(Ok(state.secrets.get(key).cloned()))
    }

    fn set(&self, key: &str, value: &[u8]) -> Reply<()> {
        let mut state = self.record(Call::SecureSet(key.to_owned()));
        state.secrets.insert(key.to_owned(), value.to_vec());
        Reply::ready(Ok(()))
    }

    fn remove(&self, key: &str) -> Reply<()> {
        let mut state = self.record(Call::SecureRemove(key.to_owned()));
        state.secrets.remove(key);
        Reply::ready(Ok(()))
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

    fn now<T>(mut reply: Reply<T>) -> ServiceResult<T> {
        reply.try_take().expect("the mock answers at once")
    }

    #[test]
    fn dialogs_answer_the_script_then_cancel() {
        let mock = Mock::new();
        let services = mock.services();
        let file = PickedFile {
            name: "a.txt".into(),
            path: None,
            contents: b"hi".to_vec(),
        };
        mock.answer_open(Ok(vec![file.clone()]));
        let options = OpenOptions {
            multiple: true,
            ..OpenOptions::default()
        };
        assert_eq!(now(services.files().open(options.clone())), Ok(vec![file]));
        assert_eq!(
            now(services.files().open(options.clone())),
            Err(ServiceError::Cancelled)
        );
        assert_eq!(
            mock.calls(),
            vec![Call::Open(options.clone()), Call::Open(options)]
        );
    }

    #[test]
    fn notifications_follow_the_permission() {
        let mock = Mock::new();
        let services = mock.services();
        let note = Notification {
            id: "n".into(),
            title: "t".into(),
            body: "b".into(),
        };
        assert_eq!(
            now(services.notifications().notify(note.clone())),
            Err(ServiceError::Denied)
        );
        mock.answer_request(Permission::Notifications, PermissionState::Denied);
        assert_eq!(
            now(services.permissions().request(Permission::Notifications)),
            Ok(PermissionState::Denied)
        );
        mock.set_permission(Permission::Notifications, PermissionState::Prompt);
        mock.answer_request(Permission::Notifications, PermissionState::Granted);
        assert_eq!(
            now(services.permissions().request(Permission::Notifications)),
            Ok(PermissionState::Granted)
        );
        assert_eq!(
            now(services.permissions().status(Permission::Notifications)),
            Ok(PermissionState::Granted)
        );
        assert_eq!(now(services.notifications().notify(note)), Ok(()));
    }

    #[test]
    fn secure_storage_round_trips() {
        let services = Mock::new().services();
        let storage = services.secure_storage();
        assert_eq!(now(storage.get("token")), Ok(None));
        assert_eq!(now(storage.set("token", b"s3cret")), Ok(()));
        assert_eq!(now(storage.get("token")), Ok(Some(b"s3cret".to_vec())));
        assert_eq!(now(storage.remove("token")), Ok(()));
        assert_eq!(now(storage.remove("token")), Ok(()));
        assert_eq!(now(storage.get("token")), Ok(None));
    }

    #[test]
    fn a_single_service_can_be_replaced() {
        let mock = Mock::new();
        let services = Services::unsupported().with_haptics(mock.clone());
        assert_eq!(services.haptics().play(Haptic::Success), Ok(()));
        assert_eq!(
            now(services.share().share(ShareItem::Text("x".into()))),
            Err(ServiceError::Unsupported)
        );
        assert_eq!(mock.calls(), vec![Call::Haptic(Haptic::Success)]);
    }
}
