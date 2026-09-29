//! Chunks, component layouts and the verified module.

use std::fmt;

use crate::op::Op;
use crate::value::Value;

/// A byte range in a chunk's source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    /// The first byte.
    pub start: u32,
    /// One past the last byte.
    pub end: u32,
}

/// What a chunk runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkKind {
    /// A `fn`.
    Fn,
    /// An `action`.
    Action,
    /// A closure expression.
    Closure,
    /// A `computed` value.
    Computed,
    /// A `state` initializer.
    StateInit,
    /// An `input` default.
    InputDefault,
    /// A `const` value.
    Const,
    /// A record field default.
    FieldDefault,
    /// A view event handler: the payload arrives in `r0`, then the bindings of
    /// every enclosing `for`/`match` region the handler reads.
    Handler,
}

/// A runnable body.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Code {
    /// The instructions.
    pub ops: Box<[Op]>,
    /// The constant pool [`Op::Const`] and [`Op::DisplayDim`] index.
    pub consts: Box<[Value]>,
    /// The operand table variable-operand instructions index.
    pub ext: Box<[u32]>,
    /// The source span of each instruction, index-parallel to `ops`.
    pub spans: Box<[Span]>,
}

/// One function of a module.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    /// The declared name (`Component.member` for a component member).
    pub name: Box<str>,
    /// What it runs.
    pub kind: ChunkKind,
    /// The index of the source file it is declared in.
    pub module: u32,
    /// The number of arguments, arriving in `r0..`.
    pub params: u16,
    /// The frame size.
    pub regs: u16,
    /// The registers a closure's captured values arrive in, in capture order.
    pub captures: Box<[u16]>,
    /// The body, or why the compiler could not produce one; calling a chunk
    /// without a body faults.
    pub body: Result<Code, Box<str>>,
}

/// A component's runtime layout.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Component {
    /// Its name.
    pub name: Box<str>,
    /// The state names, by slot.
    pub states: Box<[Box<str>]>,
    /// The input names, by slot.
    pub inputs: Box<[Box<str>]>,
    /// The event names, by index.
    pub events: Box<[Box<str>]>,
    /// Each state slot's initializer chunk; a slot without one starts `Nil`.
    pub state_inits: Box<[Option<u32>]>,
    /// Each input slot's default chunk; an input without one is required.
    pub input_defaults: Box<[Option<u32>]>,
    /// The `fn`/`action`/`computed` members, by name.
    pub members: Box<[(Box<str>, u32)]>,
    /// The view's event handler chunks, in source order; a view node names
    /// its handlers by index into this table.
    pub handlers: Box<[u32]>,
}

impl Component {
    /// The member chunk named `name`.
    pub fn member(&self, name: &str) -> Option<u32> {
        self.members.iter().find(|(n, _)| &**n == name).map(|m| m.1)
    }

    /// The state slot named `name`.
    pub fn state(&self, name: &str) -> Option<usize> {
        self.states.iter().position(|s| &**s == name)
    }

    /// The input slot named `name`.
    pub fn input(&self, name: &str) -> Option<usize> {
        self.inputs.iter().position(|s| &**s == name)
    }
}

/// A native function a module calls: the path it was compiled against and
/// the hash of the schema signature it was checked with. Linking resolves it
/// against a registry once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeImport {
    /// The function's full path, such as `viso::text::upper`.
    pub path: Box<str>,
    /// The [`NativeFunction::signature`](crate::native::NativeFunction::signature)
    /// it was compiled against.
    pub signature: u64,
    /// The number of arguments a call passes.
    pub params: u16,
}

/// A verified set of chunks, component layouts and native imports.
///
/// Construction checks every register, jump target, operand-table range,
/// constant, chunk and native reference, state/input slot and event index,
/// so the interpreter only meets well-formed code.
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub(crate) chunks: Box<[Chunk]>,
    pub(crate) components: Box<[Component]>,
    pub(crate) natives: Box<[NativeImport]>,
}

/// Why a module failed verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyError {
    /// The chunk.
    pub chunk: u32,
    /// The instruction, if the fault is in one.
    pub pc: Option<u32>,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.pc {
            Some(pc) => write!(f, "chunk {} at {pc}: {}", self.chunk, self.message),
            None => write!(f, "chunk {}: {}", self.chunk, self.message),
        }
    }
}

impl std::error::Error for VerifyError {}

