//! The brushes a view's `ShaderView`s paint with.
//!
//! A view names each shader it paints with by its index in the view's shader
//! table. The build packs each as a brush in the language the target's
//! backend reads, beside the program the reference interpreter runs; loading
//! it decodes the [`BrushProgram`] a node store's slots hold. The same bytes
//! load as the same program for as long as anything holds it, whichever view
//! or package names them, so every node painting with one shader in a window
//! shares one slot, and the renderer one pipeline.
//!
//! A shader view's `shader` value is the shader's record of parameters, in
//! declaration order; [`encode_params`] writes it as one instance of the
//! brush, the box left for paint to place.

use std::cell::RefCell;
use std::sync::{Arc, Weak};

use viso_behavior::Value;
use viso_ende::{Decode, DecodeError, content_hash};
use viso_ui::Srgb;
use viso_ui::shader::{BrushLayout, BrushProgram, PackedBrush, ParamKind, ParamValue};

thread_local! {
    /// Every program loaded and still held, by the hash of its bytes.
    static LOADED: RefCell<Vec<(u64, Weak<BrushProgram>)>> = const { RefCell::new(Vec::new()) };
}

/// The program the packed brush `bytes` encodes, shared with every other load
/// of the same bytes while one is held.
///
/// # Errors
///
/// Bytes that are not a packed brush.
pub fn load_shader(bytes: &[u8]) -> Result<Arc<BrushProgram>, DecodeError> {
    let key = content_hash(bytes);
    LOADED.with(|loaded| {
        let mut loaded = loaded.borrow_mut();
        if let Some(program) = loaded
            .iter()
            .find(|(k, _)| *k == key)
            .and_then(|(_, program)| program.upgrade())
        {
            return Ok(program);
        }
        let program = Arc::new(BrushProgram::from(PackedBrush::decode_from_slice(bytes)?));
        loaded.retain(|(_, program)| program.strong_count() > 0);
        loaded.push((key, Arc::downgrade(&program)));
        Ok(program)
    })
}

/// [`load_shader`] for the bytes `view!` embeds.
///
/// # Panics
///
/// If they are not a packed brush. The macro packs them at build time, so
/// this does not happen for bytes it produced.
#[doc(hidden)]
pub fn __embedded_shader(bytes: &'static [u8]) -> Arc<BrushProgram> {
    load_shader(bytes).unwrap_or_else(|error| panic!("an embedded shader does not load: {error}"))
}

/// The parameters `value`, a shader's record of them, give under `layout`, as
/// one encoded instance whose box is zero; `None` for a value of another
/// shape.
pub(crate) fn encode_params(layout: &BrushLayout, value: &Value) -> Option<Arc<[u8]>> {
    let Value::Agg(record) = value else {
        return None;
    };
    if record.fields.len() != layout.params.len() {
        return None;
    }
    let mut values = Vec::with_capacity(layout.params.len());
    for (param, field) in layout.params.iter().zip(record.fields.iter()) {
        values.push(param_value(param.kind, field)?);
    }
    let mut out = Vec::new();
    layout.encode(&values, &mut out).ok()?;
    Some(out.into())
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
