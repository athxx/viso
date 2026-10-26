//! The glTF 2.0 reader: a `.glb` container or `.gltf` JSON, its buffers,
//! accessors, images, materials, meshes, nodes, skins and animations.
//!
//! The JSON is read whole into a value tree (nesting bounded), then each
//! array of the document is turned into the model's types. Every byte read
//! through an accessor is bounds-checked against its buffer view, and every
//! index against what it names.

use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

use viso_ende::JsonValue;
use viso_math::{Mat4, Quat, Vec3, vec3};

use super::geometry;
use super::mips::{self, ImageKind};
use super::{
    AlphaMode, Channel, ChannelPath, Interpolation, ModelAnimation, ModelAsset, ModelImage,
    ModelMaterial, ModelMesh, ModelNode, ModelPrimitive, ModelRig, ModelSkin, ModelTexture,
    TextureRef, Trs, Wrap,
};

/// Why an asset did not decode as a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelError(pub String);

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ModelError {}

fn err<T>(message: impl Into<String>) -> Result<T, ModelError> {
    Err(ModelError(message.into()))
}

/// The extensions a file may require: those whose absence would change what
/// it shows and that this reader honours.
const SUPPORTED: &[&str] = &[
    "KHR_materials_unlit",
    "KHR_materials_emissive_strength",
    "KHR_mesh_quantization",
];

/// The most elements one accessor may hold: past it the file is refused
/// rather than allocated for.
const MAX_ELEMENTS: usize = 1 << 24;

const GLB_MAGIC: u32 = 0x4654_6c67;
const CHUNK_JSON: u32 = 0x4e4f_534a;
const CHUNK_BIN: u32 = 0x004e_4942;

pub(super) fn decode(
    bytes: &[u8],
    resolve: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
) -> Result<ModelAsset, ModelError> {
    let (json, bin) = container(bytes)?;
    let root = JsonValue::parse(json).map_err(|e| ModelError(format!("glTF JSON: {e}")))?;
    let version = root
        .get("asset")
        .and_then(|a| a.get("version"))
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    if !version.starts_with("2.") {
        return err(format!("glTF version `{version}`, not 2.x"));
    }
    for ext in array(&root, "extensionsRequired") {
        let name = ext.as_str().unwrap_or("");
        if !SUPPORTED.contains(&name) {
            return err(format!("the model requires extension `{name}`"));
        }
    }
    let buffers = buffers(&root, bin, resolve)?;
    let doc = Doc {
        root: &root,
        buffers,
        views: views(&root)?,
    };
    let mut images = doc.images(resolve);
    let textures = doc.textures(images.len());
    let materials = doc.materials(textures.len())?;
    // Each image's chain, averaged as its first use reads it: color before a
    // normal map before data.
    let mut kinds: Vec<Option<ImageKind>> = vec![None; images.len()];
    for m in &materials {
        let uses = [
            (m.base_color_texture, ImageKind::Color),
            (m.emissive_texture, ImageKind::Color),
            (m.normal_texture, ImageKind::Normal),
            (m.metallic_roughness_texture, ImageKind::Data),
            (m.occlusion_texture, ImageKind::Data),
        ];
        for (texture, kind) in uses {
            let image = texture
                .and_then(|t| textures.get(t.texture as usize))
                .and_then(|t| t.image);
            if let Some(slot) = image.and_then(|i| kinds.get_mut(i as usize))
                && slot.is_none_or(|k| precedence(kind) < precedence(k))
            {
                *slot = Some(kind);
            }
        }
    }
    for (image, kind) in images.iter_mut().zip(kinds) {
        image.kind = kind.unwrap_or_default();
        image.mips = mips::chain(&image.rgba, image.width, image.height, image.kind);
    }
    let mut meshes = doc.meshes(materials.len())?;
    // A normal map needs tangents: build them where the file has none,
    // along the texture coordinates the map reads.
    for mesh in &mut meshes {
        for p in &mut mesh.primitives {
            let Some(normal) = p
                .material
                .and_then(|m| materials.get(m as usize))
                .and_then(|m| m.normal_texture)
            else {
                continue;
            };
            let uvs = if normal.uv_set == 1 && !p.uvs1.is_empty() {
                &p.uvs1
            } else {
                &p.uvs
            };
            if p.tangents.is_empty() && !uvs.is_empty() {
                p.tangents = geometry::tangents(&p.positions, &p.normals, uvs, &p.indices);
            }
        }
    }
    let (nodes, roots, order) = doc.nodes(meshes.len())?;
    let mut skins = doc.skins(nodes.len())?;
    for (s, skin) in skins.iter_mut().enumerate() {
        skin.reach = reach(skin, s as u32, &nodes, &meshes);
    }
    let animations = doc.animations(nodes.len())?;
    let mesh_bounds = meshes
        .iter()
        .map(|m| {
            m.primitives
                .iter()
                .fold(([f32::MAX; 3], [f32::MIN; 3]), |(lo, hi), p| {
                    (
                        core::array::from_fn(|i| lo[i].min(p.bounds.0[i])),
                        core::array::from_fn(|i| hi[i].max(p.bounds.1[i])),
                    )
                })
        })
        .collect();
    Ok(ModelAsset {
        rig: Arc::new(ModelRig {
            nodes,
            roots,
            order,
            skins,
            animations,
            mesh_bounds,
        }),
        meshes,
        materials,
        textures,
        images,
    })
}

/// Which use of an image its mip chain is built for, lowest first.
fn precedence(kind: ImageKind) -> u8 {
    match kind {
        ImageKind::Color => 0,
        ImageKind::Normal => 1,
        ImageKind::Data => 2,
    }
}

