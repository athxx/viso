//! The footprint profile a build is made for, and the memory budget its
//! caches share (Architecture §50A.3–§50A.4).
//!
//! A profile is fixed when the app is built: `viso build` passes the target's
//! `profile` (`VISO_FOOTPRINT`), its `budget` (`VISO_BUDGET`) and its
//! `workers` (`VISO_WORKERS`) to the build, which bakes them in; a plain
//! `cargo build` resolves the profile from the target and the backend
//! features. The profile gives every byte budget a default; an app adjusts
//! them with [`Application::memory_budget`](crate::Application::memory_budget).
//!
//! The budget is a run-time contract: the text, image and GPU caches are
//! sized from it, a frame over a hard limit first drops what can be rebuilt,
//! cold before hot, and a limit still exceeded after that is reported as a
//! [`BudgetExceeded`] — never a silent out-of-memory.

use viso_gpu::ShaderLang;

include!(concat!(env!("OUT_DIR"), "/footprint.rs"));

const KIB: u64 = 1 << 10;
const MIB: u64 = 1 << 20;

/// The device classes a build can be made for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Footprint {
    Desktop,
    Mobile,
    Web,
    /// Linux with GLES 3.0+ or Vulkan 1.1, at least 128 MB of memory.
    EmbeddedGpu,
    /// Linux drawn on the CPU, at least 32 MB of memory.
    EmbeddedCpu,
}

impl Footprint {
    /// The profile this build was made for.
    pub const fn of_build() -> Footprint {
        BUILD_FOOTPRINT
    }

    /// The name `Viso.toml` gives it.
    pub const fn name(self) -> &'static str {
        match self {
            Footprint::Desktop => "desktop",
            Footprint::Mobile => "mobile",
            Footprint::Web => "web",
            Footprint::EmbeddedGpu => "embedded-gpu",
            Footprint::EmbeddedCpu => "embedded-cpu",
        }
    }

    /// Whether a build of this profile may draw through a backend whose
    /// programs are in `lang`: the languages its packages carry.
    pub const fn allows(self, lang: ShaderLang) -> bool {
        match self {
            Footprint::Desktop | Footprint::Mobile => true,
            Footprint::Web => matches!(lang, ShaderLang::Wgsl | ShaderLang::None),
            Footprint::EmbeddedGpu => matches!(lang, ShaderLang::Glsl | ShaderLang::SpirV),
            Footprint::EmbeddedCpu => matches!(lang, ShaderLang::None),
        }
    }

    /// The threads the profile runs work on besides the main one, by
    /// default: `None` sizes them to the machine.
    pub const fn default_workers(self) -> Option<usize> {
        match self {
            Footprint::Desktop | Footprint::Mobile | Footprint::Web => None,
            Footprint::EmbeddedGpu => Some(1),
            Footprint::EmbeddedCpu => Some(0),
        }
    }

    /// The budget the profile plans for before the build's and the app's
    /// overrides.
    pub const fn default_budget(self) -> MemoryBudget {
        match self {
            Footprint::Desktop | Footprint::Web => MemoryBudget {
                heap: None,
                gpu: None,
                faces: 256 * MIB,
                shaping: 16 * MIB,
                glyph_atlas: MIB,
                images: 256 * MIB,
            },
            Footprint::Mobile => MemoryBudget {
                heap: None,
                gpu: None,
                faces: 64 * MIB,
                shaping: 4 * MIB,
                glyph_atlas: MIB,
                images: 96 * MIB,
            },
            Footprint::EmbeddedGpu => MemoryBudget {
                heap: Some(48 * MIB),
                gpu: Some(64 * MIB),
                faces: 16 * MIB,
                shaping: 2 * MIB,
                glyph_atlas: MIB,
                images: 16 * MIB,
            },
            Footprint::EmbeddedCpu => MemoryBudget {
                heap: Some(16 * MIB),
                gpu: Some(16 * MIB),
                faces: 4 * MIB,
                shaping: 512 * KIB,
                glyph_atlas: 256 * KIB,
                images: 4 * MIB,
            },
        }
    }
}

// A build whose backend's programs its profile's packages do not carry is
// refused here rather than failing on the device.
const _: () = assert!(
    Footprint::of_build().allows(<viso_gpu::Backend as viso_gpu::GpuBackend>::SHADER_LANG),
    "this build's GPU backend draws in a shader language its footprint profile does not carry"
);