impl Module {
    /// Verifies and builds a module.
    pub fn new(
        chunks: Vec<Chunk>,
        components: Vec<Component>,
        natives: Vec<NativeImport>,
    ) -> Result<Module, VerifyError> {
        let module = Module {
            chunks: chunks.into(),
            components: components.into(),
            natives: natives.into(),
        };
        let mut max_states = 0;
        let mut max_inputs = 0;
        let mut max_events = 0;
        for component in module.components.iter() {
            max_states = max_states.max(component.states.len());
            max_inputs = max_inputs.max(component.inputs.len());
            max_events = max_events.max(component.events.len());
            let refs = component
                .state_inits
                .iter()
                .chain(component.input_defaults.iter())
                .flatten()
                .chain(component.members.iter().map(|m| &m.1))
                .chain(component.handlers.iter());
            for &chunk in refs {
                if chunk as usize >= module.chunks.len() {
                    return Err(VerifyError {
                        chunk,
                        pc: None,
                        message: format!("component `{}` names a missing chunk", component.name),
                    });
                }
            }
            for &chunk in component.handlers.iter() {
                let handler = &module.chunks[chunk as usize];
                if handler.kind != ChunkKind::Handler || handler.params == 0 {
                    return Err(VerifyError {
                        chunk,
                        pc: None,
                        message: format!(
                            "component `{}` names a handler chunk that does not take a payload",
                            component.name
                        ),
                    });
                }
            }
        }
        let limits = Limits {
            chunks: module
                .chunks
                .iter()
                .map(|c| (c.params, c.captures.len()))
                .collect(),
            natives: module.natives.iter().map(|n| n.params).collect(),
            states: max_states,
            inputs: max_inputs,
            events: max_events,
        };
        for (index, chunk) in module.chunks.iter().enumerate() {
            verify_chunk(index as u32, chunk, &limits)?;
        }
        Ok(module)
    }

    /// Every chunk.
    pub fn chunks(&self) -> &[Chunk] {
        &self.chunks
    }

    /// The chunk `index`.
    pub fn chunk(&self, index: u32) -> &Chunk {
        &self.chunks[index as usize]
    }

    /// Every component layout.
    pub fn components(&self) -> &[Component] {
        &self.components
    }

    /// The component named `name`, by index.
    pub fn component(&self, name: &str) -> Option<u32> {
        self.components
            .iter()
            .position(|c| &*c.name == name)
            .map(|i| i as u32)
    }

    /// The layout of component `index`.
    pub fn layout(&self, index: u32) -> &Component {
        &self.components[index as usize]
    }

    /// Every native import, by index.
    pub fn natives(&self) -> &[NativeImport] {
        &self.natives
    }
}

/// What instructions may reference.
struct Limits {
    /// Each chunk's parameter and capture counts.
    chunks: Vec<(u16, usize)>,
    /// Each native import's parameter count.
    natives: Vec<u16>,
    states: usize,
    inputs: usize,
    events: usize,
}

fn verify_chunk(index: u32, chunk: &Chunk, limits: &Limits) -> Result<(), VerifyError> {
    let fail = |pc: Option<usize>, message: String| VerifyError {
        chunk: index,
        pc: pc.map(|pc| pc as u32),
        message,
    };
    if chunk.params > chunk.regs {
        return Err(fail(None, "more parameters than registers".into()));
    }
    if let Some(r) = chunk.captures.iter().find(|r| **r >= chunk.regs) {
        return Err(fail(
            None,
            format!("capture register r{r} is out of the frame"),
        ));
    }
    let Ok(code) = &chunk.body else {
        return Ok(());
    };
    if code.spans.len() != code.ops.len() {
        return Err(fail(
            None,
            "the span table does not match the instructions".into(),
        ));
    }
    if code
        .consts
        .iter()
        .any(|c| matches!(c, Value::Closure(_) | Value::Handle(_)))
    {
        return Err(fail(None, "a constant that is not plain data".into()));
    }
    let Some(last) = code.ops.last() else {
        return Err(fail(None, "an empty body".into()));
    };
    if !matches!(last, Op::Return { .. } | Op::Jump { .. } | Op::Unreachable) {
        return Err(fail(
            Some(code.ops.len() - 1),
            "the body can run off its end".into(),
        ));
    }
    let v = Verifier {
        code,
        regs: chunk.regs,
        limits,
    };
    for (pc, op) in code.ops.iter().enumerate() {
        v.op(op).map_err(|message| fail(Some(pc), message))?;
    }
    Ok(())
}

struct Verifier<'a> {
    code: &'a Code,
    regs: u16,
    limits: &'a Limits,
}

type Check = Result<(), String>;

