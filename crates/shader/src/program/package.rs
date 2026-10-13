//! A brush as a view package carries it: its layout and its code in each
//! language it was built for — the one the target's backend reads and the
//! reference interpreter's — generated when the package is built.
//!
//! The GPU languages travel as their text or SPIR-V bytes; the headless
//! target's code is the program itself, which the reference interpreter runs,
//! so it travels as the program. Unpacking moves a GPU language's bytes into a
//! [`ProgramCode`] without generating anything; decoding a CPU program
//! revalidates it, so the interpreter never meets an ill-typed tree whatever
//! wrote the bytes.
//!
//! A package carries no source span: spans place a declaration in the file it
//! was compiled from, which a runtime never reads, so a brush whose code did
//! not change packs to the same bytes wherever an edit moved it in its file.

use std::sync::Arc;

use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder};
use viso_gpu::{AddressMode, FilterMode, SamplerDesc};
pub use viso_gpu::{ProgramCode, ShaderLang};

use super::ops::{BinaryOp, Intrinsic, UnaryOp};
use super::{
    Binding, Block, BrushLayout, BrushParam, BrushTexture, Builtin, CpuShader, Expr, ExprKind,
    Function, Local, Loop, ParamKind, Place, Program, Record, Root, Scalar, Span, Stage, Stmt,
    Texel, Ty, code,
};

/// A brush built for the backends it targets: its code once per language.
#[derive(Debug, Clone, PartialEq)]
pub struct PackedBrush {
    pub name: Box<str>,
    pub layout: BrushLayout,
    pub code: Vec<PackedCode>,
}

/// A brush's code as a package carries it.
#[derive(Debug, Clone, PartialEq)]
pub enum PackedCode {
    /// Source text in `Msl`, `Wgsl` or `Hlsl`.
    Text(ShaderLang, String),
    /// A SPIR-V module, its words as little-endian bytes.
    SpirV(Vec<u8>),
    /// The program, for the reference interpreter.
    Program(Box<Program>),
}

impl PackedBrush {
    /// `program`, laid out as the brush `layout`, built for each of `langs`
    /// once; [`ShaderLang::None`] carries the program for the reference
    /// interpreter. Both are kept without their spans.
    ///
    /// # Errors
    ///
    /// SPIR-V without the `spirv` feature, or naga rejecting the WGSL.
    pub fn pack(
        program: &Program,
        layout: BrushLayout,
        langs: &[ShaderLang],
    ) -> Result<PackedBrush, String> {
        let interface = program.interface();
        let mut packed = Vec::with_capacity(langs.len());
        for (at, &lang) in langs.iter().enumerate() {
            if langs[..at].contains(&lang) {
                continue;
            }
            packed.push(match lang {
                ShaderLang::None => PackedCode::Program(Box::new(unspanned(program))),
                lang => match code(program, &interface, lang)? {
                    ProgramCode::Msl(t) => PackedCode::Text(ShaderLang::Msl, t),
                    ProgramCode::Wgsl(t) => PackedCode::Text(ShaderLang::Wgsl, t),
                    ProgramCode::Hlsl(t) => PackedCode::Text(ShaderLang::Hlsl, t),
                    ProgramCode::SpirV(b) => PackedCode::SpirV(b),
                    ProgramCode::Cpu(_) => unreachable!("a GPU language gives GPU code"),
                },
            });
        }
        let mut layout = layout;
        for param in &mut layout.params {
            param.source_span = Span::default();
        }
        for texture in &mut layout.textures {
            texture.source_span = Span::default();
        }
        Ok(PackedBrush {
            name: program.name.clone(),
            layout,
            code: packed,
        })
    }

    /// The languages its code is in, in order.
    pub fn langs(&self) -> impl Iterator<Item = ShaderLang> + '_ {
        self.code.iter().map(PackedCode::lang)
    }

    /// Its code as a backend's program compiler takes it, once per language.
    pub fn unpack(self) -> Vec<ProgramCode> {
        self.code.into_iter().map(PackedCode::unpack).collect()
    }
}

