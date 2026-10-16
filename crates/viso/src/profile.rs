//! Frame profiling, for the tool that captures: each frame's spans timed on
//! the UI thread, the GPU's time on it and the work it counted, kept as
//! [`FrameSample`]s for the dev channel or a headless run's trace.
//!
//! Built into development and headless builds only. While nothing captures,
//! a [`time`]d span costs one thread-local read; a release build compiles
//! it away.

#[cfg(not(any(feature = "hot-reload", feature = "headless")))]
use viso_view::profile::Span;

#[cfg(any(feature = "hot-reload", feature = "headless"))]
pub(crate) use live::{Profiler, time};

/// Times nothing: the build has no tool to capture for.
#[cfg(not(any(feature = "hot-reload", feature = "headless")))]
pub(crate) struct Timer;

/// Ends nothing, so a guard ends its span the same way in every build.
#[cfg(not(any(feature = "hot-reload", feature = "headless")))]
impl Drop for Timer {
    #[inline(always)]
    fn drop(&mut self) {}
}

/// Times what the returned guard lives for as `span` of the frame being
/// sampled.
#[cfg(not(any(feature = "hot-reload", feature = "headless")))]
#[inline(always)]
pub(crate) fn time(span: Span) -> Timer {
    let _ = span;
    Timer
}

#[cfg(any(feature = "hot-reload", feature = "headless"))]
mod live {
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::time::Duration;

    use viso_platform::Instant;
    use viso_view::profile::{Count, FrameSample, MAX_FRAMES, Span, Timed};

    use viso_gpu::GpuBackend;
    use viso_runtime::RuntimeCx;

    use crate::{AppDriver, Application};

    /// How many frames a sample waits for the GPU to time it.
    const GPU_WAIT: u64 = 4;

    /// How often the HUD changes, and the frames it shows the time of.
    #[cfg(feature = "hot-reload")]
    const HUD_EVERY: u64 = 500_000_000;
    const HUD_SPAN: u64 = 1_000_000_000;

    /// How many samples, or how long, a batch for the host gathers.
    const BATCH_FRAMES: usize = 32;
    const BATCH_WAIT: Duration = Duration::from_millis(100);

    thread_local! {
        /// While a capture runs, when it began.
        static ORIGIN: Cell<Option<Instant>> = const { Cell::new(None) };
        /// The spans of the frame being sampled.
        static SPANS: Cell<[Option<Timed>; Span::COUNT]> =
            const { Cell::new([None; Span::COUNT]) };
    }

    fn nanos(d: Duration) -> u64 {
        u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
    }

    /// Times what it lives for as one span of the frame being sampled.
    pub(crate) struct Timer {
        span: Span,
        started: Option<Instant>,
    }

    /// Times what the returned guard lives for as `span` of the frame being
    /// sampled; nothing while no capture runs.
    #[inline]
    pub(crate) fn time(span: Span) -> Timer {
        Timer {
            span,
            started: ORIGIN.with(Cell::get).map(|_| Instant::now()),
        }
    }

    impl Drop for Timer {
        fn drop(&mut self) {
            let (Some(started), Some(origin)) = (self.started, ORIGIN.with(Cell::get)) else {
                return;
            };
            let took = nanos(started.elapsed());
            let start = nanos(started.saturating_duration_since(origin));
            SPANS.with(|spans| {
                let mut all = spans.get();
                let slot = &mut all[self.span as usize];
                match slot {
                    Some(timed) => timed.nanos += took,
                    None => *slot = Some(Timed { start, nanos: took }),
                }
                spans.set(all);
            });
        }
    }

    /// The capture under way, if any.
    #[derive(Default)]
    pub(crate) struct Profiler {
        capture: Option<Capture>,
    }

