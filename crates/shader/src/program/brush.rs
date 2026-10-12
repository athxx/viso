//! The brush profile: a program that paints a node's box.
//!
//! The renderer owns three members, which every brush declares:
//!
//! - `uniform viewport_size: Vec2F32` — the target's size in pixels, which
//!   `to_clip` maps a position against;
//! - `instance rect_pos: Vec2F32` and `instance rect_size: Vec2F32` — the
//!   box, in target pixels from the target's top-left corner.
//!
//! Every other instance member is a parameter, of a kind a host value
//! converts into ([`ParamKind`]). A brush declares no other uniform, since
//! nothing on the host would write it, and binds no texture or sampler.
//!
//! [`BrushLayout`] is what a draw is written from: the instance stride, where
//! the box goes, and where each parameter goes. A node's parameters encode
//! once into an instance of their own ([`BrushLayout::encode`]), the box left
//! zero; a parameter change rewrites its bytes in place
//! ([`BrushLayout::set`]); a draw copies the instance and places the box in
//! it ([`BrushLayout::place`]).

use viso_gpu::ProgramAbi;

use super::layout::{FieldDescriptor, ShaderInterface};
use super::{EncodeError, Program, ProgramError, Span, Ty};

/// The uniform the renderer writes the target size into.
pub const VIEWPORT_SIZE: &str = "viewport_size";
/// The instance member the renderer writes the box's position into.
pub const RECT_POS: &str = "rect_pos";
/// The instance member the renderer writes the box's size into.
pub const RECT_SIZE: &str = "rect_size";

/// A program breaks the brush profile.
const PROFILE: &str = "E8107";

/// The type of a brush parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParamKind {
    F32,
    I32,
    U32,
    Vec2F32,
    Vec3F32,
    ColorLinear,
}

impl ParamKind {
    /// The kind of an instance member of `ty`, when a host value converts
    /// into it.
    pub const fn of(ty: Ty) -> Option<ParamKind> {
        Some(match ty {
            Ty::F32 => ParamKind::F32,
            Ty::I32 => ParamKind::I32,
            Ty::U32 => ParamKind::U32,
            Ty::VEC2 => ParamKind::Vec2F32,
            Ty::VEC3 => ParamKind::Vec3F32,
            Ty::Color => ParamKind::ColorLinear,
            _ => return None,
        })
    }

    pub const fn ty(self) -> Ty {
        match self {
            ParamKind::F32 => Ty::F32,
            ParamKind::I32 => Ty::I32,
            ParamKind::U32 => Ty::U32,
            ParamKind::Vec2F32 => Ty::VEC2,
            ParamKind::Vec3F32 => Ty::VEC3,
            ParamKind::ColorLinear => Ty::Color,
        }
    }

    /// Its source spelling.
    pub const fn name(self) -> &'static str {
        match self {
            ParamKind::F32 => "F32",
            ParamKind::I32 => "I32",
            ParamKind::U32 => "U32",
            ParamKind::Vec2F32 => "Vec2F32",
            ParamKind::Vec3F32 => "Vec3F32",
            ParamKind::ColorLinear => "ColorLinear",
        }
    }
}

/// A brush parameter's value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParamValue {
    F32(f32),
    I32(i32),
    U32(u32),
    Vec2F32([f32; 2]),
    Vec3F32([f32; 3]),
    /// Linear-light RGBA.
    ColorLinear([f32; 4]),
}

impl ParamValue {
    pub const fn kind(self) -> ParamKind {
        match self {
            ParamValue::F32(_) => ParamKind::F32,
            ParamValue::I32(_) => ParamKind::I32,
            ParamValue::U32(_) => ParamKind::U32,
            ParamValue::Vec2F32(_) => ParamKind::Vec2F32,
            ParamValue::Vec3F32(_) => ParamKind::Vec3F32,
            ParamValue::ColorLinear(_) => ParamKind::ColorLinear,
        }
    }

    /// The zero of `kind`, what an unset parameter holds.
    pub const fn zero(kind: ParamKind) -> ParamValue {
        match kind {
            ParamKind::F32 => ParamValue::F32(0.0),
            ParamKind::I32 => ParamValue::I32(0),
            ParamKind::U32 => ParamValue::U32(0),
            ParamKind::Vec2F32 => ParamValue::Vec2F32([0.0; 2]),
            ParamKind::Vec3F32 => ParamValue::Vec3F32([0.0; 3]),
            ParamKind::ColorLinear => ParamValue::ColorLinear([0.0; 4]),
        }
    }

