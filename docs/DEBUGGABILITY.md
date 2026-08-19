# Production Debuggability Contract

Application Launcher must remain diagnosable in the same optimized release
configuration that is used normally. A debug build is not an acceptable
substitute for evidence from a release-only hang, crash, CPU storm, or stale
GUI/daemon state.

## Runtime Modes

The mode is process-local and is included in every semantic snapshot.

| Mode | Entry | Always available | Runtime behavior |
| --- | --- | --- | --- |
| `minimal` | `APPLICATIONLAUNCHER_DIAGNOSTICS=minimal` before process startup | ELF identity, full external symbols, panic hook, independent diagnostic endpoint | Event and counter calls return before locking or allocating. The endpoint blocks in `accept` and consumes no idle CPU. |
| `observable` | Default | Minimal facilities plus bounded events, counters, gauges, and worker registry | Records only lifecycle/state boundaries. Rendering, row layout, search ranking, title-spinner updates, and audio samples are excluded. |
| `runtime-activated` | An authorized `--diagnose auto` collector is attached | Repeated stacks, process state, semantic snapshot, journals, checksums, and requested perf/core evidence | Temporary. Only the exact same-user socket peer PID is authorized with `PR_SET_PTRACER`; authorization expires after 60 seconds and is explicitly revoked after capture. |

The permitted state transitions are:

```text
minimal -> runtime-activated -> minimal
observable -> runtime-activated -> observable
```

A build never silently changes the configured base mode. Heavy collection is
not scheduled by the GUI update loop or by the daemon D-Bus executor.

## Budgets

The constants are exported by `src/observability.rs` and included in diagnostic
snapshots.

| Facility | Hard limit or measured budget |
| --- | --- |
| Observable idle CPU | At most 2,500 ppm (0.25% of one CPU) |
| Structured event write p99 | At most 50 microseconds |
| Observable resident-memory increment | At most 1 MiB |
| Stripped production binary | At most 64 MiB per component |
| Exact unstripped or separate debug artifact | At most 512 MiB per file |
| Build archive retention | 3 builds per component, plus still-running build IDs |
| Flight recorder | 512 events; oldest event is evicted first |
| Persistent trace ring | 512 checksummed 4 KiB slots; 2 MiB per component |
| Named workers | 64 concurrent entries |
| Event string field | 96 UTF-8 bytes |
| Boundary payload | 2 KiB |
| Semantic snapshot | 1 MiB |
| Panic log | 4 MiB per component |
| Normal diagnostic bundle | 32 MiB, excluding an explicitly requested full core |
| Normal diagnostic collection | 45 seconds |
| Per-command text capture | 4 MiB |
| Repeated stacks | 3 by default; caller values are clamped to 1 through 10 |
| Endpoint discovery | 32 socket candidates, 4 live targets, 250 ms response timeout per candidate |
| Checksum manifest | 256 files and 8 directory levels |

Build metadata, exact binary archiving, and split-symbol generation have zero
runtime CPU and RSS cost. The independent diagnostic server is blocked while
idle. Observable mode has deliberately bounded nonzero work at coarse lifecycle
boundaries; claiming literal zero cost for an always-on recorder would be
incorrect.

## Structured State

Every event has a monotonic timestamp, wall timestamp, sequence, thread ID and
name, category, action, optional operation and parent IDs, object/reason fields,
old/new state, and optional duration. Fields are truncated before insertion and
likely credential values are redacted.

Counters are a fixed enum backed by atomics. Gauges are also fixed atomics.
Neither can grow due to external input. The worker registry is bounded and uses
an RAII registration so completed and unwound workers disappear.

Do not add event recording to:

- `eframe::App::update` frame iteration;
- visible-row or tile rendering;
- fuzzy-rank candidate or token loops;
- braille-spinner title updates;
- audio-level samples.

Add events around infrequent lifecycle transitions, retries, failures, snapshot
replacement, process startup/shutdown, and external operation boundaries.

## Live Capture

Run this while preserving the faulty processes:

```bash
applicationlauncher --diagnose auto
```

Optional activated-only evidence:

```bash
applicationlauncher --diagnose auto --perf
applicationlauncher --diagnose auto --core
```

`--core` is explicit because a full core can contain credentials, document
contents, prompts, and other private process memory. Normal text evidence is
line-redacted for common credential forms. Environment variables and open file
contents are not collected.

