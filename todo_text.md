# Viso Text / Font Runtime — X1 → X12

`Viso_Text_Font_Runtime.md` (3071 lines) has a large, well-tested algorithm layer and a
real end-to-end A8 + color-emoji path on macOS. It does **not** have a satisfied §29
Definition of Done. This program closes the gap.

## What the audit found

`crates/text` is 11300 lines across 30 modules with 208 passing unit tests and two
benchmark gates. The Unicode work is genuine: the official UAX #9 / #14 / #29 conformance
corpora (`BidiTest.txt`, `BidiCharacterTest.txt`, `LineBreakTest.txt`,
`GraphemeBreakTest.txt`, `WordBreakTest.txt`) are in `crates/text/tests/fixtures/` and are
consumed by `bidi.rs`, `line_break.rs` and `segment.rs`. Shaping is `rustybuzz`,
segmentation is `icu_segmenter`, and `crates/viso/src/system_fonts.rs` (963 lines) is a real
CoreText adapter that rebuilds an sfnt container from `CTFontCopyAvailableTables` and
rasterizes color glyphs through the live `CTFont`.

Three findings block the DoD.

**1. §13 Glyph Representation is four `todo!()` stubs.** The entire quality/performance core
of the spec — 420 lines of specification and 13 DoD items — will panic if called:

```text
glyph_representation.rs:48  todo!("TF-P3: Temporal Promotion decision with hysteresis")
mtsdf.rs:29                 todo!("TF-P3: sdfer multi-channel distance field")
outline_cache.rs:29         todo!("TF-P3: retained outline tessellation")
coverage.rs:28              todo!("TF-P1: cmap coverage test")
```

`sdfer` is already a dependency and unused. There is no MTSDF lane, no OutlineVector lane,
no Temporal Promotion, and no Coverage Accelerator — only a `face_covers` free function that
re-parses the whole face on every probe.

**2. Most of the crate has no consumer.** The runtime does not use it. Grepped across
`crates/render/src`, `crates/viso/src`, `crates/ui/src` and `crates/widgets/src`, these have
**zero** references: `FontCache`, `GlyphResidency`, `GlyphKey`, `Admission`, `TextPosition`,
`CaretAffinity`, `Utf16Bridge`, `Progressive`, `SystemFontCatalog`, `ExternalFontProvider`,
`LineBreakTailoring`, `GraphemeCheckpoint`. `paragraph.rs` (1467 lines, 19 tests — the
canonical paragraph pipeline) is referenced only by a benchmark. The 208 tests prove the
algorithms are correct in isolation; they do not prove the runtime behaves as specified.

Concretely, what that costs:

- `crates/widgets/src/controls/text_input.rs` edits through `viso_ui::Buffer` / `Selection` /
  `Motion` / `ImeEvent`, not through `viso-text`'s `text_position` / `caret` / `selection` /
  `hit_test` / `ime`. Its own module docs list grapheme-cluster stepping as a later slice. So
  BiDi dual-caret affinity, ligature caret metadata, logical-range IME with revision, and
  grapheme stepping are all specified, implemented, tested — and not reachable from the
  control the user types into.
- `crates/viso/src/text_content.rs` builds its own simplified pipeline directly on
  `BidiInfo` + `LineBreaker` + `Shaper`, bypassing `paragraph.rs`. Incremental paragraph
  invalidation, last-good paragraph, and the worker boundary are therefore not in effect at
  runtime even though `incremental_edit.rs` proves `incremental == full`.
- It constructs `FontFallback::new(1 << 30)` — a 1 GiB budget standing in for §9's
  byte-budgeted SLRU face cache, which is implemented in `font_cache.rs` and unused. §18
  cache lifetimes and §19 memory budgets do not hold at runtime.

**3. `crates/render/src/glyph_atlas.rs` violates the DoD directly.** When a glyph does not
fit it performs a generational **whole-atlas wipe** (`glyph_atlas.rs:214`: reset the packer,
clear the backing, re-upload). The DoD says `Atlas 满时不正常执行 whole-atlas reset`, and
§13.10 requires page-age + CLOCK. The page/pool model with per-kind budgets already exists
in `glyph_cache.rs` (812 lines, per-kind residency pools, page epochs, admission with
fallback) and render does not use it.

## Ordering principle

Fix what panics, then what violates a stated contract, then connect what is already built,
then gate it. Each section's Done gate must be green and its contract frozen before the next
builds on it. Nothing below may become a prerequisite of anything above it.

