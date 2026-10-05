//! A type as the runtime converts its values: the [`ValueSchema`] a hot
//! reload and a persisted state retype through.

use viso_behavior::StableId;
use viso_behavior::retype::{
    DeclBody, FieldDesc, IntType, PayloadDesc, TypeDecl, TypeDesc, ValueSchema, VariantDesc,
};

use super::{FieldInfo, TypeSchemas, VariantPayload, ty_name};
use crate::hir::ty::Ty;
use crate::resolve::SymbolId;

impl TypeSchemas {
    /// `ty` as a value schema, each record and enum it reaches described by
    /// these declarations, and the chunk computing field `index` of record
    /// `record`'s default given by `default(record, index)`.
    pub fn value_schema(
        &self,
        ty: &Ty,
        default: &dyn Fn(SymbolId, u32) -> Option<u32>,
    ) -> ValueSchema {
        let mut builder = Builder {
            schemas: self,
            default,
            symbols: Vec::new(),
            decls: Vec::new(),
        };
        let root = builder.desc(ty);
        ValueSchema {
            root,
            decls: builder.decls.into(),
        }
    }
}

struct Builder<'a> {
    schemas: &'a TypeSchemas,
    default: &'a dyn Fn(SymbolId, u32) -> Option<u32>,
    /// The symbol of each declaration, by index.
    symbols: Vec<SymbolId>,
    decls: Vec<TypeDecl>,
}

impl Builder<'_> {
    fn desc(&mut self, ty: &Ty) -> TypeDesc {
        let int = |bits, signed| TypeDesc::Int(IntType { bits, signed });
        let boxed = |b: &mut Self, t: &Ty| Box::new(b.desc(t));
        match ty {
            Ty::I8 => int(8, true),
            Ty::I16 => int(16, true),
            Ty::I32 => int(32, true),
            Ty::I64 => int(64, true),
            Ty::U8 => int(8, false),
            Ty::U16 => int(16, false),
            Ty::U32 => int(32, false),
            Ty::U64 => int(64, false),
            Ty::F32 => TypeDesc::F32,
            Ty::F64 => TypeDesc::F64,
            Ty::Unit => TypeDesc::Unit,
            Ty::Native(id) => TypeDesc::Native(id.0),
            Ty::Tuple(items) => TypeDesc::Tuple(self.all(items)),
            Ty::Fn(params, ret) => TypeDesc::Fn(self.all(params), boxed(self, ret)),
            Ty::List(t) => TypeDesc::List(boxed(self, t)),
            Ty::Option(t) => TypeDesc::Option(boxed(self, t)),
            Ty::Range(t) => TypeDesc::Range(boxed(self, t)),
            Ty::RangeInclusive(t) => TypeDesc::RangeInclusive(boxed(self, t)),
            Ty::Result(a, b) => TypeDesc::Result(boxed(self, a), boxed(self, b)),
            Ty::Resource(a, b) | Ty::ResourceState(a, b) => {
                TypeDesc::Resource(boxed(self, a), boxed(self, b))
            }
            Ty::Named(id) => TypeDesc::Named(self.declaration(*id)),
            other => TypeDesc::Plain(ty_name(other).into()),
        }
    }

    fn all(&mut self, items: &[Ty]) -> Box<[TypeDesc]> {
        items.iter().map(|t| self.desc(t)).collect()
    }

    /// The index of `symbol`'s declaration, described on first reach.
    fn declaration(&mut self, symbol: SymbolId) -> u32 {
        if let Some(i) = self.symbols.iter().position(|&s| s == symbol) {
            return i as u32;
        }
        let index = self.decls.len();
        self.symbols.push(symbol);
        self.decls.push(TypeDecl {
            id: StableId {
                hi: symbol.hi,
                lo: symbol.lo,
            },
            name: self
                .schemas
                .names
                .get(&symbol)
                .map_or("", String::as_str)
                .into(),
            body: DeclBody::Opaque,
        });
        let schemas = self.schemas;
        let body = if let Some(fields) = schemas.records.get(&symbol) {
            DeclBody::Record(self.fields(Some(symbol), fields))
        } else if let Some(variants) = schemas.enums.get(&symbol) {
            DeclBody::Enum(
                variants
                    .iter()
                    .map(|v| VariantDesc {
                        name: v.name.as_str().into(),
                        payload: match &v.payload {
                            VariantPayload::Unit => PayloadDesc::Unit,
                            VariantPayload::Tuple(items) => PayloadDesc::Tuple(self.all(items)),
                            VariantPayload::Record(fields) => {
                                PayloadDesc::Record(self.fields(None, fields))
                            }
                        },
                    })
                    .collect(),
            )
        } else {
            DeclBody::Opaque
        };
        self.decls[index].body = body;
        index as u32
    }

    /// `fields` described, a record's defaulted ones with their chunks.
    fn fields(&mut self, record: Option<SymbolId>, fields: &[FieldInfo]) -> Box<[FieldDesc]> {
        fields
            .iter()
            .enumerate()
            .map(|(index, f)| FieldDesc {
                name: f.name.as_str().into(),
                ty: self.desc(&f.ty),
                default: record
                    .filter(|_| f.has_default)
                    .and_then(|record| (self.default)(record, index as u32)),
            })
            .collect()
    }
}