/// Byte budgets for what the app keeps in memory. `heap`, `gpu` and `images`
/// are limits a frame is checked against ([`BudgetExceeded`] past them,
/// `None` for none); the others size a cache, which evicts within its budget
/// on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryBudget {
    /// The process heap, as a [`Counting`](crate::alloc::Counting) allocator
    /// sees it; unchecked without one.
    pub heap: Option<u64>,
    /// Bytes held on the device: the renderer's buffers and targets, glyph
    /// atlases and images.
    pub gpu: Option<u64>,
    /// Loaded font faces.
    pub faces: u64,
    /// Shaped text runs.
    pub shaping: u64,
    /// One glyph atlas plane: its texels (A8), rounded down to a square of
    /// whole pages.
    pub glyph_atlas: u64,
    /// The textures of the images shown. An image a node shows stays
    /// resident, so this is checked like a hard limit, never evicted from.
    pub images: u64,
}

impl MemoryBudget {
    /// The budget this build plans for: its profile's defaults with the
    /// build's overrides (`budget` in the target's `Viso.toml` section).
    pub const fn of_build() -> MemoryBudget {
        let mut b = Footprint::of_build().default_budget();
        let [heap, gpu, faces, shaping, glyph_atlas, images] = BUILD_BUDGET;
        if heap.is_some() {
            b.heap = heap;
        }
        if gpu.is_some() {
            b.gpu = gpu;
        }
        if let Some(v) = faces {
            b.faces = v;
        }
        if let Some(v) = shaping {
            b.shaping = v;
        }
        if let Some(v) = glyph_atlas {
            b.glyph_atlas = v;
        }
        if let Some(v) = images {
            b.images = v;
        }
        b
    }

    /// The edge, in texels, of a square glyph atlas plane of whole
    /// `page`-texel pages within [`glyph_atlas`](Self::glyph_atlas): at least
    /// one page.
    pub(crate) fn atlas_edge(&self, page: u32) -> u32 {
        let page = page.max(1);
        let edge = (self.glyph_atlas as f64).sqrt() as u32;
        (edge / page).max(1) * page
    }
}

static SESSION: std::sync::OnceLock<MemoryBudget> = std::sync::OnceLock::new();

/// Fixes the session's budget: the app's
/// [`memory_budget`](crate::Application::memory_budget), read once before
/// the first window opens. Later calls keep the first.
pub(crate) fn install(budget: MemoryBudget) {
    let _ = SESSION.set(budget);
}

/// The session's budget: the installed one, else the build's.
pub(crate) fn budget() -> &'static MemoryBudget {
    SESSION.get_or_init(MemoryBudget::of_build)
}

/// The threads this build runs work on besides the main one: the build's
/// `workers`, else its profile's, else the machine's.
pub(crate) fn workers() -> usize {
    BUILD_WORKERS
        .or(Footprint::of_build().default_workers())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map_or(1, |n| n.get().saturating_sub(1).clamp(1, 8))
        })
}

/// What a frame found over its hard limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetKind {
    Heap,
    Gpu,
    /// The textures of the images shown, within [`MemoryBudget::images`].
    Images,
}

/// A hard limit still exceeded after the caches were trimmed: the bytes in
/// use and the limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetExceeded {
    pub kind: BudgetKind,
    pub used: u64,
    pub budget: u64,
}

impl std::fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self.kind {
            BudgetKind::Heap => "heap",
            BudgetKind::Gpu => "GPU memory",
            BudgetKind::Images => "image textures",
        };
        write!(
            f,
            "BudgetExceeded: {what} at {} KiB over its {} KiB budget ({} profile), after trimming caches",
            self.used / KIB,
            self.budget / KIB,
            Footprint::of_build().name()
        )
    }
}

/// Watches the hard limits frame by frame.
#[derive(Debug, Default)]
pub(crate) struct BudgetWatch {
    /// The limits reported over, each once until it is back under.
    reported: [bool; 3],
    /// A trim already ran for the current excess.
    trimmed: bool,
}

/// The bytes a frame found in use.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Usage {
    /// The process heap, when a counting allocator sees it.
    pub heap: Option<u64>,
    /// Everything held on the device, `images` included.
    pub gpu: u64,
    pub images: u64,
}

/// What the watch asks of the frame.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Within,
    /// Over a limit: drop what can be rebuilt, then look again.
    Trim,
    /// Over a limit after trimming: report it.
    Exceeded(BudgetExceeded),
}

