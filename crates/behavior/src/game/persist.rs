//! Persisted state: each `@persist` state of a game's systems, stored under
//! its key so the next run starts from the value this one left.
//!
//! The host installs a [`Persist`] service on the VM a [`Scheduler`] starts
//! with, around a [`PersistStore`]: [`MemoryStore`] for tests, [`DirStore`]
//! for a directory of files. Every persisted state is loaded before the
//! start; one that does not load takes its initializer and leaves a
//! [`PersistReport`]. A stored value of another type converts by the type
//! compatibility matrix or a `@migrate` function from its old type. Writes
//! happen at tick boundaries, once an interval, for the states that
//! changed; the store takes them without blocking on IO, and
//! [`Scheduler::suspend`] makes them durable.
//!
//! [`Scheduler`]: super::Scheduler
//! [`Scheduler::suspend`]: super::Scheduler::suspend

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use viso_ende::{DecodeError, Decoder, Encoder};

use crate::module::{Module, PersistSlot};
use crate::retype::{Retyping, ValueSchema};
use crate::value::Value;
use crate::vm::{Instance, Vm};
use crate::wire::{read_value, write_value};

/// The capability a module persisting state is granted at link.
pub const PERSIST_CAPABILITY: &str = "storage.persist";

/// Where persisted values live: blobs by key.
pub trait PersistStore {
    /// The blob stored under `key`, `None` when there is none.
    ///
    /// # Errors
    ///
    /// Why the store could not be read.
    fn load(&mut self, key: &str) -> Result<Option<Vec<u8>>, String>;

    /// Stores `blob` under `key`, replacing what was there, without blocking
    /// on IO.
    fn store(&mut self, key: &str, blob: Vec<u8>);

    /// Blocks until every blob stored so far is durable.
    ///
    /// # Errors
    ///
    /// The first write that failed since the last flush.
    fn flush(&mut self) -> Result<(), String>;
}

/// The [`PersistStore`] a scheduler persists through, installed as a service
/// of the VM it starts with.
pub struct Persist {
    pub(crate) store: Box<dyn PersistStore>,
}

impl Persist {
    /// Persists through `store`.
    pub fn new(store: impl PersistStore + 'static) -> Persist {
        Persist {
            store: Box::new(store),
        }
    }
}

impl std::fmt::Debug for Persist {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Persist")
    }
}

/// A persisted state that did not load, or a write that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistReport {
    /// The state's key; empty for a failed flush.
    pub key: Box<str>,
    /// The stable diagnostic code: `E6103` when the module was not granted
    /// `storage.persist`, otherwise `E9111`.
    pub code: &'static str,
    pub message: String,
}

/// An in-memory [`PersistStore`]; its clones share the blobs, so a test can
/// start a second game on what the first stored.
#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    blobs: Rc<RefCell<BTreeMap<String, Vec<u8>>>>,
}

impl MemoryStore {
    /// The blob stored under `key`.
    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.blobs.borrow().get(key).cloned()
    }

    /// Every key stored, in order.
    pub fn keys(&self) -> Vec<String> {
        self.blobs.borrow().keys().cloned().collect()
    }
}

impl PersistStore for MemoryStore {
    fn load(&mut self, key: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(self.get(key))
    }

    fn store(&mut self, key: &str, blob: Vec<u8>) {
        self.blobs.borrow_mut().insert(key.to_owned(), blob);
    }

    fn flush(&mut self) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(not(target_family = "wasm"))]
pub use dir::DirStore;

#[cfg(not(target_family = "wasm"))]
mod dir {
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;

    use super::PersistStore;

    enum Job {
        Write(String, Vec<u8>),
        Flush(Sender<()>),
    }

    /// A [`PersistStore`] over a directory, one file a key. A background
    /// thread writes each blob to a temporary file, syncs it and renames it
    /// over the old one, so a crash leaves the old blob or the new, never a
    /// torn one; blobs stored again before it got to them are written once.
    pub struct DirStore {
        dir: PathBuf,
        jobs: Option<Sender<Job>>,
        writer: Option<JoinHandle<()>>,
        errors: Arc<Mutex<Vec<String>>>,
    }

