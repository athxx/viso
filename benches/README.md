# Benchmarks (§7.3, §21 performance contract)

Performance is an architectural contract, not a late pass. No change may be
described as a performance improvement without a benchmark / profiler trace /
allocation-count / GPU-timing / memory comparison (AGENTS §7.3).

Phase 0 scaffolds this directory. Benches are added alongside the subsystems
they measure (node arena traversal, layout, batching, glyph cache, large-list
scroll, …) and wired into CI as perf-regression gates.

## Status

Scaffold only. Benches land with their subsystems in later phases.
