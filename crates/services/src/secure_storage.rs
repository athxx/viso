//! Small secrets kept by the OS credential store.

use crate::reply::Reply;

/// Secrets (tokens, passwords, keys) in the OS credential store, scoped to
/// the app: the Keychain, the Windows Credential Manager, the Secret Service,
/// the Android Keystore. Values are meant to be small.
pub trait SecureStorage {
    /// The value stored under `key`, or `None`.
    fn get(&self, key: &str) -> Reply<Option<Vec<u8>>>;

    /// Store `value` under `key`, replacing any previous value.
    fn set(&self, key: &str, value: &[u8]) -> Reply<()>;

    /// Delete `key`; deleting a missing key succeeds.
    fn remove(&self, key: &str) -> Reply<()>;
}