/// `program` with every span cleared, as its codec carries it.
fn unspanned(program: &Program) -> Program {
    let mut program = program.clone();
    for record in &mut program.records {
        record.span = Span::default();
        for field in &mut record.fields {
            field.span = Span::default();
        }
    }
    for members in [
        &mut program.uniforms,
        &mut program.instance,
        &mut program.varyings,
        &mut program.textures,
        &mut program.samplers,
    ] {
        for member in members {
            member.span = Span::default();
        }
    }
    let entries = program.vertex.iter_mut().chain(&mut program.fragment);
    for function in program.functions.iter_mut().chain(entries) {
        function.span = Span::default();
    }
    program
}

impl PackedCode {
    /// The language it is in.
    pub fn lang(&self) -> ShaderLang {
        match self {
            PackedCode::Text(lang, _) => *lang,
            PackedCode::SpirV(_) => ShaderLang::SpirV,
            PackedCode::Program(_) => ShaderLang::None,
        }
    }

    /// It as a backend's program compiler takes it.
    pub fn unpack(self) -> ProgramCode {
        match self {
            PackedCode::Text(ShaderLang::Msl, t) => ProgramCode::Msl(t),
            PackedCode::Text(ShaderLang::Wgsl, t) => ProgramCode::Wgsl(t),
            PackedCode::Text(_, t) => ProgramCode::Hlsl(t),
            PackedCode::SpirV(b) => ProgramCode::SpirV(b),
            PackedCode::Program(p) => ProgramCode::Cpu(Arc::new(CpuShader::new(*p))),
        }
    }
}

/// How deep an expression or block may nest in a decoded program: deeper is
/// malformed, so hostile bytes cannot overflow the decoder's stack.
const MAX_DEPTH: u32 = 256;

/// How many languages a brush can carry: each GPU language and the program.
const LANGS: usize = 5;

fn malformed(dec: &Decoder<'_>) -> DecodeError {
    DecodeError::Malformed {
        offset: dec.position(),
    }
}

fn len(dec: &mut Decoder<'_>) -> Result<usize, DecodeError> {
    let n = dec.read_varint()?;
    // Every element takes at least one byte.
    usize::try_from(n)
        .ok()
        .filter(|&n| n <= dec.remaining())
        .ok_or_else(|| malformed(dec))
}

fn u32_of(dec: &mut Decoder<'_>) -> Result<u32, DecodeError> {
    let v = dec.read_varint()?;
    u32::try_from(v).map_err(|_| malformed(dec))
}

fn name(dec: &mut Decoder<'_>) -> Result<Box<str>, DecodeError> {
    Ok(dec.read_str()?.into())
}

fn list<T>(
    dec: &mut Decoder<'_>,
    mut item: impl FnMut(&mut Decoder<'_>) -> Result<T, DecodeError>,
) -> Result<Vec<T>, DecodeError> {
    let n = len(dec)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(item(dec)?);
    }
    Ok(out)
}

fn write_list<T>(enc: &mut Encoder, items: &[T], mut item: impl FnMut(&mut Encoder, &T)) {
    enc.write_varint(items.len() as u64);
    for i in items {
        item(enc, i);
    }
}

const fn scalar_tag(s: Scalar) -> u8 {
    match s {
        Scalar::Bool => 0,
        Scalar::I32 => 1,
        Scalar::U32 => 2,
        Scalar::F32 => 3,
    }
}

fn read_scalar(dec: &mut Decoder<'_>) -> Result<Scalar, DecodeError> {
    Ok(match dec.read_u8()? {
        0 => Scalar::Bool,
        1 => Scalar::I32,
        2 => Scalar::U32,
        3 => Scalar::F32,
        _ => return Err(malformed(dec)),
    })
}

