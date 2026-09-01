# Characterization Baseline (§64)

Viso provides **no runtime compatibility** with makepad. Migration success is
judged against measured behavior/performance baselines captured from makepad,
not "does the old code still run".

Phase 0 scaffolds this directory. Real baselines are captured before each
subsystem migration and stored here.

## Scenarios (§64.1)

```
minimal_window      large_list_10k     large_list_100k    text_stress
code_editor         animated_dashboard image_gallery      3d_scene
mobile_form         hot_reload_ui      ime_text_input     nested_scroll
multi_window        ui_zoo
```

## Per-scenario metrics (§64.2)

cold/warm startup · idle CPU · CPU/GPU frame time · input-to-present latency ·
draw calls · pipeline switches · GPU uploads/frame · allocations/frame ·
peak/steady memory · layout node count · dirty node count · text shaping time ·
glyph cache hit rate · scroll/resize performance · hot-reload latency ·
accessibility snapshot.

## Behavior snapshots (§64.3)

screenshots · input tapes · semantic/a11y snapshots · focus order · IME
sequences · scroll/gesture traces · shader golden outputs · widget interaction
traces.

## Status

Scaffold only. No baselines captured yet — captured per scenario as the
corresponding subsystem is migrated.