The tooling program (`TODO.md`, T0 → T10) is a genuine prerequisite for three DoD items —
`assets/fonts/` auto-registration needs `Viso.toml` and an asset pipeline, and the WASM lane
needs a web target — so those are deferred by name at the bottom rather than blocked on here.

Gate for every section: `cargo xtask check-deps` · `cargo fmt --all -- --check` ·
`cargo clippy --workspace --all-targets -- -D warnings` · `cargo test --workspace`.
Build/test with `CARGO_TARGET_DIR=/tmp/rust_tmp`. Release builds only for timing (AGENTS 36).
Commit per section (source + this file in one commit, AGENTS 40).

---

## X1 — Coverage Accelerator (§10)

Goal: remove the first `todo!()` and stop re-parsing a face per coverage probe. Smallest
independent unit, and `fallback` is the hot path that pays for it today.

- [x] `coverage::Coverage`: per-face coverage sets derived from the cmap once, not per probe.
      A sparse directory over 512-scalar pages; each present page is a page-local inclusive
      range list or a 64-byte bitset, whichever is smaller, and a wholly-covered page is a
      tag with no payload. Absent pages cost nothing, so there is no resident 1.1M-scalar
      table and no `HashSet<char>` per face. The cmap subtables are enumerated (never the
      scalar space) and every candidate is confirmed through `glyph_index`.
- [x] Keep `face_covers` as the cold one-shot probe it is; route `fallback`'s per-run and
      `text_content`'s per-cluster queries through `Coverage`. `Candidate::mapped_len` (a
      `Face::parse` per run) and `TextShaper::face_covers` (a `Face::parse` per grapheme) are
      both gone.
- [x] Coverage sets are built lazily per face on first query — `Coverage::face_parses()` is
      zero until asked — and dropped with the face via `forget` / `clear`, which release
      their bytes (§18). `Coverage::bytes()` is the cost for the Face Cache to charge;
      `FontFallback::coverage_bytes()` exposes it for the candidate faces. Budget
      *enforcement* is X8's, where the Face Cache first has a runtime owner.
- [x] Tests: coverage agrees with `ttf_parser::glyph_index` for every codepoint in the
      fixture's cmap, and with `face_covers` for a spread of runs; 1500 warm runs over three
      shapes leave `coverage_face_parses()` at 1, and four distinct candidates parse exactly
      four times; an unregistered — or forgotten — face is a clean `false`, never a panic;
      both page encodings are exercised and agree with the scalars they were built from;
      bytes are charged on build and released on drop; unparseable bytes cover nothing and
      parse once. A filter pass is documented and tested as *not* a shaping decision (§10):
      the shaper's coverage-miss flag stays the authority for complex clusters.
- [x] Gate green → commit.

---

## X2 — One glyph residency model (§13.9, §13.10, §19)

Goal: delete the whole-atlas wipe. `viso-text` owns residency metadata, `viso-render` owns
GPU pages; today render owns both and does it wrong.

- [x] `render`'s glyph atlas adopts `viso_text::GlyphResidency`: page-age + CLOCK eviction,
      per-kind pools (A8 / MTSDF / RGBA / Vector) with independent byte budgets.
- [x] Remove the generational whole-atlas wipe. A full atlas evicts the coldest page and
      re-admits; it never clears live glyphs. Atlas-full is a normal steady state, not a
      reset event.
- [x] Memory pressure evicts within one pool. It must not cascade into a full text-cache
      clear (DoD: `memory pressure 不引发全 Text cache 连锁清空`).
- [x] Counters (AGENTS 61, §25): resident glyphs / pages / upload bytes per pool, evictions
      per frame, admission failures. Wired from this section and never removed. They live on
      the facade's `TextCounters` beside `GlyphResidency`, not on render's frozen `FrameStats`.
- [x] Tests: admitting past the budget evicts the coldest page and keeps the hot glyphs
      resident; a glyph evicted and re-requested re-admits without a whole-atlas upload;
      pool budgets are independent (filling the RGBA pool evicts nothing from A8); a
      steady-state frame over a warm working set admits nothing and uploads zero bytes.
- [x] Gate green → commit.

---

## X3 — MTSDF lane (§13.2, §13.5)

Goal: a scalable representation that exists. Generated msdfgen-style from `ttf-parser`
outlines inside `viso-text`; `sdfer` was dropped from both manifests rather than adapted.

