# Footprint baseline

Written by `cargo xtask footprint --record`.

- Date: 2026-10-28
- Machine: Apple M4, 16 GiB, macOS 27.0.1 (26A434)
- Toolchain: rustc 1.99.1 (58cf8f9c3 2026-10-13)
- Makepad: 70c90b7c0bc0 (2026-09-12)
- Board (the `-board` rows): aarch64, 5 CPUs, 1.9 GiB, Linux 6.16.10-200.fc42.aarch64 (aarch64-unknown-linux-gnu)
- Build: opt-level=3 lto=thin codegen-units=1 panic=unwind strip=symbols debug=false incremental=false
- Each value: the median of 3 runs after one warm-up

## Desktop window: Viso and Makepad

| metric | Viso counter | Makepad counter | Viso list | Makepad list |
|---|---:|---:|---:|---:|
| binary | 8.91 MiB | 12.24 MiB | 8.91 MiB | 12.29 MiB |
| text | 8.27 MiB | 11.38 MiB | 8.27 MiB | 11.41 MiB |
| data | 338.2 KiB | 425.4 KiB | 338.2 KiB | 426.4 KiB |
| bss | 7.0 KiB | 13.8 KiB | 7.0 KiB | 13.8 KiB |
| first frame (ms) | 93.8 | 2178.6 | 93.7 | 2231.6 |
| peak RSS | 84.59 MiB | 346.28 MiB | 84.95 MiB | 348.02 MiB |
| idle RSS | 84.11 MiB | 281.55 MiB | 84.69 MiB | 285.31 MiB |
| peak footprint | 76.77 MiB | 318.89 MiB | 82.17 MiB | 325.52 MiB |
| idle footprint | 40.13 MiB | 165.59 MiB | 31.56 MiB | 172.19 MiB |
| GPU (OS) | 5.25 MiB | 18.91 MiB | 10.22 MiB | 21.39 MiB |
| heap live | 1.49 MiB | 103.22 MiB | 1.72 MiB | 103.61 MiB |
| heap peak | 1.49 MiB | 137.58 MiB | 1.72 MiB | 137.64 MiB |
| idle CPU (ms/s) | 0.0 | 3.3 | 0.0 | 0.0 |
| idle wakeups (/s) | 0.0 | 0.3 | 0.0 | 0.3 |

## Every subject

| subject | binary | text | data | bss | first frame (ms) | peak RSS | idle RSS | peak footprint | idle footprint | GPU (OS) | GPU (budget) | heap live | heap peak | idle CPU (ms/s) | idle wakeups (/s) | click frame p50 (µs) | click frame p95 (µs) | scroll frame p50 (µs) | scroll frame p95 (µs) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| viso/desktop/counter | 8.91 MiB | 8.27 MiB | 338.2 KiB | 7.0 KiB | 93.8 | 84.59 MiB | 84.11 MiB | 76.77 MiB | 40.13 MiB | 5.25 MiB | — | 1.49 MiB | 1.49 MiB | 0.0 | 0.0 | — | — | — | — |
| viso/desktop/list | 8.91 MiB | 8.27 MiB | 338.2 KiB | 7.0 KiB | 93.7 | 84.95 MiB | 84.69 MiB | 82.17 MiB | 31.56 MiB | 10.22 MiB | — | 1.72 MiB | 1.72 MiB | 0.0 | 0.0 | — | — | — | — |
| viso/embedded-cpu/counter | 6.92 MiB | 6.42 MiB | 245.0 KiB | 5.8 KiB | 96.7 | 89.70 MiB | 88.17 MiB | 27.25 MiB | 25.67 MiB | 80.0 KiB | — | 5.01 MiB | 6.42 MiB | 0.0 | 0.0 | — | — | — | — |
| viso/embedded-cpu/list | 6.92 MiB | 6.42 MiB | 245.0 KiB | 5.8 KiB | 88.3 | 108.02 MiB | 103.64 MiB | 45.41 MiB | 40.92 MiB | 80.0 KiB | — | 10.12 MiB | 13.96 MiB | 0.0 | 0.0 | — | — | — | — |
| viso/headless/counter | 9.18 MiB | 8.55 MiB | 343.9 KiB | 1.7 KiB | 6.2 | 15.22 MiB | — | 4.64 MiB | — | — | 1.07 MiB | 3.04 MiB | 3.04 MiB | — | — | 21 | 27 | — | — |
| viso/headless/list | 9.18 MiB | 8.55 MiB | 343.9 KiB | 1.7 KiB | 6.5 | 16.78 MiB | — | 6.16 MiB | — | — | 1.12 MiB | 4.19 MiB | 4.20 MiB | — | — | 26 | 31 | 31 | 284 |
| viso/embedded-cpu-headless/counter | 7.42 MiB | 6.91 MiB | 252.3 KiB | 1.4 KiB | 5.9 | 15.23 MiB | — | 4.84 MiB | — | — | 328.6 KiB | 2.82 MiB | 2.82 MiB | — | — | 20 | 23 | — | — |
| viso/embedded-cpu-headless/list | 7.42 MiB | 6.91 MiB | 252.3 KiB | 1.4 KiB | 6.3 | 16.80 MiB | — | 6.36 MiB | — | — | 375.8 KiB | 4.00 MiB | 4.01 MiB | — | — | 26 | 31 | 27 | 278 |
| makepad/desktop/counter | 12.24 MiB | 11.38 MiB | 425.4 KiB | 13.8 KiB | 2178.6 | 346.28 MiB | 281.55 MiB | 318.89 MiB | 165.59 MiB | 18.91 MiB | — | 103.22 MiB | 137.58 MiB | 3.3 | 0.3 | — | — | — | — |
| makepad/desktop/list | 12.29 MiB | 11.41 MiB | 426.4 KiB | 13.8 KiB | 2231.6 | 348.02 MiB | 285.31 MiB | 325.52 MiB | 172.19 MiB | 21.39 MiB | — | 103.61 MiB | 137.64 MiB | 0.0 | 0.3 | — | — | — | — |
| makepad/desktop-stock/counter | 11.67 MiB | 10.81 MiB | 425.2 KiB | 13.7 KiB | — | — | — | — | — | — | — | — | — | — | — | — | — | — | — |
| viso/embedded-cpu-board/counter | 6.88 MiB | 6.65 MiB | 240.7 KiB | 4.0 KiB | 30.0 | 13.79 MiB | 13.79 MiB | — | — | — | — | 5.18 MiB | 5.18 MiB | 0.0 | 0.0 | — | — | — | — |
| viso/embedded-cpu-board/list | 6.88 MiB | 6.65 MiB | 240.7 KiB | 4.0 KiB | 30.0 | 14.08 MiB | 14.08 MiB | — | — | — | — | 5.43 MiB | 5.43 MiB | 0.0 | 0.0 | — | — | — | — |
| viso/embedded-gpu-board/counter | 7.41 MiB | 7.17 MiB | 252.2 KiB | 5.3 KiB | 130.0 | 142.82 MiB | 142.82 MiB | — | — | — | — | 6.74 MiB | 6.74 MiB | 0.0 | 0.0 | — | — | — | — |
| viso/embedded-gpu-board/list | 7.41 MiB | 7.17 MiB | 252.2 KiB | 5.3 KiB | 130.0 | 143.16 MiB | 143.16 MiB | — | — | — | — | 6.97 MiB | 6.97 MiB | 0.0 | 0.0 | — | — | — | — |
