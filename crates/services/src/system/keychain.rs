//! Secure storage in the Keychain (macOS and iOS): one generic password per
//! key, under the app's service name.
//!
//! On macOS the items go to the login keychain: the data protection keychain
//! needs a keychain-access-group entitlement, which an unsigned binary lacks.

use std::ptr::{self, NonNull};

use objc2_core_foundation::{
    CFBoolean, CFData, CFDictionary, CFRetained, CFString, CFType, kCFBooleanTrue,
};
use objc2_security::{
    SecItemAdd, SecItemCopyMatching, SecItemDelete, SecItemUpdate, errSecAuthFailed,
    errSecInteractionNotAllowed, errSecItemNotFound, errSecSuccess, errSecUserCanceled,
    kSecAttrAccount, kSecAttrService, kSecClass, kSecClassGenericPassword, kSecMatchLimit,
    kSecMatchLimitOne, kSecReturnData, kSecValueData,
};

use crate::reply::{Reply, ServiceError, ServiceResult};
use crate::secure_storage::SecureStorage;

pub(crate) struct Keychain {
    service: CFRetained<CFString>,
}

impl Keychain {
    pub(crate) fn new(app: &str) -> Self {
        Self {
            service: CFString::from_str(app),
        }
    }

    /// The query naming `key`'s item, plus `extra` pairs.
    fn query(
        &self,
        key: &str,
        extra: &[(&CFString, &CFType)],
    ) -> CFRetained<CFDictionary<CFString, CFType>> {
        let account = CFString::from_str(key);
        // SAFETY: the kSec* keys and values are immutable Security framework
        // constants, initialized before any code runs.
        let (class, generic, service, account_key) = unsafe {
            (
                kSecClass,
                kSecClassGenericPassword,
                kSecAttrService,
                kSecAttrAccount,
            )
        };
        let mut keys: Vec<&CFString> = vec![class, service, account_key];
        let mut values: Vec<&CFType> = vec![generic, &self.service, &account];
        for (key, value) in extra {
            keys.push(key);
            values.push(value);
        }
        CFDictionary::from_slices(&keys, &values)
    }

    fn read(&self, key: &str) -> ServiceResult<Option<Vec<u8>>> {
        // SAFETY: immutable Security and CoreFoundation constants.
        let (return_data, limit, one, yes) = unsafe {
            (
                kSecReturnData,
                kSecMatchLimit,
                kSecMatchLimitOne,
                kCFBooleanTrue,
            )
        };
        let yes: &CFBoolean =
            yes.ok_or_else(|| ServiceError::Failed("no kCFBooleanTrue".into()))?;
        let query = self.query(key, &[(return_data, yes), (limit, one)]);
        let mut result: *const CFType = ptr::null();
        // SAFETY: the query maps CFString keys to CF values as SecItem expects,
        // and `result` is a valid out pointer.
        let status = unsafe { SecItemCopyMatching(query.as_opaque(), &mut result) };
        if status == errSecItemNotFound {
            return Ok(None);
        }
        check(status)?;
        let Some(result) = NonNull::new(result.cast_mut()) else {
            return Ok(None);
        };
        // SAFETY: SecItemCopyMatching returns the result retained (+1), which
        // the CFRetained takes over.
        let result = unsafe { CFRetained::from_raw(result) };
        let data = result
            .downcast::<CFData>()
            .map_err(|_| ServiceError::Failed("the keychain item holds no data".into()))?;
        Ok(Some(data.to_vec()))
    }

    fn write(&self, key: &str, value: &[u8]) -> ServiceResult<()> {
        // SAFETY: an immutable Security framework constant.
        let value_key = unsafe { kSecValueData };
        let data = CFData::from_bytes(value);
        let update = CFDictionary::<CFString, CFType>::from_slices(&[value_key], &[&data]);
        let query = self.query(key, &[]);
        // SAFETY: both dictionaries map CFString keys to CF values.
        let status = unsafe { SecItemUpdate(query.as_opaque(), update.as_opaque()) };
        if status != errSecItemNotFound {
            return check(status);
        }
        let attributes = self.query(key, &[(value_key, &data)]);
        // SAFETY: the attributes map CFString keys to CF values; a null result
        // pointer asks for no result.
        check(unsafe { SecItemAdd(attributes.as_opaque(), ptr::null_mut()) })
    }

    fn delete(&self, key: &str) -> ServiceResult<()> {
        let query = self.query(key, &[]);
        // SAFETY: the query maps CFString keys to CF values.
        let status = unsafe { SecItemDelete(query.as_opaque()) };
        if status == errSecItemNotFound {
            return Ok(());
        }
        check(status)
    }
}

fn check(status: i32) -> ServiceResult<()> {
    match status {
        s if s == errSecSuccess => Ok(()),
        s if s == errSecUserCanceled => Err(ServiceError::Cancelled),
        s if s == errSecAuthFailed || s == errSecInteractionNotAllowed => Err(ServiceError::Denied),
        s => Err(ServiceError::Failed(format!("keychain status {s}"))),
    }
}

impl SecureStorage for Keychain {
    fn get(&self, key: &str) -> Reply<Option<Vec<u8>>> {
        Reply::ready(self.read(key))
    }

    fn set(&self, key: &str, value: &[u8]) -> Reply<()> {
        Reply::ready(self.write(key, value))
    }

    fn remove(&self, key: &str) -> Reply<()> {
        Reply::ready(self.delete(key))
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    fn now<T>(mut reply: Reply<T>) -> ServiceResult<T> {
        reply.try_take().expect("the keychain answers at once")
    }

    #[test]
    fn a_secret_round_trips_through_the_keychain() {
        let keychain = Keychain::new("dev.viso.services-test");
        let key = format!("round-trip-{}", std::process::id());
        assert_eq!(now(keychain.get(&key)), Ok(None));
        assert_eq!(now(keychain.set(&key, b"first")), Ok(()));
        assert_eq!(now(keychain.set(&key, b"second")), Ok(()));
        assert_eq!(now(keychain.get(&key)), Ok(Some(b"second".to_vec())));
        assert_eq!(now(keychain.remove(&key)), Ok(()));
        assert_eq!(now(keychain.remove(&key)), Ok(()));
        assert_eq!(now(keychain.get(&key)), Ok(None));
    }
}
