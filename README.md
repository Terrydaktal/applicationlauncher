# applicationlauncher

`applicationlauncher` is a Rust GUI launcher and persistent window-session tracker for KDE Plasma on Wayland. The GUI combines searchable open windows and installed applications. The companion `applicationlauncherd` process records window activation and closure history, maintains crash-recovery, five-minute, hourly and named snapshots, and can restore missing windows without closing unrelated work.

## Shared program metadata

Keep `../program-metadata` alongside this checkout, just like `../fuzzy-rank`.
Progs and Application Launcher link that headless Rust library directly; the
Progs GUI does not need to run. It owns inventory/provenance classifications,
exact installation and launch-file resolution, pinned ELF versions/build IDs,
installed Node/Python/JAR/Electron metadata, package availability, Pi startup
receipt validation and recorded-input build checks. Launcher modules under
`src/windows/versions/` retain process-identity guards, background scheduling
and presentation adapters, not independent discovery algorithms.

Installed versions are not running-loaded versions. Show Info captures a process
snapshot when opened and reuses metadata while its observed executable path and
identity stay unchanged; reopen it to inspect later updates. An inaccessible
running executable retains its actual permission error. An exact absolute raw
argv[0] can supply a separately labelled, passively validated on-disk candidate,
never a running version or build ID. Only these inferred candidates receive a
bounded worker refresh (at most once per ten seconds); rendering does not read
their files. Unsupported or
ambiguous evidence includes a reason rather than a guessed version. Source/user programs are never executed
just because their name matches an allowlist. Native version execution requires
the shared backend's fresh trusted-system checks or an explicit exact-path local
grant in `$XDG_CONFIG_HOME/program-metadata/version-probes.toml` (default
`~/.config/program-metadata/version-probes.toml`). Granted local probes are a
targeted fallback after passive evidence, not inventory scans or inferred argv0
queries. Fish uses `--no-config --version` against separately pinned running and
installed images, including authorized retained/deleted images. No open shell
is restarted. Removing a grant applies to the next worker query; reopen Show Info
to refresh its existing snapshot. See
[`program-metadata/README.md`](../program-metadata/README.md) for supported
evidence, safety limits and the separate shared-crate test commands.

The GUI and daemon publish their own version and GUI/service role in immutable
ELF product records, read passively from separately pinned running/on-disk images.
Older binaries without that record remain explanatory Unknown when no other
verified version evidence exists. An unresolved deployment wrapper cannot hide a
known native image or prove that it is the current published installation.

For package-installed native processes, build colouring can be verified from
fresh exact package/file evidence without a PATH alias or desktop launcher.
Each subprocess still compares its own pinned running and disk images; it does
not inherit the parent's build status. The window owner's desktop entry may be
passed as an unverified candidate, but only a worker-verified current desktop
record and static launch chain matching that child's exact image can associate
its name/comment/icon. Runtime binaries alone never identify interpreted app
payloads. Window titles, geometry and activation history remain owner-only.
Verified package-managed native installations do not search for local build
metadata. Not-applicable Uncompiled changes results display and copy as `-`;
detailed reasons remain in the backend, while unknown results stay distinct.

Renamed native `uv-bin` installations use the shared passive uv version decoder;
neither uv nor uvx is launched to read this metadata. Recognising the product does
not turn an unmanaged uv executable into a uv-managed Python runtime or package.
Python source helpers can display their own docstring description, proven CLI
entry-point role and local development project directory even without Git. The
interpreted application calls this a **Source directory**. A nearby pyproject's
version is not a helper version without an exact supported entry-point binding,
and no running version or source-since-start claim is invented from current files.

Codex's code-mode host uses the shared passive native constructor decoder, so a
retained deleted image keeps its own compiled helper version rather than the
parent CLI's release or today's Cargo manifest version. Exact native Cargo target
metadata can associate a separately named helper with an indexed source checkout;
the main command's description and GUI capabilities are not copied to it.

SSH's native version comes from its exact portable OpenSSH release evidence or
the trusted fixed client `-V` recipe, independently of the owning package release
and OpenSSL library version. Executable capabilities use exact command/service/
desktop targets rather than the entire package's roles. SSH can retain SERVICE
when a real local unit launches it, without inheriting ssh-keygen's JOB role or
claiming that the inspected tunnel process belongs to that unit.

Shared discovery is not limited to per-application version recipes. Supported
same-product ELF version accessors and loader-bound product libraries can supply
compiled release evidence without launching the application. Mutable-pointer
defaults are visibly labelled `(compiled release initializer)`, not claimed as
observed runtime values. Current XDG launch routes also correct GUI roles for
source builds whose desktop files remain owned by a distro package. Upstream
package comparisons use that exact association without changing FRK/CLO ownership
or authorizing inferred AUR requests. Large Ninja graphs are inspected in bounded
batches under child-only filesystem-write denial, rather than failing at 512
targets. A deleted, inaccessible loaded library remains an explicit unknown;
the replacement's version is shown only as the separate on-disk version.

Desktop metadata can also belong to a retained running image rather than the
newer published build. Show Info validates that exact pinned-image association
and reads the current desktop fields without changing the independently resolved
installation or claiming the old image is current. Missing installed icons stay
missing; unrelated source-tree images are not guessed as application icons.

The shared Qt 6/KDE reader follows supported compiled QStringLiteral arguments to
loader-validated Qt metadata calls. It recovers complete fork suffixes without
launching GUI applications or reading today's source version into an older image.
These values retain the `(compiled Qt application version)` qualifier; unsupported
code layouts or ambiguous identities remain explanatory unavailable results.

Shared native version support includes the audited GNU coreutils `--version`
recipes, so tools such as `cat` report their exact executable product banner
separately from the owning package release. Passive inventories still never
execute these probes. Ninja freshness distinguishes a revalidated observation of
no newer recorded inputs from an unverified full build result. The confirmed
observation and checked-input counts are green. The main row uses a compact
summary such as `Yes · 25 inputs checked · 3 newer · 2 missing · added: unknown`;
newer and missing recorded inputs are counted separately, without verbose
backend reasons or example paths. Newly added, unrecorded inputs remain unknown.
Only uncertainty suffixes (`added: unknown`, `full build status unknown` or
`scan incomplete`) are yellow; known No/Yes observations and their counts stay
green/red. This includes an always-run generator that can change a version
header without a source edit. Incomplete scans cannot become successful no-newer
observations; invalidated inspection stays Unknown without false zero counts.
No project build code runs.

Pacman and positively identified AUR lookups use one shared backend. Pacman reads
its existing sync snapshot; AUR may request the current version of one package.
Concurrent identical requests share unfinished work, with no retained AUR result
cache or new disk cache. Reopening Info can therefore perform a new online check;
the loading text describes a version lookup, not a database refresh.

The launcher wrapper and build fingerprint include the shared library's inputs,
so backend changes are picked up on rebuild. No new daemon is introduced.

Self-updated executables may remain running after their old image is renamed or
deleted. The shared resolver can recover the current installation from the
identity-checked process launch name and the same publishing directory, without
depending on an updater's backup filename format. Such a launch chain is labelled
inferred: its installed version/build ID never replaces the pinned running image's
evidence. Ambiguous or uncorroborated matches remain unresolved.

Python runtime metadata is separate from the script's metadata. Validated uv
interpreters are identified as `UVR`; their prebuilt-runtime compilation status
is not applicable. A validated `.venv` route is retained in the current launch
chain without claiming it records the historical launch. Local scripts can show
their known source checkout and DEV/FRK/CLO provenance without borrowing another
tool's version or capabilities. Source changes since start remain unknown unless
the application published a startup source snapshot. A missing GNU build ID is
reported as not embedded, independently of whether the version was found.

CopyQ's local-build version is read from its pinned Qt ELF constant without
starting another CopyQ instance. Mousepad's version is read from its private
library: running inspection binds the actual mapped library, while installed
inspection follows supported loader metadata. The main executable's build ID
remains separate, and a changed or unproven library cannot be hidden by equality
of the main build IDs. Component-aware worker queries bypass the main-only inner
version cache; opening a new Info snapshot inspects current installation state.

