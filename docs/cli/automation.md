# Automation and CI

Everything `viso` does can be driven by a script, a CI job, an editor or an AI agent
without parsing its English output: `--json` switches every command to a stream of
events, every outcome has a stable exit code, and no command waits for an answer off
a terminal.

## `--json`: a stream of events

```bash
viso check --json
```

Each line on stdout is one JSON object — JSON Lines, UTF-8, no line breaks inside
an event. A long command keeps writing as it goes; it never saves everything for one
object at the end. The last event is always the `summary`.

```json
{"schema":"viso.cli.event","schema_version":1,"seq":0,"type":"summary","timestamp_ms":1792223904138,"session_id":"1328a-18df42252ba20f78","command":"check","payload":{"status":"success","exit_code":0,"elapsed_ms":8,"warning_count":0,"error_count":0,"artifact_count":0,"package":"my_app","file_count":1}}
```

Every event has the same envelope:

```text
schema          "viso.cli.event"
schema_version  1
seq             0, 1, 2, … within one run
type            what the payload is
timestamp_ms    Unix time in milliseconds
session_id      this run; for run and serve, the dev session's ID
command         the command path, such as "build" or "android emulator list"
payload         the event's data
```

The event types:

```text
progress      phase, message, current?, total?
diagnostic    severity, code, message, primary location, related, notes, fixes
artifact      kind, path, target, profile, build_id, size, hash, cached
device        platform, id, name, state (booting, ready, installing, launched, stopped)
test          name, domain, status (pass, fail, skip), duration_ms, message?
trace         a game tick's probe values
profile       frame metrics: p50, p95, max per metric
inspect       an event in the inspected app: input, focus, dirty
server        url, host, port, lan
dev           one edit's outcome in a dev session: applied, rejected, held, warm_restarted
runtime       a connected app's state in a dev session
doctor_check  name, status (ok, warn, missing, error), message?, suggestion?
result        what a query command answers: config, schema, explain, dump, list, --dry-run
log           level, source (app, device, tool), message
summary       status, exit_code, elapsed_ms, and counts
```

Under `--json`, what the app, the device and the tools print arrives as `log`
events, so stdout is never mixed. `summary.payload.exit_code` is always the process's
exit code.

**Diagnostics** have one shape everywhere — the compiler's, the configuration's, the
CLI's own:

```json
{"type":"diagnostic","payload":{"schema_version":"1.0","severity":"error","code":"E3101",
 "message":"…","primary":{"file":"src/app.vs","byte_start":120,"byte_end":128,"line":7,
 "column_utf16":5,"end_line":7,"end_column_utf16":13},"related":[],"notes":[],"fixes":[]}}
```

A fix with edits can be applied as it is; a fix with no edits names the command to
run next.

**Compatibility.** Within a `schema_version`, events only gain optional fields, new
types and new values. Ignore fields and types you do not know.

`--quiet` with `--json` leaves out `progress` and `log` events. `viso lsp` and
`viso completion` do not take `--json`.

## Exit codes

```text
0     success
1     the sources, configuration or tests failed: diagnostics, failed expectations, snapshot mismatches
2     the command line was wrong: unknown command or flag, conflicting flags, a bad argument
3     the environment is missing something: an SDK, a tool, a target, a device; a lock is held
4     the build failed: Rust compile, link or shader errors
5     running failed: the app crashed, a device could not be reached, a test runner crashed
6     packaging, signing or exporting failed
7     an internal protocol or tooling failure
130   interrupted (Ctrl-C, SIGINT, SIGTERM)
```

Every CLI diagnostic code belongs to one exit code: `CLI_*` exit 2, `ENV_*` 3,
`BUILD_*` 4, `RUN_*` 5, `TEST_*` 1, `PACKAGE_*` and `EXPORT_*` 6, `INTERNAL_*` 7.
`viso explain <CODE>` describes any of them.

## No prompts off a terminal

A command that would ask a question — which emulator to use, whether to accept the
Android SDK licenses, whether to create a profile — asks only when stdin, stdout and
stderr are all a terminal and `CI` is not set. Otherwise it fails at once with a
diagnostic naming the flag or command that answers it:

```text
error[DEVICE_AMBIGUOUS]: several android emulator profiles and no default
 = note: profiles: pixel, tablet
 = note: set a default with `[android] device = "pixel"` in `.viso/local.toml`
 = help: run `viso run android --device pixel`
```

It never reads an answer waiting on stdin.

## Cancelling

The first Ctrl-C (or SIGTERM) cancels cooperatively: no new work starts, the tools
`viso` started are ended with everything they started, and the step in progress —
a download, a write, a swap of `dist/` — finishes or rolls back. The command exits
`130` with a `summary` whose status is `cancelled`. A second Ctrl-C ends it at once.

## Locks

Two `viso` processes never write the same thing at once: builds, packages, the dev
session and the downloads each take a lock under `target/viso/locks/` or
`~/.viso/`. A build waits for another build of the same target; a second `viso run`
in the same project fails with `ENV_LOCK_HELD`, naming the process that holds it.
A holder keeps its lock fresh; one not refreshed for 90 seconds — left by a process
that crashed — is taken over, with a warning.

## A CI job

```bash
set -e
viso doctor --json                 # record the environment
viso fmt --check                   # formatting
viso check --a11y strict --json    # sources and configuration
viso test --json                   # unit, UI and game tests, headless
viso build --profile release
viso package --signing required    # fails rather than shipping unsigned
```

UI tests run headless with software rendering, so they need no GPU or display. Keep
`tests/snapshots/` in version control; when a snapshot departs, publish
`target/viso/snapshots/` as a build artifact to see the diff.

Pass secrets — the Android keystore's passwords — through the environment from your
CI's secret store. `viso` never prints them, not even under `--verbose`.