/// The JSON text and the binary chunk of `bytes`.
fn container(bytes: &[u8]) -> Result<(&str, Option<&[u8]>), ModelError> {
    if read_u32(bytes, 0) != Some(GLB_MAGIC) {
        let text = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
        let json =
            std::str::from_utf8(text).map_err(|_| ModelError("glTF JSON is not UTF-8".into()))?;
        return Ok((json, None));
    }
    if read_u32(bytes, 4) != Some(2) {
        return err("a GLB container of a version other than 2");
    }
    let total = (read_u32(bytes, 8).unwrap_or(0) as usize).min(bytes.len());
    let mut at = 12;
    let mut json = None;
    let mut bin = None;
    while at + 8 <= total {
        let len = read_u32(bytes, at).unwrap_or(0) as usize;
        let kind = read_u32(bytes, at + 4).unwrap_or(0);
        let start = at + 8;
        let Some(end) = start.checked_add(len).filter(|&e| e <= total) else {
            return err("a GLB chunk runs past the file");
        };
        let data = &bytes[start..end];
        match kind {
            CHUNK_JSON if json.is_none() => {
                let text = std::str::from_utf8(data)
                    .map_err(|_| ModelError("GLB JSON is not UTF-8".into()))?;
                json = Some(text.trim_end_matches(['\0', ' ']));
            }
            CHUNK_BIN if bin.is_none() => bin = Some(data),
            _ => {}
        }
        at = end.next_multiple_of(4);
    }
    match json {
        Some(json) => Ok((json, bin)),
        None => err("a GLB with no JSON chunk"),
    }
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at + 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// The members of array `key` of `v`, or none.
fn array<'a>(v: &'a JsonValue, key: &str) -> &'a [JsonValue] {
    v.get(key).and_then(JsonValue::as_array).unwrap_or(&[])
}

/// `v` as an index: a whole, non-negative number.
fn as_index(v: &JsonValue) -> Option<usize> {
    let n = v.as_f64()?;
    (n >= 0.0 && n.fract() == 0.0 && n < 4.0e9).then_some(n as usize)
}

/// Member `key` of `v` as an index below `len`: `Ok(None)` when absent, an
/// error when out of range.
fn index_of(v: &JsonValue, key: &str, len: usize, what: &str) -> Result<Option<usize>, ModelError> {
    match v.get(key) {
        None => Ok(None),
        Some(i) => match as_index(i) {
            Some(i) if i < len => Ok(Some(i)),
            _ => err(format!("{what} `{key}` names nothing")),
        },
    }
}

fn number(v: &JsonValue, key: &str, default: f32) -> f32 {
    v.get(key)
        .and_then(JsonValue::as_f64)
        .map_or(default, |n| n as f32)
}

fn floats<const N: usize>(v: &JsonValue, key: &str, default: [f32; N]) -> [f32; N] {
    match v.get(key).and_then(JsonValue::as_array) {
        Some(a) if a.len() == N => {
            core::array::from_fn(|i| a[i].as_f64().map_or(default[i], |n| n as f32))
        }
        _ => default,
    }
}

fn name(v: &JsonValue) -> String {
    v.get("name")
        .and_then(JsonValue::as_str)
        .unwrap_or("")
        .to_owned()
}

/// The bytes of each buffer.
fn buffers<'a>(
    root: &JsonValue,
    bin: Option<&'a [u8]>,
    resolve: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
) -> Result<Vec<Cow<'a, [u8]>>, ModelError> {
    let mut out = Vec::new();
    for (i, buffer) in array(root, "buffers").iter().enumerate() {
        let length = buffer.get("byteLength").and_then(as_index).unwrap_or(0);
        let bytes: Cow<'a, [u8]> = match buffer.get("uri").and_then(JsonValue::as_str) {
            Some(uri) => Cow::Owned(load_uri(uri, resolve)?),
            None if i == 0 => match bin {
                Some(bin) => Cow::Borrowed(bin),
                None => return err("buffer 0 has no URI and the file no binary chunk"),
            },
            None => return err(format!("buffer {i} has no URI")),
        };
        if bytes.len() < length {
            return err(format!(
                "buffer {i} holds {} bytes of the {length} it declares",
                bytes.len()
            ));
        }
        out.push(bytes);
    }
    Ok(out)
}

