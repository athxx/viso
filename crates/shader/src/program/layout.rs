//! The Viso Shader Layout Algorithm: the one place a program's interface gets
//! its byte layout, and the descriptors every backend, the host encoder and
//! the reference interpreter read it from.
//!
//! - **Instance data** is tightly packed in declaration order, every leaf
//!   4-byte aligned, as a `#[repr(C)]` struct of `f32`/`u32`/`i32` arrays
//!   lays out: a vector takes `4 × lanes` bytes, a matrix its columns one
//!   after another, a `ColorLinear` 16 bytes. The stride is the sum.
//! - **Uniform data** follows the uniform address space rules WGSL states and
//!   Metal's constant buffers meet: a scalar aligns to 4, a two-lane vector to
//!   8, a three- or four-lane vector and a `ColorLinear` to 16 (a three-lane
//!   vector still takes 12 bytes, so a scalar may follow in its last 4); a
//!   matrix aligns as its column, which it strides by. The block size rounds
//!   up to 16.
//! - A **record** member flattens into its leaves, in field order, each
//!   placed by the rules of the block it is in; its descriptor names the
//!   leaf by its dotted path.
//! - **Varyings** take locations in declaration order: `F32` lanes
//!   interpolate perspective-correct, integer lanes are flat.
//!
//! [`LAYOUT_VERSION`] enters [`ShaderInterface::layout_key`], so a change to
//! these rules changes every pipeline cache key.

use viso_gpu::{AttrFormat, InstanceLayout};

use super::{Binding, ExprKind, Program, ProgramError, Scalar, Span, Stage, Ty};

/// The version of the rules above.
pub const LAYOUT_VERSION: u32 = 1;

/// How a varying crosses from the vertex to the fragment stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Interpolation {
    /// Not a varying.
    None,
    /// Perspective-correct across the primitive.
    Perspective,
    /// The provoking vertex's value.
    Flat,
}

/// One laid-out leaf of an interface block, or one varying.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldDescriptor {
    /// Stable across builds while the shader, the block and the dotted name
    /// stay the same.
    pub stable_field_id: u64,
    /// The member's name, a record leaf's by its dotted path.
    pub name: Box<str>,
    /// A scalar, vector, matrix or `ColorLinear`.
    pub ty: Ty,
    pub alignment: u32,
    pub size: u32,
    /// The byte offset in its block; a varying's location.
    pub offset: u32,
    /// Always 0: the shader type set has no arrays.
    pub array_stride: u32,
    /// The byte stride between a matrix's columns, 0 for a non-matrix.
    pub matrix_stride: u32,
    pub interpolation: Interpolation,
    pub source_span: Span,
    /// The interface member it lays out.
    pub member: u32,
    /// The record fields from the member to the leaf.
    pub path: Box<[u32]>,
}

/// A laid-out uniform block or instance record.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct BlockLayout {
    /// Bytes one block (one instance) takes.
    pub size: u32,
    pub fields: Vec<FieldDescriptor>,
}

/// A texture or sampler binding and the slot it binds at.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BindingDescriptor {
    pub name: Box<str>,
    pub index: u32,
    pub ty: Ty,
    pub source_span: Span,
}

/// An entry point as the backends name it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntryDescriptor {
    pub stage: Stage,
    pub name: &'static str,
}

/// A program's interface, laid out.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShaderInterface {
    pub shader_id: u64,
    pub layout_version: u32,
    pub uniforms: BlockLayout,
    pub instance: BlockLayout,
    pub varyings: Vec<FieldDescriptor>,
    pub textures: Vec<BindingDescriptor>,
    pub samplers: Vec<BindingDescriptor>,
    /// Whether the vertex entry reads textures or samplers; the fragment
    /// entry always may.
    pub vertex_textures: bool,
    pub entries: Vec<EntryDescriptor>,
}

/// The backend name of each entry.
pub const VERTEX_ENTRY: &str = "viso_vertex";
pub const FRAGMENT_ENTRY: &str = "viso_fragment";

/// FNV-1a over `parts`, each followed by a zero byte.
pub(crate) fn fnv(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for &b in part.iter().chain(&[0u8]) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// Which block a leaf is laid out in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Rules {
    Uniform,
    Instance,
}