impl Verifier<'_> {
    fn reg(&self, r: u16) -> Check {
        if r < self.regs {
            Ok(())
        } else {
            Err(format!("register r{r} is out of the frame"))
        }
    }

    fn regs(&self, rs: &[u16]) -> Check {
        rs.iter().try_for_each(|r| self.reg(*r))
    }

    fn target(&self, target: u32) -> Check {
        if (target as usize) < self.code.ops.len() {
            Ok(())
        } else {
            Err(format!("jump target @{target} is out of the body"))
        }
    }

    fn slot(&self, slot: u32, limit: usize, what: &str) -> Check {
        if (slot as usize) < limit {
            Ok(())
        } else {
            Err(format!("{what} {slot} is out of range"))
        }
    }

    /// Checks that chunk `func` exists and takes `argc` arguments and `captures`
    /// captured values.
    fn chunk(&self, func: u32, argc: u32, captures: usize) -> Check {
        let Some(&(params, caps)) = self.limits.chunks.get(func as usize) else {
            return Err(format!("chunk {func} is out of range"));
        };
        if u32::from(params) != argc {
            return Err(format!("chunk {func} takes {params} arguments, not {argc}"));
        }
        if caps != captures {
            return Err(format!(
                "chunk {func} captures {caps} values, not {captures}"
            ));
        }
        Ok(())
    }

    fn constant(&self, index: u32) -> Check {
        self.slot(index, self.code.consts.len(), "constant")
    }

    /// The operand words `ext[at..at + n]`.
    fn words(&self, at: usize, n: usize) -> Result<&[u32], String> {
        at.checked_add(n)
            .and_then(|end| self.code.ext.get(at..end))
            .ok_or_else(|| "the operand table is too short".to_string())
    }

    /// A `head.., n, regs..` operand list: checks the `n` registers after `head`
    /// words and returns the head.
    fn list(&self, ext: u32, head: usize) -> Result<&[u32], String> {
        let at = ext as usize;
        let words = self.words(at, head + 1)?;
        let n = words[head] as usize;
        for &r in self.words(at + head + 1, n)? {
            self.reg16(r)?;
        }
        Ok(&words[..head])
    }

    fn reg16(&self, r: u32) -> Check {
        match u16::try_from(r) {
            Ok(r) => self.reg(r),
            Err(_) => Err(format!("register r{r} is out of the frame")),
        }
    }

    fn op(&self, op: &Op) -> Check {
        match *op {
            Op::Const { dst, index } => {
                self.reg(dst)?;
                self.constant(index)
            }
            Op::Int { dst, .. } | Op::Nil { dst } => self.reg(dst),
            Op::Move { dst, src }
            | Op::Neg { dst, src, .. }
            | Op::Not { dst, src }
            | Op::BitNot { dst, src, .. }
            | Op::Cast { dst, src, .. }
            | Op::Len { dst, src }
            | Op::Tag { dst, src }
            | Op::IsNil { dst, src }
            | Op::Display { dst, src, .. }
            | Op::Field { dst, src, .. } => self.regs(&[dst, src]),
            Op::DisplayDim { dst, src, suffix } => {
                self.regs(&[dst, src])?;
                self.constant(u32::from(suffix))?;
                match &self.code.consts[usize::from(suffix)] {
                    Value::Str(_) => Ok(()),
                    _ => Err("a unit suffix that is not a string".into()),
                }
            }
            Op::LoadState { dst, slot } => {
                self.reg(dst)?;
                self.slot(slot, self.limits.states, "state slot")
            }
            Op::StoreState { src, slot } => {
                self.reg(src)?;
                self.slot(slot, self.limits.states, "state slot")
            }
            Op::LoadInput { dst, slot } => {
                self.reg(dst)?;
                self.slot(slot, self.limits.inputs, "input slot")
            }
            Op::AddI64 { dst, a, b }
            | Op::SubI64 { dst, a, b }
            | Op::MulI64 { dst, a, b }
            | Op::LtI64 { dst, a, b }
            | Op::LeI64 { dst, a, b }
            | Op::AddF64 { dst, a, b }
            | Op::SubF64 { dst, a, b }
            | Op::MulF64 { dst, a, b }
            | Op::DivF64 { dst, a, b }
            | Op::LtF64 { dst, a, b }
            | Op::LeF64 { dst, a, b }
            | Op::Arith { dst, a, b, .. }
            | Op::Eq { dst, a, b }
            | Op::Ne { dst, a, b } => self.regs(&[dst, a, b]),
            Op::Index { dst, list, index } => self.regs(&[dst, list, index]),
            Op::Call { dst, ext } => {
                self.reg(dst)?;
                let head = self.list(ext, 1)?;
                self.chunk(head[0], self.code.ext[ext as usize + 1], 0)
            }
            Op::Native { dst, ext } => {
                self.reg(dst)?;
                let head = self.list(ext, 1)?;
                let argc = self.code.ext[ext as usize + 1];
                match self.limits.natives.get(head[0] as usize) {
                    None => Err(format!("native import {} is out of range", head[0])),
                    Some(&params) if u32::from(params) != argc => Err(format!(
                        "native import {} takes {params} arguments, not {argc}",
                        head[0]
                    )),
                    Some(_) => Ok(()),
                }
            }
            Op::CallValue { dst, ext } => {
                self.reg(dst)?;
                let head = self.list(ext, 1)?;
                self.reg16(head[0])
            }
            Op::Closure { dst, ext } => {
                self.reg(dst)?;
                let head = self.list(ext, 1)?;
                let func = head[0];
                let n = self.code.ext[ext as usize + 1] as usize;
                let params = self.limits.chunks.get(func as usize).map_or(0, |c| c.0);
                self.chunk(func, u32::from(params), n)
            }
            Op::Make { dst, ext } => {
                self.reg(dst)?;
                self.list(ext, 1).map(drop)
            }
            Op::List { dst, ext } | Op::Concat { dst, ext } => {
                self.reg(dst)?;
                self.list(ext, 0).map(drop)
            }
            Op::Emit { ext } => {
                let head = self.list(ext, 1)?;
                self.slot(head[0], self.limits.events, "event")
            }
            Op::SetPath { root, ext } => {
                self.reg(root)?;
                let at = ext as usize;
                let words = self.words(at, 2)?;
                self.reg16(words[0])?;
                let n = words[1] as usize;
                let steps = self.words(at + 2, n.checked_mul(2).ok_or("too many steps")?)?;
                for &[kind, arg] in steps.as_chunks::<2>().0 {
                    match kind {
                        0 => {}
                        1 => self.reg16(arg)?,
                        kind => return Err(format!("unknown path step kind {kind}")),
                    }
                }
                Ok(())
            }
            Op::Jump { target } => self.target(target),
            Op::JumpIf { cond, target } | Op::JumpUnless { cond, target } => {
                self.reg(cond)?;
                self.target(target)
            }
            Op::Switch { src, ext } => {
                self.reg(src)?;
                let at = ext as usize;
                let words = self.words(at, 4)?;
                self.target(words[2])?;
                let n = words[3] as usize;
                for &t in self.words(at + 4, n)? {
                    self.target(t)?;
                }
                Ok(())
            }
            Op::Return { src } => self.reg(src),
            Op::Unreachable => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(params: u16, regs: u16, ops: Vec<Op>) -> Chunk {
        let spans = vec![Span::default(); ops.len()].into();
        Chunk {
            name: "f".into(),
            kind: ChunkKind::Fn,
            module: 0,
            params,
            regs,
            captures: Box::new([]),
            body: Ok(Code {
                ops: ops.into(),
                consts: Box::new([]),
                ext: Box::new([]),
                spans,
            }),
        }
    }

    fn verify(chunks: Vec<Chunk>) -> Result<Module, VerifyError> {
        Module::new(chunks, Vec::new(), Vec::new())
    }

    #[test]
    fn well_formed_code_verifies() {
        let ops = vec![Op::Int { dst: 0, value: 1 }, Op::Return { src: 0 }];
        assert!(verify(vec![chunk(0, 1, ops)]).is_ok());
    }

    #[test]
    fn a_register_outside_the_frame_is_rejected() {
        let ops = vec![Op::Move { dst: 0, src: 2 }, Op::Return { src: 0 }];
        let e = verify(vec![chunk(0, 2, ops)]).unwrap_err();
        assert_eq!(
            (e.pc, e.message.as_str()),
            (Some(0), "register r2 is out of the frame")
        );
    }

    #[test]
    fn a_jump_outside_the_body_is_rejected() {
        let e = verify(vec![chunk(0, 1, vec![Op::Jump { target: 1 }])]).unwrap_err();
        assert_eq!(e.message, "jump target @1 is out of the body");
    }

    #[test]
    fn a_body_that_can_run_off_its_end_is_rejected() {
        let e = verify(vec![chunk(0, 1, vec![Op::Nil { dst: 0 }])]).unwrap_err();
        assert_eq!(e.message, "the body can run off its end");
    }

    #[test]
    fn calls_are_checked_against_the_callee_arity() {
        let mut caller = chunk(
            0,
            1,
            vec![Op::Call { dst: 0, ext: 0 }, Op::Return { src: 0 }],
        );
        // `f(r0)` against a callee without parameters.
        if let Ok(code) = &mut caller.body {
            code.ext = Box::new([1, 1, 0]);
        }
        let callee = chunk(0, 1, vec![Op::Nil { dst: 0 }, Op::Return { src: 0 }]);
        let e = verify(vec![caller, callee]).unwrap_err();
        assert_eq!(e.message, "chunk 1 takes 0 arguments, not 1");
    }

    #[test]
    fn a_state_slot_outside_every_component_is_rejected() {
        let ops = vec![Op::LoadState { dst: 0, slot: 0 }, Op::Return { src: 0 }];
        let e = verify(vec![chunk(0, 1, ops)]).unwrap_err();
        assert_eq!(e.message, "state slot 0 is out of range");
    }
}
