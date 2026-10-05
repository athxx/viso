//! Shader values on the host: what the instance and uniform encoder writes
//! into a buffer through the layout's descriptors, and what the reference
//! interpreter computes with.

use super::layout::BlockLayout;
use super::{Binding, Program, Scalar, Ty};

/// One lane of a scalar or vector.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Lane {
    Bool(bool),
    I32(i32),
    U32(u32),
    F32(f32),
}

impl Lane {
    /// Its bits as a buffer stores them; a `Bool` as 0 or 1.
    pub fn bits(self) -> u32 {
        match self {
            Lane::Bool(b) => u32::from(b),
            Lane::I32(v) => v.cast_unsigned(),
            Lane::U32(v) => v,
            Lane::F32(v) => v.to_bits(),
        }
    }

    /// The lane of `scalar` stored as `bits`.
    pub fn from_bits(scalar: Scalar, bits: u32) -> Lane {
        match scalar {
            Scalar::Bool => Lane::Bool(bits != 0),
            Scalar::I32 => Lane::I32(bits.cast_signed()),
            Scalar::U32 => Lane::U32(bits),
            Scalar::F32 => Lane::F32(f32::from_bits(bits)),
        }
    }

    /// Its value as an `f32`, converting an integer.
    pub fn f32(self) -> f32 {
        match self {
            Lane::Bool(b) => f32::from(u8::from(b)),
            Lane::I32(v) => v as f32,
            Lane::U32(v) => v as f32,
            Lane::F32(v) => v,
        }
    }
}

/// A shader value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A scalar (one lane), vector or `ColorLinear` (four `F32` lanes).
    Lanes(u8, [Lane; 4]),
    /// An `n`×`n` matrix by column.
    Matrix(u8, [[f32; 4]; 4]),
    /// A record's or `VertexOutput`'s fields in order.
    Record(Vec<Value>),
    /// The texture binding at this index, as an intrinsic's argument.
    Texture(u32),
    /// The sampler binding at this index.
    Sampler(u32),
}

impl Value {
    /// A one-lane value; unused lanes hold the lane type's zero, as every
    /// constructed value does, so equal values compare equal.
    pub fn scalar(lane: Lane) -> Value {
        let zero = match lane {
            Lane::Bool(_) => Lane::Bool(false),
            Lane::I32(_) => Lane::I32(0),
            Lane::U32(_) => Lane::U32(0),
            Lane::F32(_) => Lane::F32(0.0),
        };
        Value::Lanes(1, [lane, zero, zero, zero])
    }

    pub fn f32(v: f32) -> Value {
        Value::scalar(Lane::F32(v))
    }

    pub fn u32(v: u32) -> Value {
        Value::scalar(Lane::U32(v))
    }

    pub fn i32(v: i32) -> Value {
        Value::scalar(Lane::I32(v))
    }

    pub fn bool(v: bool) -> Value {
        Value::scalar(Lane::Bool(v))
    }

    /// A vector or color of `F32` lanes.
    pub fn floats(lanes: &[f32]) -> Value {
        let mut out = [Lane::F32(0.0); 4];
        for (o, v) in out.iter_mut().zip(lanes) {
            *o = Lane::F32(*v);
        }
        Value::Lanes(lanes.len() as u8, out)
    }

    /// The zero value of `ty`.
    pub fn zero(program: &Program, ty: Ty) -> Value {
        match ty {
            Ty::Scalar(s) | Ty::Vector(s, _) => {
                let n = ty.lanes().map_or(1, |(_, n)| n);
                Value::Lanes(n, [Lane::from_bits(s, 0); 4])
            }
            Ty::Color => Value::floats(&[0.0; 4]),
            Ty::Matrix(n) => Value::Matrix(n, [[0.0; 4]; 4]),
            Ty::Record(_) | Ty::VertexOutput => Value::Record(
                program
                    .fields(ty)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(_, t)| Value::zero(program, t))
                    .collect(),
            ),
            Ty::Texture(_) => Value::Texture(0),
            Ty::Sampler => Value::Sampler(0),
            Ty::Unit => Value::Record(Vec::new()),
        }
    }

    /// Its lanes, for a scalar, vector or color.
    pub fn lanes(&self) -> &[Lane] {
        match self {
            Value::Lanes(n, lanes) => &lanes[..usize::from(*n)],
            _ => &[],
        }
    }

    /// Its `F32` lanes.
    pub fn to_floats(&self) -> Vec<f32> {
        self.lanes().iter().map(|l| l.f32()).collect()
    }
}

/// Why values do not encode into a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeError(pub String);

