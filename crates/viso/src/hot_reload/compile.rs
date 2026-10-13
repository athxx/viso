//! The shaders a staged patch brings, compiled off the loop.
//!
//! A patch whose views paint with a program no window's renderer holds waits
//! for it: staging hands each such program, with the compiler of each window
//! drawing with a GPU, to one compile thread, and the patch commits at the
//! first frame boundary after its compiles finished — every program then
//! adopted by its window's renderer before the views commit, so the slots the
//! commit fills install without compiling on the frame. The first program the
//! backend refuses refuses the whole patch.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use viso_gpu::{Backend, GpuBackend, ProgramCompiler, ProgramOf, TextureFormat};
use viso_platform::{LoopWaker, WindowId};
use viso_render::BrushProgram;
use viso_view::dev::wire::Revision;

/// One program to build for one window.
pub(super) struct Work {
    pub(super) window: WindowId,
    pub(super) compiler: <Backend as GpuBackend>::Compiler,
    /// The format the window's renderer draws into.
    pub(super) format: TextureFormat,
    pub(super) program: Arc<BrushProgram>,
    /// The file whose view paints with it, for the refusal's message.
    pub(super) file: usize,
}

/// A program built for a window, for its renderer to adopt.
pub(super) struct Built {
    pub(super) window: WindowId,
    pub(super) program: Arc<BrushProgram>,
    pub(super) compiled: ProgramOf<Backend>,
}

/// A program the backend refused: the file painting with it and why.
pub(super) struct Refused {
    pub(super) file: usize,
    pub(super) message: String,
}

/// What the compiles of the patch to `revision` came to.
pub(super) struct Finished {
    pub(super) revision: Revision,
    pub(super) built: Result<Vec<Built>, Refused>,
}

/// The compile thread of a session.
pub(super) struct ShaderCompiles {
    jobs: Sender<(Revision, Vec<Work>)>,
    finished: Receiver<Finished>,
}

impl ShaderCompiles {
    /// Starts the compile thread, which wakes the loop with `waker` each time
    /// a patch's compiles finished; `None` when no thread starts. It ends
    /// with the session.
    pub(super) fn start(waker: LoopWaker) -> Option<ShaderCompiles> {
        let (jobs, work) = mpsc::channel::<(Revision, Vec<Work>)>();
        let (done, finished) = mpsc::channel();
        thread::Builder::new()
            .name("viso-shader-compile".into())
            .spawn(move || {
                for (revision, work) in work {
                    let built = work.into_iter().map(build).collect();
                    if done.send(Finished { revision, built }).is_err() {
                        return;
                    }
                    waker.wake();
                }
            })
            .ok()?;
        Some(ShaderCompiles { jobs, finished })
    }

    /// Hands the compiles of the patch to `revision` to the thread. Whether
    /// it took them.
    pub(super) fn submit(&self, revision: Revision, work: Vec<Work>) -> bool {
        self.jobs.send((revision, work)).is_ok()
    }

    /// The next patch whose compiles finished.
    pub(super) fn poll(&self) -> Option<Finished> {
        self.finished.try_recv().ok()
    }
}

/// `work`'s program built for its window, or why the backend refused it.
fn build(work: Work) -> Result<Built, Refused> {
    let refused = |why: String| Refused {
        file: work.file,
        message: format!("shader `{}`: {why}", work.program.name),
    };
    let desc = work
        .program
        .desc(Backend::SHADER_LANG, work.format)
        .map_err(|error| refused(error.to_string()))?;
    let compiled = work.compiler.compile(&desc).map_err(refused)?;
    Ok(Built {
        window: work.window,
        program: Arc::clone(&work.program),
        compiled,
    })
}
