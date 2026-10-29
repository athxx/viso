//! The effects a component's view runs: their entries in the handler table and
//! when they run. The compiler emits them into the behavior module and the view
//! package; the view runtime schedules them.

use core::time::Duration;

use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder};

/// A varint that must fit a `u32`.
fn read_u32_varint(dec: &mut Decoder<'_>) -> Result<u32, DecodeError> {
    let offset = dec.position();
    u32::try_from(dec.read_varint()?).map_err(|_| DecodeError::Malformed { offset })
}

/// When an `effect` runs, after the commit that mounts its component and the
/// commits that change its dependencies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectRun {
    /// Once, after the mount.
    Mount,
    /// After each commit that changes a dependency's value.
    Change,
    /// After the mount and after each commit that changes a dependency's value.
    MountAndChange,
}

/// An `effect` of a component's view: its entries in the handler table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentEffect {
    /// The region entry computing the dependency values as a list; `None` for
    /// an effect without dependencies.
    pub deps: Option<u32>,
    /// The effect-kind chunk of the behavior module that is the body.
    pub body: u32,
    /// When it runs.
    pub run: EffectRun,
    /// What it loads, for a `resource`: its dependencies are then the key and
    /// its body starts the loader.
    pub resource: Option<ResourceLoad>,
}

/// How a `resource` loads: the effect carrying it runs on mount and on each
/// key change, and its body starts the loader task without handlers; the view
/// host moves the state through `loading`, `ready`, `error` and `reloading`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceLoad {
    /// The region entry reading the resource's state.
    pub state: u32,
    /// The handler-kind chunk writing its payload to the state.
    pub write: u32,
    /// How long a key must hold before its loader starts.
    pub debounce: Option<Duration>,
    /// How long a loaded value stays cached under its key.
    pub cache_for: Option<Duration>,
    /// Whether an error is cached as a value is.
    pub cache_errors: bool,
    /// Whether a key change cancels the load in flight.
    pub keep_latest: bool,
    /// The pool of a `ResourceScope::shared` resource, one per declaration:
    /// every instance of it in the view shares one load and one cache per
    /// key. `None` for a resource each instance loads and caches alone.
    pub shared: Option<u32>,
}

impl Encode for ComponentEffect {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_varint(self.deps.map_or(0, |d| u64::from(d) + 1));
        enc.write_varint(u64::from(self.body));
        enc.write_u8(match self.run {
            EffectRun::Mount => 0,
            EffectRun::Change => 1,
            EffectRun::MountAndChange => 2,
        });
        match &self.resource {
            None => enc.write_u8(0),
            Some(load) => {
                enc.write_u8(1);
                enc.write_varint(u64::from(load.state));
                enc.write_varint(u64::from(load.write));
                write_duration(enc, load.debounce);
                write_duration(enc, load.cache_for);
                enc.write_bool(load.cache_errors);
                enc.write_bool(load.keep_latest);
                enc.write_varint(load.shared.map_or(0, |p| u64::from(p) + 1));
            }
        }
    }
}

/// An optional duration: `0` for none, else its nanoseconds plus one.
pub fn write_duration(enc: &mut Encoder, duration: Option<Duration>) {
    let nanos = duration.map_or(0, |d| {
        u64::try_from(d.as_nanos()).unwrap_or(u64::MAX - 1) + 1
    });
    enc.write_varint(nanos);
}

pub fn read_duration(dec: &mut Decoder<'_>) -> Result<Option<Duration>, DecodeError> {
    Ok(dec.read_varint()?.checked_sub(1).map(Duration::from_nanos))
}

fn read_resource(dec: &mut Decoder<'_>) -> Result<ResourceLoad, DecodeError> {
    Ok(ResourceLoad {
        state: read_u32_varint(dec)?,
        write: read_u32_varint(dec)?,
        debounce: read_duration(dec)?,
        cache_for: read_duration(dec)?,
        cache_errors: dec.read_bool()?,
        keep_latest: dec.read_bool()?,
        shared: read_u32_varint(dec)?.checked_sub(1),
    })
}

impl Decode for ComponentEffect {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let deps = read_u32_varint(dec)?.checked_sub(1);
        let body = read_u32_varint(dec)?;
        let offset = dec.position();
        let run = match dec.read_u8()? {
            0 => EffectRun::Mount,
            1 => EffectRun::Change,
            2 => EffectRun::MountAndChange,
            _ => return Err(DecodeError::Malformed { offset }),
        };
        let resource = match dec.read_u8()? {
            0 => None,
            1 => Some(read_resource(dec)?),
            _ => return Err(DecodeError::Malformed { offset }),
        };
        Ok(ComponentEffect {
            deps,
            body,
            run,
            resource,
        })
    }
}