fn write_ty(enc: &mut Encoder, ty: Ty) {
    match ty {
        Ty::Unit => enc.write_u8(0),
        Ty::Scalar(s) => {
            enc.write_u8(1);
            enc.write_u8(scalar_tag(s));
        }
        Ty::Vector(s, n) => {
            enc.write_u8(2);
            enc.write_u8(scalar_tag(s));
            enc.write_u8(n);
        }
        Ty::Matrix(n) => {
            enc.write_u8(3);
            enc.write_u8(n);
        }
        Ty::Color => enc.write_u8(4),
        Ty::Record(i) => {
            enc.write_u8(5);
            enc.write_varint(i.into());
        }
        Ty::VertexOutput => enc.write_u8(6),
        Ty::Texture(Texel::F32) => enc.write_u8(7),
        Ty::Texture(Texel::Vec4F32) => enc.write_u8(8),
        Ty::Sampler => enc.write_u8(9),
    }
}

fn read_ty(dec: &mut Decoder<'_>) -> Result<Ty, DecodeError> {
    Ok(match dec.read_u8()? {
        0 => Ty::Unit,
        1 => Ty::Scalar(read_scalar(dec)?),
        2 => Ty::Vector(read_scalar(dec)?, dec.read_u8()?),
        3 => Ty::Matrix(dec.read_u8()?),
        4 => Ty::Color,
        5 => Ty::Record(u32_of(dec)?),
        6 => Ty::VertexOutput,
        7 => Ty::Texture(Texel::F32),
        8 => Ty::Texture(Texel::Vec4F32),
        9 => Ty::Sampler,
        _ => return Err(malformed(dec)),
    })
}

fn write_binding(enc: &mut Encoder, b: &Binding) {
    enc.write_str(&b.name);
    write_ty(enc, b.ty);
}

fn read_binding(dec: &mut Decoder<'_>) -> Result<Binding, DecodeError> {
    Ok(Binding {
        name: name(dec)?,
        ty: read_ty(dec)?,
        span: Span::default(),
    })
}

const UNARY: [UnaryOp; 3] = [UnaryOp::Neg, UnaryOp::Not, UnaryOp::BitNot];

const BINARY: [BinaryOp; 18] = [
    BinaryOp::Add,
    BinaryOp::Sub,
    BinaryOp::Mul,
    BinaryOp::Div,
    BinaryOp::Rem,
    BinaryOp::BitAnd,
    BinaryOp::BitOr,
    BinaryOp::BitXor,
    BinaryOp::Shl,
    BinaryOp::Shr,
    BinaryOp::Eq,
    BinaryOp::Ne,
    BinaryOp::Lt,
    BinaryOp::Le,
    BinaryOp::Gt,
    BinaryOp::Ge,
    BinaryOp::And,
    BinaryOp::Or,
];

/// The index of `item` in `table`, which holds every value of its type.
fn tag<T: PartialEq>(table: &[T], item: &T) -> u64 {
    table
        .iter()
        .position(|t| t == item)
        .expect("the table holds every value") as u64
}

fn pick<T: Copy>(dec: &mut Decoder<'_>, table: &[T]) -> Result<T, DecodeError> {
    let i = dec.read_varint()?;
    usize::try_from(i)
        .ok()
        .and_then(|i| table.get(i).copied())
        .ok_or_else(|| malformed(dec))
}

