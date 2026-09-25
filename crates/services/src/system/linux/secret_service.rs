//! Secure storage in the freedesktop Secret Service (GNOME Keyring, KWallet,
//! KeePassXC): one item per key in the default collection, found by its
//! `service` and `account` attributes. Secrets cross the bus in a `plain`
//! session: the session bus is private to the user.
//!
//! A locked collection or item is unlocked through the service's prompt,
//! which the user may dismiss.

use std::collections::HashMap;

use zbus::blocking::Connection;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use super::bus::{Worker, failed, failure, signals};
use crate::reply::{Reply, ServiceError, ServiceResult};
use crate::secure_storage::SecureStorage;

const SECRETS: &str = "org.freedesktop.secrets";
const SECRETS_PATH: &str = "/org/freedesktop/secrets";
const SERVICE: &str = "org.freedesktop.Secret.Service";
const COLLECTION: &str = "org.freedesktop.Secret.Collection";
const ITEM: &str = "org.freedesktop.Secret.Item";
const PROMPT: &str = "org.freedesktop.Secret.Prompt";

/// The path the service answers for "no object".
const NONE: &str = "/";

/// A secret as the service passes it: session, parameters, value, type.
type Secret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);

pub(super) struct SecretService {
    app: String,
    /// The open session, carried across calls.
    worker: Worker<Option<OwnedObjectPath>>,
}

impl SecretService {
    pub(super) fn new(app: &str) -> Self {
        Self {
            app: app.to_owned(),
            worker: Worker::new("viso-secrets"),
        }
    }
}

impl SecureStorage for SecretService {
    fn get(&self, key: &str) -> Reply<Option<Vec<u8>>> {
        let attributes = attributes(&self.app, key);
        self.worker.ask(move |connection, session| {
            let session = open(connection, session)?;
            let Some(item) = find(connection, &attributes)?.into_iter().next() else {
                return Ok(None);
            };
            let (_, _, value, _): Secret = call(connection, &item, ITEM, "GetSecret", &session)?;
            Ok(Some(value))
        })
    }

    fn set(&self, key: &str, value: &[u8]) -> Reply<()> {
        let attributes = attributes(&self.app, key);
        let label = format!("{}/{key}", self.app);
        let value = value.to_vec();
        self.worker.ask(move |connection, session| {
            let session = open(connection, session)?;
            let collection = default_collection(connection)?;
            unlock(connection, vec![collection.clone()])?;
            let mut properties: HashMap<&str, Value<'_>> = HashMap::new();
            properties.insert("org.freedesktop.Secret.Item.Label", Value::from(label));
            properties.insert(
                "org.freedesktop.Secret.Item.Attributes",
                Value::from(attributes),
            );
            let secret = (
                &session,
                Vec::<u8>::new(),
                value,
                "application/octet-stream",
            );
            let (_, pending): (OwnedObjectPath, OwnedObjectPath) = call(
                connection,
                &collection,
                COLLECTION,
                "CreateItem",
                &(properties, secret, true),
            )?;
            prompt(connection, &pending).map(drop)
        })
    }

    fn remove(&self, key: &str) -> Reply<()> {
        let attributes = attributes(&self.app, key);
        self.worker.ask(move |connection, _| {
            for item in find(connection, &attributes)? {
                let pending: OwnedObjectPath = call(connection, &item, ITEM, "Delete", &())?;
                prompt(connection, &pending)?;
            }
            Ok(())
        })
    }
}

fn attributes(app: &str, key: &str) -> HashMap<String, String> {
    HashMap::from([
        ("service".to_owned(), app.to_owned()),
        ("account".to_owned(), key.to_owned()),
    ])
}

/// Call `interface.method` on the service's object at `path`.
fn call<B, R>(
    connection: &Connection,
    path: &str,
    interface: &str,
    method: &str,
    body: &B,
) -> ServiceResult<R>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
    R: serde::de::DeserializeOwned + zbus::zvariant::Type,
{
    connection
        .call_method(Some(SECRETS), path, Some(interface), method, body)
        .map_err(failure)?
        .body()
        .deserialize()
        .map_err(failure)
}