- [x] `mtsdf::MtsdfGlyph` / `MtsdfGenerator`: multi-channel distance field with a true
      distance channel for AA, source-to-distance range, bearing/extent, scratch reuse across
      generations (no per-glyph allocation).
- [x] Resolution buckets and a quality window (§13.5): beyond the window, request the next
      bucket or hand off to X5's OutlineVector — never scale one field without bound.
- [x] `viso-shader` / `viso-render`: the MTSDF sampling lane and its pipeline, distinct from
      the A8 coverage lane, sampling the MTSDF pool from X2. The field atlas is a
      `TextureFormat::Rgba8Data` plane — four data channels, never premultiplied, since a
      distance is not a color. `GlyphLane` on the authoring primitive selects the pipeline;
      the two lanes share one instance buffer and never merge into one batch. The screen-space
      range comes from a `span` varying (device pixels per uv unit), not a derivative, so the
      headless raster mirrors the MSL exactly.
- [x] Viso 1.0 adds no plain single-channel SDF lane (DoD, explicit).
- [x] Tests: a generated field reconstructs the glyph's coverage within tolerance at bucket
      scale and at the window edges; corners survive (a sharp-corner glyph does not round —
      this is what multi-channel buys); generation allocates no per-glyph scratch after
      warm-up; a request past the window returns the next bucket rather than stretching.
      Plus the lane end to end: an MTSDF run draws through its own pipeline, a field
      reproduces the coverage it was generated from, and magnified 4x its edge band stays
      decisively narrower than the same glyph stretched from an A8 bitmap.
- [x] Gate green → commit.

X3 ships the lane **drivable on explicit request**: a caller that generates a field and
authors `GlyphLane::Mtsdf` gets it. Deciding *for* a glyph that it should be on this lane is
the promotion state machine, which is X4.

---

## X4 — Temporal Promotion with hysteresis (§13.3, §13.4, §13.11, §13.7)

Goal: the state machine that chooses a representation per glyph from observed behavior,
without recomputing per glyph per frame.

- [x] `RepresentationState::resolve`: current kind, active bucket, transform-quality window,
      pending-promotion accounting. Retained between frames; the decision is amortized, not
      per-frame (DoD: `policy 不按每 glyph / 每 frame 重算`).
- [x] Hysteresis both ways (§13.4): sustained scale/rotation promotes to MTSDF, settling
      demotes back to exact coverage at a frame boundary. Transient motion must not thrash.
- [x] Promotion never blocks the frame: while an MTSDF generation is pending the glyph keeps
      drawing last-good coverage (§13.3). Same for the async exact-coverage regeneration
      after settle.
- [x] CJK policy (§13.7): default to exact coverage; promotion thresholds account for the
      much larger working set.
- [x] Quality fallback (§13.11): any representation can fall back to coverage under
      residency pressure, because coverage is always correct. No glyph keeps Coverage +
      MTSDF + Vector resident by default (DoD, explicit).
- [x] Tests: a one-frame scale spike promotes nothing; a sustained zoom promotes once and
      only once; settling demotes once, at a frame boundary, never mid-frame; a pending
      promotion draws the previous representation and the frame's raster/shape counters stay
      at zero; forcing residency pressure demotes to coverage rather than dropping the glyph.
- [x] Gate green → commit.

---

## X5 — OutlineVector lane (§13.6)

Goal: extreme zoom without an unbounded distance field, and without tessellating per frame.

- [x] `outline_cache`: retained tessellated contours keyed by face and glyph, bounded, with
      `viso-render` owning the vertex buffers (this crate holds identity and extent).
- [x] Promotion from X4 at the top of the MTSDF quality window; steady state reuses the
      retained mesh and re-tessellates nothing (DoD: `稳态不每帧 tessellate`).
- [x] Its own residency budget in X2's pool set.
- [x] Tests: two consecutive frames at extreme zoom tessellate once; the retained outline's
      rendered coverage matches the A8 raster of the same glyph at the same scale within
      tolerance; eviction and re-request re-tessellate exactly once.
- [x] Gate green → commit.

---

## X6 — Wire the canonical paragraph pipeline (§12.4, §12.19, §12.21, §12.20)

Goal: the runtime uses `paragraph.rs`. This is the largest single piece of built-and-unused
work in the crate (1467 lines, 19 tests, proven `incremental == full` by benchmark).

- [x] `crates/viso/src/text_content.rs` routes through `paragraph::Paragraph` instead of
      assembling `BidiInfo` + `LineBreaker` + `Shaper` itself. One pipeline, one definition of
      correct (§12.4).
