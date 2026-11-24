# Debugging Releases

The normative **Stateful**, schema-version-6 contract is
[`../debuggability.toml`](../debuggability.toml). This is its operator guide.
Ordinary launches remain observable without a tracing flag. Build verification
never restarts an existing application.

## Artifacts

| Artifact | Location | Evidence |
| --- | --- | --- |
| `release-observable` | Default `target/release/{applicationlauncher,applicationlauncherd}` | Bounded events, counters, gauges/high waters, worker states, sink health and independent diagnostic socket |
| `release-minimal` | `target/release-profiles/release-minimal/` | Exact optimized ELF, private symbols/unwind data, immutable identity/source manifests, required recovery records and external capture; no optional in-process diagnostic runtime |
| `diagnostic-build` | `target/release-profiles/diagnostic-build/` | Same release optimization plus explicitly gated fault hooks and contained synthetic probes |

Runtime `off` in an observable artifact is **not** an erased artifact.
`--build-info` reports the actual profile, features, lock/source hashes and
canonical product semantics without starting the GUI, daemon or worker.

Build, verify, archive and publish the normal observable pair:

```bash
scripts/build-debuggable-release
```

This delegates to `scripts/build-release-matrix`, which:

1. Compiles variants in `target/profile-build`, never over the active pair.
2. Packages actual ELFs with compressed full DWARF and GNU build IDs.
3. Rejects minimal observer symbols/strings, residual Python observer AST/bytecode,
   altered product dependencies and ordinary-release fault hooks.
4. Archives exact/stripped binaries, split symbols, hash-verified source inputs,
   immutable Rust source packages, the generated lock and native `DT_NEEDED` modules.
5. Runs private packaged-daemon and archive-only symbol-recovery trials.
6. Compares canonical product digests and designated optimized instructions.
7. Publishes the observable pair after its gates pass. The wrapper's build-ID
   agreement check also guards against a partial two-file publication.

Compatibility repair uses `--only release-observable`: it runs that artifact's
gates, not a new three-profile comparison, and does not overwrite a complete
matrix receipt. Plain Cargo builds remain useful for development but do not
constitute matrix verification. A source change after linking refuses archival
rather than recording a nearby but different source tree.

Optimization level, LTO/codegen defaults, target CPU, allocator, panic strategy,
renderer and product features remain unchanged.

### Erasure Mechanism

`build-support/minimal_workspace.py` generates a disposable manifest using the
authoritative product source. `cfg` and token macros erase own observers before
Rust type checking. `lifecycle_sources.py` erases Python timing and optional
traceback capture without removing required errors or recovery writes.

Pinned Linux `log`/`tracing` observer adapters erase transitive event tokens and
identity-only instrumentation wrappers. They are generation tools, not an
alternative product implementation or proof by themselves: actual linked
observer symbols/markers must be zero. Product dependency versions must remain
identical. New dependency API usage fails compilation or verification rather
than silently broadening this supported graph.

Physically omit optional source modules and test the remaining product:

```bash
uv run --offline --no-project --python /usr/bin/python python -I -B build-support/minimal_workspace.py --negative --test
```

Safety checks, operation IDs used by product coordination, required CLI errors,
restart plans/status, scheduling clocks and renderer behavior are not erased as
convenience logs.

## Runtime Controls

Observable startup controls:

```text
APPLICATIONLAUNCHER_DIAGNOSTICS=off
APPLICATIONLAUNCHER_DIAGNOSTIC_CATEGORIES=baseline,stderr,update-timings
```

The default is observable. Legacy runtime `minimal` aliases `off`. An explicitly
empty category allow-list disables all optional categories. Disabled macros do
not evaluate fields or log arguments. Baseline-disabled startup creates no
recorder writer; the independent same-user control socket remains available.

`baseline` covers state, counters, gauges and workers; `stderr` covers optional
log macros; `update-timings` permits, but does not activate, detailed update
tracing. Fault hooks exist only in `diagnostic-build`.

For a slow update, an agent can directly invoke the running daemon's same-user
D-Bus API `LifecycleTiming1.ActivateUpdateTimings(seconds)` on `/LifecycleTiming`.
The user need not run a command or restart the daemon. There is no
`--trace-updates` flag. Leases last at most 300 seconds; zero stops new leases,
and existing workers retain their original deadline.

