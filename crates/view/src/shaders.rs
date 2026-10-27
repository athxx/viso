//! The brushes a view's `ShaderView`s paint with.
//!
//! A view names each shader it paints with by its index in the view's shader
//! table. The build packs each as a brush in the language the target's
//! backend reads, beside the program the reference interpreter runs; loading
//! it decodes the [`BrushProgram`] a node store's slots hold. The same bytes
//! load as the same program for as long as anything holds it, whichever view
//! or package names them and whichever thread loads them — a build's embedded
//! shaders load on the loop, a patch's on the dev link's reader — so every node
//! painting with one shader in a window shares one slot, and the renderer one
//! pipeline. A package carries no source span, so a reload of a file whose
//! shader did not change loads the program already running.
//!
//! A shader view's `shader` value is the shader's record of parameters, in
//! declaration order, then its images; [`encode_shader`] writes the
//! parameters as one instance of the brush, the box left for paint to place,
//! and names the images' resources.

use std::sync::{Arc, Mutex, PoisonError, Weak};

use viso_behavior::Value;
use viso_ende::{Decode, DecodeError, content_hash};
use viso_ui::AssetBytes;
use viso_ui::Srgb;
use viso_ui::resource::ResourceId;
use viso_ui::shader::{
    BrushLayout, BrushProgram, NativeKernel, PackedBrush, ParamKind, ParamValue,
};

use crate::package::ViewShader;

/// A program loaded and still held: the hash of its bytes, the bytes —
/// borrowed where they are embedded — whether it runs on the CPU as Rust
/// built from it, and it.
type Loaded = (u64, AssetBytes, bool, Weak<BrushProgram>);

/// Every program loaded and still held.
static LOADED: Mutex<Vec<Loaded>> = Mutex::new(Vec::new());

/// The program the packed brush `bytes` encodes, shared with every other load
/// of the same bytes while one is held.
///
/// # Errors
///
/// Bytes that are not a packed brush.
pub fn load_shader(bytes: &[u8]) -> Result<Arc<BrushProgram>, DecodeError> {
    load_kept(bytes, |bytes| AssetBytes::Shared(bytes.into()), None)
}

/// [`load_shader`], keeping the bytes a new program is held by as `keep`
/// says: borrowed when they are `'static`, else copied. The CPU runs the
/// program as `native`, the Rust built from it, when that is given; a
/// program already held without it is not shared then.
pub(crate) fn load_kept<'a>(
    bytes: &'a [u8],
    keep: fn(&'a [u8]) -> AssetBytes,
    native: Option<&'static NativeKernel>,
) -> Result<Arc<BrushProgram>, DecodeError> {
    let key = content_hash(bytes);
    let mut loaded = LOADED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(program) = loaded
        .iter()
        .filter(|(k, held, built, _)| *k == key && **held == *bytes && (*built || native.is_none()))
        .find_map(|(_, _, _, program)| program.upgrade())
    {
        return Ok(program);
    }
    let packed = PackedBrush::decode_from_slice(bytes)?;
    let program = Arc::new(BrushProgram::unpacked(packed, native));
    loaded.retain(|(_, _, _, program)| program.strong_count() > 0);
    loaded.push((key, keep(bytes), native.is_some(), Arc::downgrade(&program)));
    Ok(program)
}

/// [`load_shader`] for each shader of `shaders`, in order.
///
/// # Errors
///
/// The first shader that is not a packed brush.
pub fn load_shaders(shaders: &[ViewShader]) -> Result<Vec<Arc<BrushProgram>>, DecodeError> {
    shaders
        .iter()
        .map(|shader| load_shader(shader.bytes()))
        .collect()
}

/// [`load_shader`] for the bytes `view!` embeds, which the program is held
/// by where they lie, run on the CPU as `native`, the Rust `view!` built
/// from it, where the build has a CPU backend.
///
/// # Panics
///
/// If they are not a packed brush. The macro packs them at build time, so
/// this does not happen for bytes it produced.
#[doc(hidden)]
pub fn __embedded_shader(
    bytes: &'static [u8],
    native: Option<&'static NativeKernel>,
) -> Arc<BrushProgram> {
    load_kept(bytes, AssetBytes::Static, native)
        .unwrap_or_else(|error| panic!("an embedded shader does not load: {error}"))
}

/// What `value`, a shader's record, gives under `layout`: its parameters as
/// one encoded instance whose box is zero, and the resource of each image
/// after them; `None` for a value of another shape.
pub(crate) fn encode_shader(
    layout: &BrushLayout,
    value: &Value,
) -> Option<(Arc<[u8]>, Vec<ResourceId>)> {
    let Value::Agg(record) = value else {
        return None;
    };
    if record.fields.len() != layout.params.len() + layout.textures.len() {
        return None;
    }
    let (params, images) = record.fields.split_at(layout.params.len());
    let mut values = Vec::with_capacity(layout.params.len());
    for (param, field) in layout.params.iter().zip(params) {
        values.push(param_value(param.kind, field)?);
    }
    let mut out = Vec::new();
    layout.encode(&values, &mut out).ok()?;
    let images = images
        .iter()
        .map(crate::values::resource)
        .collect::<Option<Vec<_>>>()?;
    Some((out.into(), images))
}

/// `value` as a parameter of `kind`: `F32` from a float, `I32` and `U32` from
/// an integer in range, the vectors from records of floats, `ColorLinear`
/// from a `Color` (sRGB `0xRRGGBBAA`) in linear light.
fn param_value(kind: ParamKind, value: &Value) -> Option<ParamValue> {
    let lanes = |value: &Value| -> Option<Vec<f32>> {
        let Value::Agg(lanes) = value else {
            return None;
        };
        lanes
            .fields
            .iter()
            .map(|lane| lane.as_float().map(|lane| lane as f32))
            .collect()
    };
    Some(match kind {
        ParamKind::F32 => ParamValue::F32(value.as_float()? as f32),
        ParamKind::I32 => ParamValue::I32(i32::try_from(value.as_int()?).ok()?),
        ParamKind::U32 => ParamValue::U32(u32::try_from(value.as_int()?).ok()?),
        ParamKind::Vec2F32 => ParamValue::Vec2F32(lanes(value)?.try_into().ok()?),
        ParamKind::Vec3F32 => ParamValue::Vec3F32(lanes(value)?.try_into().ok()?),
        ParamKind::ColorLinear => {
            let color = Srgb::from_rgba32(value.as_int()? as u32).into_linear_straight();
            ParamValue::ColorLinear([color.r, color.g, color.b, color.a])
        }
    })
}