- [x] Incremental invalidation (§12.19) is in effect at runtime: an edit reshapes a bounded
      neighborhood regardless of document size, and the result equals a full recompute.
- [x] Last-good paragraph (§12.21): a pending reflow draws the previous layout rather than
      blocking input.
- [x] `LineBreakTailoring` reaches the runtime: zh-Hans / zh-Hant / ja / ko get their own
      line-break tailoring from the resolved locale (DoD, four separate items).
- [x] Tests: the runtime path and a full recompute agree on line structure for a scripted
      edit sequence; a single-character edit in a 10k-word paragraph reshapes a bounded
      number of runs; a static text frame executes zero resolve / shape / raster (DoD:
      `static text steady frame 不执行 resolve/shape/raster`) — asserted on counters, not
      inferred.
- [x] Gate green → commit.

---

## X7 — Wire editing correctness into the control (§12.5, §12.13–§12.17, §12.2)

Goal: `TextInput` edits through the world-ready model. Today it has its own simpler one, so
seven DoD items are true of the library and false of the product.

- [x] `text_input.rs` adopts `viso_text::TextPosition` / `TextOffset` / `CaretAffinity` as
      its position model. No integer stands for more than one index space (§12.2).
- [x] Grapheme-cluster stepping from `segment` (§12.5), replacing per-`char` motion.
- [x] BiDi dual caret (§12.13): one logical offset at a direction or wrap boundary resolves
      to two visual positions, selected by affinity.
- [x] Ligature caret (§12.14) from GDEF / shaper metadata; the caret never lands inside an
      illegal grapheme interior.
- [x] Hit testing (§12.15) returns a logical `TextPosition` + affinity from the shaped
      geometry, never an average-glyph-width guess. Enables click-to-place and drag-select,
      which the control's docs currently defer.
- [x] Selection (§12.16): logical range is the source of truth, rendered as multiple visual
      fragments across direction runs. (Fragments are resolved from the control's
      selection; painting the highlight and the caret is not wired yet.)
- [x] IME (§12.17): logical range + revision; candidate rects come from the visual caret map.
- [x] Source text is never implicitly normalized (§12.3) — assert it survives a round trip.
- [x] Tests: caret motion over combining marks, emoji ZWJ sequences and ligatures steps whole
      graphemes; a caret at an RTL/LTR boundary reports both visual positions and affinity
      picks; a mixed-direction selection renders as the expected fragment set; a CJK IME
      composition preserves the logical range across a revision; hit-testing a proportional
      RTL run returns the same offset the shaped geometry implies.
- [x] Gate green → commit.

---

## X8 — Face and shaping cache budgets (§9, §11, §18, §19)

Goal: the 1 GiB placeholder goes away and the implemented SLRU takes over.

- [ ] `FontCache` (byte-budgeted SLRU) replaces `FontFallback::new(1 << 30)` in the facade.
      Budget from the resolved configuration, not a constant.
- [ ] Recency is not updated per glyph (§9.3) — per face, per admission window.
- [ ] Pinned hot faces (§9.4) survive eviction: the system UI face and the resolved CJK
      fallback are not evictable in a normal frame.
- [ ] Shaping cache gets its own budget (§11), separate from the face cache.
- [ ] Each cache's lifetime is independent (§18): dropping a face drops its coverage set, its
      shaping entries and its glyph residency, and nothing else.
- [ ] Counters (§25): face cache bytes / hits / misses / evictions, shaping cache the same.
- [ ] Tests: exceeding the face budget evicts by SLRU order and keeps pinned faces; shaping a
      run 1000 times touches recency a bounded number of times, not 1000; dropping one face
      leaves other faces' caches intact; budget exhaustion in one cache does not evict from
      the other.
- [ ] Gate green → commit.

---

## X9 — Text Work Scheduler (§14.4, §12.20)

Goal: `text_work.rs` (540 lines, 10 tests, used only by a benchmark) runs the runtime's text
work, so the main thread stops doing the expensive parts.

- [ ] The scheduler owns shaping / MTSDF generation / exact-coverage regeneration off the main
      thread, with the frame budget as its admission rule (§14.1).
- [ ] The worker boundary respects unsafe shaping boundaries (§12.7, §12.9): a reshape at a
      worker boundary does not mechanically cut a `ShapedRun`.
- [ ] Main-thread work per frame is bounded and measured; a pending unit yields last-good
      output rather than a stall.
