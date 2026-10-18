# Benchmarks (§7.3, §21 performance contract)

Performance is an architectural contract, not a late pass. No change may be
described as a performance improvement without a benchmark / profiler trace /
allocation-count / GPU-timing / memory comparison (AGENTS §7.3).

Phase 0 scaffolds this directory. Benches are added alongside the subsystems
they measure (node arena traversal, layout, batching, glyph cache, large-list
scroll, …) and wired into CI as perf-regression gates.

## Footprint (`benches/footprint`)

`cargo xtask footprint` builds the reference app in `benches/footprint` for
each footprint profile, and Makepad's equivalent apps in
`benches/footprint/makepad` against a Makepad checkout, all with the same
release settings, and measures binary and section sizes, first-frame time,
RSS and memory footprint, GPU memory, the counted heap, idle CPU and wakeups,
and per-frame UI-thread time for clicks and list scrolls (Architecture
§50A.5). `--record` writes `footprint/baseline.json` and `baseline.md`;
`--check` holds the Viso subjects to that baseline within each metric's
tolerance.

The subsystem micro-benchmarks live with their crates
(`crates/*/benches`).