/// The bytes a URI names: a data URI decoded, or a relative path within the
/// asset's directory read through `resolve`.
fn load_uri(
    uri: &str,
    resolve: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
) -> Result<Vec<u8>, ModelError> {
    if let Some(data) = uri.strip_prefix("data:") {
        let Some((meta, payload)) = data.split_once(',') else {
            return err("a data URI without a comma");
        };
        if !meta.ends_with(";base64") {
            return Ok(percent_decode(payload));
        }
        return base64(payload.as_bytes())
            .ok_or_else(|| ModelError("a data URI's base64 is malformed".into()));
    }
    let path = percent_decode(uri);
    let path =
        String::from_utf8(path).map_err(|_| ModelError(format!("URI `{uri}` is not UTF-8")))?;
    let escapes = path.starts_with('/')
        || path.starts_with('\\')
        || path.contains(':')
        || path.split(['/', '\\']).any(|part| part == "..");
    if escapes {
        return err(format!("URI `{uri}` leaves the asset's directory"));
    }
    resolve(&path).ok_or_else(|| ModelError(format!("`{path}` could not be read")))
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (
                b.get(i + 1).and_then(|&c| hex(c)),
                b.get(i + 2).and_then(|&c| hex(c)),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Standard base64, padding optional, whitespace skipped.
fn base64(input: &[u8]) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in input {
        if c == b'=' {
            break;
        }
        if c.is_ascii_whitespace() {
            continue;
        }
        acc = (acc << 6) | value(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// A buffer view: a range of a buffer, and the stride of its elements.
#[derive(Debug, Clone, Copy)]
struct View {
    buffer: usize,
    offset: usize,
    length: usize,
    stride: Option<usize>,
}

fn views(root: &JsonValue) -> Result<Vec<View>, ModelError> {
    let buffers = array(root, "buffers");
    array(root, "bufferViews")
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let Some(buffer) = v.get("buffer").and_then(as_index) else {
                return err(format!("buffer view {i} names no buffer"));
            };
            let length = v.get("byteLength").and_then(as_index).unwrap_or(0);
            let declared = buffers
                .get(buffer)
                .and_then(|b| b.get("byteLength"))
                .and_then(as_index);
            let offset = v.get("byteOffset").and_then(as_index).unwrap_or(0);
            match declared {
                Some(len) if offset.checked_add(length).is_some_and(|end| end <= len) => {}
                _ => return err(format!("buffer view {i} lies outside its buffer")),
            }
            let stride = v.get("byteStride").and_then(as_index).filter(|&s| s > 0);
            Ok(View {
                buffer,
                offset,
                length,
                stride,
            })
        })
        .collect()
}

/// A component type: its size and how it reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Component {
    I8,
    U8,
    I16,
    U16,
    U32,
    F32,
}

impl Component {
    fn of(code: usize) -> Option<Component> {
        Some(match code {
            5120 => Component::I8,
            5121 => Component::U8,
            5122 => Component::I16,
            5123 => Component::U16,
            5125 => Component::U32,
            5126 => Component::F32,
            _ => return None,
        })
    }

    fn size(self) -> usize {
        match self {
            Component::I8 | Component::U8 => 1,
            Component::I16 | Component::U16 => 2,
            Component::U32 | Component::F32 => 4,
        }
    }

    /// The component at `b`, as a float: normalized to `[0, 1]` or
    /// `[-1, 1]` when `normalized`.
    fn float(self, b: &[u8], normalized: bool) -> f32 {
        let n = normalized;
        match self {
            Component::I8 => {
                let v = f32::from(b[0] as i8);
                if n { (v / 127.0).max(-1.0) } else { v }
            }
            Component::U8 => {
                let v = f32::from(b[0]);
                if n { v / 255.0 } else { v }
            }
            Component::I16 => {
                let v = f32::from(i16::from_le_bytes([b[0], b[1]]));
                if n { (v / 32767.0).max(-1.0) } else { v }
            }
            Component::U16 => {
                let v = f32::from(u16::from_le_bytes([b[0], b[1]]));
                if n { v / 65535.0 } else { v }
            }
            Component::U32 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32,
            Component::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        }
    }

    fn uint(self, b: &[u8]) -> Option<u32> {
        Some(match self {
            Component::U8 => u32::from(b[0]),
            Component::U16 => u32::from(u16::from_le_bytes([b[0], b[1]])),
            Component::U32 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            _ => return None,
        })
    }
}

/// How many components an accessor type has.
fn lanes(kind: &str) -> Option<usize> {
    Some(match kind {
        "SCALAR" => 1,
        "VEC2" => 2,
        "VEC3" => 3,
        "VEC4" => 4,
        "MAT4" => 16,
        _ => return None,
    })
}

struct Doc<'a> {
    root: &'a JsonValue,
    buffers: Vec<Cow<'a, [u8]>>,
    views: Vec<View>,
}

/// An accessor resolved: where its elements lie and how they read.
struct Accessor<'d> {
    bytes: Option<&'d [u8]>,
    stride: usize,
    component: Component,
    normalized: bool,
    lanes: usize,
    count: usize,
}

impl Accessor<'_> {
    fn element(&self, i: usize) -> Option<&[u8]> {
        let bytes = self.bytes?;
        let at = i * self.stride;
        bytes.get(at..at + self.lanes * self.component.size())
    }
}

impl<'a> Doc<'a> {
    /// View `index`'s bytes.
    fn view(&self, index: usize) -> Result<(&[u8], Option<usize>), ModelError> {
        let Some(view) = self.views.get(index) else {
            return err(format!("buffer view {index} does not exist"));
        };
        let Some(buffer) = self.buffers.get(view.buffer) else {
            return err(format!("buffer view {index} names a missing buffer"));
        };
        match buffer.get(view.offset..view.offset + view.length) {
            Some(bytes) => Ok((bytes, view.stride)),
            None => err(format!("buffer view {index} lies outside its buffer")),
        }
    }