## Project Structure

```text
.
├── src/
│   ├── bin/applicationlauncherd.rs
│   ├── tracker/
│   │   └── terminal_upgrade.rs
│   ├── windows/
│   ├── app/
│   │   ├── commands.rs
│   │   ├── feed.rs
│   │   ├── helpers.rs
│   │   ├── popups.rs
│   │   ├── settings.rs
│   │   ├── terminal_maintenance.rs
│   │   ├── view.rs
│   │   └── mod.rs
│   ├── launch/
│   ├── diagnostics.rs
│   ├── diagnostic_capture.rs
│   ├── observability.rs
│   ├── replay.rs
│   ├── audio.rs
│   ├── audio/
│   │   ├── analysis.rs
│   │   ├── firefox.rs
│   │   ├── monitor.rs
│   │   └── monitor_integration_tests.rs
│   ├── models.rs
│   ├── ranking_model.rs
│   ├── search.rs
│   └── main.rs
├── kwin/applicationlauncher-window-feed/
├── firefox/audio-bridge.js
├── scripts/
│   ├── applicationlauncher
│   ├── install-firefox-audio-bridge.py
│   └── build-debuggable-release
├── docs/DEBUGGABILITY.md
├── Cargo.toml
├── Cargo.lock
└── README.md
```

- `src/main.rs`: GUI CLI parsing, single-instance startup, and native window creation.
- `src/app/`: Launcher state, feeds, commands, search-row rendering, settings, popups, and the `eframe` update loop.
- `src/app/terminal_maintenance.rs`: Separate terminal-restart confirmation/progress popup. Uses background D-Bus requests and never closes or launches terminals itself.
- `src/tracker/terminal_upgrade.rs`: Optional bulk-request adapter for terminal-owned updates. Maps tracked windows conservatively to terminal UUIDs, requests updates through their D-Bus APIs and observes results; never launches replacements or closes terminals.
- `src/launch/`: Desktop-entry parsing and application/window launch actions.
- `src/audio.rs`: Stream-to-app/window attribution, volume controls and measured icon-ring rendering.
- `src/audio/monitor.rs`: Named background worker using native PulseAudio per-playback-stream monitors. Publishes a single replaceable latest-result slot; never records a microphone or the mixed desktop output.
- `src/audio/analysis.rs`: Fixed-memory stereo PCM analysis producing eight frequency-band levels and a peak level, with no saved audio.
- `src/audio/firefox.rs`: Bounded off-GUI reader and exact window attribution for the optional Firefox bridge; rejects stale, incomplete, insecure or ambiguous snapshots.
- `firefox/audio-bridge.js`: Privileged AutoConfig observer for Firefox tab playback, mute, title and window lifecycle events. Publishes only current window titles and audible tab titles to a private runtime snapshot, not URLs or page content.
- `scripts/install-firefox-audio-bridge.py`: Installs that bridge into a Firefox installation, preserving existing AutoConfig with an idempotent marked loader and one backup. Takes the repository JavaScript as input; writes the loader, bridge and preference file under `--firefox-dir` (default `/usr/lib/firefox`). Does not edit profiles or restart Firefox.
- `src/audio/monitor_integration_tests.rs`: Opt-in isolated null-sink tests for stream isolation, silence, pause/resume, mute, device moves and server loss, plus an explicit read-only live compatibility probe.
- `src/diagnostics.rs`: GUI single-instance control and foreground activation.
- `src/observability.rs`: Shared bounded events, counters, workers, panic handling, and independent diagnostic endpoints.
- `src/diagnostic_capture.rs`: Activated GUI/daemon evidence collector, checksums, privacy filtering, and debug doctor.
- `src/replay.rs`: Versioned boundary-event schema, persistent ring decoding, deterministic replay, and opt-in fault plans.
- `src/search.rs`: Fuzzy ranking, transient-title normalization, sorting, and highlighting.
- `src/ranking_model.rs`: Version-checked loading of an optional caller-owned field reranker.
- `src/models.rs`: Shared window, application, feed, and audio data types.
- `src/tracker/`: Daemon client, private SQLite persistence, restore policy, service installation, and D-Bus service.
- `src/tracker/tmux.rs`: Bounded tmux client and active-pane discovery, exact-session reattachment, and named-session recreation. It saves the pane directory and a restricted program kind, not pane contents or arbitrary commands.
- `src/bin/applicationlauncherd.rs`: Persistent background tracker entry point.
- `src/windows/`: KWin snapshot consumption, process metadata, terminal integration, and icon resolution.
- `src/windows/process_tree.rs`: On-demand background process-tree snapshots for Show Info, with real ancestry, application descendants, sibling helpers and detached tmux pane branches.
- `src/windows/terminal/tmux.rs`: Background per-client active-pane monitoring and latest-state window metadata overlays, independent of tmux's desktop-title forwarding setting.
- `src/windows/versions.rs`: On-demand, bounded running/on-disk executable version and build-ID probes for Show Info. `versions/freshness.rs` checks local build status without compiling.
- `src/windows/versions/application.rs`: Read-only application-tile inspection. Separates desktop launch wrappers from installed application binaries, with bounded launch-chain resolution and no script execution.
- `src/windows/versions/lookup.rs`: Independent binary-version and package-version workers per information window for expanded process sections, with bounded/deduplicated requests, PID/executable identity guards and cancellation of stale popup work.
- `src/windows/versions/node.rs`: Read-only installed Node application/package discovery, including symlinked CLI commands whose process title hides the script path; application metadata stays separate from Node runtime versions and ELF identities.
- `src/windows/versions/script.rs`: Bounded, read-only Python and Java launch-target identification from process arguments. Resolves explicit script/JAR paths and labels modules/classes without importing or executing application code or guessing their files.
- `src/windows/versions/pi.rs`: Bounded reader for Coherence Pi's installed build manifest and per-process startup receipt. Validates PID/start time, ownership, file safety, freshness and SDK/extension/patch identities without running Node or reading conversation contents.
- `src/windows/versions/installation.rs`: Bounded, cached, read-only lookup of Progs' saved installation classifications for Show Info, including local fork launch wrappers.
- `src/windows/versions/available.rs`: Read-only Pacman sync-database and Paru/AUR package-version queries, exact package matching, bounded caching and timeouts.
- `src/popups/foreground.rs`: On-demand Wayland info-window reactivation, matching the launcher PID and a unique native viewport identity before unminimizing, activating and raising it.
- `kwin/applicationlauncher-window-feed/`: Transactional KWin script that sends compositor window events to the daemon.
- `scripts/applicationlauncher`: Fast launch wrapper that activates compatible release binaries, repairs incompatible binary pairs in the background, and reports newer source without compiling it.
- `scripts/lib/release-compatibility.sh`: Cached binary-build checks and bounded paired rebuilds for the wrapper.
- `scripts/build-debuggable-release`: Produces build-ID-indexed exact binaries, split symbols, hashes, and `build-info.json`.
- `docs/DEBUGGABILITY.md`: Runtime modes, budgets, capture guarantees, privacy rules, and progress invariants.
- `Cargo.toml`: Package metadata and Rust dependencies.
- `Cargo.lock`: Locked dependency graph for reproducible builds.
- `README.md`: Project documentation for the current GUI application.

## What It Does

- Shows open windows in the main panel and installed applications in a conjoined side panel.
- Filters windows and applications from the same search field.
- Activates existing windows or launches new applications without closing the launcher.
- Supports icon-grid mode for the application panel, including configurable icon size, tile size, label visibility, and label font size.
- Keeps normal applications ahead of system settings modules on the default page when system modules are shown.
- Provides context actions on windows, including closing the window and opening Show Info with metadata and a process tree.
- Opens independent Show Info windows for different window entries and application tiles, keeping each window's selection, geometry, dropdowns and background lookups separate. Selecting Show Info again for the same target brings its existing information window to the foreground. Closing one leaves the others intact; at most 32 information windows can be open at once.
  On Wayland, reselecting an open target uses KWin activation because the native backend's ordinary Focus command is a no-op. Each info viewport has a stable, process-specific application ID; lookup requires both the exact ID and PID and rejects missing, ambiguous or malformed matches. A single background worker keeps only the latest pending target, with two-second command timeouts and no lock held during compositor I/O. It never recreates the info window, moves or resizes it, or focuses a different application when lookup fails. No focus worker runs on redraws, scrolling or metadata updates, and the worker exits when idle.