impl Rules {
    const fn word(self) -> &'static str {
        match self {
            Rules::Uniform => "uniform",
            Rules::Instance => "instance",
        }
    }

    /// A leaf's alignment, size and matrix stride.
    fn place(self, ty: Ty) -> (u32, u32, u32) {
        let lanes = match ty {
            Ty::Color => 4,
            other => u32::from(other.lanes().map_or(1, |(_, n)| n)),
        };
        match (self, ty) {
            (Rules::Instance, Ty::Matrix(n)) => {
                let n = u32::from(n);
                (4, 4 * n * n, 4 * n)
            }
            (Rules::Instance, _) => (4, 4 * lanes, 0),
            (Rules::Uniform, Ty::Matrix(n)) => {
                let column = if n == 2 { 8 } else { 16 };
                (column, column * u32::from(n), column)
            }
            (Rules::Uniform, _) => {
                let align = match lanes {
                    1 => 4,
                    2 => 8,
                    _ => 16,
                };
                (align, 4 * lanes, 0)
            }
        }
    }
}

fn round_up(value: u32, to: u32) -> u32 {
    value.div_ceil(to) * to
}

impl Program {
    /// The interface laid out by the Viso Shader Layout Algorithm.
    pub fn interface(&self) -> ShaderInterface {
        let shader_id = fnv(&[self.name.as_bytes()]);
        let varyings = self
            .varyings
            .iter()
            .enumerate()
            .map(|(location, v)| {
                let (_, size, _) = Rules::Instance.place(v.ty);
                let interpolation = if v.ty.is_float() {
                    Interpolation::Perspective
                } else {
                    Interpolation::Flat
                };
                FieldDescriptor {
                    stable_field_id: fnv(&[self.name.as_bytes(), b"varying", v.name.as_bytes()]),
                    name: v.name.clone(),
                    ty: v.ty,
                    alignment: 4,
                    size,
                    offset: location as u32,
                    array_stride: 0,
                    matrix_stride: 0,
                    interpolation,
                    source_span: v.span,
                    member: location as u32,
                    path: Box::new([]),
                }
            })
            .collect();
        let bindings = |b: &[Binding]| -> Vec<BindingDescriptor> {
            b.iter()
                .enumerate()
                .map(|(index, b)| BindingDescriptor {
                    name: b.name.clone(),
                    index: index as u32,
                    ty: b.ty,
                    source_span: b.span,
                })
                .collect()
        };
        let mut entries = Vec::new();
        if self.vertex.is_some() {
            entries.push(EntryDescriptor {
                stage: Stage::Vertex,
                name: VERTEX_ENTRY,
            });
        }
        if self.fragment.is_some() {
            entries.push(EntryDescriptor {
                stage: Stage::Fragment,
                name: FRAGMENT_ENTRY,
            });
        }
        let mut vertex_textures = false;
        if let Some(v) = &self.vertex {
            v.body.visit(&mut |e| {
                vertex_textures |= matches!(e.kind, ExprKind::Texture(_) | ExprKind::Sampler(_));
            });
        }
        ShaderInterface {
            shader_id,
            layout_version: LAYOUT_VERSION,
            uniforms: self.block(&self.uniforms, Rules::Uniform),
            instance: self.block(&self.instance, Rules::Instance),
            varyings,
            textures: bindings(&self.textures),
            samplers: bindings(&self.samplers),
            vertex_textures,
            entries,
        }
    }

    fn block(&self, members: &[Binding], rules: Rules) -> BlockLayout {
        let mut fields = Vec::new();
        let mut offset = 0;
        for (index, member) in members.iter().enumerate() {
            let mut path = Vec::new();
            self.leaves(
                member.ty,
                &member.name,
                member.span,
                index as u32,
                &mut path,
                rules,
                &mut offset,
                &mut fields,
            );
        }
        let size = match rules {
            Rules::Uniform => round_up(offset, 16),
            Rules::Instance => offset,
        };
        BlockLayout { size, fields }
    }