The collector discovers private PID-specific sockets for the GUI and daemon. It
does not depend on either GUI repainting or the daemon D-Bus executor. For each
responding target it captures:

- application build identity, executable identity, and ELF build ID;
- bounded `/proc` status, scheduling, memory-map, cgroup, I/O, limits, and
  per-thread state;
- the bounded semantic snapshot and recent structured events;
- repeated all-thread stacks using `eu-stack`, with `gdb` fallback;
- a bounded `ps -L` snapshot and relevant user journal records;
- optional short `perf` recording or explicit full live core;
- a SHA-256 manifest covering the completed bundle.

Output is timestamped below
`$XDG_STATE_HOME/applicationlauncher/diagnostics`, with directory mode `0700`
and ordinary evidence mode `0600`.

## Release Artifacts

Build and archive a production release with:

```bash
scripts/build-debuggable-release
```

The normal release profile keeps full DWARF and explicitly requests a GNU SHA-1
ELF build ID. For each GUI and daemon ELF build ID the script stores:

- the exact unstripped Cargo output;
- a stripped copy with a GNU debug link;
- a separate debug-symbol file;
- `build-info.json` with source revision, dirty-tree hash, application and ELF
  build IDs, binary hashes and sizes, target, compiler, linker, flags, features,
  and dependency graph;
- a SHA-256 artifact manifest.

The default archive is
`$XDG_STATE_HOME/applicationlauncher/builds/by-build-id/BUILD_ID`. Artifact
generation never changes or restarts a running process. Successful generation
retains the newest three builds for each component and any older build ID still
mapped by a running launcher or daemon. Override the count with
`--keep-builds COUNT` or `APPLICATIONLAUNCHER_BUILD_KEEP_BUILDS`.

## Progress Invariants

- The diagnostic endpoint blocks in `accept`; it has no idle polling loop.
- Diagnostic authorization is revoked by explicit completion or a 60-second
  timeout.
- Every command has a timeout and bounded captured stdout/stderr.
- Endpoint discovery has fixed candidate, target, and response-time bounds.
- Bundle growth stops at the global text/perf budget. A core is exempt only when
  explicitly requested.
- Manifest traversal has fixed file-count and directory-depth bounds.
- The recorder evicts before inserting past capacity.
- The persistent trace writer uses a bounded non-blocking queue and writes the
  payload before its commit header; incomplete slots are ignored during replay.
- The worker registry refuses entries after its fixed maximum.
- Daemon retry loops retain their existing sleep/backoff and do not emit an
  event on every poll.
- Capture failures are accumulated in `capture.json`; one failed evidence source
  does not discard successful evidence from either process.

## Boundary Replay And Fault Injection

The persistent ring stores versioned typed boundary records for window-feed
resynchronization, terminal actions, attention dispatch, tracker mutations,
search decisions, icon resolution, timers, and explicitly injected faults. It
does not record visible rows, fuzzy-rank candidates, spinner frames, or audio
samples. This keeps normal rendering and search paths out of the persistence
cost while retaining the external inputs and lifecycle decisions that can be
replayed.

Replay a ring or a captured bundle with:

```bash
applicationlauncher replay PATH_TO_RING_OR_BUNDLE
```

Replay validates sequence ordering, schema versions, checksums, and the
deterministic state reducer. Decision explanations are recorded only while a
diagnostic peer has explicitly activated runtime collection.

Fault injection requires both `APPLICATIONLAUNCHER_ALLOW_FAULT_INJECTION=1`
and a comma-separated `APPLICATIONLAUNCHER_FAULT_INJECTION` value. Supported
points are `window-feed-drop`, `window-feed-duplicate`,
`terminal-send-failure`, `tracker-write-busy`, and `diagnostic-response-delay`.
An optional `APPLICATIONLAUNCHER_FAULT_SEED` applies the fault every Nth
operation. The normal process path performs only a cheap disabled-plan check;
no fault is active by default.

## Verification

Run:

```bash
applicationlauncher debug-doctor
cargo test --all-targets --locked
```

`debug-doctor` verifies ELF identity, available symbolization data, required
tools, the private output mode, the independent endpoint, exact-PID ptrace
authorization, recorder p99 latency, retained recorder RSS, idle endpoint CPU,
core routing, and whether the current ELF has an archived artifact. Tests cover
hard bounds, redaction, checksums, unavailable endpoints, malformed ELF input,
panic reports, a diagnosable blocked process, and a contained native-crash
probe.