    fn accessor(&self, index: usize) -> Result<(Accessor<'_>, &'a JsonValue), ModelError> {
        let Some(a) = array(self.root, "accessors").get(index) else {
            return err(format!("accessor {index} does not exist"));
        };
        let component = a
            .get("componentType")
            .and_then(as_index)
            .and_then(Component::of);
        let lanes = a.get("type").and_then(JsonValue::as_str).and_then(lanes);
        let (Some(component), Some(lanes)) = (component, lanes) else {
            return err(format!("accessor {index} is of an unsupported type"));
        };
        if lanes == 16 && component != Component::F32 {
            return err(format!("accessor {index} is a matrix of integers"));
        }
        let Some(count) = a.get("count").and_then(as_index) else {
            return err(format!("accessor {index} has no count"));
        };
        if count > MAX_ELEMENTS {
            return err(format!("accessor {index} holds {count} elements"));
        }
        let element = lanes * component.size();
        let normalized = a.get("normalized").and_then(JsonValue::as_bool) == Some(true);
        let offset = a.get("byteOffset").and_then(as_index).unwrap_or(0);
        let (bytes, stride) = match a.get("bufferView").and_then(as_index) {
            Some(v) => {
                let (bytes, stride) = self.view(v)?;
                let stride = stride.unwrap_or(element);
                if stride < element {
                    return err(format!("accessor {index}'s stride is under its element"));
                }
                let end = count
                    .checked_sub(1)
                    .map(|last| last * stride + element)
                    .unwrap_or(0);
                let Some(bytes) = offset.checked_add(end).and_then(|e| bytes.get(offset..e)) else {
                    return err(format!("accessor {index} reads past its buffer view"));
                };
                (Some(bytes), stride)
            }
            None => (None, element),
        };
        Ok((
            Accessor {
                bytes,
                stride,
                component,
                normalized,
                lanes,
                count,
            },
            a,
        ))
    }

    /// Accessor `index`'s elements as floats, `want` lanes each (missing
    /// lanes zero, extra ones dropped), sparse substitutions applied.
    fn floats(&self, index: usize, want: usize) -> Result<Vec<f32>, ModelError> {
        let (acc, json) = self.accessor(index)?;
        let mut out = vec![0.0f32; acc.count * want];
        if acc.bytes.is_some() {
            let size = acc.component.size();
            for i in 0..acc.count {
                let e = acc
                    .element(i)
                    .ok_or_else(|| ModelError("accessor read out of bounds".into()))?;
                for l in 0..acc.lanes.min(want) {
                    out[i * want + l] = acc.component.float(&e[l * size..], acc.normalized);
                }
            }
        }
        self.sparse(
            json,
            acc.count,
            |i, e| {
                let size = acc.component.size();
                for l in 0..acc.lanes.min(want) {
                    out[i * want + l] = acc.component.float(&e[l * size..], acc.normalized);
                }
            },
            acc.lanes * acc.component.size(),
        )?;
        Ok(out)
    }

    /// Accessor `index`'s elements as unsigned integers, `want` lanes each.
    fn uints(&self, index: usize, want: usize) -> Result<Vec<u32>, ModelError> {
        let (acc, json) = self.accessor(index)?;
        let size = acc.component.size();
        if acc.component.uint(&[0; 4]).is_none() {
            return err(format!("accessor {index} is not of unsigned integers"));
        }
        let mut out = vec![0u32; acc.count * want];
        if acc.bytes.is_some() {
            for i in 0..acc.count {
                let e = acc
                    .element(i)
                    .ok_or_else(|| ModelError("accessor read out of bounds".into()))?;
                for l in 0..acc.lanes.min(want) {
                    out[i * want + l] = acc.component.uint(&e[l * size..]).unwrap_or(0);
                }
            }
        }
        self.sparse(
            json,
            acc.count,
            |i, e| {
                for l in 0..acc.lanes.min(want) {
                    out[i * want + l] = acc.component.uint(&e[l * size..]).unwrap_or(0);
                }
            },
            acc.lanes * size,
        )?;
        Ok(out)
    }

    /// Calls `set` with each sparse substitution of accessor `json`: the
    /// element it replaces and its `element`-byte value.
    fn sparse(
        &self,
        json: &JsonValue,
        count: usize,
        mut set: impl FnMut(usize, &[u8]),
        element: usize,
    ) -> Result<(), ModelError> {
        let Some(sparse) = json.get("sparse") else {
            return Ok(());
        };
        let n = sparse.get("count").and_then(as_index).unwrap_or(0);
        let (Some(indices), Some(values)) = (sparse.get("indices"), sparse.get("values")) else {
            return err("a sparse accessor without indices or values");
        };
        let component = indices
            .get("componentType")
            .and_then(as_index)
            .and_then(Component::of)
            .filter(|c| c.uint(&[0; 4]).is_some());
        let Some(component) = component else {
            return err("sparse indices are not unsigned integers");
        };
        let view = |v: &JsonValue, len: usize| -> Result<&[u8], ModelError> {
            let Some(view) = v.get("bufferView").and_then(as_index) else {
                return err("sparse data names no buffer view");
            };
            let (bytes, _) = self.view(view)?;
            let offset = v.get("byteOffset").and_then(as_index).unwrap_or(0);
            offset
                .checked_add(len)
                .and_then(|end| bytes.get(offset..end))
                .ok_or_else(|| ModelError("sparse data reads past its view".into()))
        };
        let size = component.size();
        let idx = view(indices, n * size)?;
        let vals = view(values, n * element)?;
        for k in 0..n {
            let i = component.uint(&idx[k * size..]).unwrap_or(u32::MAX) as usize;
            if i >= count {
                return err("a sparse index past its accessor");
            }
            set(i, &vals[k * element..(k + 1) * element]);
        }
        Ok(())
    }

