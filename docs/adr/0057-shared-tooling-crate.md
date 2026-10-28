# ADR 0057 — The tools share their services in `viso-tooling`, reporting through a `Reporter`

- Status: Accepted
- Date: 2026-10-28

## Context

The `viso` command held every tooling service in its own crate: build orchestration, the dev
session and its control endpoint, the inspector client, simulator and emulator control, the
platform toolchains, delivery and deployment. They reported straight into the command's
`Output`, which renders human text or the JSON event stream. Studio needs the same services,
and it cannot depend on a binary crate or render through the command's output.

## Decision

1. **One shared crate.** `tools/tooling` (`viso-tooling`) holds the services:
   - `build`, `dev`, `inspect`, `device`, `toolchain`, `delivery` and `deploy`;
   - `process`, `atomic`, `interrupt` and `secret`, which run programs, write files whole,
     catch the user's interrupt and keep secrets out of what is shown;
   - `error`, the exit codes and the codes the services raise.

   It does not parse arguments or render.
2. **A reporting trait.** The services report through `viso_tooling::Reporter`:
   - diagnostics in a `Source`, failures under a `Code` (returning their exit), errors,
     log lines and device states;
   - dev, rebuild, runtime and profile events.

   It is object-safe, so a service takes `&mut dyn Reporter`. The CLI's `Output` implements
   it, and Studio implements it its own way. `Source` and `DevIds` move with it.
3. **The inspector client is data.** `inspect` asks a session's control endpoint for the
   census, node and frame detail and the event journal, and decodes what comes back. It
   finds a project's session by its lock and follows a window's journal. It returns typed
   errors (`AskError`, `NoSession`). The command turns those into failures and renders the
   answers.
4. **The CLI is a thin consumer.** `check-deps` allows `viso-cli` only:
   - `viso-tooling`, `viso-project`, `viso-package`, `viso-dsl`, `viso-dev-protocol`,
     `viso-schema` and `viso-ende`;
   - `viso-image`, for comparing a snapshot with its golden image.

   Its `NEVER` edges forbid `viso-svg` and `viso-text`, because decoding a changed asset
   belongs to the dev session. `viso-tooling` is on the compiler side: its closure reaches
   no runtime, platform or GPU crate.

## Consequences

- Studio links the same services the command runs, and a fix to one lands in both.
- The `Reporter` grows when a service reports something new. A front end then decides how
  to show it.
- The command's rendering stays in `viso-cli`. Its unit tests shrink to argument parsing,
  rendering and command orchestration. The services' tests move with them, and report into
  a counting `Reporter`.