- Re-focuses the existing launcher instance instead of opening a second one.
- Persists window size, pinned applications, and launcher settings under `$HOME/.config/applicationlauncher/`.
- Drag pinned applications onto one another to reorder them in either application grid or list. The drop target is highlighted, and holding a drag near a panel edge scrolls that panel. Order is saved only after a valid drop; Escape or dropping elsewhere leaves it unchanged. Application scrollbars have a reserved gutter so expanded handles never cover icons or change the number of columns.
- Renders strikethrough in open-window and Recently closed titles as native styling, without changing saved titles or restore metadata.

## Runtime Architecture

The project builds a native `eframe` / `egui` GUI and a separate user-session daemon.

- Persistent tracking:
  `applicationlauncherd` owns the window-feed D-Bus service, records current and closed windows in SQLite WAL mode, and survives GUI closure. The GUI fetches a snapshot only when a generation counter changes.
- Recovery:
  State is debounced to disk. The latest automatic checkpoint is offered on every new boot, including after a clean reboot, and after an unclean daemon restart. A confirmed same-boot mass disappearance of tracked windows also creates a protected recovery candidate. The candidate is kept until the user restores or dismisses it, so a launcher or daemon crash while the prompt is open cannot replace it with an empty or partial state.
- Tiered session history:
  The daemon separately archives the current session every five minutes and keeps the newest 120 captures (about 10 hours). Once that tier is full, the first outgoing oldest snapshot is promoted to hourly history, then one in every 12 outgoing snapshots, keeping at most 120 hourly entries. The tiers do not overlap: promotion preserves the original snapshot ID, timestamp, window geometry and restore commands rather than copying or recapturing it. Together they hold about 10 hours of detailed history plus five older days during continuous operation. The first archive waits for a complete, nonempty window feed; snapshots are deferred during active restores. Capture timing and promotion progress persist across daemon restarts, missed intervals are not backfilled, and unchanged sessions are still archived. Retention is by count, not wall-clock expiry, so downtime does not expire older entries. A pending recovery decision does not stop these separate archives or allow them to replace the protected recovery checkpoint. Insertion, promotion, pruning and schedule updates are one SQLite transaction. Only the two automatic history tiers are pruned; named snapshots and the recovery checkpoint are excluded. Use `F9` -> `Saved sessions` to select a timestamped five-minute or hourly snapshot and restore it.
- Restoration:
  Existing matching windows are reused and repositioned, only missing windows are launched, and unrelated windows are never closed. Terminal replay is restricted to shell/CWD, `codex resume --last`, `agy -c`, `htop`, `nvtop`, and local tmux reattachment or named-session recreation. Codex restoration and cloning preserve an explicitly observed `--dangerously-bypass-approvals-and-sandbox` (including `--yolo`) using an allowlist of replayable options; arbitrary original commands or prompt text are not replayed. Older snapshots without recorded options do not implicitly enable bypass mode.
- tmux restoration:
  Local tmux clients are saved with their server socket, session ID/name and creation time, including custom `-S`/`-L` servers. The active pane's directory, supported program kind and allowlisted Codex options are also saved. Live sessions are reattached by exact identity without detaching existing clients. If the session is gone, restoration uses `tmux new-session -A -s NAME -c DIRECTORY` on the saved socket: an existing session of that name is attached without replaying a command, while a new session starts `codex resume --last`, `agy -c`, `htop`, `nvtop`, or a shell according to the saved pane. A snapshot without pane details, or with an unsupported program, creates a shell and reports an adjusted/partial restore rather than guessing a command. Pane content, additional panes/windows, arbitrary commands, and exact Codex session IDs are not saved; `--last` may resume a different Codex session if several share a directory. Remote tmux sessions inside SSH are not reconstructed.
- Recent window reopening:
  The newest recently closed window can be reopened globally with `Ctrl+Shift+T`. The KWin shortcut is ignored while Chrome, Chromium, or Firefox is active, preserving browser tab-reopen behavior. A successful reopen removes that entry from the history list.

- Window loading:
  Uses the KWin event feed for incremental updates, with bounded reconciliation through `kdotool`, then resolves metadata such as title, class, PID, icon, executable path, and terminal child processes.
- Terminal titles:
  Fish's `fish ~/repos/xfce4-terminal` command-plus-directory captions are normalized to `fish - ~/repos/xfce4-terminal - Terminal` without repeating the command. Matching foreground-command captions such as `htop ~` use the same handling, while older directory-only titles remain supported. Spaces and separator characters inside directories stay intact; known local process/pane directories replace abbreviations, but SSH keeps its remote directory and explicit shell label. Tmux session prefixes, strikethrough and spinner markers are preserved, and raw window/pane titles are unchanged. Parsing adds no subprocess, polling or filesystem lookup.
- Live tmux windows:
  A read-only background monitor maps each local tmux client to its active pane. Window entries use that pane's foreground program, working directory and actual title, preserving braille spinners and attention markers even when `set-titles` is off. Codex entries match the native tmux desktop title: `tmux: diet - codex: ~/tasks/diet - Check ChatGPT link access - Terminal`, with activity immediately before the directory when present. Both legacy `task | project` titles and newer Codex title selections are normalized using the pane's real directory rather than its abbreviated project label; task separators become ` - `. The session label, directory and task remain searchable. The window PID/executable still identify the terminal emulator; Show Info separately lists the tmux client, server socket, session and pane. Queries are batched per server at at most ten updates/second, publish only the latest state, briefly tolerate unavailable servers, and do not rerank searches for spinner-only changes. Idle shells are checked for entry into tmux every two seconds; no remote SSH pane inference or terminal input injection is used.
- Window and process version information:
  Each expanded Show Info process section reports that executable's running version, latest installed local version, local uncompiled-changes status, and both ELF build IDs. The running ELF build ID is coloured using build IDs (or the same executable inode), never just version strings: uncommitted rebuilds can print identical versions while containing different code. This applies independently to the terminal emulator, Fish, Bash, tmux, the active program and other inspected processes. Known noninteractive version switches are probed in a background worker against separately pinned running and on-disk ELF binaries, so deleted old executables retain their real running version after replacement. Unknown executables and shell scripts are never launched for inspection. "Latest" means the local installed binary, not a network/update check or an unbuilt source version. For local checkouts, Progs' published installation links can identify a newer build even while an older immutable release directory still exists (for example, tmux). The executable name and checkout must match, and up to 64 distinct aliases must agree on the same build; ambiguous targets are not guessed. Links and the pinned disk file are checked again after probing, so installation changes during inspection invalidate the comparison. Uncompiled changes use the selected installed build's path with a read-only Ninja target dry-run or a bounded Cargo dep-info/source/configuration timestamp check; the Cargo result is qualified because it is not Cargo's build planner. Ninja's always-run checks, such as Meson revision-header generators, do not prove uncompiled changes: without evidence of changed/missing inputs or commands the result is Unknown, not a false Yes. Missing/unsupported build metadata also reports Unknown, never a guessed No. Source status is checked again after reopening Show Info and expanding the section; no application is compiled or restarted. A disk binary replaced during inspection is reported as Unknown. Installed Arch package versions remain separate from executable versions. A single worker services at most 16 queued requests and 512 per-popup results; duplicate expansions reuse results. Closing/reopening cancels queued work and discards stale in-flight results. Both process start time and running executable identity are checked to reject PID reuse and exec changes.
  Node-backed terminal applications are resolved separately from their runtime. A bounded, read-only lookup validates the script against a package.json bin entry and shows its installed package name/version, canonical entry point, and manifest. CLI programs such as Pi that overwrite their command line can be identified through their command symlink; this is explicitly labelled an inferred command lookup, and ambiguous matches are rejected. Installed package metadata is not claimed to be the exact version already loaded by an older process. Node's version, Arch package, ELF IDs and binary freshness are labelled as runtime information, never as Pi's version. No application JavaScript, npm commands, or application --version switches are executed to obtain package metadata.