impl BlockLayout {
    /// The bytes of one block holding `members`, one per interface member of
    /// `bindings`, each leaf at its descriptor's offset.
    ///
    /// # Errors
    ///
    /// A member count or a value shape that does not fit.
    pub fn encode(&self, bindings: &[Binding], members: &[Value]) -> Result<Vec<u8>, EncodeError> {
        if members.len() != bindings.len() {
            return Err(EncodeError(format!(
                "{} values for {} members",
                members.len(),
                bindings.len()
            )));
        }
        let mut bytes = vec![0u8; self.size as usize];
        for f in &self.fields {
            let mut value = &members[f.member as usize];
            for &step in &f.path {
                value = match value {
                    Value::Record(fields) => fields
                        .get(step as usize)
                        .ok_or_else(|| EncodeError(format!("`{}` is missing a field", f.name)))?,
                    _ => return Err(EncodeError(format!("`{}` is not a record value", f.name))),
                };
            }
            let mut put = |at: u32, bits: u32| {
                let at = at as usize;
                bytes[at..at + 4].copy_from_slice(&bits.to_le_bytes());
            };
            match (f.ty, value) {
                (Ty::Matrix(n), Value::Matrix(m, columns)) if *m == n => {
                    for (c, column) in columns.iter().take(usize::from(n)).enumerate() {
                        for (r, v) in column.iter().take(usize::from(n)).enumerate() {
                            put(
                                f.offset + c as u32 * f.matrix_stride + 4 * r as u32,
                                v.to_bits(),
                            );
                        }
                    }
                }
                (ty, Value::Lanes(n, lanes)) => {
                    let (scalar, want) = match ty {
                        Ty::Color => (Scalar::F32, 4),
                        other => other
                            .lanes()
                            .ok_or_else(|| EncodeError(format!("`{}` holds no lanes", f.name)))?,
                    };
                    let fits = *n == want
                        && lanes[..usize::from(*n)]
                            .iter()
                            .all(|l| Lane::from_bits(scalar, l.bits()) == *l);
                    if !fits {
                        return Err(EncodeError(format!("`{}` is not a {ty:?}", f.name)));
                    }
                    for (i, lane) in lanes[..usize::from(*n)].iter().enumerate() {
                        put(f.offset + 4 * i as u32, lane.bits());
                    }
                }
                _ => return Err(EncodeError(format!("`{}` has the wrong shape", f.name))),
            }
        }
        Ok(bytes)
    }

    /// The members of `bindings` a block's `bytes` hold.
    pub fn decode(&self, program: &Program, bindings: &[Binding], bytes: &[u8]) -> Vec<Value> {
        let mut members: Vec<Value> = bindings
            .iter()
            .map(|b| Value::zero(program, b.ty))
            .collect();
        let word = |at: u32| -> u32 {
            let at = at as usize;
            bytes
                .get(at..at + 4)
                .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        for f in &self.fields {
            let mut slot = &mut members[f.member as usize];
            for &step in &f.path {
                let Value::Record(fields) = slot else {
                    break;
                };
                slot = &mut fields[step as usize];
            }
            *slot = match f.ty {
                Ty::Matrix(n) => {
                    let mut columns = [[0.0; 4]; 4];
                    for (c, column) in columns.iter_mut().take(usize::from(n)).enumerate() {
                        for (r, v) in column.iter_mut().take(usize::from(n)).enumerate() {
                            *v = f32::from_bits(word(
                                f.offset + c as u32 * f.matrix_stride + 4 * r as u32,
                            ));
                        }
                    }
                    Value::Matrix(n, columns)
                }
                ty => {
                    let (scalar, n) = match ty {
                        Ty::Color => (Scalar::F32, 4),
                        other => other.lanes().unwrap_or((Scalar::F32, 1)),
                    };
                    let mut lanes = [Lane::from_bits(scalar, 0); 4];
                    for (i, lane) in lanes.iter_mut().take(usize::from(n)).enumerate() {
                        *lane = Lane::from_bits(scalar, word(f.offset + 4 * i as u32));
                    }
                    Value::Lanes(n, lanes)
                }
            };
        }
        members
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::{Record, Span};

    #[test]
    fn values_round_trip_through_a_block() {
        let b = |name: &str, ty| Binding {
            name: name.into(),
            ty,
            span: Span::default(),
        };
        let p = Program {
            name: "S".into(),
            records: vec![Record {
                name: "L".into(),
                fields: vec![b("dir", Ty::VEC3), b("on", Ty::U32)],
                span: Span::default(),
            }],
            uniforms: vec![
                b("t", Ty::F32),
                b("l", Ty::Record(0)),
                b("m", Ty::Matrix(2)),
            ],
            instance: vec![b("n", Ty::I32), b("c", Ty::Color)],
            ..Program::default()
        };
        let i = p.interface();
        let uniforms = vec![
            Value::f32(1.5),
            Value::Record(vec![Value::floats(&[1.0, 2.0, 3.0]), Value::u32(7)]),
            Value::Matrix(
                2,
                [
                    [1.0, 2.0, 0.0, 0.0],
                    [3.0, 4.0, 0.0, 0.0],
                    [0.0; 4],
                    [0.0; 4],
                ],
            ),
        ];
        let bytes = i.uniforms.encode(&p.uniforms, &uniforms).expect("encodes");
        assert_eq!(bytes.len(), 48);
        assert_eq!(&bytes[16..20], &1.0f32.to_le_bytes());
        assert_eq!(&bytes[28..32], &7u32.to_le_bytes());
        assert_eq!(&bytes[40..44], &3.0f32.to_le_bytes());
        assert_eq!(i.uniforms.decode(&p, &p.uniforms, &bytes), uniforms);

        let instance = vec![Value::i32(-2), Value::floats(&[0.25, 0.5, 0.75, 1.0])];
        let bytes = i.instance.encode(&p.instance, &instance).expect("encodes");
        assert_eq!(bytes.len(), 20);
        assert_eq!(i.instance.decode(&p, &p.instance, &bytes), instance);
        assert!(
            i.instance
                .encode(&p.instance, &[Value::f32(1.0), Value::f32(1.0)])
                .is_err()
        );
    }
}