    fn images(&self, resolve: &mut dyn FnMut(&str) -> Option<Vec<u8>>) -> Vec<ModelImage> {
        let decode = |bytes: &[u8]| {
            viso_image::raster::decode_straight(bytes)
                .ok()
                .map(|image| ModelImage {
                    width: image.width,
                    height: image.height,
                    rgba: image.rgba,
                    kind: ImageKind::Color,
                    mips: Vec::new(),
                })
        };
        array(self.root, "images")
            .iter()
            .map(|image| {
                let bytes = if let Some(uri) = image.get("uri").and_then(JsonValue::as_str) {
                    load_uri(uri, resolve).ok().map(Cow::Owned)
                } else {
                    image
                        .get("bufferView")
                        .and_then(as_index)
                        .and_then(|v| self.view(v).ok())
                        .map(|(b, _)| Cow::Borrowed(b))
                };
                bytes.and_then(|b| decode(&b))
            })
            // A picture that does not decode leaves its textures unsampled
            // rather than failing the model.
            .map(|image| {
                image.unwrap_or(ModelImage {
                    width: 0,
                    height: 0,
                    rgba: Vec::new(),
                    kind: ImageKind::Color,
                    mips: Vec::new(),
                })
            })
            .collect()
    }

    fn textures(&self, images: usize) -> Vec<ModelTexture> {
        let samplers = array(self.root, "samplers");
        array(self.root, "textures")
            .iter()
            .map(|t| {
                let webp = t
                    .get("extensions")
                    .and_then(|e| e.get("EXT_texture_webp"))
                    .and_then(|e| e.get("source"));
                let image = webp
                    .or_else(|| t.get("source"))
                    .and_then(as_index)
                    .filter(|&i| i < images)
                    .map(|i| i as u32);
                let sampler = t
                    .get("sampler")
                    .and_then(as_index)
                    .and_then(|s| samplers.get(s));
                let wrap = |key| match sampler.and_then(|s| s.get(key)).and_then(as_index) {
                    Some(33071) => Wrap::Clamp,
                    Some(33648) => Wrap::Mirror,
                    _ => Wrap::Repeat,
                };
                let (s, t) = (wrap("wrapS"), wrap("wrapT"));
                let nearest =
                    sampler.and_then(|s| s.get("magFilter")).and_then(as_index) == Some(9728);
                ModelTexture {
                    image,
                    // One wrap serves both axes: the stricter of the two.
                    wrap: if s == t { s } else { Wrap::Clamp },
                    nearest,
                }
            })
            .collect()
    }

    fn materials(&self, textures: usize) -> Result<Vec<ModelMaterial>, ModelError> {
        let tex = |v: Option<&JsonValue>| -> Option<TextureRef> {
            let v = v?;
            let texture = v
                .get("index")
                .and_then(as_index)
                .filter(|&i| i < textures)?;
            Some(TextureRef {
                texture: texture as u32,
                uv_set: v.get("texCoord").and_then(as_index).unwrap_or(0) as u32,
            })
        };
        array(self.root, "materials")
            .iter()
            .map(|m| {
                let pbr = m.get("pbrMetallicRoughness");
                let none = JsonValue::Null;
                let p = pbr.unwrap_or(&none);
                let ext = m.get("extensions");
                let strength = ext
                    .and_then(|e| e.get("KHR_materials_emissive_strength"))
                    .map_or(1.0, |e| number(e, "emissiveStrength", 1.0));
                let emissive = floats(m, "emissiveFactor", [0.0; 3]).map(|c| c * strength);
                let alpha = match m.get("alphaMode").and_then(JsonValue::as_str) {
                    Some("MASK") => AlphaMode::Mask(number(m, "alphaCutoff", 0.5)),
                    Some("BLEND") => AlphaMode::Blend,
                    _ => AlphaMode::Opaque,
                };
                let normal = m.get("normalTexture");
                let occlusion = m.get("occlusionTexture");
                Ok(ModelMaterial {
                    name: name(m),
                    base_color: floats(p, "baseColorFactor", [1.0; 4]),
                    base_color_texture: tex(p.get("baseColorTexture")),
                    metallic: number(p, "metallicFactor", 1.0).clamp(0.0, 1.0),
                    roughness: number(p, "roughnessFactor", 1.0).clamp(0.0, 1.0),
                    metallic_roughness_texture: tex(p.get("metallicRoughnessTexture")),
                    normal_texture: tex(normal),
                    normal_scale: normal.map_or(1.0, |n| number(n, "scale", 1.0)),
                    occlusion_texture: tex(occlusion),
                    occlusion_strength: occlusion
                        .map_or(1.0, |o| number(o, "strength", 1.0))
                        .clamp(0.0, 1.0),
                    emissive,
                    emissive_texture: tex(m.get("emissiveTexture")),
                    alpha,
                    double_sided: m.get("doubleSided").and_then(JsonValue::as_bool) == Some(true),
                    unlit: ext.and_then(|e| e.get("KHR_materials_unlit")).is_some(),
                })
            })
            .collect()
    }

    fn meshes(&self, materials: usize) -> Result<Vec<ModelMesh>, ModelError> {
        array(self.root, "meshes")
            .iter()
            .enumerate()
            .map(|(m, mesh)| {
                let mut primitives = Vec::new();
                for p in array(mesh, "primitives") {
                    if let Some(p) = self
                        .primitive(p, materials)
                        .map_err(|e| ModelError(format!("mesh {m}: {e}")))?
                    {
                        primitives.push(p);
                    }
                }
                Ok(ModelMesh {
                    name: name(mesh),
                    primitives,
                })
            })
            .collect()
    }

