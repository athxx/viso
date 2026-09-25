//! Secure storage in the Credential Manager: one generic credential per
//! key, targeted `app/key`, persisted for the user on this machine. A
//! value holds at most 2560 bytes.

use std::ptr;

use windows::Win32::Foundation::ERROR_NOT_FOUND;
use windows::Win32::Security::Credentials::{
    CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW,
    CredDeleteW, CredFree, CredReadW, CredWriteW,
};
use windows::core::{HRESULT, HSTRING, PWSTR};

use crate::reply::{Reply, ServiceError, ServiceResult};
use crate::secure_storage::SecureStorage;

pub(super) struct Credentials {
    app: String,
}

fn not_found(error: &windows::core::Error) -> bool {
    error.code() == HRESULT::from_win32(ERROR_NOT_FOUND.0)
}

fn failed(error: windows::core::Error) -> ServiceError {
    ServiceError::Failed(error.message())
}

impl Credentials {
    pub(super) fn new(app: &str) -> Self {
        Self {
            app: app.to_owned(),
        }
    }

    fn target(&self, key: &str) -> HSTRING {
        HSTRING::from(format!("{}/{key}", self.app))
    }

    fn read(&self, key: &str) -> ServiceResult<Option<Vec<u8>>> {
        let mut credential: *mut CREDENTIALW = ptr::null_mut();
        // SAFETY: the target outlives the call; `credential` is a valid out
        // pointer.
        match unsafe { CredReadW(&self.target(key), CRED_TYPE_GENERIC, None, &mut credential) } {
            Err(e) if not_found(&e) => return Ok(None),
            Err(e) => return Err(failed(e)),
            Ok(()) => {}
        }
        // SAFETY: the read succeeded, so `credential` points to a credential
        // whose blob holds `CredentialBlobSize` bytes; it is freed once
        // copied.
        let value = unsafe {
            let found = &*credential;
            let value = if found.CredentialBlob.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(found.CredentialBlob, found.CredentialBlobSize as usize)
                    .to_vec()
            };
            CredFree(credential as *const _);
            value
        };
        Ok(Some(value))
    }

    fn write(&self, key: &str, value: &[u8]) -> ServiceResult<()> {
        if value.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
            return Err(ServiceError::Failed(format!(
                "a credential holds at most {CRED_MAX_CREDENTIAL_BLOB_SIZE} bytes"
            )));
        }
        let mut target: Vec<u16> = format!("{}/{key}", self.app)
            .encode_utf16()
            .chain([0])
            .collect();
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target.as_mut_ptr()),
            CredentialBlobSize: value.len() as u32,
            CredentialBlob: value.as_ptr().cast_mut(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..Default::default()
        };
        // SAFETY: the target and blob outlive the call, which copies them
        // and never writes through the blob pointer.
        unsafe { CredWriteW(&credential, 0) }.map_err(failed)
    }

    fn delete(&self, key: &str) -> ServiceResult<()> {
        // SAFETY: the target outlives the call.
        match unsafe { CredDeleteW(&self.target(key), CRED_TYPE_GENERIC, None) } {
            Err(e) if not_found(&e) => Ok(()),
            result => result.map_err(failed),
        }
    }
}

impl SecureStorage for Credentials {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn now<T>(mut reply: Reply<T>) -> ServiceResult<T> {
        reply
            .try_take()
            .expect("the credential manager answers at once")
    }

    #[test]
    fn a_secret_round_trips_through_the_credential_manager() {
        let credentials = Credentials::new("dev.viso.services-test");
        let key = format!("round-trip-{}", std::process::id());
        assert_eq!(now(credentials.get(&key)), Ok(None));
        assert_eq!(now(credentials.set(&key, b"first")), Ok(()));
        assert_eq!(now(credentials.set(&key, b"second")), Ok(()));
        assert_eq!(now(credentials.get(&key)), Ok(Some(b"second".to_vec())));
        assert_eq!(now(credentials.remove(&key)), Ok(()));
        assert_eq!(now(credentials.remove(&key)), Ok(()));
        assert_eq!(now(credentials.get(&key)), Ok(None));
    }
}