fn write_expr(enc: &mut Encoder, e: &Expr) {
    write_ty(enc, e.ty);
    let exprs = |enc: &mut Encoder, args: &[Expr]| write_list(enc, args, write_expr);
    match &e.kind {
        ExprKind::Bool(v) => {
            enc.write_u8(0);
            enc.write_bool(*v);
        }
        ExprKind::I32(v) => {
            enc.write_u8(1);
            enc.write_varint_signed((*v).into());
        }
        ExprKind::U32(v) => {
            enc.write_u8(2);
            enc.write_varint((*v).into());
        }
        ExprKind::F32(v) => {
            enc.write_u8(3);
            enc.write_f32(*v);
        }
        ExprKind::Local(i)
        | ExprKind::Uniform(i)
        | ExprKind::Instance(i)
        | ExprKind::Varying(i)
        | ExprKind::Texture(i)
        | ExprKind::Sampler(i) => {
            enc.write_u8(match &e.kind {
                ExprKind::Local(_) => 4,
                ExprKind::Uniform(_) => 5,
                ExprKind::Instance(_) => 6,
                ExprKind::Varying(_) => 7,
                ExprKind::Texture(_) => 8,
                _ => 9,
            });
            enc.write_varint((*i).into());
        }
        ExprKind::Unary(op, a) => {
            enc.write_u8(10);
            enc.write_varint(tag(&UNARY, op));
            write_expr(enc, a);
        }
        ExprKind::Binary(op, a, b) => {
            enc.write_u8(11);
            enc.write_varint(tag(&BINARY, op));
            write_expr(enc, a);
            write_expr(enc, b);
        }
        ExprKind::Construct(args) => {
            enc.write_u8(12);
            exprs(enc, args);
        }
        ExprKind::Swizzle(a, lanes) => {
            enc.write_u8(13);
            write_expr(enc, a);
            enc.write_raw(lanes);
        }
        ExprKind::Member(a, i) => {
            enc.write_u8(14);
            write_expr(enc, a);
            enc.write_varint((*i).into());
        }
        ExprKind::Index(a, b) => {
            enc.write_u8(15);
            write_expr(enc, a);
            write_expr(enc, b);
        }
        ExprKind::Call(f, args) => {
            enc.write_u8(16);
            enc.write_varint((*f).into());
            exprs(enc, args);
        }
        ExprKind::Intrinsic(i, args) => {
            enc.write_u8(17);
            enc.write_varint(tag(Intrinsic::ALL, i));
            exprs(enc, args);
        }
        ExprKind::Convert(a) => {
            enc.write_u8(18);
            write_expr(enc, a);
        }
    }
}

fn read_expr(dec: &mut Decoder<'_>, depth: u32) -> Result<Expr, DecodeError> {
    if depth > MAX_DEPTH {
        return Err(malformed(dec));
    }
    let ty = read_ty(dec)?;
    let sub = |dec: &mut Decoder<'_>| read_expr(dec, depth + 1).map(Box::new);
    let args = |dec: &mut Decoder<'_>| list(dec, |dec| read_expr(dec, depth + 1));
    let kind = match dec.read_u8()? {
        0 => ExprKind::Bool(dec.read_bool()?),
        1 => ExprKind::I32(i32::try_from(dec.read_varint_signed()?).map_err(|_| malformed(dec))?),
        2 => ExprKind::U32(u32_of(dec)?),
        3 => ExprKind::F32(dec.read_f32()?),
        4 => ExprKind::Local(u32_of(dec)?),
        5 => ExprKind::Uniform(u32_of(dec)?),
        6 => ExprKind::Instance(u32_of(dec)?),
        7 => ExprKind::Varying(u32_of(dec)?),
        8 => ExprKind::Texture(u32_of(dec)?),
        9 => ExprKind::Sampler(u32_of(dec)?),
        10 => ExprKind::Unary(pick(dec, &UNARY)?, sub(dec)?),
        11 => ExprKind::Binary(pick(dec, &BINARY)?, sub(dec)?, sub(dec)?),
        12 => ExprKind::Construct(args(dec)?),
        13 => {
            let a = sub(dec)?;
            let lanes = dec.read_raw(4)?;
            ExprKind::Swizzle(a, [lanes[0], lanes[1], lanes[2], lanes[3]])
        }
        14 => ExprKind::Member(sub(dec)?, u32_of(dec)?),
        15 => ExprKind::Index(sub(dec)?, sub(dec)?),
        16 => ExprKind::Call(u32_of(dec)?, args(dec)?),
        17 => ExprKind::Intrinsic(pick(dec, Intrinsic::ALL)?, args(dec)?),
        18 => ExprKind::Convert(sub(dec)?),
        _ => return Err(malformed(dec)),
    };
    Ok(Expr { ty, kind })
}

fn write_block(enc: &mut Encoder, block: &Block) {
    write_list(enc, &block.0, write_stmt);
}