    struct Capture {
        id: u32,
        hud: bool,
        /// Whether its samples go out in batches as they are taken, or are
        /// kept for its end.
        stream: bool,
        origin: Instant,
        /// When the frame being sampled ran its first phase.
        began: Option<Instant>,
        frame: u64,
        /// What the frame being sampled counted so far.
        counts: [Option<u64>; Count::COUNT],
        /// The epoch the frame being sampled was presented in, 0 for none.
        presented: u64,
        last_epoch: u64,
        /// Samples waiting for the GPU's time, with the epoch they were
        /// presented in.
        held: VecDeque<(FrameSample, u64)>,
        ready: Vec<FrameSample>,
        dropped: u32,
        batched: Instant,
        /// The cumulative counters the next frame's take their difference
        /// from.
        baseline: Baseline,
        times: Vec<(u64, u64)>,
        /// The frames of the last [`HUD_SPAN`], for the HUD: when each
        /// began, its time and the GPU's when known.
        recent: VecDeque<(u64, u64, Option<u64>)>,
        /// When the HUD last changed, `None` before it first shows.
        #[cfg(feature = "hot-reload")]
        hud_at: Option<u64>,
    }

    /// Counters that only grow, as the frame before left them.
    #[derive(Debug, Clone, Copy, Default)]
    pub(crate) struct Baseline {
        pub(crate) reactive_evals: u64,
        pub(crate) dynamic_evals: u64,
        pub(crate) dynamic_fallbacks: u64,
        pub(crate) resource_loads: u64,
    }

    /// What a capture handed over: its id, its samples and those it could
    /// not keep.
    pub(crate) type Batch = (u32, Vec<FrameSample>, u32);

    impl Profiler {
        /// Whether a capture runs.
        #[inline]
        pub(crate) fn is_on(&self) -> bool {
            self.capture.is_some()
        }

        /// Whether the capture under way draws the frame time over the
        /// windows.
        #[cfg(feature = "hot-reload")]
        pub(crate) fn hud(&self) -> bool {
            self.capture.as_ref().is_some_and(|c| c.hud)
        }

        /// Starts the capture `id` with the cumulative counters as they
        /// stand, its samples handed over in batches with `stream`, ending
        /// the one under way, whose rest it returns.
        pub(crate) fn start(
            &mut self,
            id: u32,
            hud: bool,
            stream: bool,
            baseline: Baseline,
        ) -> Option<Batch> {
            let ended = self.end();
            let origin = Instant::now();
            ORIGIN.with(|o| o.set(Some(origin)));
            SPANS.with(|spans| spans.set([None; Span::COUNT]));
            self.capture = Some(Capture {
                id,
                hud,
                stream,
                origin,
                began: None,
                frame: 0,
                counts: [None; Count::COUNT],
                presented: 0,
                last_epoch: 0,
                held: VecDeque::new(),
                ready: Vec::new(),
                dropped: 0,
                batched: origin,
                baseline,
                times: Vec::new(),
                recent: VecDeque::new(),
                #[cfg(feature = "hot-reload")]
                hud_at: None,
            });
            ended
        }

        /// Ends the capture under way, returning what it holds.
        pub(crate) fn end(&mut self) -> Option<Batch> {
            let mut capture = self.capture.take()?;
            ORIGIN.with(|o| o.set(None));
            let held = std::mem::take(&mut capture.held);
            for (sample, _) in held {
                capture.keep(sample);
            }
            Some((capture.id, capture.ready, capture.dropped))
        }

        /// The capture's id, if one runs.
        #[cfg(feature = "hot-reload")]
        pub(crate) fn id(&self) -> Option<u32> {
            self.capture.as_ref().map(|c| c.id)
        }

        /// The frame's first phase begins.
        #[inline]
        pub(crate) fn begin_frame(&mut self) {
            if let Some(capture) = &mut self.capture {
                capture.began = Some(Instant::now());
            }
        }

        /// Adds `value` to what the frame counted of `count`.
        #[inline]
        pub(crate) fn count(&mut self, count: Count, value: u64) {
            if let Some(capture) = &mut self.capture {
                let slot = &mut capture.counts[count as usize];
                *slot = Some(slot.unwrap_or(0) + value);
            }
        }

        /// Counts what grew of the cumulative counters since the frame
        /// before, and keeps them for the next.
        pub(crate) fn grew(&mut self, now: Baseline) {
            let Some(capture) = &mut self.capture else {
                return;
            };
            let before = std::mem::replace(&mut capture.baseline, now);
            for (count, now, before) in [
                (
                    Count::ReactiveEvals,
                    now.reactive_evals,
                    before.reactive_evals,
                ),
                (Count::DynamicEvals, now.dynamic_evals, before.dynamic_evals),
                (
                    Count::DynamicFallbacks,
                    now.dynamic_fallbacks,
                    before.dynamic_fallbacks,
                ),
                (
                    Count::ResourceLoads,
                    now.resource_loads,
                    before.resource_loads,
                ),
            ] {
                capture.counts[count as usize] = Some(now.saturating_sub(before));
            }
        }

