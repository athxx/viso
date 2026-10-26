# Web

## The web targets

| Target | What it is | Status |
|---|---|---|
| `web-gpu` | The full Viso renderer in the browser: WebAssembly and WebGPU | Available |
| `web-dom` | The view lowered to DOM elements, for browser semantics, accessibility and search engines | Not available yet |
| `web-hybrid` | A DOM shell with WebGPU islands | Not available yet |

`web-dom` and `web-hybrid` are reported as unavailable (`C0011`, exit 3) until the
DOM backend lands. To put a view into an existing web project today, see
[Exporting to web projects](export.md).

`web-gpu` needs the `wasm32-unknown-unknown` Rust target and `wasm-bindgen` at the
version your `Cargo.lock` pins; `viso doctor web-gpu` checks both, and `--fix` adds
the Rust target. The browser must support WebGPU and the page must be a secure
context — `localhost` is one.

## Run it in a browser

```bash
viso run web-gpu                    # build, serve, open the default browser
viso run web-gpu --browser chrome   # chrome, safari or firefox
viso run web-gpu --browser none     # serve only; open the printed URL yourself
```

Hot reload works as on the desktop: a saved `.vs` edit patches the page in place,
keeping its state.

## `viso serve`

`viso serve` is the same dev session without a browser of its own — for opening the
page elsewhere, or on other devices:

```bash
viso serve web-gpu                    # http://127.0.0.1:8000/
viso serve web-gpu --open             # and open it
viso serve web-gpu --port 9000
viso serve web-gpu --lan              # to the other devices of your network
viso serve web-gpu --host 0.0.0.0 --lan
viso serve                            # the target [web] default_target names
```

It listens on the loopback interface only, unless you pass `--lan`. A busy default
port moves to the next free one, and says so; a busy `--port` is an error.

With `--lan`, any device on the network can load the page and drive the session —
`viso serve` warns you. Other devices' browsers offer WebGPU only on a secure
context, which a plain-HTTP page on a LAN address is not.

Set defaults in `Viso.toml`:

```toml
[web]
default_target = "web-gpu"

[web.serve]
port = 9000
open = true
```

Every response carries `X-Content-Type-Options: nosniff`, a same-origin resource
policy and a Content Security Policy that limits script sources (your app's own
network requests, images and fonts are not restricted). Requests whose `Host` is not
the address being served are refused, which stops DNS rebinding.

## Build a site

```bash
viso build web-gpu                       # dev
viso build web-gpu --profile shipping    # optimized for size, precompressed
```

The bundle in `dist/web/` is a static site:

```text
index.html
sw.js                        with a linked package's web/worker.js
<app>.<hash>.js
<app>_bg.<hash>.wasm
<app>_bg.<hash>.wasm.br      shipping: Brotli and gzip copies
<app>_bg.<hash>.wasm.gz
```

When a package the app links carries `web/worker.js` (as `viso-push` and
`viso-background` do), the bundle also holds `sw.js`, the app's service worker, which
the page registers; see [What linked packages declare](packaging.md#what-linked-packages-declare).

The module and its glue are named by their content, so a server can cache them
forever and revalidate only `index.html` and `sw.js`. Serve `.wasm` as `application/wasm`, and
the `.br`/`.gz` copies when the client accepts them.

How each profile builds for the web (all keys under `[profile.<name>.web]`):

| Key | Values | dev | release | shipping |
|---|---|---|---|---|
| `optimize` | `none`, `speed`, `size` (through `wasm-opt`) | `none` | `speed` | `size` |
| `strip` | names and producers sections removed | the profile's `strip` | | `true` |
| `source_maps` | DWARF kept for the browser's debugger | the profile's `source_maps` | | `false` |
| `brotli`, `gzip` | precompressed copies | `false` | `false` | `true` |

A shipping web build needs `wasm-opt` (from Binaryen); without it, dev and release
builds warn and keep the module unoptimized.

`viso package web-gpu` writes the same bundle as `site/` plus a zip of it — see
[Building and packaging](packaging.md).