fn read_block(dec: &mut Decoder<'_>, depth: u32) -> Result<Block, DecodeError> {
    if depth > MAX_DEPTH {
        return Err(malformed(dec));
    }
    Ok(Block(list(dec, |dec| read_stmt(dec, depth + 1))?))
}

fn write_stmt(enc: &mut Encoder, stmt: &Stmt) {
    match stmt {
        Stmt::Let(local, e) => {
            enc.write_u8(0);
            enc.write_varint((*local).into());
            write_expr(enc, e);
        }
        Stmt::Declare(local) => {
            enc.write_u8(1);
            enc.write_varint((*local).into());
        }
        Stmt::Assign(place, e) => {
            enc.write_u8(2);
            match place.root {
                Root::Local(i) => {
                    enc.write_u8(0);
                    enc.write_varint(i.into());
                }
                Root::Varying(i) => {
                    enc.write_u8(1);
                    enc.write_varint(i.into());
                }
            }
            write_list(enc, &place.path, |enc, i| enc.write_varint((*i).into()));
            write_expr(enc, e);
        }
        Stmt::If(c, a, b) => {
            enc.write_u8(3);
            write_expr(enc, c);
            write_block(enc, a);
            write_block(enc, b);
        }
        Stmt::For(l) => {
            enc.write_u8(4);
            enc.write_varint(l.var.into());
            write_expr(enc, &l.start);
            write_expr(enc, &l.end);
            enc.write_bool(l.inclusive);
            enc.write_varint(l.max.into());
            enc.write_bool(l.guarded);
            write_block(enc, &l.body);
        }
        Stmt::Break => enc.write_u8(5),
        Stmt::Continue => enc.write_u8(6),
        Stmt::Return(None) => enc.write_u8(7),
        Stmt::Return(Some(e)) => {
            enc.write_u8(8);
            write_expr(enc, e);
        }
        Stmt::Discard => enc.write_u8(9),
    }
}

fn read_stmt(dec: &mut Decoder<'_>, depth: u32) -> Result<Stmt, DecodeError> {
    Ok(match dec.read_u8()? {
        0 => Stmt::Let(u32_of(dec)?, read_expr(dec, depth)?),
        1 => Stmt::Declare(u32_of(dec)?),
        2 => {
            let root = match dec.read_u8()? {
                0 => Root::Local(u32_of(dec)?),
                1 => Root::Varying(u32_of(dec)?),
                _ => return Err(malformed(dec)),
            };
            let path = list(dec, u32_of)?;
            Stmt::Assign(Place { root, path }, read_expr(dec, depth)?)
        }
        3 => Stmt::If(
            read_expr(dec, depth)?,
            read_block(dec, depth)?,
            read_block(dec, depth)?,
        ),
        4 => Stmt::For(Box::new(Loop {
            var: u32_of(dec)?,
            start: read_expr(dec, depth)?,
            end: read_expr(dec, depth)?,
            inclusive: dec.read_bool()?,
            max: u32_of(dec)?,
            guarded: dec.read_bool()?,
            body: read_block(dec, depth)?,
        })),
        5 => Stmt::Break,
        6 => Stmt::Continue,
        7 => Stmt::Return(None),
        8 => Stmt::Return(Some(read_expr(dec, depth)?)),
        9 => Stmt::Discard,
        _ => return Err(malformed(dec)),
    })
}

const BUILTINS: [Builtin; 3] = [Builtin::VertexId, Builtin::InstanceId, Builtin::FragCoord];

fn write_function(enc: &mut Encoder, f: &Function) {
    enc.write_str(&f.name);
    enc.write_u8(match f.stage {
        None => 0,
        Some(Stage::Vertex) => 1,
        Some(Stage::Fragment) => 2,
    });
    enc.write_varint(f.params.into());
    write_list(enc, &f.builtins, |enc, b| {
        enc.write_varint(tag(&BUILTINS, b))
    });
    write_ty(enc, f.ret);
    write_list(enc, &f.locals, |enc, l| {
        enc.write_str(&l.name);
        write_ty(enc, l.ty);
        enc.write_bool(l.mutable);
    });
    write_block(enc, &f.body);
}