        /// The frame was presented in `epoch` of the backend that times
        /// frames, 0 when none does.
        pub(crate) fn presented(&mut self, epoch: u64) {
            if let Some(capture) = &mut self.capture
                && epoch != 0
                && epoch != capture.last_epoch
            {
                capture.presented = epoch;
                capture.last_epoch = epoch;
            }
        }

        /// The frame ended: samples it, its GPU time to come from `times`
        /// (as the backend's `finished_frame_times`), and hands over the
        /// batch due, if any.
        pub(crate) fn end_frame(
            &mut self,
            times: impl FnOnce(&mut Vec<(u64, u64)>),
        ) -> Option<Batch> {
            let capture = self.capture.as_mut()?;
            let end = Instant::now();
            let spans = SPANS.with(|spans| spans.replace([None; Span::COUNT]));
            let began = capture.began.take().unwrap_or(end);
            let began_at = nanos(began.saturating_duration_since(capture.origin));
            let input = spans[Span::Input as usize];
            let at = input.map_or(began_at, |t| t.start.min(began_at));
            let mut sample = FrameSample {
                frame: capture.frame,
                at,
                nanos: input.map_or(0, |t| t.nanos) + nanos(end.saturating_duration_since(began)),
                counts: std::mem::replace(&mut capture.counts, [None; Count::COUNT]),
                ..FrameSample::default()
            };
            for (slot, span) in sample.spans.iter_mut().zip(spans) {
                *slot = span.map(|t| Timed {
                    start: t.start.saturating_sub(at),
                    nanos: t.nanos,
                });
            }
            capture.frame += 1;
            if capture.hud {
                capture.recent.push_back((sample.at, sample.nanos, None));
                while capture
                    .recent
                    .front()
                    .is_some_and(|&(at, ..)| at + HUD_SPAN < sample.at)
                {
                    capture.recent.pop_front();
                }
            }
            let presented = std::mem::take(&mut capture.presented);
            capture.held.push_back((sample, presented));
            capture.times.clear();
            times(&mut capture.times);
            for &(epoch, gpu) in &capture.times {
                if let Some((sample, _)) = capture.held.iter_mut().find(|(_, e)| *e == epoch) {
                    sample.gpu = Some(gpu);
                    if let Some(recent) = capture.recent.iter_mut().find(|r| r.0 == sample.at) {
                        recent.2 = Some(gpu);
                    }
                }
            }
            let last = capture.frame;
            while let Some((sample, epoch)) = capture.held.front() {
                let waited = last - sample.frame;
                if *epoch != 0 && sample.gpu.is_none() && waited <= GPU_WAIT {
                    break;
                }
                let (sample, _) = capture.held.pop_front().expect("a held sample");
                capture.keep(sample);
            }
            let due = capture.stream && capture.ready.len() >= BATCH_FRAMES
                || (!capture.ready.is_empty() && capture.batched.elapsed() >= BATCH_WAIT);
            if !due {
                return None;
            }
            capture.batched = end;
            Some((
                capture.id,
                std::mem::take(&mut capture.ready),
                std::mem::take(&mut capture.dropped),
            ))
        }
    }

    #[cfg(feature = "hot-reload")]
    impl Profiler {
        /// The HUD's line when it is due to change: the frame times of the
        /// last second, every half second of frames.
        pub(crate) fn hud_line(&mut self) -> Option<String> {
            let capture = self.capture.as_mut().filter(|c| c.hud)?;
            let &(last, ..) = capture.recent.back()?;
            if capture.hud_at.is_some_and(|at| last < at + HUD_EVERY) {
                return None;
            }
            capture.hud_at = Some(last);
            let mut frames: Vec<u64> = capture.recent.iter().map(|r| r.1).collect();
            frames.sort_unstable();
            let rank = |p: f64| {
                frames[((p * frames.len() as f64).ceil() as usize).clamp(1, frames.len()) - 1]
            };
            let ms = |nanos: u64| nanos as f64 / 1e6;
            let mut line = format!(
                "frame {:.2} ms · p95 {:.2} ms",
                ms(rank(0.5)),
                ms(rank(0.95))
            );
            let gpu: Vec<u64> = capture.recent.iter().filter_map(|r| r.2).collect();
            if !gpu.is_empty() {
                let mean = gpu.iter().sum::<u64>() / gpu.len() as u64;
                line.push_str(&format!(" · gpu {:.2} ms", ms(mean)));
            }
            line.push_str(&format!(" · {} frames/s", frames.len()));
            Some(line)
        }
    }

