# Footprint baseline

Written by `cargo xtask footprint --record`.

- Date: 2026-10-29
- Machine: Apple M4, 16 GiB, macOS 27.0.1 (26A434)
- Toolchain: rustc 1.99.1 (58cf8f9c3 2026-10-13)
- Makepad: 70c90b7c0bc0 (2026-09-12)
- Board (the `-board` rows): aarch64, 5 CPUs, 1.9 GiB, Linux 6.16.10-200.fc42.aarch64 (aarch64-unknown-linux-gnu)
- Build: opt-level=3 lto=thin codegen-units=1 panic=unwind strip=symbols debug=false incremental=false; shipping subjects: opt-level=s lto=fat codegen-units=1 panic=unwind strip=symbols debug=false incremental=false
- Each value: the median of 3 runs after one warm-up

## Desktop window: Viso and Makepad

| metric | Viso counter | Makepad counter | Viso list | Makepad list |
|---|---:|---:|---:|---:|
| binary | 8.45 MiB | 12.24 MiB | 8.45 MiB | 12.29 MiB |
| text | 7.83 MiB | 11.38 MiB | 7.83 MiB | 11.41 MiB |
| data | 331.1 KiB | 425.4 KiB | 331.1 KiB | 426.4 KiB |
| bss | 6.9 KiB | 13.8 KiB | 6.9 KiB | 13.8 KiB |
| first frame (ms) | 152.0 | 2178.6 | 139.2 | 2231.6 |
| peak RSS | 89.80 MiB | 346.28 MiB | 90.03 MiB | 348.02 MiB |
| idle RSS | 89.50 MiB | 281.55 MiB | 89.72 MiB | 285.31 MiB |
| peak footprint | 79.33 MiB | 318.89 MiB | 88.50 MiB | 325.52 MiB |
| idle footprint | 28.47 MiB | 165.59 MiB | 37.94 MiB | 172.19 MiB |
| GPU (OS) | 5.25 MiB | 18.91 MiB | 14.17 MiB | 21.39 MiB |
| heap live | 1.44 MiB | 103.22 MiB | 1.67 MiB | 103.61 MiB |
| heap peak | 1.44 MiB | 137.58 MiB | 1.67 MiB | 137.64 MiB |
| idle CPU (ms/s) | 0.0 | 3.3 | 0.0 | 0.0 |
| idle wakeups (/s) | 0.0 | 0.3 | 0.0 | 0.3 |

## Every subject

| subject | binary | text | data | bss | first frame (ms) | peak RSS | idle RSS | peak footprint | idle footprint | GPU (OS) | GPU (budget) | heap live | heap peak | idle CPU (ms/s) | idle wakeups (/s) | click frame p50 (µs) | click frame p95 (µs) | scroll frame p50 (µs) | scroll frame p95 (µs) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| viso/desktop/counter | 8.45 MiB | 7.83 MiB | 331.1 KiB | 6.9 KiB | 152.0 | 89.80 MiB | 89.50 MiB | 79.33 MiB | 28.47 MiB | 5.25 MiB | — | 1.44 MiB | 1.44 MiB | 0.0 | 0.0 | — | — | — | — |
| viso/desktop/list | 8.45 MiB | 7.83 MiB | 331.1 KiB | 6.9 KiB | 139.2 | 90.03 MiB | 89.72 MiB | 88.50 MiB | 37.94 MiB | 14.17 MiB | — | 1.67 MiB | 1.67 MiB | 0.0 | 0.0 | — | — | — | — |
| viso/embedded-cpu/counter | 3.94 MiB | 3.70 MiB | 153.7 KiB | 5.7 KiB | 123.5 | 93.91 MiB | 92.45 MiB | 29.41 MiB | 27.94 MiB | 80.0 KiB | — | 4.96 MiB | 6.37 MiB | 0.0 | 0.0 | — | — | — | — |
| viso/embedded-cpu/list | 3.94 MiB | 3.70 MiB | 153.7 KiB | 5.7 KiB | 116.6 | 111.81 MiB | 107.81 MiB | 47.28 MiB | 43.28 MiB | 80.0 KiB | — | 10.08 MiB | 13.92 MiB | 0.0 | 0.0 | — | — | — | — |
| viso/headless/counter | 9.03 MiB | 8.41 MiB | 343.3 KiB | 1.6 KiB | 5.5 | 14.95 MiB | — | 4.56 MiB | — | — | 1.07 MiB | 3.00 MiB | 3.00 MiB | — | — | 20 | 24 | — | — |
| viso/headless/list | 9.03 MiB | 8.41 MiB | 343.3 KiB | 1.6 KiB | 5.8 | 16.50 MiB | — | 6.03 MiB | — | — | 1.12 MiB | 4.21 MiB | 4.22 MiB | — | — | 25 | 29 | 22 | 286 |
| viso/embedded-cpu-headless/counter | 7.28 MiB | 6.77 MiB | 251.7 KiB | 1.3 KiB | 5.3 | 14.98 MiB | — | 4.75 MiB | — | — | 328.6 KiB | 2.78 MiB | 2.78 MiB | — | — | 19 | 23 | — | — |
| viso/embedded-cpu-headless/list | 7.28 MiB | 6.77 MiB | 251.7 KiB | 1.3 KiB | 5.8 | 16.61 MiB | — | 6.30 MiB | — | — | 375.8 KiB | 3.97 MiB | 3.98 MiB | — | — | 25 | 30 | 24 | 309 |
| makepad/desktop/counter | 12.24 MiB | 11.38 MiB | 425.4 KiB | 13.8 KiB | 2178.6 | 346.28 MiB | 281.55 MiB | 318.89 MiB | 165.59 MiB | 18.91 MiB | — | 103.22 MiB | 137.58 MiB | 3.3 | 0.3 | — | — | — | — |
| makepad/desktop/list | 12.29 MiB | 11.41 MiB | 426.4 KiB | 13.8 KiB | 2231.6 | 348.02 MiB | 285.31 MiB | 325.52 MiB | 172.19 MiB | 21.39 MiB | — | 103.61 MiB | 137.64 MiB | 0.0 | 0.3 | — | — | — | — |
| makepad/desktop-stock/counter | 11.67 MiB | 10.81 MiB | 425.2 KiB | 13.7 KiB | — | — | — | — | — | — | — | — | — | — | — | — | — | — | — |
| viso/embedded-cpu-board/counter | 3.76 MiB | 3.62 MiB | 142.5 KiB | 3.5 KiB | 20.0 | 11.73 MiB | 11.73 MiB | — | — | — | — | 5.14 MiB | 5.14 MiB | 0.0 | 0.0 | — | — | — | — |
| viso/embedded-cpu-board/list | 3.76 MiB | 3.62 MiB | 142.5 KiB | 3.5 KiB | 20.0 | 12.03 MiB | 12.03 MiB | — | — | — | — | 5.38 MiB | 5.38 MiB | 0.0 | 0.0 | — | — | — | — |
| viso/embedded-gpu-board/counter | 4.26 MiB | 4.10 MiB | 153.6 KiB | 5.6 KiB | 120.0 | 140.89 MiB | 140.89 MiB | — | — | — | — | 6.70 MiB | 6.70 MiB | 0.0 | 0.0 | — | — | — | — |
| viso/embedded-gpu-board/list | 4.26 MiB | 4.10 MiB | 153.6 KiB | 5.6 KiB | 110.0 | 141.05 MiB | 141.05 MiB | — | — | — | — | 6.93 MiB | 6.93 MiB | 0.0 | 0.0 | — | — | — | — |