fn read_function(dec: &mut Decoder<'_>) -> Result<Function, DecodeError> {
    Ok(Function {
        name: name(dec)?,
        stage: match dec.read_u8()? {
            0 => None,
            1 => Some(Stage::Vertex),
            2 => Some(Stage::Fragment),
            _ => return Err(malformed(dec)),
        },
        params: u32_of(dec)?,
        builtins: list(dec, |dec| pick(dec, &BUILTINS))?,
        ret: read_ty(dec)?,
        locals: list(dec, |dec| {
            Ok(Local {
                name: name(dec)?,
                ty: read_ty(dec)?,
                mutable: dec.read_bool()?,
            })
        })?,
        body: read_block(dec, 0)?,
        span: Span::default(),
    })
}

fn write_entry(enc: &mut Encoder, f: Option<&Function>) {
    enc.write_bool(f.is_some());
    if let Some(f) = f {
        write_function(enc, f);
    }
}

fn read_entry(dec: &mut Decoder<'_>) -> Result<Option<Function>, DecodeError> {
    Ok(if dec.read_bool()? {
        Some(read_function(dec)?)
    } else {
        None
    })
}

/// Without its spans.
impl Encode for Program {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_str(&self.name);
        write_list(enc, &self.records, |enc, r| {
            enc.write_str(&r.name);
            write_list(enc, &r.fields, write_binding);
        });
        for members in [
            &self.uniforms,
            &self.instance,
            &self.varyings,
            &self.textures,
            &self.samplers,
        ] {
            write_list(enc, members, write_binding);
        }
        write_list(enc, &self.functions, write_function);
        write_entry(enc, self.vertex.as_ref());
        write_entry(enc, self.fragment.as_ref());
    }
}

impl Decode for Program {
    fn decode(dec: &mut Decoder<'_>) -> Result<Program, DecodeError> {
        Ok(Program {
            name: name(dec)?,
            records: list(dec, |dec| {
                Ok(Record {
                    name: name(dec)?,
                    fields: list(dec, read_binding)?,
                    span: Span::default(),
                })
            })?,
            uniforms: list(dec, read_binding)?,
            instance: list(dec, read_binding)?,
            varyings: list(dec, read_binding)?,
            textures: list(dec, read_binding)?,
            samplers: list(dec, read_binding)?,
            functions: list(dec, read_function)?,
            vertex: read_entry(dec)?,
            fragment: read_entry(dec)?,
        })
    }
}

const FILTERS: [FilterMode; 3] = [
    FilterMode::Nearest,
    FilterMode::Linear,
    FilterMode::MipmapLinear,
];

const ADDRESSES: [AddressMode; 3] = [
    AddressMode::ClampToEdge,
    AddressMode::Repeat,
    AddressMode::Mirror,
];

const KINDS: [ParamKind; 6] = [
    ParamKind::F32,
    ParamKind::I32,
    ParamKind::U32,
    ParamKind::Vec2F32,
    ParamKind::Vec3F32,
    ParamKind::ColorLinear,
];

impl Encode for BrushLayout {
    fn encode(&self, enc: &mut Encoder) {
        for v in [
            self.stride,
            self.rect_pos,
            self.rect_size,
            self.uniform_size,
            self.viewport_size,
        ] {
            enc.write_varint(v.into());
        }
        // `0` for a brush that does not read the clock, else its offset + 1.
        enc.write_varint(self.time.map_or(0, |at| u64::from(at) + 1));
        write_list(enc, &self.params, |enc, p| {
            enc.write_str(&p.name);
            enc.write_varint(tag(&KINDS, &p.kind));
            enc.write_varint(p.offset.into());
            enc.write_u64(p.stable_field_id);
        });
        write_list(enc, &self.textures, |enc, t| enc.write_str(&t.name));
        write_list(enc, &self.samplers, |enc, s| {
            enc.write_varint(tag(&FILTERS, &s.filter));
            enc.write_varint(tag(&ADDRESSES, &s.address));
        });
        enc.write_u8(u8::from(self.vertex_textures));
    }
}

