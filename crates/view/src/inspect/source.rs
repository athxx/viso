//! The tool's half of an inspection: what each node of a compiled view is in
//! its source, by the index a census names it with.

use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder};

use super::census::{
    count, malformed, read_option_str, read_strings, write_option_str, write_strings,
};

/// The compiled views of a package, by file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceTable {
    pub files: Vec<FileSources>,
}

impl SourceTable {
    /// Its wire form.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }

    /// The table `bytes` spell.
    ///
    /// # Errors
    ///
    /// Bytes that are not a table.
    pub fn from_bytes(bytes: &[u8]) -> Result<SourceTable, DecodeError> {
        SourceTable::decode_from_slice(bytes)
    }

    /// The view of `file`, as a census names it.
    pub fn file(&self, file: &str) -> Option<&FileSources> {
        self.files.iter().find(|sources| sources.file == file)
    }
}

/// One compiled view's nodes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileSources {
    /// The file as a census names it: as the build recorded it.
    pub file: String,
    /// The file as a tool shows it: its path from the project root.
    pub path: String,
    /// Its module path within its package.
    pub module: Vec<String>,
    /// Each static node, by static index.
    pub statics: Vec<NodeSource>,
    /// Each node a region mounts, by region, arm and arm item, ascending.
    pub regional: Vec<((u32, u32, u32), NodeSource)>,
    /// The reactive sources its view reads, by durable identity, ascending.
    pub states: Vec<StateSource>,
}

impl FileSources {
    /// The static node `index`.
    pub fn static_node(&self, index: u32) -> Option<&NodeSource> {
        self.statics.get(index as usize)
    }

    /// The node item `item` of arm `arm` of region `region` mounts.
    pub fn region_node(&self, region: u32, arm: u32, item: u32) -> Option<&NodeSource> {
        self.regional
            .binary_search_by_key(&(region, arm, item), |(at, _)| *at)
            .ok()
            .map(|at| &self.regional[at].1)
    }

    /// The source a runtime cell of durable identity `key` is.
    pub fn state(&self, key: (u64, u64)) -> Option<&StateSource> {
        self.states
            .binary_search_by_key(&key, |state| state.key)
            .ok()
            .map(|at| &self.states[at])
    }
}

/// A reactive source of a view: a `state`, `input` or `computed` its
/// bindings read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StateSource {
    /// Its durable identity: the key its runtime cell carries.
    pub key: (u64, u64),
    /// Its name in the source.
    pub name: String,
    /// Its type as source spells it, when the compiler inferred one.
    pub type_name: Option<String>,
    /// The 1-based line and UTF-16 column its declaration starts at.
    pub start: (u32, u32),
}

/// A node as its source declares it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeSource {
    /// Its widget type: `Button`, `Column`, …
    pub type_name: String,
    /// The name `node name: Type { … }` gives it.
    pub name: Option<String>,
    /// The styles applied to it, in the order they apply.
    pub styles: Vec<String>,
    /// The components whose view it is the root of, the outermost first: a
    /// view inlines the components it uses, so a component is its root node.
    pub components: Vec<String>,
    /// The component whose view declares it.
    pub owner: String,
    /// Its declaration's byte range in the file.
    pub bytes: (u32, u32),
    /// The 1-based line and UTF-16 column of the declaration's start and
    /// end, as a diagnostic's location gives them.
    pub start: (u32, u32),
    pub end: (u32, u32),
    /// Each property bound to a reactive source, with the source's durable
    /// identity, in declaration order.
    pub bindings: Vec<(String, (u64, u64))>,
}

impl Encode for SourceTable {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_varint(self.files.len() as u64);
        for file in &self.files {
            enc.write_str(&file.file);
            enc.write_str(&file.path);
            write_strings(enc, &file.module);
            enc.write_varint(file.statics.len() as u64);
            for node in &file.statics {
                node.encode(enc);
            }
            enc.write_varint(file.regional.len() as u64);
            for ((region, arm, item), node) in &file.regional {
                enc.write_varint(u64::from(*region));
                enc.write_varint(u64::from(*arm));
                enc.write_varint(u64::from(*item));
                node.encode(enc);
            }
            enc.write_varint(file.states.len() as u64);
            for state in &file.states {
                enc.write_u64(state.key.0);
                enc.write_u64(state.key.1);
                enc.write_str(&state.name);
                write_option_str(enc, state.type_name.as_deref());
                enc.write_varint(u64::from(state.start.0));
                enc.write_varint(u64::from(state.start.1));
            }
        }
    }
}

impl Decode for SourceTable {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let files = (0..count(dec)?)
            .map(|_| {
                let file = dec.read_str()?.to_owned();
                let path = dec.read_str()?.to_owned();
                let module = read_strings(dec)?;
                let statics = (0..count(dec)?)
                    .map(|_| NodeSource::decode(dec))
                    .collect::<Result<_, _>>()?;
                let regional = (0..count(dec)?)
                    .map(|_| {
                        let at = (read_u32(dec)?, read_u32(dec)?, read_u32(dec)?);
                        Ok((at, NodeSource::decode(dec)?))
                    })
                    .collect::<Result<Vec<_>, DecodeError>>()?;
                if !regional.is_sorted_by(|(a, _), (b, _)| a < b) {
                    return Err(malformed(dec));
                }
                let states = (0..count(dec)?)
                    .map(|_| {
                        Ok(StateSource {
                            key: (dec.read_u64()?, dec.read_u64()?),
                            name: dec.read_str()?.to_owned(),
                            type_name: read_option_str(dec)?,
                            start: (read_u32(dec)?, read_u32(dec)?),
                        })
                    })
                    .collect::<Result<Vec<_>, DecodeError>>()?;
                if !states.is_sorted_by(|a, b| a.key < b.key) {
                    return Err(malformed(dec));
                }
                Ok(FileSources {
                    file,
                    path,
                    module,
                    statics,
                    regional,
                    states,
                })
            })
            .collect::<Result<_, DecodeError>>()?;
        Ok(SourceTable { files })
    }
}

fn read_u32(dec: &mut Decoder<'_>) -> Result<u32, DecodeError> {
    u32::try_from(dec.read_varint()?).map_err(|_| malformed(dec))
}

impl Encode for NodeSource {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_str(&self.type_name);
        write_option_str(enc, self.name.as_deref());
        write_strings(enc, &self.styles);
        write_strings(enc, &self.components);
        enc.write_str(&self.owner);
        let (bytes, start, end) = (self.bytes, self.start, self.end);
        for v in [bytes.0, bytes.1, start.0, start.1, end.0, end.1] {
            enc.write_varint(u64::from(v));
        }
        enc.write_varint(self.bindings.len() as u64);
        for (property, (hi, lo)) in &self.bindings {
            enc.write_str(property);
            enc.write_u64(*hi);
            enc.write_u64(*lo);
        }
    }
}

impl Decode for NodeSource {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(NodeSource {
            type_name: dec.read_str()?.to_owned(),
            name: read_option_str(dec)?,
            styles: read_strings(dec)?,
            components: read_strings(dec)?,
            owner: dec.read_str()?.to_owned(),
            bytes: (read_u32(dec)?, read_u32(dec)?),
            start: (read_u32(dec)?, read_u32(dec)?),
            end: (read_u32(dec)?, read_u32(dec)?),
            bindings: (0..count(dec)?)
                .map(|_| {
                    let property = dec.read_str()?.to_owned();
                    Ok((property, (dec.read_u64()?, dec.read_u64()?)))
                })
                .collect::<Result<_, DecodeError>>()?,
        })
    }
}