    /// One primitive, or `None` for one that draws no triangles.
    fn primitive(
        &self,
        p: &JsonValue,
        materials: usize,
    ) -> Result<Option<ModelPrimitive>, ModelError> {
        let mode = p.get("mode").and_then(as_index).unwrap_or(4);
        if !matches!(mode, 4..=6) {
            return Ok(None);
        }
        let Some(attrs) = p.get("attributes") else {
            return Ok(None);
        };
        let attr = |name: &str| attrs.get(name).and_then(as_index);
        let Some(position) = attr("POSITION") else {
            return Ok(None);
        };
        let positions = self.floats(position, 3)?;
        let n = positions.len() / 3;
        if n == 0 {
            return Ok(None);
        }
        let chunk3 = |v: Vec<f32>| -> Vec<[f32; 3]> {
            v.as_chunks::<3>()
                .0
                .iter()
                .map(|c| [c[0], c[1], c[2]])
                .collect()
        };
        let sized = |what: &str, len: usize| -> Result<(), ModelError> {
            if len == n {
                Ok(())
            } else {
                err(format!("{what} has {len} elements for {n} vertices"))
            }
        };
        let positions = chunk3(positions);
        let mut normals = match attr("NORMAL") {
            Some(a) => {
                let v = chunk3(self.floats(a, 3)?);
                sized("NORMAL", v.len())?;
                v.into_iter().map(geometry::unit).collect()
            }
            None => Vec::new(),
        };
        let texcoords = |name: &str| -> Result<Vec<[f32; 2]>, ModelError> {
            let Some(a) = attr(name) else {
                return Ok(Vec::new());
            };
            let v: Vec<[f32; 2]> = self
                .floats(a, 2)?
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| [c[0], c[1]])
                .collect();
            sized(name, v.len())?;
            Ok(v)
        };
        let uvs = texcoords("TEXCOORD_0")?;
        let uvs1 = texcoords("TEXCOORD_1")?;
        let mut tangents = match attr("TANGENT") {
            Some(a) => {
                let v: Vec<[f32; 4]> = self
                    .floats(a, 4)?
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| {
                        let t = geometry::unit([c[0], c[1], c[2]]);
                        [t[0], t[1], t[2], if c[3] < 0.0 { -1.0 } else { 1.0 }]
                    })
                    .collect();
                sized("TANGENT", v.len())?;
                v
            }
            None => Vec::new(),
        };
        let colors = match attr("COLOR_0") {
            Some(a) => {
                let (acc, _) = self.accessor(a)?;
                let rgb = acc.lanes == 3;
                let v: Vec<[f32; 4]> = self
                    .floats(a, 4)?
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| [c[0], c[1], c[2], if rgb { 1.0 } else { c[3] }])
                    .collect();
                sized("COLOR_0", v.len())?;
                v
            }
            None => Vec::new(),
        };
        let (mut joints, mut weights) = match (attr("JOINTS_0"), attr("WEIGHTS_0")) {
            (Some(j), Some(w)) => {
                let j: Vec<[u16; 4]> = self
                    .uints(j, 4)?
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| [c[0], c[1], c[2], c[3]].map(|i| i.min(u32::from(u16::MAX)) as u16))
                    .collect();
                let w: Vec<[f32; 4]> = self
                    .floats(w, 4)?
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| {
                        let c = [c[0].max(0.0), c[1].max(0.0), c[2].max(0.0), c[3].max(0.0)];
                        let sum: f32 = c.iter().sum();
                        if sum > 0.0 {
                            c.map(|x| x / sum)
                        } else {
                            [1.0, 0.0, 0.0, 0.0]
                        }
                    })
                    .collect();
                sized("JOINTS_0", j.len())?;
                sized("WEIGHTS_0", w.len())?;
                (j, w)
            }
            _ => (Vec::new(), Vec::new()),
        };
        let mut indices = match p.get("indices").and_then(as_index) {
            Some(a) => self.uints(a, 1)?,
            None => (0..n as u32).collect(),
        };
        if let Some(bad) = indices.iter().find(|&&i| i as usize >= n) {
            return err(format!("index {bad} past its {n} vertices"));
        }
        indices = match mode {
            5 => geometry::strip(&indices),
            6 => geometry::fan(&indices),
            _ => {
                indices.truncate(indices.len() / 3 * 3);
                indices
            }
        };
        let mut positions = positions;
        let mut uvs = uvs;
        let mut uvs1 = uvs1;
        let mut colors = colors;
        if normals.is_empty() {
            // Flat shading, as the format asks of a mesh without normals: each
            // triangle gets vertices of its own.
            let flat = geometry::Flat::of(&indices);
            positions = flat.gather(&positions);
            uvs = flat.gather(&uvs);
            uvs1 = flat.gather(&uvs1);
            colors = flat.gather(&colors);
            tangents = flat.gather(&tangents);
            joints = flat.gather(&joints);
            weights = flat.gather(&weights);
            indices = (0..positions.len() as u32).collect();
            normals = geometry::flat_normals(&positions);
        }
        let material = match p.get("material") {
            None => None,
            Some(m) => match as_index(m) {
                Some(m) if m < materials => Some(m as u32),
                _ => return err("a primitive names a missing material"),
            },
        };
        let bounds = positions
            .iter()
            .fold(([f32::MAX; 3], [f32::MIN; 3]), |(lo, hi), p| {
                (
                    core::array::from_fn(|i| lo[i].min(p[i])),
                    core::array::from_fn(|i| hi[i].max(p[i])),
                )
            });
        Ok(Some(ModelPrimitive {
            positions,
            normals,
            uvs,
            uvs1,
            tangents,
            colors,
            joints,
            weights,
            indices,
            material,
            bounds,
        }))
    }

    /// The nodes, the default scene's roots, and its nodes parents first.
    #[allow(clippy::type_complexity)]
    fn nodes(&self, meshes: usize) -> Result<(Vec<ModelNode>, Vec<u32>, Vec<u32>), ModelError> {
        let json = array(self.root, "nodes");
        let count = json.len();
        let skins = array(self.root, "skins").len();
        let mut nodes = Vec::with_capacity(count);
        for (i, n) in json.iter().enumerate() {
            let rest = match n.get("matrix").and_then(JsonValue::as_array) {
                Some(m) if m.len() == 16 => {
                    let v: [f32; 16] =
                        core::array::from_fn(|k| m[k].as_f64().unwrap_or(0.0) as f32);
                    decompose(&Mat4 { v })
                }
                _ => {
                    let t = floats(n, "translation", [0.0; 3]);
                    let r = floats(n, "rotation", [0.0, 0.0, 0.0, 1.0]);
                    let s = floats(n, "scale", [1.0; 3]);
                    Trs {
                        translation: vec3(t[0], t[1], t[2]),
                        rotation: Quat::new(r[0], r[1], r[2], r[3]).normalize(),
                        scale: vec3(s[0], s[1], s[2]),
                    }
                }
            };
            let mut children = Vec::new();
            for c in array(n, "children") {
                match as_index(c) {
                    Some(c) if c < count && c != i => children.push(c as u32),
                    _ => return err(format!("node {i} has a child that is not a node")),
                }
            }
            nodes.push(ModelNode {
                name: name(n),
                parent: None,
                children,
                rest,
                mesh: index_of(n, "mesh", meshes, "node")?.map(|m| m as u32),
                skin: index_of(n, "skin", skins, "node")?.map(|s| s as u32),
            });
        }
        for i in 0..count {
            for c in nodes[i].children.clone() {
                let child = &mut nodes[c as usize];
                if child.parent.is_some() {
                    return err(format!("node {c} has two parents"));
                }
                child.parent = Some(i as u32);
            }
        }
        let scenes = array(self.root, "scenes");
        let scene = self
            .root
            .get("scene")
            .and_then(as_index)
            .and_then(|s| scenes.get(s))
            .or_else(|| scenes.first());
        let roots: Vec<u32> = match scene {
            Some(s) => {
                let mut roots = Vec::new();
                for r in array(s, "nodes") {
                    match as_index(r) {
                        Some(r) if r < count => roots.push(r as u32),
                        _ => return err("a scene names a missing node"),
                    }
                }
                roots
            }
            None => (0..count as u32)
                .filter(|&n| nodes[n as usize].parent.is_none())
                .collect(),
        };
        let mut seen = vec![false; count];
        let mut order = Vec::with_capacity(count);
        let mut stack: Vec<u32> = roots.iter().rev().copied().collect();
        while let Some(n) = stack.pop() {
            if std::mem::replace(&mut seen[n as usize], true) {
                return err(format!("node {n} is reached twice: a cycle"));
            }
            order.push(n);
            stack.extend(nodes[n as usize].children.iter().rev());
        }
        // A scene root drawn as a root even if it has a parent elsewhere.
        for &r in &roots {
            if nodes[r as usize].parent.is_some_and(|p| !seen[p as usize]) {
                nodes[r as usize].parent = None;
            }
        }
        Ok((nodes, roots, order))
    }

    fn skins(&self, nodes: usize) -> Result<Vec<ModelSkin>, ModelError> {
        array(self.root, "skins")
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let mut joints = Vec::new();
                for j in array(s, "joints") {
                    match as_index(j) {
                        Some(j) if j < nodes => joints.push(j as u32),
                        _ => return err(format!("skin {i} names a joint that is not a node")),
                    }
                }
                if joints.len() > usize::from(u16::MAX) {
                    return err(format!("skin {i} has {} joints", joints.len()));
                }
                let inverse_bind = match s.get("inverseBindMatrices").and_then(as_index) {
                    Some(a) => {
                        let m = self.floats(a, 16)?;
                        if m.len() / 16 < joints.len() {
                            return err(format!("skin {i} has too few inverse bind matrices"));
                        }
                        m.as_chunks::<16>()
                            .0
                            .iter()
                            .take(joints.len())
                            .map(|&v| Mat4 { v })
                            .collect()
                    }
                    None => vec![Mat4::IDENTITY; joints.len()],
                };
                Ok(ModelSkin {
                    name: name(s),
                    joints,
                    inverse_bind,
                    reach: Vec::new(),
                })
            })
            .collect()
    }

    fn animations(&self, nodes: usize) -> Result<Vec<ModelAnimation>, ModelError> {
        array(self.root, "animations")
            .iter()
            .enumerate()
            .map(|(a, anim)| {
                let samplers = array(anim, "samplers");
                let mut channels = Vec::new();
                let mut duration = 0.0f32;
                for c in array(anim, "channels") {
                    let target = c.get("target");
                    let node = target.and_then(|t| t.get("node")).and_then(as_index);
                    let path = match target
                        .and_then(|t| t.get("path"))
                        .and_then(JsonValue::as_str)
                    {
                        Some("translation") => ChannelPath::Translation,
                        Some("rotation") => ChannelPath::Rotation,
                        Some("scale") => ChannelPath::Scale,
                        // Morph weights and extension paths are not animated.
                        _ => continue,
                    };
                    let Some(node) = node.filter(|&n| n < nodes) else {
                        continue;
                    };
                    let Some(sampler) = c
                        .get("sampler")
                        .and_then(as_index)
                        .and_then(|s| samplers.get(s))
                    else {
                        return err(format!("animation {a} has a channel without a sampler"));
                    };
                    let interpolation =
                        match sampler.get("interpolation").and_then(JsonValue::as_str) {
                            Some("STEP") => Interpolation::Step,
                            Some("CUBICSPLINE") => Interpolation::CubicSpline,
                            _ => Interpolation::Linear,
                        };
                    let (Some(input), Some(output)) = (
                        sampler.get("input").and_then(as_index),
                        sampler.get("output").and_then(as_index),
                    ) else {
                        return err(format!("animation {a} has a sampler without keys"));
                    };
                    let times = self.floats(input, 1)?;
                    let lanes = if path == ChannelPath::Rotation { 4 } else { 3 };
                    let values = self.floats(output, lanes)?;
                    let per_key = if interpolation == Interpolation::CubicSpline {
                        3
                    } else {
                        1
                    };
                    if times.is_empty() || values.len() < times.len() * per_key * lanes {
                        return err(format!("animation {a} has fewer values than keys"));
                    }
                    if times.windows(2).any(|w| w[1] < w[0]) || times.iter().any(|t| !t.is_finite())
                    {
                        return err(format!("animation {a}'s key times do not ascend"));
                    }
                    duration = duration.max(*times.last().unwrap_or(&0.0));
                    channels.push(Channel {
                        node: node as u32,
                        path,
                        interpolation,
                        times,
                        values,
                    });
                }
                Ok(ModelAnimation {
                    name: name(anim),
                    channels,
                    duration,
                })
            })
            .collect()
    }
}