- [ ] Tests: an edit cadence performs no shaping on the main thread (the existing bench gate,
      re-asserted against the runtime path); a frame that exceeds its budget defers rather
      than overruns; deferred work completes deterministically in headless.
- [ ] Gate green → commit.

---

## X10 — High-refresh and resource gates (§14, §26)

Goal: the benchmark matrix measures the runtime, not module-level fixtures.

- [ ] `high_refresh.rs` and `incremental_edit.rs` extended to drive the wired runtime path
      (X6–X9), keeping their existing assertions as the floor.
- [ ] §26 matrix coverage for the categories this program can measure headlessly: startup,
      resolution / fallback, face cache, shaping, world-ready correctness / editing, glyph
      representation / atlas.
- [ ] Per-tier regression gates at 60 / 120 / 144 / 240 Hz frame budgets (§14.1), release
      builds only (AGENTS 36).
- [ ] A scrolling CJK document and a mixed-direction editing session as steady-state cases:
      constant draw / upload / shape counts across identical frames.
- [ ] Gate green → commit.

---

## X11 — Inspector (§25)

Goal: the DoD's last functional item — `Inspector 能解释每一个 fallback 和 cache miss`.

- [ ] Per-fallback explanation: requested family / role / locale, the chain walked, why each
      face declined (no coverage / not resident / declined by provider), what was chosen.
- [ ] Per-cache-miss explanation for face, shaping, coverage and glyph residency: the key, the
      budget state, and the eviction that caused it if there was one.
- [ ] Representation explanation per glyph: current kind, bucket, promotion state, and the
      observed transform that drove it.
- [ ] Exposed through the debug introspection surface (AGENTS 62), not a print path; stripped
      or feature-gated so it imposes no steady-state release cost (AGENTS 60).
- [ ] Tests: a forced fallback reports the full chain with a reason per declining face; a
      forced eviction reports the evicted key and the admission that caused it.
- [ ] Gate green → commit.

---

## X12 — Program Done

- [ ] Walk `Viso_Text_Font_Runtime.md` §29 item by item and tick it in the spec with the test
      or counter that proves it. An item with no evidence is not ticked.
- [ ] Items that cannot be verified in this environment are listed by name with the reason
      (AGENTS 69, §53) rather than silently ticked.
- [ ] FREEZE: `GlyphImageKind` and the representation state machine, `GlyphResidency` pool and
      eviction contract, `TextPosition` / `TextOffset` / `CaretAffinity`, the paragraph
      pipeline entry contract, face and shaping cache budget contracts, the MTSDF bucket and
      quality-window constants.
- [ ] `TODO.md` header corrected: the text/font program is complete because this file says so
      with evidence, not by assertion.

---

## Deferred — named, with the reason

- **Windows DirectWrite / Linux fontconfig / Android platform adapters (§4.2).** Only the
  macOS CoreText adapter exists. Each needs its own device to verify against, and the
  cross-platform build lane is the tooling program's job (`TODO.md` T4). The trait seam
  (`SystemFontProvider`, `ColorGlyphRasterizer`) is already the right shape and needs no
  change to accept them.
- **`assets/fonts/` automatic registration (§3.1).** Requires `Viso.toml` and an asset
  pipeline to scan and emit a `FontManifest` at build time. `font_manifest.rs` and
  `app_fonts.rs` are ready to consume one. Blocked on `TODO.md` T0/T4 — a real cross-program
  dependency, not a gap in this crate.
- **WASM / Canvas lane (§16) and Progressive / Remote fonts (§17).** `font_provider.rs`
  (`ExternalFontProvider`, `ExternalFetchCoordinator`, `SubsetRange`) and `progressive.rs`
  are implemented and tested; they have no consumer because there is no web target yet
  (`TODO.md` deferred web phases). Wiring them without one would be unverifiable.
- **Real-device high-refresh cadence at 120 / 144 / 240 Hz.** Headless has no present loop,
  so X10's gates measure CPU frame budget, not device cadence. Stated as a budget contract,
  not a measured frame rate.
- **3000 installed fonts startup behavior (§4.1, DoD item 3).** The structural property —
  no `FontDB`, no eager enumeration, CoreText queried per role on demand — is testable and
  will be tested. The absolute startup number on a machine with 3000 installed fonts is not
  reproducible here.
- **Thai / Lao / Khmer dictionary segmenter providers (§12.11).** The provider seam is
  specified and `icu_segmenter` supports it; shipping a dictionary is a data-packaging
  decision that belongs with the asset pipeline.