/// The session, opened on first use.
fn open(
    connection: &Connection,
    session: &mut Option<OwnedObjectPath>,
) -> ServiceResult<OwnedObjectPath> {
    if let Some(session) = session {
        return Ok(session.clone());
    }
    let (_, opened): (OwnedValue, OwnedObjectPath) = call(
        connection,
        SECRETS_PATH,
        SERVICE,
        "OpenSession",
        &("plain", Value::from("")),
    )?;
    *session = Some(opened.clone());
    Ok(opened)
}

/// The items carrying `attributes`, unlocked.
fn find(
    connection: &Connection,
    attributes: &HashMap<String, String>,
) -> ServiceResult<Vec<OwnedObjectPath>> {
    let (mut unlocked, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) =
        call(connection, SECRETS_PATH, SERVICE, "SearchItems", attributes)?;
    if !locked.is_empty() {
        unlock(connection, locked.clone())?;
        unlocked.extend(locked);
    }
    Ok(unlocked)
}

/// The default collection, created when there is none yet.
fn default_collection(connection: &Connection) -> ServiceResult<OwnedObjectPath> {
    let collection: OwnedObjectPath =
        call(connection, SECRETS_PATH, SERVICE, "ReadAlias", &"default")?;
    if collection.as_str() != NONE {
        return Ok(collection);
    }
    let mut properties: HashMap<&str, Value<'_>> = HashMap::new();
    properties.insert(
        "org.freedesktop.Secret.Collection.Label",
        Value::from("Login"),
    );
    let (created, pending): (OwnedObjectPath, OwnedObjectPath) = call(
        connection,
        SECRETS_PATH,
        SERVICE,
        "CreateCollection",
        &(properties, "default"),
    )?;
    if created.as_str() != NONE {
        return Ok(created);
    }
    let result =
        prompt(connection, &pending)?.ok_or_else(|| failed("the service created no collection"))?;
    OwnedObjectPath::try_from(result).map_err(failed)
}

fn unlock(connection: &Connection, objects: Vec<OwnedObjectPath>) -> ServiceResult<()> {
    let (_, pending): (Vec<OwnedObjectPath>, OwnedObjectPath) =
        call(connection, SECRETS_PATH, SERVICE, "Unlock", &objects)?;
    prompt(connection, &pending).map(drop)
}

/// Show the prompt at `path`, if any, and wait for the user: its result,
/// or `Cancelled` when they dismissed it.
fn prompt(connection: &Connection, path: &ObjectPath<'_>) -> ServiceResult<Option<OwnedValue>> {
    if path.as_str() == NONE {
        return Ok(None);
    }
    let mut completed = signals(connection, path.as_str(), PROMPT, "Completed")?;
    connection
        .call_method(Some(SECRETS), path, Some(PROMPT), "Prompt", &"")
        .map_err(failure)?;
    let message = completed
        .next()
        .ok_or(ServiceError::Cancelled)?
        .map_err(failure)?;
    let (dismissed, result): (bool, OwnedValue) = message.body().deserialize().map_err(failure)?;
    if dismissed {
        Err(ServiceError::Cancelled)
    } else {
        Ok(Some(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now<T>(reply: Reply<T>) -> ServiceResult<T> {
        let mut reply = reply;
        loop {
            if let Some(result) = reply.try_take() {
                return result;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Needs a session bus with an unlocked Secret Service.
    #[test]
    #[ignore = "needs a Secret Service"]
    fn a_secret_round_trips_through_the_secret_service() {
        let secrets = SecretService::new("dev.viso.services-test");
        let key = format!("round-trip-{}", std::process::id());
        assert_eq!(now(secrets.get(&key)), Ok(None));
        assert_eq!(now(secrets.set(&key, b"first")), Ok(()));
        assert_eq!(now(secrets.set(&key, b"second")), Ok(()));
        assert_eq!(now(secrets.get(&key)), Ok(Some(b"second".to_vec())));
        assert_eq!(now(secrets.remove(&key)), Ok(()));
        assert_eq!(now(secrets.remove(&key)), Ok(()));
        assert_eq!(now(secrets.get(&key)), Ok(None));
    }
}