    /// Writes its lanes from the start of `out`.
    fn write(self, out: &mut [u8]) {
        let mut put = |lane: usize, bits: u32| {
            out[4 * lane..4 * lane + 4].copy_from_slice(&bits.to_le_bytes());
        };
        match self {
            ParamValue::F32(v) => put(0, v.to_bits()),
            ParamValue::I32(v) => put(0, v.cast_unsigned()),
            ParamValue::U32(v) => put(0, v),
            ParamValue::Vec2F32(v) => v.iter().enumerate().for_each(|(i, v)| put(i, v.to_bits())),
            ParamValue::Vec3F32(v) => v.iter().enumerate().for_each(|(i, v)| put(i, v.to_bits())),
            ParamValue::ColorLinear(v) => {
                v.iter().enumerate().for_each(|(i, v)| put(i, v.to_bits()));
            }
        }
    }
}

/// A brush parameter, laid out.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BrushParam {
    pub name: Box<str>,
    pub kind: ParamKind,
    /// Its byte offset in the instance.
    pub offset: u32,
    /// The instance leaf's [`FieldDescriptor::stable_field_id`].
    pub stable_field_id: u64,
    pub source_span: Span,
}

/// Where a brush draw's bytes go.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BrushLayout {
    /// Bytes one instance takes.
    pub stride: u32,
    /// The byte offsets of `rect_pos` and `rect_size` in the instance.
    pub rect_pos: u32,
    pub rect_size: u32,
    /// Bytes the uniform block takes.
    pub uniform_size: u32,
    /// The byte offset of `viewport_size` in the uniform block.
    pub viewport_size: u32,
    /// The parameters, in declaration order.
    pub params: Vec<BrushParam>,
}

impl BrushLayout {
    /// The layout of `program`, laid out as `interface`, as a brush.
    ///
    /// # Errors
    ///
    /// Each way it breaks the profile, `E8107`: a reserved member missing or
    /// of another type, a uniform the renderer does not write, a parameter of
    /// a type no host value converts into, a texture or sampler.
    pub fn new(
        program: &Program,
        interface: &ShaderInterface,
    ) -> Result<BrushLayout, Vec<ProgramError>> {
        let mut errors = Vec::new();
        let mut error = |span: Span, message: String| {
            errors.push(ProgramError::new(PROFILE, span, message));
        };
        let leaf = |fields: &[FieldDescriptor], member: usize| -> Option<u32> {
            fields
                .iter()
                .find(|f| f.member as usize == member)
                .map(|f| f.offset)
        };

        let mut viewport_size = None;
        for (i, u) in program.uniforms.iter().enumerate() {
            if &*u.name != VIEWPORT_SIZE {
                error(
                    u.span,
                    format!(
                        "a brush declares no uniform but `{VIEWPORT_SIZE}`, which the renderer \
                         writes; make `{}` an `instance` parameter",
                        u.name
                    ),
                );
            } else if u.ty != Ty::VEC2 {
                error(
                    u.span,
                    format!(
                        "`{VIEWPORT_SIZE}` is the target's size in pixels, a `Vec2F32`, not a \
                         `{}`",
                        program.spell(u.ty)
                    ),
                );
            } else {
                viewport_size = leaf(&interface.uniforms.fields, i);
            }
        }

        let (mut rect_pos, mut rect_size) = (None, None);
        let mut params = Vec::new();
        for (i, m) in program.instance.iter().enumerate() {
            let reserved = match &*m.name {
                RECT_POS => Some((&mut rect_pos, "position")),
                RECT_SIZE => Some((&mut rect_size, "size")),
                _ => None,
            };
            if let Some((slot, what)) = reserved {
                if m.ty == Ty::VEC2 {
                    *slot = leaf(&interface.instance.fields, i);
                } else {
                    error(
                        m.span,
                        format!(
                            "`{}` is the box's {what} in pixels, a `Vec2F32`, not a `{}`",
                            m.name,
                            program.spell(m.ty)
                        ),
                    );
                }
                continue;
            }
            let Some(kind) = ParamKind::of(m.ty) else {
                error(
                    m.span,
                    format!(
                        "no host value converts into `{}`, a `{}`; a brush parameter is `F32`, \
                         `I32`, `U32`, `Vec2F32`, `Vec3F32` or `ColorLinear`",
                        m.name,
                        program.spell(m.ty)
                    ),
                );
                continue;
            };
            if let Some(f) = interface
                .instance
                .fields
                .iter()
                .find(|f| f.member as usize == i)
            {
                params.push(BrushParam {
                    name: m.name.clone(),
                    kind,
                    offset: f.offset,
                    stable_field_id: f.stable_field_id,
                    source_span: m.span,
                });
            }
        }

        for b in program.textures.iter().chain(&program.samplers) {
            error(
                b.span,
                format!(
                    "a brush binds no texture or sampler; `{}` has no source",
                    b.name
                ),
            );
        }
        let declares =
            |members: &[super::Binding], name: &str| members.iter().any(|m| &*m.name == name);
        let reserved = [
            (
                "uniform",
                VIEWPORT_SIZE,
                declares(&program.uniforms, VIEWPORT_SIZE),
            ),
            ("instance", RECT_POS, declares(&program.instance, RECT_POS)),
            (
                "instance",
                RECT_SIZE,
                declares(&program.instance, RECT_SIZE),
            ),
        ];
        for (word, name, declared) in reserved {
            if !declared {
                error(
                    Span::default(),
                    format!(
                        "a brush declares `{word} {name}: Vec2F32;`, which the renderer writes"
                    ),
                );
            }
        }

        match (errors.is_empty(), viewport_size, rect_pos, rect_size) {
            (true, Some(viewport_size), Some(rect_pos), Some(rect_size)) => Ok(BrushLayout {
                stride: interface.instance.size,
                rect_pos,
                rect_size,
                uniform_size: interface.uniforms.size,
                viewport_size,
                params,
            }),
            _ => Err(errors),
        }
    }

