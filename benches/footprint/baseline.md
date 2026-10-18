# Footprint baseline

Written by `cargo xtask footprint --record`; the method is Architecture §50A.5.

- Date: 2026-10-18
- Machine: Apple M4, 16 GiB, macOS 27.0.1 (26A434)
- Toolchain: rustc 1.99.1 (58cf8f9c3 2026-10-13)
- Makepad: 70c90b7c0bc0 (2026-09-12)
- Build: opt-level=3 lto=thin codegen-units=1 panic=unwind strip=symbols debug=false incremental=false
- Each value: the median of 3 runs after one warm-up

## Desktop window: Viso and Makepad

| metric | Viso counter | Makepad counter | Viso list | Makepad list |
|---|---:|---:|---:|---:|
| binary | 5.01 MiB | 12.24 MiB | 5.01 MiB | 12.29 MiB |
| __TEXT | 4.61 MiB | 11.38 MiB | 4.61 MiB | 11.41 MiB |
| data | 198.6 KiB | 425.4 KiB | 198.6 KiB | 426.4 KiB |
| bss | 5.1 KiB | 13.8 KiB | 5.1 KiB | 13.8 KiB |
| first frame (ms) | 109.5 | 2178.6 | 97.8 | 2231.6 |
| peak RSS | 80.92 MiB | 346.28 MiB | 81.03 MiB | 348.02 MiB |
| idle RSS | 80.62 MiB | 281.55 MiB | 80.78 MiB | 285.31 MiB |
| peak footprint | 76.13 MiB | 318.89 MiB | 81.30 MiB | 325.52 MiB |
| idle footprint | 25.30 MiB | 165.59 MiB | 58.52 MiB | 172.19 MiB |
| GPU (OS) | 5.38 MiB | 18.91 MiB | 10.34 MiB | 21.39 MiB |
| heap live | 5.45 MiB | 103.22 MiB | 5.57 MiB | 103.61 MiB |
| heap peak | 5.45 MiB | 137.58 MiB | 5.57 MiB | 137.64 MiB |
| idle CPU (ms/s) | 0.0 | 3.3 | 0.0 | 0.0 |
| idle wakeups (/s) | 0.0 | 0.3 | 0.0 | 0.3 |

## Every subject

| subject | binary | __TEXT | data | bss | first frame (ms) | peak RSS | idle RSS | peak footprint | idle footprint | GPU (OS) | GPU (budget) | heap live | heap peak | idle CPU (ms/s) | idle wakeups (/s) | click frame p50 (µs) | click frame p95 (µs) | scroll frame p50 (µs) | scroll frame p95 (µs) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| viso/desktop/counter | 5.01 MiB | 4.61 MiB | 198.6 KiB | 5.1 KiB | 109.5 | 80.92 MiB | 80.62 MiB | 76.13 MiB | 25.30 MiB | 5.38 MiB | — | 5.45 MiB | 5.45 MiB | 0.0 | 0.0 | — | — | — | — |
| viso/desktop/list | 5.01 MiB | 4.61 MiB | 198.6 KiB | 5.1 KiB | 97.8 | 81.03 MiB | 80.78 MiB | 81.30 MiB | 58.52 MiB | 10.34 MiB | — | 5.57 MiB | 5.57 MiB | 0.0 | 0.0 | — | — | — | — |
| viso/embedded-cpu/counter | 4.53 MiB | 4.14 MiB | 187.1 KiB | 4.4 KiB | 100.5 | 85.89 MiB | 82.67 MiB | 26.45 MiB | 24.77 MiB | 80.0 KiB | — | 7.69 MiB | 9.09 MiB | 0.0 | 0.0 | — | — | — | — |
| viso/embedded-cpu/list | 4.53 MiB | 4.14 MiB | 187.1 KiB | 4.4 KiB | 105.5 | 103.69 MiB | 91.80 MiB | 44.16 MiB | 31.30 MiB | 80.0 KiB | — | 12.68 MiB | 16.52 MiB | 0.0 | 0.0 | — | — | — | — |
| viso/headless/counter | 5.46 MiB | 5.05 MiB | 209.1 KiB | 1.1 KiB | 8.6 | 13.38 MiB | — | 4.48 MiB | — | — | 1.07 MiB | 2.99 MiB | 2.99 MiB | — | — | 18 | 32 | — | — |
| viso/headless/list | 5.46 MiB | 5.05 MiB | 209.1 KiB | 1.1 KiB | 5.2 | 14.56 MiB | — | 5.69 MiB | — | — | 1.08 MiB | 4.03 MiB | 4.05 MiB | — | — | 18 | 21 | 149 | 178 |
| viso/embedded-cpu-headless/counter | 4.95 MiB | 4.56 MiB | 194.6 KiB | 1.1 KiB | 8.7 | 13.17 MiB | — | 4.34 MiB | — | — | 326.1 KiB | 1.49 MiB | 1.49 MiB | — | — | 25 | 31 | — | — |
| viso/embedded-cpu-headless/list | 4.95 MiB | 4.56 MiB | 194.6 KiB | 1.1 KiB | 5.5 | 14.31 MiB | — | 5.50 MiB | — | — | 336.6 KiB | 2.53 MiB | 2.54 MiB | — | — | 18 | 21 | 152 | 174 |
| makepad/desktop/counter | 12.24 MiB | 11.38 MiB | 425.4 KiB | 13.8 KiB | 2178.6 | 346.28 MiB | 281.55 MiB | 318.89 MiB | 165.59 MiB | 18.91 MiB | — | 103.22 MiB | 137.58 MiB | 3.3 | 0.3 | — | — | — | — |
| makepad/desktop/list | 12.29 MiB | 11.41 MiB | 426.4 KiB | 13.8 KiB | 2231.6 | 348.02 MiB | 285.31 MiB | 325.52 MiB | 172.19 MiB | 21.39 MiB | — | 103.61 MiB | 137.64 MiB | 0.0 | 0.3 | — | — | — | — |
| makepad/desktop-stock/counter | 11.67 MiB | 10.81 MiB | 425.2 KiB | 13.7 KiB | — | — | — | — | — | — | — | — | — | — | — | — | — | — | — |