    #[allow(clippy::too_many_arguments)]
    fn leaves(
        &self,
        ty: Ty,
        name: &str,
        span: Span,
        member: u32,
        path: &mut Vec<u32>,
        rules: Rules,
        offset: &mut u32,
        out: &mut Vec<FieldDescriptor>,
    ) {
        if let Ty::Record(i) = ty {
            let record = &self.records[i as usize];
            for (f, field) in record.fields.iter().enumerate() {
                path.push(f as u32);
                let name = format!("{name}.{}", field.name);
                self.leaves(
                    field.ty, &name, field.span, member, path, rules, offset, out,
                );
                path.pop();
            }
            return;
        }
        let (alignment, size, matrix_stride) = rules.place(ty);
        *offset = round_up(*offset, alignment);
        out.push(FieldDescriptor {
            stable_field_id: fnv(&[
                self.name.as_bytes(),
                rules.word().as_bytes(),
                name.as_bytes(),
            ]),
            name: name.into(),
            ty,
            alignment,
            size,
            offset: *offset,
            array_stride: 0,
            matrix_stride,
            interpolation: Interpolation::None,
            source_span: span,
            member,
            path: path.clone().into_boxed_slice(),
        });
        *offset += size;
    }
}

/// The host attribute format of an instance leaf, when one exists.
pub fn attr_format(ty: Ty) -> Option<AttrFormat> {
    Some(match ty {
        Ty::F32 => AttrFormat::Float1,
        Ty::VEC2 => AttrFormat::Float2,
        Ty::VEC3 => AttrFormat::Float3,
        Ty::VEC4 | Ty::Color => AttrFormat::Float4,
        Ty::U32 => AttrFormat::Uint1,
        Ty::Vector(Scalar::U32, 2) => AttrFormat::Uint2,
        Ty::Vector(Scalar::U32, 4) => AttrFormat::Uint4,
        _ => return None,
    })
}

impl ShaderInterface {
    /// A key that changes with the layout rules and with any descriptor:
    /// the pipeline cache key's layout part.
    pub fn layout_key(&self) -> u64 {
        let mut parts: Vec<Vec<u8>> = vec![
            self.layout_version.to_le_bytes().to_vec(),
            self.shader_id.to_le_bytes().to_vec(),
        ];
        for (tag, block) in [(b'u', &self.uniforms), (b'i', &self.instance)] {
            parts.push(vec![tag]);
            parts.push(block.size.to_le_bytes().to_vec());
            for f in &block.fields {
                parts.push(field_bytes(f));
            }
        }
        for f in &self.varyings {
            parts.push(field_bytes(f));
        }
        for b in self.textures.iter().chain(&self.samplers) {
            parts.push(format!("{}:{}:{:?}", b.name, b.index, b.ty).into_bytes());
        }
        parts.push(vec![u8::from(self.vertex_textures)]);
        let refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
        fnv(&refs)
    }

    /// Whether `other` lays out its instance data as this does, so an
    /// instance buffer of one feeds the other.
    pub fn same_instance_layout(&self, other: &ShaderInterface) -> bool {
        let shape = |b: &BlockLayout| -> Vec<(Ty, u32, u32)> {
            b.fields.iter().map(|f| (f.ty, f.offset, f.size)).collect()
        };
        self.instance.size == other.instance.size && shape(&self.instance) == shape(&other.instance)
    }