    /// The program's interface as the GPU backends bind it.
    pub fn abi(&self) -> ProgramAbi {
        ProgramAbi {
            instance_stride: self.stride,
            uniform_size: self.uniform_size,
            textures: 0,
            samplers: 0,
            vertex_textures: false,
        }
    }

    /// The index of the parameter `name`.
    pub fn param(&self, name: &str) -> Option<usize> {
        self.params.iter().position(|p| &*p.name == name)
    }

    /// Encodes `values`, one per parameter in order, into `out` as one
    /// instance whose box is zero.
    ///
    /// # Errors
    ///
    /// A value count or kind that does not match; `out` is then unspecified.
    pub fn encode(&self, values: &[ParamValue], out: &mut Vec<u8>) -> Result<(), EncodeError> {
        if values.len() != self.params.len() {
            return Err(EncodeError(format!(
                "{} values for {} parameters",
                values.len(),
                self.params.len()
            )));
        }
        out.clear();
        out.resize(self.stride as usize, 0);
        for (i, value) in values.iter().enumerate() {
            self.set(out, i, *value)?;
        }
        Ok(())
    }

    /// Rewrites parameter `param` of the encoded `instance`.
    ///
    /// # Errors
    ///
    /// No such parameter, a value of another kind, or an instance shorter
    /// than the stride.
    pub fn set(
        &self,
        instance: &mut [u8],
        param: usize,
        value: ParamValue,
    ) -> Result<(), EncodeError> {
        let p = self
            .params
            .get(param)
            .ok_or_else(|| EncodeError(format!("no parameter {param}")))?;
        if value.kind() != p.kind {
            return Err(EncodeError(format!(
                "`{}` is a `{}`, not a `{}`",
                p.name,
                p.kind.name(),
                value.kind().name()
            )));
        }
        if instance.len() < self.stride as usize {
            return Err(EncodeError(format!(
                "an instance of {} bytes; the brush's is {}",
                instance.len(),
                self.stride
            )));
        }
        value.write(&mut instance[p.offset as usize..]);
        Ok(())
    }

    /// Writes the box, in target pixels, into an encoded `instance`.
    ///
    /// # Panics
    ///
    /// When `instance` is shorter than the stride.
    pub fn place(&self, instance: &mut [u8], pos: [f32; 2], size: [f32; 2]) {
        ParamValue::Vec2F32(pos).write(&mut instance[self.rect_pos as usize..]);
        ParamValue::Vec2F32(size).write(&mut instance[self.rect_size as usize..]);
    }

