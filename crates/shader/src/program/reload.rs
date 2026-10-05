//! Shader hot reload: an edited program compiles on a worker thread while the
//! current pipeline keeps drawing, and the host swaps at a frame boundary.
//!
//! [`ProgramReload::submit`] hands the worker a new source; the worker skips
//! to the newest source it has, lowers it to a program, validates it, lays it
//! out and compiles it for the backend. [`ProgramReload::poll`], called
//! between frames, takes the newest outcome:
//!
//! - a success swaps in the new pipeline and interface and hands back the old
//!   pipeline for retirement after the GPU is done with it; when the instance
//!   or uniform layout changed, the [`Swap`] carries the [`Migration`] that
//!   re-encodes the host's blocks, so the new buffer and the new pipeline go
//!   live in the same frame;
//! - a failure keeps the current pipeline and reports the front end's and the
//!   validator's spans, or the backend's log.
//!
//! An outcome for a source a newer submission has superseded is dropped.

use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread::JoinHandle;

use super::layout::{BlockLayout, ShaderInterface};
use super::{Binding, Program, ProgramError, Value};

/// Why a reload did not go live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReloadError {
    /// The front end or the validator rejected the program, at these spans.
    Program(Vec<ProgramError>),
    /// The backend compiler rejected the generated source.
    Backend(String),
}

/// The program that is live, laid out, with its pipeline.
#[derive(Debug)]
pub struct Live<P> {
    pub program: Program,
    pub interface: ShaderInterface,
    pub pipeline: P,
}

/// How a block's members carry over to a new layout: each new member takes the
/// old member of the same name and type, or starts at zero.
#[derive(Debug, Clone, PartialEq)]
pub struct Migration {
    old_program: Program,
    old_members: Vec<Binding>,
    old_layout: BlockLayout,
    new_program: Program,
    new_members: Vec<Binding>,
    new_layout: BlockLayout,
    /// Per new member, the old member it takes.
    from: Vec<Option<usize>>,
}

impl Migration {
    fn new(
        old: &Live<impl Sized>,
        new: &Program,
        new_interface: &ShaderInterface,
        uniforms: bool,
    ) -> Migration {
        let pick = |p: &Program, i: &ShaderInterface| {
            if uniforms {
                (p.uniforms.clone(), i.uniforms.clone())
            } else {
                (p.instance.clone(), i.instance.clone())
            }
        };
        let (old_members, old_layout) = pick(&old.program, &old.interface);
        let (new_members, new_layout) = pick(new, new_interface);
        let from = new_members
            .iter()
            .map(|m| {
                old_members
                    .iter()
                    .position(|o| o.name == m.name && old.program.spell(o.ty) == new.spell(m.ty))
            })
            .collect();
        Migration {
            old_program: old.program.clone(),
            old_members,
            old_layout,
            new_program: new.clone(),
            new_members,
            new_layout,
            from,
        }
    }

    /// The new block's members from the old block's.
    pub fn values(&self, old: &[Value]) -> Vec<Value> {
        self.new_members
            .iter()
            .zip(&self.from)
            .map(|(m, from)| match from {
                Some(i) => old[*i].clone(),
                None => Value::zero(&self.new_program, m.ty),
            })
            .collect()
    }

    /// `old`, blocks in the old layout one after another, re-encoded in the
    /// new layout.
    pub fn reencode(&self, old: &[u8]) -> Vec<u8> {
        let (old_size, new_size) = (self.old_layout.size as usize, self.new_layout.size as usize);
        let count = old.len().checked_div(old_size).unwrap_or(0);
        let mut out = Vec::with_capacity(count * new_size);
        for block in old.chunks_exact(old_size.max(1)).take(count) {
            let values = self
                .old_layout
                .decode(&self.old_program, &self.old_members, block);
            let encoded = self
                .new_layout
                .encode(&self.new_members, &self.values(&values))
                .unwrap_or_else(|_| vec![0; new_size]);
            out.extend_from_slice(&encoded);
        }
        out
    }
}

