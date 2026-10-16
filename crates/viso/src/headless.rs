//! The run of a headless build, and the UI scenario it plays.
//!
//! Every window gets the surface the launch names (1280 × 720 at 1x when it
//! names none) and draws through the software raster; each frame steps the
//! clock by one frame of 60 Hz; the run ends when the app settles, after the
//! frames the launch bounds it to, or once its scenario is played. No
//! service has an OS behind it.
//!
//! A scenario ([`viso_view::headless::Script`]) plays against the first
//! window ([`crate::scenario`]): input goes in through the backend's queue,
//! snapshots are read back from the raster, the report is a file the launch
//! names, and the run ends once it is played.
//!
//! A run handed a view package ([`VIEW_ENV`]) mounts it alone in its first
//! window, with no caption and none of the app's own view, so a tool
//! captures one component as the app's fonts and assets draw it.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use viso_platform::WindowId;
use viso_platform::backend::headless::HeadlessApp;
use viso_runtime::{FixedStepClock, Scheduler};
use viso_ui::{DirtyClass, NodeId};
use viso_view::headless::{
    CENSUS_ENV, FRAMES_ENV, HeadlessSurface, REPORT_ENV, SCRIPT_ENV, SNAPSHOT_ENV, SOURCES_ENV,
    SURFACE_ENV, VIEW_ENV,
};
use viso_view::inspect::{self, CensusView, WindowCensus, static_nodes};
use viso_view::profile::{PROFILE_ENV, Trace};
use viso_view::{MountRecord, ViewHost, load_view, take_mounts};

use crate::scenario::{Input, Outputs, ScriptRunner};
use crate::{AppDriver, Application, WindowState, services};

pub(crate) fn run<A: Application>() {
    let surface = viso_platform::launch_var(SURFACE_ENV)
        .and_then(|value| HeadlessSurface::from_env(&value))
        .unwrap_or_default();
    let mut app = Box::new(HeadlessApp::new());
    app.fixed_surface(surface.width, surface.height, surface.scale);
    if let Some(frames) =
        viso_platform::launch_var(FRAMES_ENV).and_then(|value| value.trim().parse().ok())
    {
        app.frame_budget(frames);
    }
    let mut driver = AppDriver::<A>::new(Some(services::Services::unsupported()));
    if let Some(path) = viso_platform::launch_var(VIEW_ENV) {
        match std::fs::read(&path) {
            Ok(package) => driver.harness.package = Some(package),
            Err(error) => {
                eprintln!("viso: cannot read the view `{path}`: {error}");
                std::process::exit(2);
            }
        }
    }
    if let Some(path) = viso_platform::launch_var(SCRIPT_ENV) {
        let report = viso_platform::launch_var(REPORT_ENV);
        let snapshots = viso_platform::launch_var(SNAPSHOT_ENV).map(PathBuf::from);
        let outputs = Outputs {
            report: report.as_deref(),
            snapshots,
            sources: viso_platform::launch_var(SOURCES_ENV),
            census: viso_platform::launch_var(CENSUS_ENV).map(PathBuf::from),
        };
        match ScriptRunner::open(&path, outputs, Input::Feed(app.feed())) {
            Ok(runner) => driver.script = Some(runner),
            Err(why) => {
                eprintln!("viso: {why}");
                std::process::exit(2);
            }
        }
    }
    let clock = FixedStepClock::new(
        viso_platform::Instant::now(),
        std::time::Duration::from_nanos(1_000_000_000 / 60),
    );
    let profile = viso_platform::launch_var(PROFILE_ENV);
    if profile.is_some() {
        let baseline = driver.profile_baseline();
        driver.profiler.start(0, false, false, baseline);
    }
    let mut driver = Scheduler::with_clock(app, driver, clock).run_returning();
    if let Some(path) = profile {
        let (_, frames, dropped) = driver.profiler.end().unwrap_or_default();
        let trace = Trace {
            target: "headless".into(),
            dropped,
            frames,
            ..Trace::default()
        };
        if let Err(error) = std::fs::write(&path, trace.to_bytes()) {
            eprintln!("viso: cannot write the profile `{path}`: {error}");
            std::process::exit(2);
        }
    }
}

/// The view package a run mounts alone, and once it is mounted the instance
/// that runs it; the views the first window's build mounted.
#[derive(Default)]
pub(crate) struct Harness {
    package: Option<Vec<u8>>,
    host: Option<Rc<RefCell<ViewHost>>>,
    first: Option<WindowId>,
    mounts: Vec<MountRecord>,
}

impl Harness {
    /// Keeps the views `ws`'s build just mounted, when it is the first
    /// window, which a scenario plays against.
    pub(crate) fn adopt(&mut self, ws: &WindowState) {
        let mut records = Vec::new();
        take_mounts(&mut records);
        if *self.first.get_or_insert(ws.window) == ws.window {
            self.mounts.append(&mut records);
        }
    }

    /// The views the first window's build mounted.
    pub(crate) fn mounts(&self) -> &[MountRecord] {
        &self.mounts
    }

    /// Whether a view waits to be mounted in place of the app's.
    pub(crate) fn is_pending(&self) -> bool {
        self.package.is_some()
    }

    /// Mounts the view as the root of `ws`, a window opened with none; a
    /// package that does not load ends the run.
    pub(crate) fn mount(&mut self, ws: &mut WindowState) {
        let Some(package) = self.package.take() else {
            return;
        };
        let loaded = match load_view(
            &package,
            &mut ws.store,
            &mut ws.states,
            &mut ws.bindings,
            &mut ws.virtual_lists,
        ) {
            Ok(loaded) => loaded,
            Err(error) => {
                eprintln!("viso: the view to mount does not load: {error}");
                std::process::exit(2);
            }
        };
        self.host = loaded.host;
        ws.root = loaded.root;
        ws.shape_pending_text();
        if let Some(root) = ws.root {
            ws.store.mark_dirty(
                root,
                DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
            );
        }
    }
}

/// The census of `ws`, its nodes named by the views `mounts` it mounts.
pub(crate) fn census(ws: &WindowState, mounts: &[MountRecord]) -> WindowCensus {
    let store = &ws.store;
    let nodes: Vec<Vec<Option<NodeId>>> = mounts
        .iter()
        .map(|mount| {
            if mount.nodes.is_empty() {
                static_nodes(store, mount.root, mount.statics)
            } else {
                mount.nodes.clone()
            }
        })
        .collect();
    let hosts: Vec<Option<std::cell::Ref<'_, ViewHost>>> = mounts
        .iter()
        .map(|mount| mount.host.as_ref().map(|host| host.borrow()))
        .collect();
    let views: Vec<CensusView<'_>> = mounts
        .iter()
        .zip(&nodes)
        .zip(&hosts)
        .map(|((mount, nodes), host)| CensusView {
            file: mount.file,
            source_hash: mount.source_hash,
            nodes,
            host: host.as_deref(),
        })
        .collect();
    inspect::census(store, ws.root, &views)
}