    impl DirStore {
        /// A store over `dir`, created if missing.
        ///
        /// # Errors
        ///
        /// When the directory cannot be created or the writer thread cannot
        /// start.
        pub fn open(dir: impl Into<PathBuf>) -> io::Result<DirStore> {
            let dir = dir.into();
            fs::create_dir_all(&dir)?;
            let (jobs, queue) = mpsc::channel();
            let errors = Arc::new(Mutex::new(Vec::new()));
            let writer = {
                let (dir, errors) = (dir.clone(), Arc::clone(&errors));
                std::thread::Builder::new()
                    .name("viso-persist".into())
                    .spawn(move || write_jobs(&dir, &queue, &errors))?
            };
            Ok(DirStore {
                dir,
                jobs: Some(jobs),
                writer: Some(writer),
                errors,
            })
        }

        fn send(&self, job: Job) -> bool {
            self.jobs
                .as_ref()
                .is_some_and(|jobs| jobs.send(job).is_ok())
        }
    }

    /// The file holding `key`: its bytes past `[A-Za-z0-9_-]` escaped as
    /// `%XX`, so every key names one file inside the directory.
    fn file_name(key: &str) -> String {
        let mut name = String::with_capacity(key.len() + 8);
        for b in key.bytes() {
            if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' {
                name.push(char::from(b));
            } else {
                name.push_str(&format!("%{b:02X}"));
            }
        }
        name.push_str(".persist");
        name
    }

    fn write_jobs(dir: &Path, queue: &Receiver<Job>, errors: &Mutex<Vec<String>>) {
        let mut pending: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut acks = Vec::new();
        while let Ok(job) = queue.recv() {
            let mut next = Some(job);
            while let Some(job) = next.take().or_else(|| queue.try_recv().ok()) {
                match job {
                    Job::Write(key, blob) => {
                        pending.insert(key, blob);
                    }
                    Job::Flush(ack) => acks.push(ack),
                }
            }
            for (key, blob) in std::mem::take(&mut pending) {
                if let Err(error) = write_atomically(dir, &key, &blob) {
                    let mut errors = errors.lock().unwrap_or_else(|e| e.into_inner());
                    errors.push(format!("writing `{key}`: {error}"));
                }
            }
            for ack in acks.drain(..) {
                let _ = ack.send(());
            }
        }
    }

    fn write_atomically(dir: &Path, key: &str, blob: &[u8]) -> io::Result<()> {
        let name = file_name(key);
        let temporary = dir.join(format!(".{name}.tmp"));
        let mut file = fs::File::create(&temporary)?;
        file.write_all(blob)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, dir.join(name))
    }

    impl PersistStore for DirStore {
        fn load(&mut self, key: &str) -> Result<Option<Vec<u8>>, String> {
            match fs::read(self.dir.join(file_name(key))) {
                Ok(blob) => Ok(Some(blob)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(format!("reading `{key}`: {error}")),
            }
        }

        fn store(&mut self, key: &str, blob: Vec<u8>) {
            if !self.send(Job::Write(key.to_owned(), blob)) {
                let mut errors = self.errors.lock().unwrap_or_else(|e| e.into_inner());
                errors.push(format!("writing `{key}`: the writer thread stopped"));
            }
        }

        fn flush(&mut self) -> Result<(), String> {
            let (ack, done) = mpsc::channel();
            if !self.send(Job::Flush(ack)) || done.recv().is_err() {
                return Err("the writer thread stopped".to_owned());
            }
            let mut errors = self.errors.lock().unwrap_or_else(|e| e.into_inner());
            match errors.drain(..).next() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }
    }

    impl Drop for DirStore {
        fn drop(&mut self) {
            // Closing the queue lets the writer finish what is queued and end.
            self.jobs = None;
            if let Some(writer) = self.writer.take() {
                let _ = writer.join();
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_key_names_one_file_inside_the_directory() {
            assert_eq!(file_name("best_score"), "best_score.persist");
            assert_eq!(file_name("../a b"), "%2E%2E%2Fa%20b.persist");
        }

        #[test]
        fn blobs_survive_the_store_and_the_last_of_a_key_wins() {
            let dir = std::env::temp_dir().join(format!("viso-dir-store-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            {
                let mut store = DirStore::open(&dir).expect("open");
                assert_eq!(store.load("k"), Ok(None));
                for i in 0..50u8 {
                    store.store("k", vec![i]);
                }
                store.store("other/key", vec![9]);
                store.flush().expect("flush");
                assert_eq!(store.load("k"), Ok(Some(vec![49])));
            }
            let mut store = DirStore::open(&dir).expect("reopen");
            assert_eq!(store.load("other/key"), Ok(Some(vec![9])));
            let _ = fs::remove_dir_all(&dir);
        }
    }
}

/// The blob marker and version.
const MAGIC: [u8; 4] = *b"VPS1";

/// A value as a build stored it: its type's spelling and schema.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Stored {
    pub(crate) spelling: Box<str>,
    pub(crate) schema: ValueSchema,
    pub(crate) value: Value,
}

impl Stored {
    /// The value of `slot`.
    pub(crate) fn of(slot: &PersistSlot, value: Value) -> Stored {
        Stored {
            spelling: slot.spelling.clone(),
            schema: slot.schema.clone(),
            value,
        }
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        enc.write_raw(&MAGIC);
        enc.write_str(&self.spelling);
        self.schema.encode(&mut enc);
        write_value(&mut enc, &self.value);
        enc.into_bytes()
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Stored, DecodeError> {
        let mut dec = Decoder::new(bytes);
        let offset = dec.position();
        if dec.read_raw(MAGIC.len())? != MAGIC {
            return Err(DecodeError::Malformed { offset });
        }
        let spelling = dec.read_str()?.into();
        let schema = ValueSchema::decode(&mut dec)?;
        let value = read_value(&mut dec)?;
        dec.finish()?;
        Ok(Stored {
            spelling,
            schema,
            value,
        })
    }

    /// The value as one of `slot`'s type: kept when the type is the same,
    /// converted by the matrix, or by the first `@migrate` function of
    /// `module` from its old type into `slot`'s; defaults and the function
    /// run on `vm` against `instance`.
    pub(crate) fn into_slot(
        self,
        slot: &PersistSlot,
        module: &Module,
        vm: &mut Vm,
        instance: &mut Instance,
    ) -> Result<Value, String> {
        if self.schema.same(&slot.schema) {
            return Ok(self.value);
        }
        let mut default = |chunk| vm.call(instance, chunk, &[]).ok().map(|o| o.value);
        if let Some(value) = Retyping::between(&self.schema, &slot.schema)
            .and_then(|retyping| retyping.apply(&self.value, &mut default))
        {
            return Ok(value);
        }
        for migrator in module.migrators() {
            if *migrator.from != *self.spelling || !migrator.ret.same(&slot.schema) {
                continue;
            }
            let mut default = |chunk| vm.call(instance, chunk, &[]).ok().map(|o| o.value);
            let Some(arg) = Retyping::between(&self.schema, &migrator.param)
                .and_then(|retyping| retyping.apply(&self.value, &mut default))
            else {
                continue;
            };
            return vm
                .call(instance, migrator.chunk, &[arg])
                .map(|outcome| outcome.value)
                .map_err(|fault| format!("its `@migrate` function faulted: {}", fault.message));
        }
        Err(format!(
            "a stored `{}` does not convert into `{}`",
            self.spelling, slot.spelling
        ))
    }
}

/// What a scheduler persists through and what it last stored.
pub(crate) struct Persistence {
    pub(crate) store: Box<dyn PersistStore>,
    /// The value last loaded or stored under each key.
    pub(crate) written: BTreeMap<Box<str>, Value>,
    /// Ticks between writes.
    pub(crate) interval: u64,
    /// The tick boundaries to pass before the next write.
    pub(crate) wait: u64,
}

impl Persistence {
    pub(crate) fn new(persist: Persist, interval: u64) -> Persistence {
        Persistence {
            store: persist.store,
            written: BTreeMap::new(),
            interval,
            wait: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retype::TypeDesc;

    #[test]
    fn a_stored_blob_round_trips_and_rejects_corruption() {
        let stored = Stored {
            spelling: "List<String>".into(),
            schema: ValueSchema {
                root: TypeDesc::List(Box::new(TypeDesc::Plain("String".into()))),
                decls: Box::new([]),
            },
            value: Value::List(Rc::new(vec![Value::Str(Rc::new("a".into()))])),
        };
        let blob = stored.encode();
        assert_eq!(Stored::decode(&blob), Ok(stored));
        for cut in 0..blob.len() {
            assert!(Stored::decode(&blob[..cut]).is_err());
        }
        let mut other = blob.clone();
        other[0] = b'X';
        assert!(Stored::decode(&other).is_err());
    }
}