    /// Checks a host instance struct's layout against the instance block:
    /// the same leaves by name, format and offset, and the same stride.
    ///
    /// # Errors
    ///
    /// Each disagreement, `E8104`.
    pub fn check_instance(&self, host: &InstanceLayout) -> Result<(), Vec<ProgramError>> {
        let mut errors = Vec::new();
        let mut error =
            |span: Span, message: String| errors.push(ProgramError::new("E8104", span, message));
        if host.stride != self.instance.size as usize {
            error(
                Span::default(),
                format!(
                    "the host instance is {} bytes; the shader's is {}",
                    host.stride, self.instance.size
                ),
            );
        }
        if host.fields.len() != self.instance.fields.len() {
            error(
                Span::default(),
                format!(
                    "the host instance has {} fields; the shader's has {}",
                    host.fields.len(),
                    self.instance.fields.len()
                ),
            );
        }
        for (field, leaf) in host.fields.iter().zip(&self.instance.fields) {
            if field.name != &*leaf.name {
                error(
                    leaf.source_span,
                    format!(
                        "host field `{}` stands where the shader has `{}`",
                        field.name, leaf.name
                    ),
                );
                continue;
            }
            match attr_format(leaf.ty) {
                Some(format) if format == field.format => {}
                Some(format) => error(
                    leaf.source_span,
                    format!(
                        "`{}` is {:?} on the host and {format:?} in the shader",
                        leaf.name, field.format
                    ),
                ),
                None => error(
                    leaf.source_span,
                    format!(
                        "`{}` has no host attribute format; write it with the instance encoder",
                        leaf.name
                    ),
                ),
            }
            if field.offset != leaf.offset as usize {
                error(
                    leaf.source_span,
                    format!(
                        "`{}` is at byte {} on the host and {} in the shader",
                        leaf.name, field.offset, leaf.offset
                    ),
                );
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

fn field_bytes(f: &FieldDescriptor) -> Vec<u8> {
    format!(
        "{}:{:?}:{}:{}:{}:{}:{:?}",
        f.name, f.ty, f.alignment, f.size, f.offset, f.matrix_stride, f.interpolation
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::Record;

    fn binding(name: &str, ty: Ty) -> Binding {
        Binding {
            name: name.into(),
            ty,
            span: Span::default(),
        }
    }

    fn offsets(b: &BlockLayout) -> Vec<(&str, u32, u32)> {
        b.fields
            .iter()
            .map(|f| (&*f.name, f.offset, f.size))
            .collect()
    }

    #[test]
    fn instance_data_packs_tightly_and_uniforms_align() {
        let p = Program {
            name: "S".into(),
            records: vec![Record {
                name: "Light".into(),
                fields: vec![binding("position", Ty::VEC3), binding("strength", Ty::F32)],
                span: Span::default(),
            }],
            uniforms: vec![
                binding("time", Ty::F32),
                binding("offset", Ty::VEC2),
                binding("light", Ty::Record(0)),
                binding("basis", Ty::Matrix(3)),
                binding("flag", Ty::U32),
            ],
            instance: vec![
                binding("pos", Ty::VEC2),
                binding("tint", Ty::Color),
                binding("radius", Ty::F32),
                binding("light", Ty::Record(0)),
            ],
            ..Program::default()
        };
        let i = p.interface();
        assert_eq!(
            offsets(&i.uniforms),
            [
                ("time", 0, 4),
                ("offset", 8, 8),
                ("light.position", 16, 12),
                ("light.strength", 28, 4),
                ("basis", 32, 48),
                ("flag", 80, 4),
            ]
        );
        assert_eq!(i.uniforms.size, 96);
        assert_eq!(i.uniforms.fields[4].matrix_stride, 16);
        assert_eq!(
            offsets(&i.instance),
            [
                ("pos", 0, 8),
                ("tint", 8, 16),
                ("radius", 24, 4),
                ("light.position", 28, 12),
                ("light.strength", 40, 4),
            ]
        );
        assert_eq!(i.instance.size, 44);
        assert_eq!(&*i.instance.fields[3].path, [0]);

        // Ids are stable and distinct per block; the key follows the layout.
        let again = p.interface();
        assert_eq!(i.layout_key(), again.layout_key());
        assert_ne!(
            i.uniforms.fields[2].stable_field_id,
            i.instance.fields[3].stable_field_id
        );
        let mut moved = p.clone();
        moved.instance.swap(0, 2);
        assert_ne!(moved.interface().layout_key(), i.layout_key());
        assert!(!moved.interface().same_instance_layout(&i));
    }

    #[test]
    fn a_host_instance_struct_is_checked_against_the_layout() {
        use viso_gpu::InstanceField;
        let p = Program {
            name: "S".into(),
            instance: vec![binding("pos", Ty::VEC2), binding("radius", Ty::F32)],
            ..Program::default()
        };
        let i = p.interface();
        const GOOD: &[InstanceField] = &[
            InstanceField {
                name: "pos",
                offset: 0,
                format: AttrFormat::Float2,
            },
            InstanceField {
                name: "radius",
                offset: 8,
                format: AttrFormat::Float1,
            },
        ];
        assert_eq!(
            i.check_instance(&InstanceLayout {
                stride: 12,
                fields: GOOD
            }),
            Ok(())
        );
        const SHIFTED: &[InstanceField] = &[
            InstanceField {
                name: "pos",
                offset: 0,
                format: AttrFormat::Float2,
            },
            InstanceField {
                name: "radius",
                offset: 12,
                format: AttrFormat::Float1,
            },
        ];
        let errors = i
            .check_instance(&InstanceLayout {
                stride: 16,
                fields: SHIFTED,
            })
            .expect_err("shifted");
        assert!(errors.iter().all(|e| e.code == "E8104"));
        assert_eq!(errors.len(), 2, "{errors:?}");
    }
}