/// `m`, an affine without shear, as translation, rotation and scale.
fn decompose(m: &Mat4) -> Trs {
    let v = &m.v;
    let col = |c: usize| vec3(v[c * 4], v[c * 4 + 1], v[c * 4 + 2]);
    let (c0, c1, c2) = (col(0), col(1), col(2));
    let mut sx = c0.length();
    let (sy, sz) = (c1.length(), c2.length());
    if c0.cross(c1).dot(c2) < 0.0 {
        sx = -sx;
    }
    let unit = |c: Vec3, s: f32| if s.abs() > 1e-12 { c * (1.0 / s) } else { c };
    let (r0, r1, r2) = (unit(c0, sx), unit(c1, sy), unit(c2, sz));
    // The rotation matrix's columns r0 r1 r2 as a quaternion.
    let (m00, m11, m22) = (r0.x, r1.y, r2.z);
    let trace = m00 + m11 + m22;
    let q = if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0;
        Quat::new(
            (r1.z - r2.y) / s,
            (r2.x - r0.z) / s,
            (r0.y - r1.x) / s,
            0.25 * s,
        )
    } else if m00 > m11 && m00 > m22 {
        let s = (1.0 + m00 - m11 - m22).sqrt() * 2.0;
        Quat::new(
            0.25 * s,
            (r1.x + r0.y) / s,
            (r2.x + r0.z) / s,
            (r1.z - r2.y) / s,
        )
    } else if m11 > m22 {
        let s = (1.0 + m11 - m00 - m22).sqrt() * 2.0;
        Quat::new(
            (r1.x + r0.y) / s,
            0.25 * s,
            (r2.y + r1.z) / s,
            (r2.x - r0.z) / s,
        )
    } else {
        let s = (1.0 + m22 - m00 - m11).sqrt() * 2.0;
        Quat::new(
            (r2.x + r0.z) / s,
            (r2.y + r1.z) / s,
            0.25 * s,
            (r0.y - r1.x) / s,
        )
    };
    Trs {
        translation: vec3(v[12], v[13], v[14]),
        rotation: q.normalize(),
        scale: vec3(sx, sy, sz),
    }
}

