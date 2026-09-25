//! Session-bus plumbing the Linux services share: worker threads that own a
//! connection, D-Bus error mapping, and signal subscriptions.

use std::cell::OnceCell;
use std::fmt::Display;
use std::sync::mpsc;

use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type;
use zbus::{Error, MatchRule};

use crate::reply::{Completer, Reply, ServiceError, ServiceResult, reply};

type Job<S> = Box<dyn FnOnce(ServiceResult<&Connection>, &mut S) + Send>;

/// A thread that runs jobs against the session bus one at a time, in the
/// order they were queued, with state `S` carried from job to job. The
/// thread starts with the first job and ends when the worker is dropped.
pub(super) struct Worker<S> {
    name: &'static str,
    jobs: OnceCell<mpsc::Sender<Job<S>>>,
}

impl<S: Default + 'static> Worker<S> {
    pub(super) const fn new(name: &'static str) -> Self {
        Self {
            name,
            jobs: OnceCell::new(),
        }
    }

    /// Queue `job`; it answers the returned reply.
    pub(super) fn ask<T: Send + 'static>(
        &self,
        job: impl FnOnce(&Connection, &mut S) -> ServiceResult<T> + Send + 'static,
    ) -> Reply<T> {
        let (completer, reply) = reply();
        self.run(move |connection, state| {
            completer.complete(connection.and_then(|c| job(c, state)));
        });
        reply
    }

    /// Queue `job`, which is told when there is no session bus.
    pub(super) fn run(
        &self,
        job: impl FnOnce(ServiceResult<&Connection>, &mut S) + Send + 'static,
    ) {
        let jobs = self.jobs.get_or_init(|| spawn(self.name));
        if let Err(mpsc::SendError(job)) = jobs.send(Box::new(job)) {
            // The thread could not start.
            job(Err(ServiceError::Unsupported), &mut S::default());
        }
    }
}

fn spawn<S: Default + 'static>(name: &str) -> mpsc::Sender<Job<S>> {
    let (jobs, queue) = mpsc::channel::<Job<S>>();
    let _ = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || serve(&queue));
    jobs
}

fn serve<S: Default>(queue: &mpsc::Receiver<Job<S>>) {
    let mut connection: Option<Connection> = None;
    let mut state = S::default();
    for job in queue {
        if connection.is_none() {
            connection = Connection::session().ok();
            state = S::default();
        }
        match &connection {
            Some(connection) => job(Ok(connection), &mut state),
            None => job(Err(ServiceError::Unsupported), &mut state),
        }
    }
}

/// Run `job` on a thread of its own with a connection of its own, for a
/// call that waits on the user for long.
pub(super) fn detached<T: Send + 'static>(
    name: &str,
    job: impl FnOnce(&Connection) -> ServiceResult<T> + Send + 'static,
) -> Reply<T> {
    let (completer, reply) = reply::<T>();
    let spawned = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || answer(completer, job));
    match spawned {
        Ok(_) => reply,
        Err(e) => Reply::err(ServiceError::Failed(e.to_string())),
    }
}

fn answer<T>(completer: Completer<T>, job: impl FnOnce(&Connection) -> ServiceResult<T>) {
    let result = match Connection::session() {
        Ok(connection) => job(&connection),
        Err(_) => Err(ServiceError::Unsupported),
    };
    completer.complete(result);
}

/// A D-Bus failure as a service error: a peer that is not running, or
/// lacks the interface, means the capability is missing here.
pub(super) fn failure(error: Error) -> ServiceError {
    const MISSING: [&str; 4] = [
        "org.freedesktop.DBus.Error.ServiceUnknown",
        "org.freedesktop.DBus.Error.UnknownInterface",
        "org.freedesktop.DBus.Error.UnknownMethod",
        "org.freedesktop.DBus.Error.UnknownObject",
    ];
    match &error {
        Error::MethodError(name, ..) if MISSING.contains(&name.as_str()) => {
            ServiceError::Unsupported
        }
        _ => ServiceError::Failed(error.to_string()),
    }
}

pub(super) fn failed(reason: impl Display) -> ServiceError {
    ServiceError::Failed(reason.to_string())
}

/// Subscribe to `interface.member` signals from the object at `path`.
pub(super) fn signals(
    connection: &Connection,
    path: &str,
    interface: &str,
    member: &str,
) -> ServiceResult<MessageIterator> {
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .interface(interface)
        .and_then(|b| b.member(member))
        .and_then(|b| b.path(path))
        .map_err(failure)?
        .build();
    MessageIterator::for_match_rule(rule, connection, None).map_err(failure)
}