    /// Writes the uniform block of a draw into a target of `viewport`
    /// pixels.
    ///
    /// # Panics
    ///
    /// When `out` is shorter than [`uniform_size`](Self::uniform_size).
    pub fn write_uniforms(&self, out: &mut [u8], viewport: [f32; 2]) {
        ParamValue::Vec2F32(viewport).write(&mut out[self.viewport_size as usize..]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::{Binding, Texel, Value};

    fn binding(name: &str, ty: Ty) -> Binding {
        Binding {
            name: name.into(),
            ty,
            span: Span {
                start: name.len() as u32,
                end: name.len() as u32 + 1,
            },
        }
    }

    fn brush(uniforms: Vec<Binding>, instance: Vec<Binding>) -> Program {
        Program {
            name: "B".into(),
            uniforms,
            instance,
            ..Program::default()
        }
    }

    fn layout(p: &Program) -> Result<BrushLayout, Vec<ProgramError>> {
        BrushLayout::new(p, &p.interface())
    }

    #[test]
    fn parameters_encode_where_the_instance_layout_puts_them() {
        let p = brush(
            vec![binding(VIEWPORT_SIZE, Ty::VEC2)],
            vec![
                binding("tint", Ty::Color),
                binding(RECT_POS, Ty::VEC2),
                binding("rings", Ty::I32),
                binding(RECT_SIZE, Ty::VEC2),
                binding("axis", Ty::VEC3),
                binding("seed", Ty::U32),
                binding("speed", Ty::F32),
            ],
        );
        let l = layout(&p).expect("a brush");
        assert_eq!((l.stride, l.rect_pos, l.rect_size), (56, 16, 28));
        assert_eq!((l.uniform_size, l.viewport_size), (16, 0));
        let kinds: Vec<_> = l
            .params
            .iter()
            .map(|p| (&*p.name, p.kind, p.offset))
            .collect();
        assert_eq!(
            kinds,
            [
                ("tint", ParamKind::ColorLinear, 0),
                ("rings", ParamKind::I32, 24),
                ("axis", ParamKind::Vec3F32, 36),
                ("seed", ParamKind::U32, 48),
                ("speed", ParamKind::F32, 52),
            ]
        );
        assert_eq!(l.param("seed"), Some(3));

        let values = [
            ParamValue::ColorLinear([0.25, 0.5, 0.75, 1.0]),
            ParamValue::I32(-3),
            ParamValue::Vec3F32([1.0, 2.0, 3.0]),
            ParamValue::U32(9),
            ParamValue::F32(0.5),
        ];
        let mut bytes = Vec::new();
        l.encode(&values, &mut bytes).expect("encodes");
        l.place(&mut bytes, [10.0, 20.0], [30.0, 40.0]);
        // The generic encoder writes the same instance from the same values.
        let interface = p.interface();
        let generic = interface
            .instance
            .encode(
                &p.instance,
                &[
                    Value::floats(&[0.25, 0.5, 0.75, 1.0]),
                    Value::floats(&[10.0, 20.0]),
                    Value::i32(-3),
                    Value::floats(&[30.0, 40.0]),
                    Value::floats(&[1.0, 2.0, 3.0]),
                    Value::u32(9),
                    Value::f32(0.5),
                ],
            )
            .expect("encodes");
        assert_eq!(bytes, generic);

        l.set(&mut bytes, 4, ParamValue::F32(2.0)).expect("sets");
        assert_eq!(&bytes[52..56], &2.0f32.to_le_bytes());
        assert!(l.set(&mut bytes, 4, ParamValue::U32(2)).is_err(), "kind");
        assert!(l.set(&mut bytes, 5, ParamValue::F32(2.0)).is_err(), "index");
        assert!(l.encode(&values[..4], &mut bytes).is_err(), "count");

        assert_eq!(l.abi(), interface.abi());
        let mut uniforms = vec![0; l.uniform_size as usize];
        l.write_uniforms(&mut uniforms, [640.0, 480.0]);
        assert_eq!(
            interface.uniforms.decode(&p, &p.uniforms, &uniforms),
            [Value::floats(&[640.0, 480.0])]
        );
    }

    #[test]
    fn a_program_off_the_profile_is_told_why() {
        let codes = |p: &Program| -> Vec<(u32, String)> {
            let errors = layout(p).expect_err("not a brush");
            assert!(errors.iter().all(|e| e.code == PROFILE));
            errors
                .into_iter()
                .map(|e| (e.span.start, e.message))
                .collect()
        };

        let missing = codes(&brush(Vec::new(), vec![binding("radius", Ty::F32)]));
        assert_eq!(missing.len(), 3, "{missing:?}");
        assert!(missing.iter().all(|(at, _)| *at == 0));
        assert!(missing[1].1.contains("`instance rect_pos: Vec2F32;`"));

        let mut p = brush(
            vec![binding(VIEWPORT_SIZE, Ty::VEC4), binding("time", Ty::F32)],
            vec![
                binding(RECT_POS, Ty::VEC2),
                binding(RECT_SIZE, Ty::F32),
                binding("basis", Ty::Matrix(2)),
                binding("mask", Ty::Vector(crate::program::Scalar::U32, 2)),
            ],
        );
        p.textures
            .push(binding("image", Ty::Texture(Texel::Vec4F32)));
        let wrong = codes(&p);
        let at: Vec<u32> = wrong.iter().map(|(at, _)| *at).collect();
        // viewport_size, time, rect_size, basis, mask, image; nothing missing.
        assert_eq!(at, [13, 4, 9, 5, 4, 5], "{wrong:?}");
        assert!(wrong[0].1.contains("not a `Vec4F32`"));
        assert!(wrong[1].1.contains("make `time` an `instance` parameter"));
        assert!(wrong[3].1.contains("`basis`, a `Mat2F32`"));
        assert!(wrong[4].1.contains("a `Vec2U32`"));
    }
}