- Show Info process trees:
  Field names use muted grey-based colours by scope: blue-grey for window metadata, green-grey for the running instance and warm grey for installation/on-disk metadata. This classification is explicit and independent of field wording or searchability. Recognized Progs classification badges use their source/role colours wherever they appear, including `[PDP]` in Package, multiple `[PEP]`/`[AEP]` roots, other fields and tree headers. Bare classification values such as `FRK` use the same palette. Only the badge text changes colour; surrounding values retain searchable highlights, red/green build identities or their ordinary colour. Unknown or malformed bracketed text is left unchanged. Styling preserves the exact text, continuous selection, raw strikethrough copying and divider positions, with no new metadata lookup or background polling.
  Within each process, application and runtime section, running-instance details come first. Native metadata then follows this fixed order: latest on-disk version and ELF build ID, Uncompiled changes, Type, Package, available Pacman/AUR and npm versions when applicable, Root package when applicable, Launch chain, Desktop file, Icon path, Name, Comment, Description and Capabilities. The running Executable path remains; resolved installation paths appear in Launch chain without a separate on-disk executable-path row. A missing Package is shown as `-` once loading finishes. Ambiguous always-run Ninja checks show `Unknown` without a parenthetical explanation. Window-only fields remain ahead of process identity, command line and running version/build ID. Interpreted applications retain their own version/build evidence and put Description and Capabilities last without borrowing their runtime's identity. Each native ELF build ID appears immediately below its corresponding running or on-disk version; optional registry results retain their position while loading.
  Settings -> Show Info -> Scroll Sensitivity controls wheel scrolling in all info windows, independently of either launcher panel. It applies immediately, is saved between launches, and supports 0.1x to 10x speed (default 2.5x). Scrollbar dragging and text selection are unchanged.
  Process header text (including PIDs, executable paths and deleted-binary markers) is read-only and selectable for copying. Use the arrow beside a header to expand or collapse its details; selecting text does not toggle the section.
  A recovered shell/Python script name appears separately after Process name. Descriptor-backed scripts such as `/proc/self/fd/3` retain their memfd script name without rewriting the raw command line or replacing the interpreter's process identity.
  Window Info is a top-down process tree with independent dropdowns, branch connectors, PIDs, executable paths and window-owner/active-process markers. The window-owner dropdown starts expanded and contains titles, geometry, minimization, activation history, desktop entry and window application versions. Other dropdowns contain only their own PID, executable, working directory, command, desktop entry and lazily loaded versions. Tmux session/pane fields appear only under the corresponding tmux client/server, not under Pi or a shell. Pi's installed application metadata stays separate from Node's runtime metadata. Collapsing details leaves the hierarchy visible. Each section keeps continuous cross-row text selection; **Copy shown info** copies the headers and currently supplied details across sections.
  The tree includes the owning process's descendants, so a Pi launcher shows Pi and its SSH tunnel as siblings rather than incorrectly presenting SSH as Pi's parent. Ancestors are included without unrelated application branches. Detached tmux panes retain their actual server ancestry; no false parent-child connection to the terminal client is drawn. Shared terminal/browser processes can cover more than one window. The snapshot is collected once per Show Info opening (or a change of target process) by a named background worker, not during startup, search, scrolling or each frame. Collection is capped at 32,768 scanned entries, 512 displayed processes, bounded ancestry/descendant depth, 16 KiB per command line and a 500 ms work budget; exited/reused processes and partial captures are reported rather than guessed. Threads are omitted. Reopen Show Info for a fresh snapshot; spinner/title changes do not refresh it or reset dropdowns/text selection. No process is started, signalled or modified to collect the tree; optional version probes use only the existing noninteractive allowlist.