/// A reload that went live.
#[derive(Debug)]
pub struct Swap<P> {
    /// The pipeline the swap replaced; retire it once the frames using it end.
    pub retired: P,
    /// Set when the instance layout changed: re-encode the instance buffer
    /// before the next draw.
    pub instance: Option<Box<Migration>>,
    /// Set when the uniform layout changed.
    pub uniforms: Option<Box<Migration>>,
}

/// What [`ProgramReload::poll`] saw.
#[derive(Debug)]
pub enum ReloadEvent<P> {
    Swapped(Swap<P>),
    /// The current pipeline stays.
    Failed(ReloadError),
}

struct Job<S> {
    generation: u64,
    source: S,
}

struct Outcome<P> {
    generation: u64,
    result: Result<(Program, ShaderInterface, P), ReloadError>,
}

/// A program the worker compiles and the host swaps in between frames.
pub struct ProgramReload<S, P> {
    live: Live<P>,
    jobs: Option<Sender<Job<S>>>,
    outcomes: Receiver<Outcome<P>>,
    submitted: u64,
    /// The newest generation an outcome arrived for.
    settled: u64,
    worker: Option<JoinHandle<()>>,
}

impl<S: Send + 'static, P: Send + 'static> ProgramReload<S, P> {
    /// Compiles `source` now, then starts the worker. `lower` turns a source
    /// into a program; `compile` builds the backend pipeline of a laid-out
    /// program. Both run on the worker for later sources.
    ///
    /// # Errors
    ///
    /// Why the first source does not build; there is no pipeline to keep.
    pub fn new<L, C>(source: S, mut lower: L, mut compile: C) -> Result<Self, ReloadError>
    where
        L: FnMut(&S) -> Result<Program, Vec<ProgramError>> + Send + 'static,
        C: FnMut(&Program, &ShaderInterface) -> Result<P, String> + Send + 'static,
    {
        let (program, interface, pipeline) = build(&source, &mut lower, &mut compile)?;
        let (jobs, inbox) = channel::<Job<S>>();
        let (outbox, outcomes) = channel();
        let worker = std::thread::Builder::new()
            .name("viso-shader-reload".into())
            .spawn(move || {
                while let Ok(mut job) = inbox.recv() {
                    // Only the newest edit is worth compiling.
                    loop {
                        match inbox.try_recv() {
                            Ok(newer) => job = newer,
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => return,
                        }
                    }
                    let result = build(&job.source, &mut lower, &mut compile);
                    if outbox
                        .send(Outcome {
                            generation: job.generation,
                            result,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            })
            .expect("the shader reload worker starts");
        Ok(ProgramReload {
            live: Live {
                program,
                interface,
                pipeline,
            },
            jobs: Some(jobs),
            outcomes,
            submitted: 0,
            settled: 0,
            worker: Some(worker),
        })
    }

    /// Queues `source`, superseding any source not yet live.
    pub fn submit(&mut self, source: S) {
        self.submitted += 1;
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(Job {
                generation: self.submitted,
                source,
            });
        }
    }

    /// The live program and pipeline.
    pub fn live(&self) -> &Live<P> {
        &self.live
    }

    /// Whether a submitted source has no outcome yet.
    pub fn pending(&self) -> bool {
        self.settled < self.submitted
    }

    /// At a frame boundary: the outcome of the newest submitted source, once
    /// it is ready.
    pub fn poll(&mut self) -> Option<ReloadEvent<P>> {
        let mut newest = None;
        while let Ok(outcome) = self.outcomes.try_recv() {
            newest = Some(outcome);
        }
        let outcome = newest?;
        self.settled = outcome.generation;
        if outcome.generation < self.submitted {
            // A newer source is on its way.
            return None;
        }
        Some(match outcome.result {
            Err(error) => ReloadEvent::Failed(error),
            Ok((program, interface, pipeline)) => {
                let changed = |a: &BlockLayout, b: &BlockLayout| {
                    a.size != b.size
                        || a.fields.len() != b.fields.len()
                        || a.fields
                            .iter()
                            .zip(&b.fields)
                            .any(|(x, y)| (&x.name, x.ty, x.offset) != (&y.name, y.ty, y.offset))
                };
                let instance = changed(&self.live.interface.instance, &interface.instance)
                    .then(|| Box::new(Migration::new(&self.live, &program, &interface, false)));
                let uniforms = changed(&self.live.interface.uniforms, &interface.uniforms)
                    .then(|| Box::new(Migration::new(&self.live, &program, &interface, true)));
                let old = std::mem::replace(
                    &mut self.live,
                    Live {
                        program,
                        interface,
                        pipeline,
                    },
                );
                ReloadEvent::Swapped(Swap {
                    retired: old.pipeline,
                    instance,
                    uniforms,
                })
            }
        })
    }
}

impl<S, P> Drop for ProgramReload<S, P> {
    fn drop(&mut self) {
        self.jobs = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn build<S, P>(
    source: &S,
    lower: &mut impl FnMut(&S) -> Result<Program, Vec<ProgramError>>,
    compile: &mut impl FnMut(&Program, &ShaderInterface) -> Result<P, String>,
) -> Result<(Program, ShaderInterface, P), ReloadError> {
    let program = lower(source).map_err(ReloadError::Program)?;
    program.validate().map_err(ReloadError::Program)?;
    let interface = program.interface();
    let pipeline = compile(&program, &interface).map_err(ReloadError::Backend)?;
    Ok((program, interface, pipeline))
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::sync_channel;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::program::{Block, Expr, ExprKind, Function, Span, Stage, Stmt, Ty};

    fn binding(name: &str, ty: Ty) -> Binding {
        Binding {
            name: name.into(),
            ty,
            span: Span::default(),
        }
    }

    fn entry(stage: Stage, ret: Ty, value: Expr) -> Function {
        Function {
            name: format!("{stage:?}").into(),
            stage: Some(stage),
            params: 0,
            builtins: Vec::new(),
            ret,
            locals: Vec::new(),
            body: Block(vec![Stmt::Return(Some(value))]),
            span: Span::default(),
        }
    }

    /// A program named `name` with `instance` members that draws nothing.
    fn program(name: &str, instance: Vec<Binding>) -> Program {
        let one = Expr::new(
            Ty::VEC4,
            ExprKind::Construct(vec![Expr::new(Ty::F32, ExprKind::F32(1.0))]),
        );
        Program {
            name: name.into(),
            instance,
            vertex: Some(entry(
                Stage::Vertex,
                Ty::VertexOutput,
                Expr::new(Ty::VertexOutput, ExprKind::Construct(vec![one.clone()])),
            )),
            fragment: Some(entry(Stage::Fragment, Ty::VEC4, one)),
            ..Program::default()
        }
    }

    fn lower(p: &Program) -> Result<Program, Vec<ProgramError>> {
        if &*p.name == "bad" {
            return Err(vec![ProgramError::new(
                "E8105",
                Span { start: 3, end: 7 },
                "not in the subset",
            )]);
        }
        Ok(p.clone())
    }

    fn compile(p: &Program, _: &ShaderInterface) -> Result<String, String> {
        if &*p.name == "rejected" {
            return Err("error: undeclared identifier".into());
        }
        Ok(p.name.to_string())
    }

    fn wait<S: Send + 'static, P: Send + 'static>(r: &mut ProgramReload<S, P>) -> ReloadEvent<P> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(event) = r.poll() {
                return event;
            }
            assert!(Instant::now() < deadline, "no reload outcome");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn a_failure_keeps_the_live_pipeline_and_a_success_swaps_it() {
        let mut r = ProgramReload::new(program("a", Vec::new()), lower, compile).expect("builds");
        assert_eq!(r.live().pipeline, "a");

        r.submit(program("bad", Vec::new()));
        let ReloadEvent::Failed(ReloadError::Program(errors)) = wait(&mut r) else {
            panic!("a front-end failure");
        };
        assert_eq!(
            (errors[0].code, errors[0].span),
            ("E8105", Span { start: 3, end: 7 })
        );
        assert_eq!(r.live().pipeline, "a");

        r.submit(program("rejected", Vec::new()));
        let ReloadEvent::Failed(ReloadError::Backend(log)) = wait(&mut r) else {
            panic!("a backend failure");
        };
        assert!(log.contains("undeclared"));
        assert_eq!(r.live().pipeline, "a");

        // A program the validator rejects never reaches the backend.
        let mut invalid = program("invalid", Vec::new());
        invalid.fragment.as_mut().unwrap().ret = Ty::F32;
        r.submit(invalid);
        assert!(matches!(
            wait(&mut r),
            ReloadEvent::Failed(ReloadError::Program(_))
        ));

        r.submit(program("b", Vec::new()));
        let ReloadEvent::Swapped(swap) = wait(&mut r) else {
            panic!("a swap");
        };
        assert_eq!(swap.retired, "a");
        assert!(swap.instance.is_none() && swap.uniforms.is_none());
        assert_eq!(r.live().pipeline, "b");
        assert!(!r.pending());
    }

    #[test]
    fn a_newer_edit_supersedes_an_older_one() {
        let (gate, permits) = sync_channel::<()>(0);
        let (started, starts) = channel::<()>();
        let worker = Mutex::new((permits, started));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let compile = move |p: &Program, _: &ShaderInterface| -> Result<String, String> {
            if &*p.name != "first" {
                // Hold the worker until the test lets it finish.
                let w = worker.lock().unwrap();
                w.1.send(()).unwrap();
                w.0.recv().unwrap();
            }
            log.lock().unwrap().push(p.name.to_string());
            Ok(p.name.to_string())
        };
        let mut r =
            ProgramReload::new(program("first", Vec::new()), lower, compile).expect("builds");
        r.submit(program("one", Vec::new()));
        starts.recv().unwrap();
        // The worker is compiling `one`; these queue behind it.
        r.submit(program("two", Vec::new()));
        r.submit(program("three", Vec::new()));
        assert!(r.pending());
        gate.send(()).unwrap();
        starts.recv().unwrap();
        gate.send(()).unwrap();
        let ReloadEvent::Swapped(swap) = wait(&mut r) else {
            panic!("a swap");
        };
        // `one` finished first but was already superseded; `two` was skipped.
        assert_eq!(
            (swap.retired.as_str(), r.live().pipeline.as_str()),
            ("first", "three")
        );
        assert_eq!(*seen.lock().unwrap(), ["first", "one", "three"]);
    }

    #[test]
    fn an_instance_layout_change_reencodes_the_buffer() {
        let old = vec![
            binding("pos", Ty::VEC2),
            binding("radius", Ty::F32),
            binding("id", Ty::U32),
        ];
        let new = vec![
            binding("radius", Ty::F32),
            binding("tint", Ty::Color),
            binding("pos", Ty::VEC2),
            binding("id", Ty::I32),
        ];
        let mut r = ProgramReload::new(program("s", old.clone()), lower, compile).expect("builds");
        let old_program = r.live().program.clone();
        let rows = [
            vec![Value::floats(&[1.0, 2.0]), Value::f32(3.0), Value::u32(9)],
            vec![Value::floats(&[4.0, 5.0]), Value::f32(6.0), Value::u32(10)],
        ];
        let mut bytes = Vec::new();
        for row in &rows {
            bytes.extend(
                r.live()
                    .interface
                    .instance
                    .encode(&old_program.instance, row)
                    .unwrap(),
            );
        }

        r.submit(program("s", new));
        let ReloadEvent::Swapped(swap) = wait(&mut r) else {
            panic!("a swap");
        };
        let migration = swap.instance.expect("the instance layout changed");
        assert!(swap.uniforms.is_none());
        let moved = migration.reencode(&bytes);
        let live = r.live();
        let stride = live.interface.instance.size as usize;
        assert_eq!(moved.len(), 2 * stride);
        let second =
            live.interface
                .instance
                .decode(&live.program, &live.program.instance, &moved[stride..]);
        // Same name and type carries over; a new member and a retyped one
        // start at zero.
        assert_eq!(
            second,
            [
                Value::f32(6.0),
                Value::floats(&[0.0; 4]),
                Value::floats(&[4.0, 5.0]),
                Value::i32(0)
            ]
        );
    }
}