    impl<A: Application> AppDriver<A> {
        /// The counters that only grow, as they stand.
        pub(crate) fn profile_baseline(&self) -> Baseline {
            let mut baseline = Baseline {
                resource_loads: self.assets.counters().decodes,
                ..Baseline::default()
            };
            for ws in &self.windows {
                let counters = ws.bindings.counters();
                baseline.reactive_evals += counters.static_binding_eval();
                baseline.dynamic_evals += counters.dynamic_binding_eval();
                baseline.dynamic_fallbacks += counters.dynamic_fallback_nodes();
            }
            baseline
        }

        /// Counts the frame's work in every window, before its dirt is
        /// cleared and its text counters closed.
        pub(crate) fn count_frame(&mut self) {
            use viso_text::GlyphImageKind as Pool;
            for ws in &self.windows {
                let profiler = &mut self.profiler;
                profiler.count(Count::Nodes, ws.store.arena().live_count() as u64);
                profiler.count(Count::DirtyNodes, ws.store.dirty_count() as u64);
                profiler.count(Count::Measured, u64::from(ws.recompute.measured));
                profiler.count(Count::LaidOut, u64::from(ws.recompute.laid_out));
                profiler.count(Count::Painted, u64::from(ws.recompute.painted));
                if let Some(text) = &ws.text {
                    let c = text.counters();
                    profiler.count(Count::ShapedRuns, c.shaped_runs());
                    profiler.count(Count::GlyphRasters, c.rasters());
                    profiler.count(Count::GlyphUploadBytes, c.atlas_upload_bytes());
                    let r = text.residency();
                    for pool in [
                        Pool::MaskA8,
                        Pool::ScalableMtsdf,
                        Pool::ColorRgba8,
                        Pool::OutlineVector,
                    ] {
                        profiler.count(Count::GlyphCacheBytes, r.pool_resident_bytes(pool) as u64);
                        profiler.count(Count::AtlasPages, r.pool_page_count(pool) as u64);
                    }
                }
            }
            let now = self.profile_baseline();
            self.profiler.grew(now);
        }

        /// Begins sampling a frame, the launch window's GPU timing it.
        pub(crate) fn begin_profiled_frame(&mut self) {
            if !self.profiler.is_on() {
                return;
            }
            self.profiler.begin_frame();
            if let Some(gpu) = self.windows.first_mut().and_then(|ws| ws.gpu.as_mut()) {
                gpu.backend.time_frames(true);
            }
        }

        /// Samples the frame that ended, hands over the batch due and
        /// redraws the HUD when its line changed.
        pub(crate) fn end_profiled_frame(&mut self, cx: &mut RuntimeCx<'_>) {
            if !self.profiler.is_on() {
                return;
            }
            let gpu = self.windows.first_mut().and_then(|ws| ws.gpu.as_mut());
            let batch = self.profiler.end_frame(|out| {
                if let Some(gpu) = gpu {
                    gpu.backend.finished_frame_times(out);
                }
            });
            if let Some(batch) = batch {
                self.hand_over(batch, false);
            }
            #[cfg(feature = "hot-reload")]
            if let Some(line) = self.profiler.hud_line() {
                self.show_hud(Some(&line), cx);
            }
            #[cfg(not(feature = "hot-reload"))]
            let _ = cx;
        }

        /// Draws `line` over every window, or removes it, redrawing them.
        #[cfg(feature = "hot-reload")]
        pub(crate) fn show_hud(&mut self, line: Option<&str>, cx: &mut RuntimeCx<'_>) {
            let mut scratch = Vec::new();
            for ws in &mut self.windows {
                if line.is_some() || ws.dev_hud.is_some() {
                    crate::hot_reload::overlay::show_hud(ws, line, &mut scratch);
                    cx.request_redraw(ws.window);
                }
            }
        }