Only new window-update jobs receive a private lease. JSONL spans distinguish
confirmation, planning, recovery-metadata publication, discovery, staging,
replacement, verification and recovery. Neither native nor compatibility updates
copy or hash executables. Older captures can contain hash/copy stages from the
removed recovery-image cache. Monotonic wall and process CPU times
are separate. Nested spans are inclusive, not additive; a missing end means
incomplete evidence, not success. IPC duration does not claim remote CPU attribution.

Files live under
`$XDG_STATE_HOME/applicationlauncher/lifecycle/timings/OPERATION.jsonl`, readable
without the UI or daemon executor. They are not automatically included in normal
capture bundles; retain the matching file alongside a bundle.

Required private restart plans, status and results remain in every build profile.
Running application images are held by open descriptors during handover, even if
unlinked; file/process/session identities and structured restart scope are checked
without content hashing. These records are not persistent executable backups:
once all descriptor holders and processes exit, an unlinked image cannot be
recovered from the plan alone. Existing recovery-image caches are left untouched.

Window updates stop the foreground application, not its dedicated return-shell
ancestry. The coordinator checks those shells for identity changes or new jobs
before handing over to the verified replacement. Native interrupted recovery
keeps the original tab held open through delayed child exit and restores input
when the pending update ends. This last safeguard requires a terminal containing
the recovery-hold fix; it cannot retrofit older running native terminal code.
Freshness compares retained restarted images with a verified published
installation rather than treating their old build path as the current version.

### Show Info Presence Checks

Process fields remain captured metadata, not periodically refetched discovery.
The `window-process-presence` worker reads only captured `/proc/PID/stat` entries
to remove proven exits, zombies or reused PIDs. At most one check per info window
is pending, with a one-second interval, 512-process/8-KiB-stat limits and a
500-ms collection budget. No process-directory scan, executable hashing, version
execution or package query occurs in this path. Unknown reads retain the entry;
a lost worker retains metadata and retries after the interval, while a late reply
cannot prune a newer capture. Surviving process IDs, cached metadata and dropdown
state are not reset. Reopen the panel to capture new processes.

Recognised image-loader branches are hidden, not terminated. The per-window
checkbox exposes their captured live entries and count; generic sandboxes and
important process/ancestor paths are never hidden by name alone. The notice
states the capture/removal policy. Observable builds use the existing bounded
worker registry; minimal builds erase that registry and worker-state strings,
not the required presence behavior. Regression coverage lives in
`src/windows/process_tree/{helper_tests,presence}.rs`,
`src/app/popups/refresh_tests.rs` and `src/popups.rs`.

## Capture

The agent can collect ordinary observable evidence directly:

```bash
applicationlauncher --diagnose auto
```

The collector discovers private same-user PID sockets independently of GUI
repaints and the daemon D-Bus executor. It collects build/process identity,
best-effort semantic state, bounded `/proc` data, repeated stacks, relevant
journals and checksums. Exact-peer-PID debugger authorization expires after 60
seconds and is explicitly revoked. `--perf` and `--core` request additional
sensitive evidence explicitly.

Minimal has no in-process diagnostic endpoint. Its external helper needs no
application flag:

```bash
uv run --offline --no-project --python /usr/bin/python python -I -B scripts/capture-release-diagnostics.py --pid PID --output PRIVATE_DIRECTORY
```

Add `--stacks` for debugger attachment, or use `--executable ARCHIVED_ELF --core
CORE` for post-exit stacks. Failed/unavailable sources are reported without
discarding other evidence. PID/start/boot identity is checked before and after.
No environments or open-file contents are collected. GDB disables auto-loading,
debuginfod, locals and frame arguments; it never executes target methods.

Same-UID sibling ptrace can be refused by Yama, especially for minimal without
debugger authorization. Passive evidence remains available; a parent-debugger
fixture does not prove arbitrary live attachment permission. Cores can contain
private memory. OOM/SIGKILL may leave no core or semantic tail.

Build archives default to
`$XDG_STATE_HOME/applicationlauncher/builds/by-build-id/ELF_BUILD_ID/`.
Three builds/component plus running and published-profile IDs and their source
archives are retained.
Automatic capture retains at most eight directories; completed captures older
than seven days expire on collection. Unfinished evidence is preserved and can
refuse new collection when the budget fills. Explicit external-helper output
is caller-managed. Required recovery plans/progress/results are never pruned.