impl BudgetWatch {
    /// Checks the bytes in use against `budget`: the heap (when counted),
    /// everything on the device, and the image textures among it.
    pub(crate) fn check(&mut self, budget: &MemoryBudget, used: Usage) -> Verdict {
        let over = [
            budget
                .heap
                .zip(used.heap)
                .filter(|(b, u)| u > b)
                .map(|(b, u)| (BudgetKind::Heap, u, b)),
            budget
                .gpu
                .filter(|b| used.gpu > *b)
                .map(|b| (BudgetKind::Gpu, used.gpu, b)),
            (used.images > budget.images).then_some((
                BudgetKind::Images,
                used.images,
                budget.images,
            )),
        ];
        if over.iter().all(Option::is_none) {
            self.trimmed = false;
            self.reported = [false; 3];
            return Verdict::Within;
        }
        if !std::mem::replace(&mut self.trimmed, true) {
            return Verdict::Trim;
        }
        for (i, o) in over.iter().enumerate() {
            match o {
                Some((kind, used, budget)) if !self.reported[i] => {
                    self.reported[i] = true;
                    return Verdict::Exceeded(BudgetExceeded {
                        kind: *kind,
                        used: *used,
                        budget: *budget,
                    });
                }
                None => self.reported[i] = false,
                _ => {}
            }
        }
        Verdict::Within
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smaller_profiles_never_plan_more() {
        let order = [
            Footprint::Desktop,
            Footprint::Mobile,
            Footprint::EmbeddedGpu,
            Footprint::EmbeddedCpu,
        ];
        for pair in order.windows(2) {
            let (a, b) = (pair[0].default_budget(), pair[1].default_budget());
            assert!(
                b.faces <= a.faces && b.shaping <= a.shaping && b.images <= a.images,
                "{pair:?}"
            );
            assert!(b.glyph_atlas <= a.glyph_atlas, "{pair:?}");
        }
    }

    #[test]
    fn profiles_pin_their_shader_languages() {
        assert!(Footprint::EmbeddedCpu.allows(ShaderLang::None));
        assert!(!Footprint::EmbeddedCpu.allows(ShaderLang::Glsl));
        assert!(Footprint::EmbeddedGpu.allows(ShaderLang::Glsl));
        assert!(!Footprint::EmbeddedGpu.allows(ShaderLang::Msl));
        assert!(Footprint::Web.allows(ShaderLang::Wgsl));
        assert!(!Footprint::Web.allows(ShaderLang::SpirV));
    }

    #[test]
    fn the_atlas_is_whole_pages() {
        let b = Footprint::EmbeddedCpu.default_budget();
        assert_eq!(b.atlas_edge(256), 512);
        assert_eq!(Footprint::Desktop.default_budget().atlas_edge(256), 1024);
        let tiny = MemoryBudget {
            glyph_atlas: 1,
            ..b
        };
        assert_eq!(tiny.atlas_edge(256), 256);
    }

    #[test]
    fn an_excess_trims_once_then_reports_once() {
        let budget = MemoryBudget {
            gpu: Some(100),
            heap: None,
            ..Footprint::EmbeddedCpu.default_budget()
        };
        let gpu = |gpu| Usage {
            heap: None,
            gpu,
            images: 0,
        };
        let mut w = BudgetWatch::default();
        let unlimited_heap = Usage {
            heap: Some(1 << 40),
            ..gpu(50)
        };
        assert_eq!(
            w.check(&budget, unlimited_heap),
            Verdict::Within,
            "no heap limit"
        );
        assert_eq!(w.check(&budget, gpu(150)), Verdict::Trim);
        let Verdict::Exceeded(e) = w.check(&budget, gpu(150)) else {
            panic!("reported");
        };
        assert_eq!((e.kind, e.used, e.budget), (BudgetKind::Gpu, 150, 100));
        assert_eq!(w.check(&budget, gpu(150)), Verdict::Within, "reported once");
        assert_eq!(w.check(&budget, gpu(90)), Verdict::Within);
        assert_eq!(
            w.check(&budget, gpu(150)),
            Verdict::Trim,
            "a new excess trims again"
        );
        let images = Usage {
            images: budget.images + 1,
            ..gpu(90)
        };
        assert_eq!(w.check(&budget, gpu(90)), Verdict::Within);
        assert_eq!(w.check(&budget, images), Verdict::Trim);
        let Verdict::Exceeded(e) = w.check(&budget, images) else {
            panic!("images reported");
        };
        assert_eq!(e.kind, BudgetKind::Images);
    }
}