        /// Starts the capture `id`, ending the one under way.
        #[cfg(feature = "hot-reload")]
        pub(crate) fn start_profile(&mut self, id: u32, hud: bool, stream: bool) {
            let baseline = self.profile_baseline();
            if let Some(ended) = self.profiler.start(id, hud, stream, baseline) {
                self.hand_over(ended, true);
            }
        }

        /// Ends the capture `id`, handing over what it holds; a capture not
        /// under way ends with nothing.
        #[cfg(feature = "hot-reload")]
        pub(crate) fn end_profile(&mut self, id: u32) {
            if self.profiler.id() != Some(id) {
                return self.hand_over((id, Vec::new(), 0), true);
            }
            if let Some(gpu) = self.windows.first_mut().and_then(|ws| ws.gpu.as_mut()) {
                gpu.backend.time_frames(false);
            }
            if let Some(ended) = self.profiler.end() {
                self.hand_over(ended, true);
            }
        }

        fn hand_over(&mut self, (id, frames, dropped): Batch, done: bool) {
            #[cfg(feature = "hot-reload")]
            self.hot_reload.send_profile(id, frames, dropped, done);
            #[cfg(not(feature = "hot-reload"))]
            let _ = (id, frames, dropped, done);
        }
    }

    impl Capture {
        fn keep(&mut self, sample: FrameSample) {
            if self.ready.len() < MAX_FRAMES {
                self.ready.push(sample);
            } else {
                self.dropped = self.dropped.saturating_add(1);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn frame(profiler: &mut Profiler, epoch: u64, times: &[(u64, u64)]) -> Option<Batch> {
            profiler.begin_frame();
            {
                let _layout = time(Span::Layout);
            }
            profiler.count(Count::Nodes, 3);
            profiler.count(Count::Nodes, 2);
            profiler.presented(epoch);
            profiler.end_frame(|out| out.extend_from_slice(times))
        }

        #[test]
        fn a_frame_is_sampled_only_while_a_capture_runs() {
            let mut profiler = Profiler::default();
            assert!(frame(&mut profiler, 0, &[]).is_none());
            profiler.start(7, false, true, Baseline::default());
            for _ in 0..BATCH_FRAMES - 1 {
                assert!(frame(&mut profiler, 0, &[]).is_none());
            }
            let (id, samples, dropped) = frame(&mut profiler, 0, &[]).expect("a batch");
            assert_eq!((id, samples.len(), dropped), (7, BATCH_FRAMES, 0));
            let first = &samples[0];
            assert_eq!(first.frame, 0);
            assert_eq!(first.count(Count::Nodes), Some(5));
            assert!(first.span(Span::Layout).is_some());
            assert!(first.span(Span::Paint).is_none());
            assert_eq!(samples[1].frame, 1);
            assert!(profiler.end().is_some_and(|(_, rest, _)| rest.is_empty()));
            assert!(time(Span::Input).started.is_none());
        }

        #[test]
        fn a_sample_waits_for_its_gpu_time_a_few_frames_at_most() {
            let mut profiler = Profiler::default();
            profiler.start(1, false, true, Baseline::default());
            frame(&mut profiler, 10, &[]);
            frame(&mut profiler, 11, &[(10, 500)]);
            let (_, samples, _) = profiler.end().expect("the capture");
            assert_eq!(samples[0].gpu, Some(500));
            assert_eq!(samples[1].gpu, None);

            profiler.start(2, false, true, Baseline::default());
            for epoch in 20..20 + GPU_WAIT + 1 {
                frame(&mut profiler, epoch, &[]);
            }
            let capture = profiler.capture.as_ref().expect("the capture");
            assert_eq!(capture.ready.len(), 1);
            assert!(capture.ready[0].gpu.is_none());
        }

        #[test]
        fn what_only_grows_is_counted_by_what_it_grew() {
            let mut profiler = Profiler::default();
            let at = |evals| Baseline {
                reactive_evals: evals,
                ..Baseline::default()
            };
            profiler.start(1, false, true, at(100));
            profiler.begin_frame();
            profiler.grew(at(130));
            profiler.end_frame(|_| {});
            let (_, samples, _) = profiler.end().expect("the capture");
            assert_eq!(samples[0].count(Count::ReactiveEvals), Some(30));
        }
    }
}
