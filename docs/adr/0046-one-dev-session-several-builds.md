# ADR 0046 — One dev session, several builds

- Status: Accepted
- Date: 2026-10-22

## Context

- Hot Reload §43 has one project session serve several runtimes. The
  session already shares one source graph (text, incremental parse,
  catalogs, assets) and compiles once per pass for runtimes with the same
  profile. Plans, shader packing, patches, ACK/NACK and last-good revisions
  are per runtime (§44).
- Only the connection layer stops it: `Expect` holds one build id, and the
  link refuses a runtime of any other build. So the runtimes of one CLI
  session are instances of one build: several pages, several processes.
- `viso run` launches one target. Each target's session takes
  `dev-<target>[-<device>].lock`. A desktop and a web build of one project
  therefore run as two sessions, each with its own source graph, watcher and
  compiles. An edit is compiled twice and reported twice.
- The Rust warm-restart machine is single-app. It snapshots the first
  runtime, closes it, launches the new executable and restores into the next
  runtime of that target (`DevSession::{request_snapshot, restore_next,
  relaunched}`, `restart_target`).
- Studio's Device Matrix needs side-by-side builds. Developers want the same
  thing from the CLI: one edit seen on the desktop and in a browser at once.

## Decision

- **`viso run` takes several targets.**
  - `viso run [TARGET]...` runs each named target as one *lane* of a single
    session: `viso run ios web-gpu`.
  - This machine is no positional target (CLI §1). No target is this
    machine, as today; `--desktop` adds it beside the targets named, as the
    first lane: `viso run web-gpu --desktop`.
  - A target named twice is a usage error.
  - At most one lane is a simulator or emulator, which `--device` names.
    Any of the web targets may be lanes, each once; they share the session's
    one page server.
  - `--browser`, `--open`, `--emulate` and `--cwd` apply to the lanes they
    fit, and are an error when no lane fits.
- **A lane** owns what is per build:
  - its dev lock (`dev-<target>[-<device>].lock`, all taken before any
    build starts, in target order) and its build lock;
  - its Rust plan, scope and `RustBuild`; a lane building for the Rust
    target of a lane before it builds in `<target-dir>/viso/lanes/<target>/`,
    so no lane links over another's executable;
  - its build id;
  - its deploy (host, browser server, simulator, emulator, board) and the
    app it launched;
  - its dev listener, or for a page its lane of the web server;
  - its restart state.
- **The session** owns what is shared:
  - one session id and token;
  - the watcher over the union of the lanes' scopes;
  - the source graph;
  - the control endpoint, published in every lane's lock;
  - the output stream.
- **The link admits each lane's build.**
  - `Expect` carries the set of build ids, each with its target.
  - A runtime of any of them is accepted.
  - A runtime of none of them is refused, as today.
  - A runtime belongs to the lane whose build it runs. `runtime` events name
    its target and build; with more than one lane, an app's lines start with
    `[target] `.
- **A Rust edit** rebuilds every lane whose scope holds the file. Restarts
  run one lane at a time, in lane order: snapshot, close, launch and restore
  belong to one lane until it lands or fails, then the next lane's restart
  starts. A lane's failed build or restart leaves the other lanes running.
- **Ending.**
  - The session ends when every lane's app has ended, or on interrupt.
  - The first interrupt closes every app; the second kills them.
  - The exit code is the first lane's app's.
  - A lane whose app exits while others run reports it, and its runtimes
    leave the session.
- **`viso game` and `inspect`** address a runtime as today. With more than
  one lane, a request naming no target goes to the first lane's app.
- **The web lanes share one page server.**
  - It listens at one port, as `--host`, `--lan` and `--port` ask.
  - One web lane is served at `/`, as before. With several, each is served
    under its target, `/web-gpu/`, `/web-dom/`, `/web-hybrid/`; `/` lists
    them, and a prefix without its slash is redirected to it.
  - A page's dev channel and its own channel open beside it
    (`<lane>/__viso/dev`, `<lane>/__viso/page`), so the server knows each
    socket's lane. A new build of a lane reloads that lane's pages alone, and
    a page's log lines carry its lane's prefix.
  - Cross-origin isolation (`Cross-Origin-Opener-Policy: same-origin`,
    `Cross-Origin-Embedder-Policy: require-corp`) is per lane: only a lane
    whose module imports or defines a shared memory (a threaded build) is
    isolated.
  - Each web lane reports its own `server` event (`target`, `url`, `host`,
    `port`, `lan`) and human line; `--open` opens every lane's page.
- **`viso serve [TARGET]...`** is that session for web targets only:
  `viso serve web-gpu web-dom web-hybrid`, any subset, each once;
  `[web] default_target` when none is named.

## Consequences

- One edit is parsed and type-checked once and planned per runtime. Desktop,
  web and device runtimes advance or stay on their last good independently,
  as §44 shows them.
- Serialized restarts keep the snapshot machine single-app. Restarting
  several targets after a Rust edit takes longer than restarting them in
  parallel, but no restore can land in the wrong lane.
- `dev-<target>` locks still keep two sessions off one target. Running two
  targets in two sessions remains possible, without the sharing.
- Studio launches lanes through the same session API rather than its own
  launcher.
- A page at `/` keeps the URLs of a single-lane session; with several web
  lanes the URLs change, so a tool reads them from the `server` events rather
  than assuming `/`.