/// Each joint's reach: the sphere, in the joint's space, around the vertices
/// it moves in every primitive drawn with skin `skin`.
fn reach(
    skin: &ModelSkin,
    index: u32,
    nodes: &[ModelNode],
    meshes: &[ModelMesh],
) -> Vec<([f32; 3], f32)> {
    let mut lo = vec![[f32::MAX; 3]; skin.joints.len()];
    let mut hi = vec![[f32::MIN; 3]; skin.joints.len()];
    for node in nodes.iter().filter(|n| n.skin == Some(index)) {
        let Some(mesh) = node.mesh.and_then(|m| meshes.get(m as usize)) else {
            continue;
        };
        for p in mesh.primitives.iter().filter(|p| p.skinned()) {
            for ((pos, joints), weights) in p.positions.iter().zip(&p.joints).zip(&p.weights) {
                for (&j, &w) in joints.iter().zip(weights) {
                    let j = j as usize;
                    if w <= 0.0 || j >= skin.joints.len() {
                        continue;
                    }
                    let q = skin.inverse_bind[j].transform_point(vec3(pos[0], pos[1], pos[2]));
                    let q = [q.x, q.y, q.z];
                    for i in 0..3 {
                        lo[j][i] = lo[j][i].min(q[i]);
                        hi[j][i] = hi[j][i].max(q[i]);
                    }
                }
            }
        }
    }
    lo.iter()
        .zip(&hi)
        .map(|(lo, hi)| {
            if lo[0] > hi[0] {
                return ([0.0; 3], 0.0);
            }
            let centre: [f32; 3] = core::array::from_fn(|i| (lo[i] + hi[i]) * 0.5);
            let half: [f32; 3] = core::array::from_fn(|i| (hi[i] - lo[i]) * 0.5);
            (
                centre,
                (half[0] * half[0] + half[1] * half[1] + half[2] * half[2]).sqrt(),
            )
        })
        .collect()
}