- Show Info installation classification:
  Application tiles show the installed application's on-disk version, Progs installation classification, installed package, available package version, ELF build ID and supported local-build status. Launch chains retain resolved symlink/wrapper paths without describing a browser as its custom launch script. Resolution follows at most 24 file/link steps and reads at most 64 KiB per script. Only literal absolute final `exec` targets and the verified Chromium `CHROME_WRAPPER`/`HERE` template are followed; dynamic, conditional, missing, cyclic or unsupported targets are explicitly unresolved, never evaluated. Scripts are never run, and interpreted applications retain their script identity rather than inheriting their interpreter's version. Every captured launch file/link is rechecked after native metadata probing. An exact Pacman package owner can supply Progs classification and description when its inner executable is not indexed as a launch command; unrelated names and conflicting descriptions are not guessed. A named background worker publishes local metadata first, then optional bounded Pacman/AUR and npm queries, using at most three queued updates. Closing the popup cancels remaining work and rejects late results. Drawing uses captured file-existence state, not repeated filesystem checks, and no application-tile probes run during startup, searching or scrolling. The pane fits its natural content height at 1200 logical pixels wide, subject to screen limits, while retaining continuous text selection.
  Capabilities (including Progs' primary-role summary) and description are also read from the saved Progs inventory. They appear in application-tile info and in the relevant process-tree sections, with Node applications kept separate from their runtime. Missing or conflicting records show `Unavailable`, rather than borrowing another application's description or treating absent capabilities as a data-only package. These are informational fields, not additional search fields; lookups run only on background workers when Show Info is used.
  Installation rows display only Progs' current classification initials: Type uses bare codes such as `FRK`, `DEV`, `CLO`, npm's `NET` or Bash's `PDT`, with the existing Progs colours. npm, uv and Cargo tools retain their explicit/dependency/unknown installation roles rather than using obsolete generic source codes; rustup remains `RUT`. Older local aliases are normalized to current names (`FKM` to `FRK`, for example), while legacy generic toolchain codes become unknown-role tool codes rather than claiming an installation reason. Package retains the owning package's name/version followed by a bracketed package code such as `[PDP]`; expanded classification and management names are omitted. They read `$XDG_CACHE_HOME/progs/scan-v1.json` (or `~/.cache/progs/scan-v1.json`) in the existing background info worker, never run Progs or trigger an inventory scan. Exact executable/alias targets take priority; a local source wrapper can identify a compiled binary only when its name and Git checkout both match. Conflicting, absent, unsupported or oversized inventory data reports UNC instead of guessing from the process name. The parser is capped at 16 MiB, 8,192 programs, 32,768 binaries and 65,536 reverse dependency edges; parsed data is reused until the inventory file changes. These classifications describe the last saved Progs inventory, not proof of the running binary's version. Refresh Progs, then close and reopen Show Info for updated classifications. Actual Pacman package versions are retained in the installation row, independently of executable versions and ELF IDs. Node application entry points and their Node runtime are classified separately.
  Root package shows one original requested target followed by the number of all other installed packages that depend on the inspected package, directly or indirectly. The root is the first target, in command order, from the package's earliest recorded installation transaction whose dependency chain includes that package. Matching uses the current saved Progs dependency graph, not a captured historical graph, so the result is explicitly marked inferred. The displayed version and badge are the root's current Progs metadata; later changes to explicit/dependency flags do not choose a different root. Counts exclude the inspected package and the displayed root, include intermediate dependencies and packages beyond explicit roots, and deduplicate cycles and shared paths. Missing graph evidence produces an explicitly qualified lower bound; absent history, unsupported requests or no matching original target show Unknown rather than selecting an alphabetical/current root. `/var/log/pacman.log` is read without execution or subprocesses, bounded to 16 MiB, 64 KiB per line, 1,024 command arguments, 65,536 historical package/target records and a 200 ms parsing budget. Only completed transactions establish requests; later reinstalls do not replace the first recorded installation. Repository-qualified sync targets, ordinary installer options and version-checked local/AUR package archive targets are supported; skipped `--needed` targets, ambiguous quoting, stdin targets and unsupported options do not invent attribution. The entire available regular log is inspected, not just a recent tail; missing or rotated-away older history cannot be reconstructed. Parsed history is cached until file identity changes, and up to 128 root/count results are cached per inventory and history revision. Reads and traversal run only in the existing on-demand background metadata worker, never in startup, searching, scrolling or frame rendering; no watcher, polling loop, new database or package operation is added.
- Available package versions:
  `Latest package version (Pacman/AUR)` is separate from the running version and latest binary on disk. Installed executable ownership identifies the package first; Progs can identify an exact package or a local source checkout's name. Unmanaged scripts and npm/uv/Cargo command names are not assumed to identify OS packages. Node application entry points and the Node runtime are checked independently. A dedicated background worker reads `pacman -Si` in configured repository priority order, then tries `paru -Si --aur` when no exact repository package is found. It never refreshes databases, installs, builds or upgrades packages, prompts for privileges, or runs discovered applications. Pacman values reflect the local sync databases and use the compact format `repository/package version (Last Pacman sync: 2 hours ago)`, switching to whole days at 24 hours and showing `less than 1 hour ago` for recent syncs. The age uses the complete timestamp and timezone offset of the latest recorded synchronization request in `/var/log/pacman.log`; the record does not prove that every repository downloaded successfully. Only the last 1 MiB of the regular log file is read, off the GUI thread, and the parsed timestamp is cached until the file changes. Age is recalculated when a package result is requested, even when the package version is cached; no timer or extra polling is added. Missing, unreadable, invalid or future timestamps show `unknown`; database download/publication timestamps are not substituted. Paru may contact the AUR using only the package name, and its results do not inherit a Pacman sync age. Each tool has a three-second timeout and a 64 KiB accepted response bound. Up to 128 results are cached for five minutes (failures for 15 seconds), with default Pacman sync-directory changes invalidating cache keys. The per-popup package queue is capped at 16 requests and sleeps when empty. Ordinary binary metadata is published first and is not blocked by slow AUR queries. Closing or retargeting an info window clears queued work and rejects stale in-flight results; no lookup runs during startup, search, scrolling or unopened process sections.
- Audio indicators:
  Rings respond to actual playback samples, not volume-slider values, MPRIS status or a synthetic animation. Each PulseAudio sink input is monitored individually on its output's monitor source, with no default-source or mixed-output fallback. The worker analyses 16 kHz stereo into eight frequency bands (60 Hz to 6.5 kHz) and peaks in 20 ms blocks, publishing at most 25 updates/second. Silence, corking, mute and stale samples clear the indicator. Audio is consumed in memory and discarded, never saved or sent to the GUI; the GUI receives only bounded, quantised levels and displays the latest result rather than replaying an update backlog. Monitoring is capped at 32 streams with requested 100 ms buffers and 256 metadata records, and reconnects with bounded retry delays after server loss. No analysis, audio-server I/O or process spawning occurs in the drawing path.

  Playback-stream change notifications refresh metadata without recreating healthy monitors. PipeWire emits these notifications when a monitor is attached, so treating every change as a disconnection would create a feedback loop and overload the shared audio services. Removal/replacement still invalidates immediately; refreshed identity, device, mute and cork state determine whether a monitor actually needs replacing.

  Audio helpers such as Chrome's separate audio process are matched to their
  owning browser process using verified same-executable ancestry. This runs only
  during background metadata discovery, is capped at 16 parents, and stops before
  unrelated shells or terminals. Distinct browser instances can therefore show
  playback on the correct window even when their stream only says `Playback`.
  Multiple windows sharing the same browser process remain ambiguous without a
  browser-side bridge; the launcher does not guess from the current tab title.

  Window waveforms still require an unambiguous stream owner. A shared browser PID or application name alone does not identify which window contains a playing background tab. When window-specific metadata is unavailable or ambiguous, audio remains indicated on the application tile instead of marking every window as playing. Volume controls remain application/process-wide. Matching uses the full window list, not just the currently searched results, and is cached separately from sample updates.

  With the optional Firefox AutoConfig bridge installed, the launcher uses Firefox's own audible-tab ownership, including background tabs. A single audible browser window owns that process's measured sound; simultaneous audible windows require exact stream/tab title matches. Window binding requires an exact unique raw title and PID. Duplicate window/track titles or an out-of-date title cannot assign sound to the wrong window: they withhold the window waveform rather than guess. Firefox's delayed speaker-icon removal is explicitly excluded so pause/mute clears ownership promptly. Private runtime files expire after 12 seconds and include the process start identity, preventing PID reuse or browser crashes from retaining old ownership. A stopped reader also expires on the GUI side. No browser I/O is performed during rendering.

  Playback meters are labelled `node.virtual=true` on PipeWire. Plasma therefore excludes these output-only analysis streams from its microphone indicator, leaving real microphone clients such as Whisper visible. The application does not impersonate another mixer, change Plasma settings or hide microphone streams. A native PulseAudio server may not implement PipeWire's virtual-node classification. Connecting meters are retired only after their asynchronous creation completes, with a bounded timeout/context reset, so cancelled creations do not accumulate orphan server streams.

  Bar lengths use a slowly adapting shared loudness reference rather than a compressed absolute decibel scale. Measured increases in each frequency band's energy emphasise drum hits and note attacks, with a fast release and brighter strokes at high levels. Bars travel farther inside the existing outer ring footprint. A fixed noise floor and absolute sample-peak gate still suppress silence; steady sound does not generate artificial beats.

- Application loading:
  Scans desktop files, parses launcher metadata, resolves icon names and icon files, and classifies likely settings modules separately from normal applications.
- Search and sorting:
  Applies fuzzy matching and custom ordering rules for windows and applications.

  ### Sorting Precedence Rules

  #### 1. Applications Panel
  * **When the search box is empty:**
    1. **Type**: Regular apps come first (settings modules are pushed to the end).
    2. **Pin Status**: Pinned applications come before unpinned applications.
    3. **Sub-ordering**: Pinned apps are sorted by their user-defined pin order. Unpinned apps are sorted alphabetically (case-insensitive) by name.
  * **When a search query is typed:**
    1. **Fuzzy Match Score**: Best/closest match score (lowest edit distance) comes first.
    2. **Exact Prefix Match Boost**: Exact prefix matches are boosted to the top of the matching subset.
    3. **Pin Status**: Pinned apps come before unpinned apps.
    4. **Sub-ordering**: Pinned apps are sorted by their pinned order.
    5. **Type**: Regular apps come before settings modules.
    6. **Name**: Alphabetically (case-insensitive) by name.

  #### 2. Open Windows Panel
  * **When the search box is empty:**
    1. **Application window count**: Applications with fewer open windows appear first.
    2. **Application Key**: Terminal/application groups remain together and are ordered alphabetically.
    3. **Console application**: Within a terminal group, console applications with fewer windows come first, then their application key alphabetically.
    4. **Tmux priority**: Within each console application, tmux sessions appear before regular terminal windows.
    5. **Window Title**: Alphabetically after transient braille and attention markers are ignored.
    The optional recent-first mode orders by last activation instead of these grouping rules.
  * **When a search query is typed:**
    With recent-first ordering enabled, fuzzy matching still filters windows and provides highlights, but matching windows are shown by last activation, newest first. Unrecorded activation times come last; equal timestamps use activation sequence, normalized title and window ID as deterministic tie-breaks. Toggling ordering applies to the current search immediately. Activation-only changes reorder cached rows without rerunning fuzzy matching; spinner-only updates retain their existing fast path. The application panel keeps its normal search ranking. Otherwise, the normal search order is:
    1. **Fuzzy Match Score**: Best metadata match across title, app name, class, executable, desktop entry, and path-like context.
    2. **Application Key**: Alphabetically (case-insensitive) by application class/key.
    3. **Window Title**: Alphabetically (case-insensitive) by window title.
- UI:
  Draws a frameless launcher window, a separate settings popup window, and a separate execution-chain popup window.
- Single-instance behavior:
  Uses a Unix socket lock so a second launch request focuses the already-running instance.

### Pi Application Build Details

Application and window/process Show Info share the installed-application fields:
current launch chain, on-disk version/build ID, package versions,
build status, installation classification and description.
Desktop file, icon, display name and comment follow those fields.
Running-process identity remains separate even when its path/version matches
the installed copy. Installed application tiles never invent a PID or claim
that their on-disk version is a running version. Redundant launcher basename,
raw/cleaned command, resolved-path and existence rows are omitted; known missing
desktop files are marked inline.

Window/process launch lookup runs only in the existing bounded background
worker when a section is first expanded. Desktop-entry targets must match the
inspected installation; otherwise verified Progs installation symlinks or the
known on-disk executable are used. A current chain is not proof of the original
route used to launch a process. Identification-method details remain internal
and are not displayed as a Show Info field.
Hosted Application and Runtime metadata remain independent.
Pi receipt refreshes also update the launch metadata if its installed entry
point changes, without reprobing the runtime or package repositories.

Show Info compares forks and Git clones with their upstream Pacman/AUR package
version. Progs' explicit fork/clone classification controls these inferred package
lookups; original local projects do not get a package-version row or a redundant
update-source row. Package-managed applications retain their package-version
comparison. The on-disk version and uncompiled-changes fields remain separate,
and hosted applications are classified independently from their runtime.

Show Info also shows **Application latest package version (npm)** for identified
npm tools, including scoped packages. It queries the package's `latest` dist-tag
using the configured npm registry, independently of the installed version and
Node.js runtime. Native Codex builds identified by their `codex-cli` version
receive an explicitly labelled `@openai/codex` upstream reference; this does not
classify the local fork as npm-installed. Manifest bin entries and exact Progs
npm paths identify other packages, not command-name guesses. Read-only `npm view`
runs only in a Show Info background worker with scripts disabled, no project npm
configuration, no installs or upgrades, and a three-second deadline. Results are
cached for five minutes (failures for 15 seconds), with at most 128 cached
packages. Missing npm, offline registries and invalid responses are reported
without displaying registry credentials. No lookup runs while scrolling,
searching, or with Show Info closed; an in-flight lookup can finish after close
but its result is discarded.

Expand the Pi process, then **Application: Pi**, in **Show Info** to see its loaded application version,
code and extension fingerprints, installed patch verification, comparison age,
coverage and source record paths. These identify Pi's JavaScript and extensions;
the separate **Runtime: Node.js** dropdown identifies the native runtime and
its ELF build ID.

Show Info renders Unicode combining strikethrough marks as native styling in
titles, process headers and metadata. Copying selected text or using **Copy shown
info** preserves the original Unicode text, including the raw window title.
Pi's Unicode program marker is normalized to `pi` in launcher titles, carrying
any strikethrough onto both letters. Raw window and tmux pane titles are unchanged.

Application and Runtime are independently expandable metadata sections under one
PID, not extra process-tree nodes. Python scripts/modules and Java JARs, source
files, modules and main classes use the same layout. Uncertain application
identities and versions remain unknown rather than borrowing runtime metadata.
Collapsing Pi's Application section stops its periodic build-receipt refreshes.
Window Info defaults to 1200 points wide and fits its opening height to the
visible content plus 8 extra points, within the monitor bounds. Initial
process-tree/metadata loading gets one additional height fit; later updates and expanded sections scroll without
continually resizing the window. Manual resizing remains available.
Each Show Info section lists running-instance details first, including its version,
ELF build ID, entry point, fingerprints and startup receipt where available.
Installation paths, launch chains, installed/latest versions, fingerprints and
desktop-entry metadata follow without an internal divider. Labels omit redundant
Application/Runtime prefixes, and installation classification is labelled Type.
Matching values are retained so their distinct sources remain clear; installed
paths are explicitly labelled.
Window-owner fields begin with titles, application key, class, window ID, geometry
and activation history. Console processes omit those window fields. Both then use
process identity, the full command line, running version/build ID, installed
version/build ID, Uncompiled changes, Type, Package, available package versions,
Root package when applicable, Launch chain, Desktop file, Icon path, Name, Comment,
Description and Capabilities. The lossy command
summary remains available for search/title metadata but is not shown in Show Info.

Coherence's installer writes `coherence-build.json` beside the selected Pi
package's `package.json`. Pi's running-build extension writes
`$XDG_RUNTIME_DIR/pi/processes/<PID>-<startticks>.json`, preserving its startup
identity while updating the latest installed comparison. ApplicationLauncher
reads both records and colours the running code fingerprint and latest extensions
fingerprint green when verified current, red when a restart or installation update
is required, and neutral when unknown. Missing fingerprints remain neutral;
the comparison reason explains the result without a separate build-status row.
A package version or matching Node ELF alone never proves that the running Pi
has current patches.

Records must belong to the current user, match the process PID and Linux start
ticks, and reside in private runtime directories. Reads reject symlinks,
non-regular or oversized files, malformed identities, unsafe permissions and
comparisons more than 90 seconds old. Each JSON read is limited to 512 KiB.
Missing or inconsistent evidence remains **Unknown**, including older Pi
processes which have not loaded the receipt publisher yet.

Only an expanded Pi process section requests a comparison refresh, at most once
every two seconds. A background worker rereads the small records and selected
Node package metadata; it does not rehash the SDK, invoke a package manager,
reprobe Node, contact Pi or inspect chats. Closing or collapsing the section
stops these refresh requests. Pi itself is responsible for keeping the
comparison current and stating the fingerprint's finite coverage.

### Optional Firefox Audio Bridge

Install once, then start Firefox normally at your next convenient browser restart:

```sh
sudo python3 scripts/install-firefox-audio-bridge.py
```

This is privileged browser-side JavaScript, not a WebExtension or a remote-debugging server. Only install trusted bridge code. The installer enables unrestricted **AutoConfig**, not unrestricted web pages or a disabled content sandbox. A fresh installation uses `firefox-autoconfig.cfg`; an existing AutoConfig filename is respected. The config repository's shared entry point loads the dictionary and autoscroll scripts separately and loads this bridge only when installed. The installer preserves that modular audio entry without adding a duplicate loader, or adds its own marked loader to other existing AutoConfig files. Re-run after a Firefox package upgrade or configuration deployment removes the installed files. No existing browser windows are closed by installation.

The pipeline is: Firefox playback/tab events -> coalesced atomic JSON -> launcher background reader -> exact window ownership -> existing per-stream PCM waveform. The bridge writes `$XDG_RUNTIME_DIR/applicationlauncher-firefox-audio/firefox-PID.json` (directory `0700`, file `0600`), including titles of private windows. There is no browsing-history archive, URL collection, saved audio or network endpoint. The file is deleted on normal browser shutdown and ignored after process exit or expiry.

Focused tests:

```sh
node --test tests/firefox_audio_bridge.test.cjs
uv run --no-project python -m unittest discover -s tests -p test_firefox_bridge_install.py
cargo test --release --bin applicationlauncher audio::
```

`tests/firefox_bridge_integration.py` additionally runs real Firefox in a disposable installation/profile with a private null-sink PulseAudio server. Set `APPLICATIONLAUNCHER_TEST_PULSEAUDIO` to a PulseAudio executable and, if needed, `APPLICATIONLAUNCHER_TEST_PULSE_MODULES`/`LD_LIBRARY_PATH` to its private module/library directories; then run `dbus-run-session --config-file=tests/firefox-private-bus.conf -- uv run --no-project python tests/firefox_bridge_integration.py`. It never attaches to an existing browser or uses the desktop audio server. `tests/firefox_audio_test_driver.js` is fixture-only and is never installed by the real installer.

For explicit live verification while Firefox is playing, enable `APPLICATIONLAUNCHER_TEST_LIVE_AUDIO=1 APPLICATIONLAUNCHER_TEST_FIREFOX_BRIDGE=1` and run `cargo test --release --bin applicationlauncher live_playback_monitor_probe -- --ignored --nocapture`. This reads individual playback samples for eight seconds and the existing tracker/bridge snapshots, checks stable monitor identities and measured attribution, and then disconnects its own monitors. It does not start microphone recording, save samples or change playback. Unlike the isolated tests, this probe uses the real audio server and requires an already-running launcher daemon.

## Features

- Dual-panel layout with open windows and an application panel shown together.
- Keyboard navigation across both panels, including cross-panel selection that follows physical row alignment.
- Independent scrolling behavior for the two panels.
- Immediate icon tooltips in application icon mode.
- Pinning and reordering of applications.
- Middle-click on a window entry to launch another instance of the underlying application.
- Right-click on a window entry to open, clone, show metadata and the process tree, or close the application.
- Optional close-on-blur behavior.
- Settings includes **Stop idle Codex sessions...**, an explicit, confirmed action that sends one `SIGINT` to each verified local idle Codex process. It checks fresh terminal-tab metadata and window titles for spinners/attention, then rechecks process identity and foreground ownership before signalling through a PID handle. It requires a shell underneath Codex and never signals terminal windows, shells or whole process groups, never force-kills or retries, and does not run automatically. Unsupported/inactive tabs and ambiguous or changed sessions are skipped. This is a signal interrupt, not a literal TUI Ctrl+C keypress; Codex versions can handle the two differently. Title-based idle detection is conservative but cannot make the final title-read/signal boundary atomic with Codex starting a new turn.
- Temporary border overlay support for highlighting a target window.

## Requirements

- Linux
- KDE Plasma on Wayland
- `kdotool` available in `PATH`
- `libpulse` (including development headers/pkg-config metadata for building), and a PulseAudio-compatible server such as `pipewire-pulse` for playback visualisation. `pactl` is used only for user-requested volume/mute changes.

Install Rust dependencies and build with Cargo. `kdotool` is the main external runtime dependency used for window activation, raising, and closing.

## Build

```bash
cargo build --release
```

To retain exact production artifacts and separate symbols indexed by ELF build
ID, use:

```bash
scripts/build-debuggable-release
```

This post-link step has no runtime CPU cost. See
[`docs/DEBUGGABILITY.md`](docs/DEBUGGABILITY.md) for the stateful production
debuggability contract and explicit CPU, latency, RSS, and size budgets.

Focused audio verification:

```bash
cargo test --release --bin applicationlauncher audio::
APPLICATIONLAUNCHER_TEST_PULSEAUDIO=/usr/bin/pulseaudio cargo test --release --bin applicationlauncher isolated_server_stream_isolation -- --ignored --nocapture
cargo test --release --bin applicationlauncher measured_filter_bank_throughput -- --ignored --nocapture
```

The integration test starts only its own private PulseAudio server with null outputs and a simulated microphone; it never modifies the desktop audio server or plays sound through hardware. It needs a PulseAudio executable, not just `pipewire-pulse`. An extracted package can be used with `APPLICATIONLAUNCHER_TEST_PULSE_MODULES` and its library directories in `LD_LIBRARY_PATH`. The separately opt-in `live_playback_monitor_probe` test requires `APPLICATIONLAUNCHER_TEST_LIVE_AUDIO=1`; it attaches playback-only monitors for five seconds and prints aggregate counts, not audio or window metadata.

## Run

```bash
./scripts/applicationlauncher
```

The wrapper checks that the built launcher and daemon have the same source-build fingerprint, even when their version numbers are identical. It caches these checks by file identity, size and timestamps. A confirmed incompatible pair queues one background `cargo build --release --locked --bins`; Cargo rebuilds whichever targets are stale. The existing GUI can appear immediately, and running processes are replaced only after both rebuilt binaries are verified compatible. Only the launcher and daemon are restarted, never user application windows. A failed repair preserves the running processes, keeps a bounded build log at `$XDG_STATE_HOME/applicationlauncher/launcher-build.log`, and waits five minutes before automatically retrying the same failed pair. Replaced files allow an immediate retry. Diagnostic and control commands never trigger repair or restart.

For compatible binaries, the wrapper compares running processes with the already-built `target/release` executables and restarts both components only when those executable files have been replaced. Source, Cargo metadata, embedded KWin files, and the local `../fuzzy-rank` dependency are checked separately; newer source alone produces a warning, not an automatic build. Run `cargo build --release --bins` explicitly to build those changes. Focused, isolated deployment regressions can be run with `uv run --no-project python -m unittest discover -s tests -p test_launcher_deployment.py`; their fake services and binaries do not touch the desktop session.

Install the command as a symbolic link so the wrapper remains updated with the repository:

```bash
ln -sfn /home/lewis/Dev/applicationlauncher/scripts/applicationlauncher "$HOME/.local/bin/applicationlauncher"
```

The compiled binary can still be run directly when an automatic freshness check is not wanted:

```bash
./target/release/applicationlauncher
```

## Settings and Data Files

The launcher writes its runtime data to:

- `$HOME/.config/applicationlauncher/settings.txt`
  Stores launcher settings such as icon mode, system module visibility, icon sizes, tile size, text sizes, row sizing, and cursor behavior.
- `$HOME/.config/applicationlauncher/window_size.txt`
  Stores the current launcher window width and height.
- `$HOME/.config/applicationlauncher/pinned_apps.txt`
  Stores pinned application desktop file paths in display order.
- `$XDG_STATE_HOME/applicationlauncher/history.sqlite3`
  Private SQLite WAL database containing current windows, closed-window history, recovery state, up to 120 five-minute snapshots plus 120 older hourly snapshots, and named snapshots. Named snapshots remain until manually deleted; automatic snapshot retention does not prune closed-window history.
  Each new snapshot records the saving daemon's version and source-build identity.
  Saved sessions displays both, with the full build identity available on hover.
  Older snapshots without this metadata show `Daemon version not recorded`;
  upgrading or promoting a snapshot to the hourly tier does not rewrite its producer.
- `$HOME/.config/systemd/user/applicationlauncherd.service`
  Auto-installed tracker service with restart-on-failure behavior.
- `$HOME/.local/bin/applicationlauncherd`
  Symbolic link to the daemon binary beside the launcher binary.
- `$XDG_STATE_HOME/applicationlauncher/panic-gui-latest.log` and `panic-daemon-latest.log`
  Private Rust panic reports with release backtraces. Reports are mode `0600`.
- `$XDG_STATE_HOME/applicationlauncher/diagnostics/`
  Checksummed, bounded `--diagnose auto` bundles covering both GUI and daemon.
- `$XDG_STATE_HOME/applicationlauncher/flight-recorder-gui.ring` and
  `flight-recorder-daemon.ring`
  Fixed-size, checksummed boundary-event rings retaining recent events across
  ordinary process crashes without unbounded disk growth.
- `$XDG_STATE_HOME/applicationlauncher/builds/by-build-id/`
  Exact release binaries, stripped copies, separate symbols, and build metadata.
  The archive keeps the newest three builds per component plus any build ID
  still used by a running launcher or daemon.

The launcher wrapper warns when the running release has no matching archived
debug-symbol artifact. Run `scripts/build-debuggable-release` after a release
build to retain exact symbols for post-mortem diagnosis.

## Settings Window

The settings UI is shown in a separate popup window rather than embedded inside the launcher.

Current settings cover:

- Application panel:
  `Show System Modules`, `Icon Grid Mode`, `Icon Size`, `Tile Size`, `Show Names`, `Name Size`
- Open window view:
  Row height, icon size, padding, text spacing, line height, title size, path size, and whether the subtitle path is shown
- General:
  `Disable text select cursor (I-beam)`

## Terminal Maintenance

Open `F10` -> `Terminal Maintenance` -> `Update outdated terminal windows...`.
The popup is an optional client of XFCE Terminal's own update API. It queries
`GetUpdateStatus(window_uuid)` on each terminal's unique D-Bus owner and sends
`UpdateInPlace(window_uuid)` for selected outdated windows. The terminal owns
confirmation, executable pinning, application resume, private recovery commands,
startup gates and KWin placement; Application Launcher does not launch replacements
or close terminal windows. Non-tmux confirmation still appears in the terminal.

The popup observes the returned terminal-owned helper service and reports its
result. Closing or restarting Application Launcher does not cancel that helper.
Duplicate starts do not repeat requests. An ambiguous shared-process window or a
terminal without the new API is ineligible rather than using a daemon-owned
fallback. Old running terminals need one manual restart to acquire the new API.
The old `BeginTerminalUpdatePreview` endpoint and tracker-owned replacement worker
have been removed. Existing saved sessions, automatic snapshots, terminal session
restoration and old named backups remain supported and are not deleted.

XFCE Terminal's right-click **Update in Place** works without Application Launcher
or its tracking daemon. New recovery metadata belongs to the terminal under
`~/.local/state/xfce4-terminal/updates/`, not to the tracker snapshot database.
Window identity mapping uses the terminal owner PID and lifetime UUID; identical
captions in one shared process are refused rather than guessed. There is no
terminal-maintenance scanning during ordinary launcher use.

Isolated adapter and popup checks:

```bash
cargo test --lib terminal_upgrade
cargo test --bin applicationlauncher app::terminal_maintenance
```

## Keyboard and Mouse Behavior

- `Up` / `Down`
  Move through the active panel. In app icon mode, movement follows the rendered grid layout.
- `Left` / `Right`
  Move within the app icon grid or switch between the windows and application panels when crossing the first or last column edge.
- `Enter`
  Activates the selected window or launches the selected application.
- `Escape`
  Closes the launcher, or closes popup windows when they are focused.
- `F5`
  Refreshes the open windows or application data, depending on context.
- `F10`
  Opens the settings popup window.
- `F9`
  Opens the separate Window History and Sessions popup.
- `Ctrl+Shift+T`
  Globally reopens the newest recently closed window, except while Chrome, Chromium, or Firefox is active.
- Mouse:
  Hover highlighting is separate from keyboard selection. Window entries and app tiles support click and context actions across the full entry area.

## Command Line Interface

The binary currently exposes this CLI surface:

```text
NAME
    applicationlauncher - A sleek application launcher for KDE Wayland in Rust

SYNOPSIS
    applicationlauncher [OPTIONS]

DESCRIPTION
    applicationlauncher is a fast, visually stunning GUI application launcher
    designed for KDE Plasma Wayland. It queries the list of all open window
    objects using kdotool, allows searching them via a fuzzy-matching interface,
    and switches focus to the selected window.

OPTIONS
    -h, --help
        Print this help information and exit.

    --close-on-blur
        Close the launcher window automatically when it loses focus.

    --theme <THEME>
        Force a specific icon theme (default: automatically detected).

    --diagnose auto [--perf] [--core]
        Capture repeated stacks, /proc state, semantic state, journals, loaded
        modules, and checksums from the running GUI and daemon. Perf and full
        cores are explicit activated-only additions.

    replay <RING_OR_BUNDLE>
        Replay the persisted typed boundary events from a flight-recorder ring
        or diagnostic bundle.

    debug-doctor
        Verify symbolization, diagnostic attachment, tools, private output, and
        bounded recorder behavior.

OPERATION
    When launched, the application retrieves a list of all open windows using
    kdotool and installed desktop applications from the local system. It renders
    a frameless GUI window containing a search input, a main window list, and an
    application side panel. As you type, both lists are filtered using a fuzzy
    matcher.

    If `APPLICATIONLAUNCHER_FIELD_RANK_MODEL` is set, or if
    `$XDG_STATE_HOME/applicationlauncher/field-rank-model.json` exists, the
    launcher loads a version-checked `fuzzy-rank::fields::FieldRankModel` and
    reranks only the leading 256 metadata matches. Without a valid active model,
    the deterministic fuzzy-rank ordering is unchanged.

    Keyboard Navigation:
        - Up/Down Arrows: Move selected window.
        - Enter: Activate selected window.
        - Escape: Close launcher.
        - F5: Refresh list.
        - F10: Open launcher settings.

EXAMPLES
    applicationlauncher
        Launch the application launcher.

    applicationlauncher --diagnose auto
        Capture both running components without replacing or restarting them.

    applicationlauncher replay ~/.local/state/applicationlauncher/diagnostics/capture-...
        Replay typed boundary events from a captured incident and print the
        deterministic state summary as JSON.

FILES
    $HOME/.config/applicationlauncher/window_size.txt
        Stores the persisted width and height of the launcher window.

    $HOME/.config/applicationlauncher/pinned_apps.txt
        Stores absolute paths of pinned desktop applications.

    $HOME/.config/applicationlauncher/settings.txt
        Stores persisted launcher settings.

PATHS
    /usr/share/icons
        System icon themes.
    /usr/share/pixmaps
        Legacy system application icons.

SECURITY NOTES
    Wayland isolates windows from querying each other directly. This tool relies on
    kdotool, which utilizes internal KWin D-Bus scripting interfaces to securely
    interact with KWin.

    Fault injection is disabled unless both
    APPLICATIONLAUNCHER_ALLOW_FAULT_INJECTION=1 and
    APPLICATIONLAUNCHER_FAULT_INJECTION are set. It is intended only for
    contained tests and diagnosis.

EXIT STATUS
    0   Success.
    1   Failure (e.g., kdotool not found or D-Bus communication failed).

AUTHORS
    Terrydaktal <9lewis9@gmail.com>
```

## Session Restore Limits

Browser windows are restored as application windows in the first release; exact tabs and URLs require browser-native session restore or a future browser extension. File-manager paths and terminal working directories are restored when reliable metadata is available. Failed or ambiguous items are reported rather than replaying unsafe commands.

Restored applications run in independent user systemd scopes, with environment
expansion disabled so literal arguments such as tmux session IDs (`$6`) arrive
unchanged. Surviving tmux sessions are reattached with their saved server/session
identity, without restarting their existing Codex program or detaching other
clients. Plasma activity membership and skip-switcher state qualify an otherwise
verified layout as `Adjusted` only when the actual state differs; merely recording
these fields does not prevent an `Exact` result.

The harmless argv regression uses only a short-lived `printf` scope, not a
terminal or session restore:

```bash
cargo test --release --lib independent_scope_preserves_literal_tmux_ids_and_shell_arguments -- --ignored
```

## Resource Optimizations

### Terminal Freshness API

The existing `com.terrydaktal.ApplicationLauncher` session-bus service exposes
`com.terrydaktal.ApplicationLauncher.Freshness1` at `/Freshness`:

- `GetProcessStatus(pid: u32, start_time: u64, image_dev: u64, image_ino: u64, launch_path: string) -> string` returns a schema-versioned JSON report with `current`, `outdated` or `unknown` status, the exact requested target, the observed process identity, ELF build IDs and a report generation.
- `ProcessStatusChanged(report: string)` publishes changed reports. Consumers must pin the service's unique owner, match the target and reject older generations; generations restart when the daemon restarts.

Requests only enqueue work or return cached reports. One background worker checks
leased targets every two seconds and sleeps when idle. Clients renew within 60
seconds (xfce4-terminal uses 20 seconds); at most 512 targets and bounded identity
caches are retained. Inspection is read-only, same-user and native ELF only. It
pins PID start time and executable device/inode, verifies launch aliases before
following retargets, rechecks process/file identity after reads, and never executes
candidate binaries, scripts or version commands. Zero identity fields allow the
caller to request discovery, as used by the terminal for its own process.

This is comparison with the current local launch target, not source-tree freshness,
package updates, interpreted code freshness or private-library component status.
Missing/unsupported/ambiguous evidence is `unknown`; consumers must not retain a
daemon-derived stale marker after service loss. GUI Show Info's richer metadata
remains separate but uses the same native-build equality policy. The terminal
continues to own title rendering, Update in Place and its independent replacement
validation; this service does not own or initiate those operations.

`cargo test --release --lib freshness::tests` checks the inspector and cache. The
uninstalled `cargo build --release --example freshness-service` example serves the
same implementation for xfce4-terminal's isolated cross-repository integration
tests; its stdin lifetime and `READY` output are test orchestration, not additions
to the production daemon's CLI.

The implemented [twelve-step optimization plan](docs/OPTIMIZATIONS.md) documents
cache invalidation, worker/queue bounds, compatibility fallbacks and safe release
verification commands. Its [regression inventory](tests/optimization-contracts.json)
links each step to the tests protecting its behavior.
