//! The package's audio messages: its one `@derive(AudioCommand)` enum is the
//! type `send_audio` takes and `AudioCommands.audio_command` receives
//! (`SchemaTy::AudioCommand`), its one `@derive(AudioEvent)` enum the type
//! `AudioBlock.send` takes and `AudioListener.audio_event` receives
//! (`SchemaTy::AudioEvent`); a package deriving neither uses `()`. A second
//! deriving enum is `E2202`. A command crosses to the audio thread as plain
//! data, so its payloads hold only scalars, unit variants, records and
//! tuples of them, at most [`MESSAGE_TOKENS`] tokens a value (`E2201`); an
//! event is built on the audio thread, which allocates nothing, so it
//! carries no payload (`E2201`, the schema-derive rule).

use viso_behavior::game::{AUDIO_COMMAND_DERIVE, MESSAGE_TOKENS};

use crate::diag::{Diagnostic, Related};
use crate::hir::infer::{TypeEnv, VariantPayload};
use crate::resolve::SymbolId;
use crate::syntax::TextRange;

use super::{Declarations, ModuleEnv, ModuleScope, Ty};

/// One enum of the package deriving `AudioCommand` or `AudioEvent`.
pub(super) struct AudioDecl {
    symbol: SymbolId,
    module: Option<usize>,
    at: TextRange,
}

/// Records `symbol`, whose `@derive` names `derive` at `at`.
pub(super) fn collect(
    derive: &str,
    symbol: SymbolId,
    at: TextRange,
    scope: &ModuleScope,
    decls: &mut Declarations,
) {
    let decl = AudioDecl {
        symbol,
        module: scope.home,
        at,
    };
    if derive == AUDIO_COMMAND_DERIVE {
        decls.audio_commands.push(decl);
    } else {
        decls.audio_events.push(decl);
    }
}

/// The type of the package's audio commands or events: its first deriving
/// enum, or `()`.
pub(super) fn message_type(derived: &[AudioDecl]) -> Ty {
    derived.first().map_or(Ty::Unit, |d| Ty::named(d.symbol))
}

/// Checks the `derive` of `symbol` at `at`: only the package's first enum
/// derives it (`E2202`), and a command's payloads are plain data that fits
/// a message (`E2201`).
pub(super) fn check(
    derive: &str,
    symbol: SymbolId,
    at: TextRange,
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let command = derive == AUDIO_COMMAND_DERIVE;
    let derived = if command {
        &env.decls.audio_commands
    } else {
        &env.decls.audio_events
    };
    let Some(index) = derived
        .iter()
        .position(|d| d.symbol == symbol && d.at == at)
    else {
        return;
    };
    if index > 0 {
        let first = &derived[0];
        let mut diagnostic = Diagnostic::error(
            "E2202",
            at,
            format!("the package already derives `{derive}`: one enum is its audio message type"),
        );
        if first.module == derived[index].module {
            diagnostic.related.push(Related::new(
                first.at,
                format!("the enum derives `{derive}` here"),
            ));
        }
        diagnostics.push(diagnostic);
        return;
    }
    if !command {
        return;
    }
    for variant in env.enum_variants(symbol).unwrap_or_default() {
        let tokens = match &variant.payload {
            VariantPayload::Unit => Some(1),
            VariantPayload::Tuple(tys) => tys
                .iter()
                .try_fold(1, |n, ty| Some(n + tokens(ty, env, 0)?)),
            VariantPayload::Record(fields) => fields
                .iter()
                .try_fold(1, |n, f| Some(n + tokens(&f.ty, env, 0)?)),
        };
        let problem = match tokens {
            None => format!(
                "`{}` carries data that is not plain: an audio command crosses to the audio \
                 thread by copy, so it holds numbers, `Bool`s, `Char`s, unit variants and \
                 records or tuples of them",
                variant.name
            ),
            Some(n) if n > MESSAGE_TOKENS => format!(
                "`{}` carries {n} values; an audio command holds at most {MESSAGE_TOKENS}, \
                 counting each record, tuple and the variant itself",
                variant.name
            ),
            Some(_) => continue,
        };
        let mut diagnostic = Diagnostic::error("E2201", at, problem);
        diagnostic
            .related
            .push(Related::new(variant.declared_at, "this variant"));
        diagnostics.push(diagnostic);
    }
}

/// The tokens a value of `ty` flattens into, `None` when it is not plain.
fn tokens(ty: &Ty, env: &ModuleEnv<'_>, depth: u32) -> Option<usize> {
    if depth > 8 {
        return None;
    }
    match ty {
        Ty::Bool
        | Ty::I8
        | Ty::I16
        | Ty::I32
        | Ty::I64
        | Ty::U8
        | Ty::U16
        | Ty::U32
        | Ty::U64
        | Ty::F32
        | Ty::F64
        | Ty::Char
        | Ty::Unit
        | Ty::Color
        | Ty::Dp
        | Ty::Px
        | Ty::Sp
        | Ty::Duration
        | Ty::Angle
        | Ty::Frequency => Some(1),
        Ty::Tuple(tys) => tys
            .iter()
            .try_fold(1, |n, ty| Some(n + tokens(ty, env, depth + 1)?)),
        Ty::Named(symbol, ..) => {
            if let Some(fields) = env.record_fields(*symbol) {
                return fields
                    .iter()
                    .try_fold(1, |n, f| Some(n + tokens(&f.ty, env, depth + 1)?));
            }
            let variants = env.enum_variants(*symbol)?;
            variants
                .iter()
                .all(|v| v.payload == VariantPayload::Unit)
                .then_some(1)
        }
        _ => None,
    }
}