impl Decode for BrushLayout {
    fn decode(dec: &mut Decoder<'_>) -> Result<BrushLayout, DecodeError> {
        Ok(BrushLayout {
            stride: u32_of(dec)?,
            rect_pos: u32_of(dec)?,
            rect_size: u32_of(dec)?,
            uniform_size: u32_of(dec)?,
            viewport_size: u32_of(dec)?,
            time: match u32_of(dec)? {
                0 => None,
                at => Some(at - 1),
            },
            params: list(dec, |dec| {
                Ok(BrushParam {
                    name: name(dec)?,
                    kind: pick(dec, &KINDS)?,
                    offset: u32_of(dec)?,
                    stable_field_id: dec.read_u64()?,
                    source_span: Span::default(),
                })
            })?,
            textures: list(dec, |dec| {
                Ok(BrushTexture {
                    name: name(dec)?,
                    source_span: Span::default(),
                })
            })?,
            samplers: list(dec, |dec| {
                Ok(SamplerDesc {
                    filter: pick(dec, &FILTERS)?,
                    address: pick(dec, &ADDRESSES)?,
                })
            })?,
            vertex_textures: match dec.read_u8()? {
                0 => false,
                1 => true,
                _ => return Err(malformed(dec)),
            },
        })
    }
}

impl Encode for PackedBrush {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_str(&self.name);
        Encode::encode(&self.layout, enc);
        enc.write_varint(self.code.len() as u64);
        for code in &self.code {
            match code {
                PackedCode::Text(lang, text) => {
                    enc.write_u8(match lang {
                        ShaderLang::Msl => 0,
                        ShaderLang::Wgsl => 1,
                        _ => 2,
                    });
                    enc.write_str(text);
                }
                PackedCode::SpirV(bytes) => {
                    enc.write_u8(3);
                    enc.write_bytes(bytes);
                }
                PackedCode::Program(program) => {
                    enc.write_u8(4);
                    program.encode(enc);
                }
            }
        }
    }
}

impl Decode for PackedBrush {
    /// # Errors
    ///
    /// Malformed bytes, a language carried twice, a program the validator
    /// rejects, or a layout that is not the program's own.
    fn decode(dec: &mut Decoder<'_>) -> Result<PackedBrush, DecodeError> {
        let name = name(dec)?;
        let layout = BrushLayout::decode(dec)?;
        let text = |dec: &mut Decoder<'_>| Ok::<_, DecodeError>(dec.read_str()?.to_owned());
        let count = len(dec)?;
        let mut code: Vec<PackedCode> = Vec::with_capacity(count.min(LANGS));
        for _ in 0..count {
            let at = dec.position();
            let next = match dec.read_u8()? {
                0 => PackedCode::Text(ShaderLang::Msl, text(dec)?),
                1 => PackedCode::Text(ShaderLang::Wgsl, text(dec)?),
                2 => PackedCode::Text(ShaderLang::Hlsl, text(dec)?),
                3 => PackedCode::SpirV(dec.read_bytes()?.to_vec()),
                4 => {
                    let program = Program::decode(dec)?;
                    // The samplers' state is the shader's, not the program's:
                    // the rest of the layout must be the program's own.
                    let own = program.validate().is_ok()
                        && BrushLayout::new(&program, &program.interface()).is_ok_and(|mut own| {
                            if own.samplers.len() == layout.samplers.len() {
                                own.samplers.clone_from(&layout.samplers);
                            }
                            own == layout
                        });
                    if !own {
                        return Err(DecodeError::Malformed { offset: at });
                    }
                    PackedCode::Program(Box::new(program))
                }
                _ => return Err(DecodeError::Malformed { offset: at }),
            };
            if code.iter().any(|c| c.lang() == next.lang()) {
                return Err(DecodeError::Malformed { offset: at });
            }
            code.push(next);
        }
        Ok(PackedBrush { name, layout, code })
    }
}