## Budgets

| Facility | Bound |
| --- | --- |
| Events | 512 events, 1000 admitted/second/process; contention drops rather than blocks |
| Persistent evidence | 512 checksummed 4 KiB slots/component; nonblocking queue 64; 64 MiB logical writes/recorder lifetime |
| Fields / boundary payload | 96 UTF-8 bytes / 2 KiB |
| Workers / semantic snapshot | 64 workers / 1 MiB; unavailable locks explicitly reported |
| Ordinary capture | 32 MiB, 45 seconds; explicit full cores are separate sensitive requests |
| External helper | 32 MiB, 45 seconds plus command-termination grace; 1 MiB/command, 128 threads |
| Update tracing | 300 seconds, 256 rows, 1 KiB/row, 128 KiB/file, depth eight |
| Timing retention | 64 files / 8 MiB; no-wait allocation, active-file locks, 128-entry scan |
| Runtime / symbols | 64 MiB stripped / 512 MiB exact or separate-symbol file |

Loss/overwrite counters and persistent sink health/first errno expose recorder
degradation. Sink failure cannot change restart policy or replace the original
product exception. The ring is optional evidence, not a durable audit trail.
No events belong in fuzzy-rank candidate loops, tile/row layout, spinner frames
or audio samples. Frame/wheel detail is capture-activated only.

Detailed update timing is admitted to distinguish delays combined by existing
recovery journals, not as an always-on profiler. Tests enforce disabled-boundary
p99 under 2 microseconds, activated-boundary p99 under 1 millisecond,
initialization p99 under 5 milliseconds and incremental Python allocation under
1 MiB. Only fixed scalar labels are captured, not titles, prompts, argv, user
paths or exception payloads. Demote it when equivalent cheaper evidence exists,
privacy needs change or measured budgets fail.

## Lifecycle Restoration Results

Tmux recovery evidence is required product state, not optional instrumentation.
Private application-update directories retain `restart-plan.json`,
`restart-events.jsonl`, `restore-status.json`, per-pane exit receipts and
`result.json` in both observable and minimal artifacts. Progress/results are
atomically replaced and bounded by the existing 2 MiB metadata limit and
128-pane scope limit. Recovery directories remain until manually removed.

The status record separates verified server replacement and local client
reattachment from each application's pending/restored/failed outcome. The
journal identifies the session, rebuilt pane and final verification reason;
exit receipts retain launcher status without recording terminal output.
Shared verification deadlines avoid serial waits for each Pi. A missing result
from an exited worker is still a failure, while a verified running server with
individual application failures completes with explicit warnings. Working
sessions are not automatically restarted as recovery.

These files include application names and working-directory/session metadata
already required by the private recovery plan. They do not add conversation
text, raw process environments, credentials or captured terminal payloads.

## Assurance And Limits

Run ordinary Rust/Python tests, diagnostic-build integration tests, negative
minimal compilation and the release matrix. Exact ELF proof and acceptance
receipts live in `target/release-profiles/`; failures retain logs/trial records.

`tests/verify_release_diagnosis.py` uses a private bus without activation
directories, private HOME/XDG state, stubbed desktop commands and no inherited
display or credentials. It exercises typed malformed-request errors, healthy
idle/startup controls, passive capture and independent snapshots. GDB stops a
private fixture before product initialization and recovers its source frame
after exit using archived stripped ELF, split symbols, source and native modules.
This proves retrieval, **not** real crash causal localization.
`tests/production_diagnostics.rs` separately tests contained diagnostic-only
panic/native crash and capture while the main thread is blocked.

Residual coverage is explicit: historical whole-GUI non-inferiority, real GPU
fault capture, heap-leak localization and arbitrary minimal sibling-debugger
permission are not certified. Optimized-code identity covers named search,
projection and build-comparison functions, not every GUI instruction.
Private-daemon/startup and Python-boundary measurements do not establish every
whole-GUI CPU/RSS/p99 budget. Link/load-time native modules are archived; later
GPU-driver loads and device memory require incident-specific evidence. No
deterministic OS/GPU/thread replay or complete named-lock ownership is claimed.
