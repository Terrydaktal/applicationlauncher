# applicationlauncher

`applicationlauncher` is a Rust GUI launcher and persistent window-session tracker for KDE Plasma on Wayland. The GUI combines searchable open windows and installed applications. The companion `applicationlauncherd` process records window activation and closure history, maintains crash-recovery, five-minute, hourly and named snapshots, and can restore missing windows without closing unrelated work.

## Search

Word queries keep the shared fuzzy matcher's normal token, typo and field-ranking
rules. ASCII punctuation-only queries such as `~`, `/`, `::` and `++` use literal
substring matching in both panels, rather than being discarded as empty tokens.
Only real symbol occurrences match and highlight; the input is not a regular
expression, wildcard or shell/home expansion. Existing Unicode/visual-title
projection, pinned-app filtering and optional recent-window ordering still apply.

**Run in Terminal** queues the entered command through Fish's interactive reader
after startup, using one shell. The command appears at the normal prompt and uses
Fish's history, pre/post-command hooks, timing and exit-status display, then returns
to the same shell without another greeting. History follows the user's normal
Fish settings, so commands containing secrets may be saved. The command handoff
variable is cleared before execution; no simulated typing or startup delay is used.

## Window info requests

XFCE Terminal's **Show Info** menu uses
`applicationlauncher --show-terminal-info PID WINDOW_UUID` to open the existing
window-information panel. A new process uses its main window for the info panel,
not the launcher, and exits when it closes; an existing launcher is neither raised
nor hidden. This works on native Wayland without relying on unsupported window
hiding. The normal launcher shortcut can open the launcher while retaining the
information panel as a separate window. Request failures also use an information
window, never the launcher search window. Resolution happens in a background
worker against current terminal D-Bus records and KWin windows; it requires the
exact terminal PID and window UUID and refuses ambiguous or closed targets. An
older running launcher GUI must be restarted once to acquire this control command.

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
metadata. Uncompiled changes is omitted for Pacman/AUR-managed installations,
interpreted entries and confirmed not-applicable results. Detailed reasons remain
in the backend, while unknown local source-build results stay visible and distinct.
Build system is also omitted for package-managed installations and when no
configured build steps are known. Source checkouts retain their build-system
field, including Unknown/Loading states; independently detected non-package
build setups remain visible. Hosted scripts never borrow their runtime's build
system, and language/version fields are unchanged.

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
- `src/tracker/terminal_upgrade.rs`: Confirmed single/bulk terminal updates. Uses native cooperation when available and daemon-owned replacement for older windows without updater APIs.
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
- `src/popups/updates.rs` and `src/app/terminal_maintenance.rs`: Show Info's terminal-update scope chooser, direct single-window updates and reviewed bulk updates.
- `src/lifecycle/mod.rs`: Headless lifecycle D-Bus API, independently managed worker launch, durable reports and cross-process restart/restore locking. Runs inside `applicationlauncherd`, not a separate permanent daemon.
- `src/lifecycle/application_update.rs` and `worker.py`: Shared application adapters and guarded program/tmux restart plans. Codex conversation detection and restart policy have one implementation for individual-program and whole-terminal updates.
- `src/lifecycle/window.py` and `runner.py`: Durable terminal-update orchestration and supervised job entry. Coordinate native terminal mechanics, retain recovery builds, gracefully stop non-tmux applications and verify replacements before authorizing the original window to close. Python 3 and system PyGObject/Gio are required only for these operations.
- `src/lifecycle/external.py` and `placement.js`: Daemon-owned compatibility replacement, compositor capture/placement/close, optional read-only tab discovery, bounded `/proc` mapping and PID-bound application startup gates. No updater API is required in the original terminal.
- `src/bin/applicationlauncherd.rs`: Persistent background tracker entry point.
- `src/freshness.rs`: Passive, cached native-process build comparison served by the daemon. Consumers identify a process and render the status themselves; this service never rewrites titles, launches programs or requests updates.
- `src/windows/`: KWin snapshot consumption, process metadata, terminal integration, and icon resolution.
- `src/windows/process_tree.rs`: On-demand background process-tree snapshots for Show Info, with real ancestry, application descendants, sibling helpers and detached tmux pane branches.
- `src/windows/tmux_info.rs`: One-shot background `tmux-mosh` JSON collection with server/socket and PID/start-time guards, shared in-flight requests and bounded read-only subprocesses.
- `src/app/popups/tmux.rs`: Server/session/attached-client groups and merged selected-client fields in Show Info; `tmux/client.rs` normalizes socket aliases and omits frontend details already represented in the process tree.
- `src/windows/terminal/tmux.rs`: Background per-client active-pane monitoring and latest-state window metadata overlays, independent of tmux's desktop-title forwarding setting.
- `src/windows/versions.rs`: On-demand, bounded running/on-disk executable version and build-ID probes for Show Info. `versions/freshness.rs` checks local build status without compiling.
- `src/windows/versions/application.rs`: Read-only application-tile inspection. Separates desktop launch wrappers from installed application binaries, with bounded launch-chain resolution and no script execution.
- `src/windows/versions/lookup.rs`: Independent binary-version and package-version workers per information window for expanded process sections, with bounded/deduplicated requests, PID/executable identity guards and cancellation of stale popup work.
- `src/windows/versions/node.rs`: Read-only installed Node application/package discovery, including symlinked CLI commands whose process title hides the script path; application metadata stays separate from Node runtime versions and ELF identities.
- `src/windows/versions/script.rs`: Bounded, read-only Python and Java launch-target identification from process arguments. Resolves explicit script/JAR paths and labels modules/classes without importing or executing application code or guessing their files.
- `src/windows/versions/pi.rs`: Bounded reader for Coherence Pi's installed build manifest and per-process startup receipt. Validates PID/start time, ownership, file safety, freshness and SDK/extension/patch identities without running Node or reading conversation contents.
- `src/windows/versions/installation.rs`: Bounded, cached, read-only lookup of Progs' saved installation classifications for Show Info, including local fork launch wrappers.
- `src/windows/versions/available.rs`: Shared read-only Pacman sync-snapshot and exact AUR RPC version queries, with bounded in-flight request coalescing and no completed-response cache.
- `src/popups/foreground.rs`: On-demand Wayland launcher/info-window reactivation, matching the launcher PID and exact native viewport identity before unminimizing, activating and raising it through one shared latest-request-wins queue.
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
- Application context menus include **Show in Progs**, opening the program manager filtered by the exact desktop entry. A unique matching program is selected automatically; shared application/wrapper entries remain available to choose without guessing by name.
- Opens independent Show Info windows for different window entries and application tiles, keeping each window's selection, geometry, dropdowns and background lookups separate. Selecting Show Info again for the same target brings its existing information window to the foreground. Closing one leaves the others intact; at most 32 information windows can be open at once.
  On Wayland, reselecting an open target uses KWin activation because the native backend's ordinary Focus command is a no-op. Each info viewport has a stable, process-specific application ID; lookup requires both the exact ID and PID and rejects missing, ambiguous or malformed matches. A single background worker keeps only the latest pending target, with two-second command timeouts and no lock held during compositor I/O. It never recreates the info window, moves or resizes it, or focuses a different application when lookup fails. No focus worker runs on redraws, scrolling or metadata updates, and the worker exits when idle.
- Re-focuses the existing launcher instance instead of opening a second one.
  The open shortcut clears older pending Show Info focus and raises only the main launcher, after any in-flight information-window activation. Information windows stay open without being restored or focused by the shortcut. Matching uses the owning PID and stable native application ID, never a title substring; terminals and information windows mentioning Applicationlauncher are not activation targets.
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
- Terminal launch quoting:
  Cloning and terminal run/CD actions pass Fish and its script as separate `--execute` arguments, without a second command-string parse by xfce4-terminal. Quoted title escape sequences and commands stay intact; titles and directories remain environment data rather than shell source.
- Terminal titles:
  Fish's `fish ~/repos/xfce4-terminal` command-plus-directory captions are normalized to `fish - ~/repos/xfce4-terminal - Terminal` without repeating the command. Matching foreground-command captions such as `htop ~` use the same handling, while older directory-only titles remain supported. Spaces and separator characters inside directories stay intact; known local process/pane directories replace abbreviations, but SSH keeps its remote directory and explicit shell label. Tmux session prefixes, strikethrough and spinner markers are preserved, and raw window/pane titles are unchanged. Parsing adds no subprocess, polling or filesystem lookup.
- Live tmux windows:
  A read-only background monitor maps each local tmux client to its active pane. Window entries use that pane's foreground program, working directory and actual title, preserving braille spinners and attention markers even when `set-titles` is off. Codex entries match the native tmux desktop title: `tmux: diet - codex: ~/tasks/diet - Check ChatGPT link access - Terminal`, with activity immediately before the directory when present. Both legacy `task | project` titles and newer Codex title selections are normalized using the pane's real directory rather than its abbreviated project label; task separators become ` - `. The session label, directory and task remain searchable. The window PID/executable still identify the terminal emulator; Show Info separately lists the tmux client, server socket, session and pane. Queries are batched per server at at most ten updates/second, publish only the latest state, briefly tolerate unavailable servers, and do not rerank searches for spinner-only changes. Idle shells are checked for entry into tmux every two seconds; no remote SSH pane inference or terminal input injection is used.
- Window and process version information:
  Each expanded Show Info process section reports that executable's running version, latest installed local version, local uncompiled-changes status, and both ELF build IDs. The running ELF build ID is coloured using build IDs (or the same executable inode), never just version strings: uncommitted rebuilds can print identical versions while containing different code. This applies independently to the terminal emulator, Fish, Bash, tmux, the active program and other inspected processes. Known noninteractive version switches are probed in a background worker against separately pinned running and on-disk ELF binaries, so deleted old executables retain their real running version after replacement. Unknown executables and shell scripts are never launched for inspection. "Latest" means the local installed binary, not a network/update check or an unbuilt source version. For local checkouts, Progs' published installation links can identify a newer build even while an older immutable release directory still exists (for example, tmux). The executable name and checkout must match, and up to 64 distinct aliases must agree on the same build; ambiguous targets are not guessed. Links and the pinned disk file are checked again after probing, so installation changes during inspection invalidate the comparison. Uncompiled changes use the selected installed build's path with read-only Ninja metadata tools or a bounded Cargo dep-info/source/configuration timestamp check; ordinary Ninja dry-runs are forbidden because they can modify response files or logs. Timestamp results are qualified, not a clean-build certification. Successful complete timestamp enumeration with no newer/missing recorded inputs is shown explicitly even when full build status remains unknown. Ninja's target-reachable always-run triggers, such as Meson revision-header generators, do not by themselves prove uncompiled source changes. Missing/unsupported metadata and invalidated checks still report Unknown. A validated partial scan may show known changed-input counts with an explicit scan-incomplete suffix, but never a successful no-newer observation. Source status is checked again after reopening Show Info and expanding the section; no application is compiled or restarted. A disk binary replaced during inspection is reported as Unknown. Installed Arch package versions remain separate from executable versions. A single worker services at most 16 queued requests and 512 per-popup results; duplicate expansions reuse results. Closing/reopening cancels queued work and discards stale in-flight results. Both process start time and running executable identity are checked to reject PID reuse and exec changes.
  Only the running **Executable Version (ELF build ID)** value uses the comparison colour: green for matching verified builds, red for different builds, and neutral when unknown or no real ID is available. **Latest version on disk (ELF build ID)** always uses the ordinary value colour. Applicable **Uncompiled changes** observations and compact counts are green for No, red for Yes and yellow for Unknown; explicit uncertainty suffixes are yellow independently of the observation colour. Detailed reasons remain in backend evidence. Loading and unavailable results stay neutral; not-applicable compilation fields are omitted. Native browser headers also accept an existing desktop entry whose current command and statically resolved wrapper chain select the inspected installation. A retained off-PATH image, missing/stale desktop entry, unrelated wrapper target or changed process does not establish that publication. Collapsed header checks use the same proof without package queries or executing wrappers.
  Node-backed terminal applications are resolved separately from their runtime. A bounded, read-only lookup validates the script against a package.json bin entry and shows its installed package name/version, canonical entry point, and manifest. CLI programs such as Pi that overwrite their command line can be identified through their command symlink; this is explicitly labelled an inferred command lookup, and ambiguous matches are rejected. Installed package metadata is not claimed to be the exact version already loaded by an older process. Node's version, Arch package, ELF IDs and binary freshness are labelled as runtime information, never as Pi's version. No application JavaScript, npm commands, or application --version switches are executed to obtain package metadata. Pi's running code and running extensions fingerprints are coloured independently against their respective installed hashes; the installed hashes remain neutral. Missing, malformed or untrusted comparisons stay neutral. Patch verification, comparison timestamps/reasons and fingerprint coverage remain internal validation evidence rather than Show Info rows.
- Show Info process trees:
  Field names use muted grey-based colours by scope: blue-grey for window metadata, green-grey for generic process and loaded-code information, rose-grey for application-specific runtime context, and warm grey for installation/on-disk metadata. Generic PIDs, parent PIDs, start times, working directories and process command lines remain green-grey, including generic process identities inside nested frontend or guest-client records. Rose-grey is reserved for details specific to what runs within an application: script/launch targets, Pi's running entry point, selected extension roots and startup receipt, and tmux/SSH/Mosh session, pane, connection, socket, activity and sizing details. Versions, fingerprints and binary modification times retain the running-process scope; installed-build records retain the installation scope throughout. This classification is explicit and independent of displayed field wording or searchability; values keep their existing colours. Recognized Progs classification badges use their source/role colours wherever they appear, including `[PDP]` in Package, multiple `[PEP]`/`[AEP]` roots, other fields and tree headers. Bare classification values such as `FRK` use the same palette. Only the badge text changes colour; surrounding values retain searchable highlights, red/green build identities or their ordinary colour. Unknown or malformed bracketed text is left unchanged. Styling preserves the exact text, continuous selection, raw strikethrough copying and divider positions, with no new metadata lookup or background polling.
  Native versions use **Executable Version (ELF build ID)** and **Latest version on disk (ELF build ID)**. Each value includes its own build ID in parentheses, for example `Xfce Terminal 1.2.0-fork0.4.9-46 (4fcbbefb36addaaee2f2948a22e2b808698149fa)`, without separate ELF-ID rows. Only the running version retains the verified green/red comparison colour; the on-disk version always stays neutral, as do missing IDs, unknown comparisons and application-only installation views. Missing-version/build-ID explanations remain visible and identical loading or unavailable messages are not repeated. Running executable image original path shows the path of the file backing the process; resolved installation paths appear in Launch chain without a separate on-disk executable-path row. Package and Root package are omitted for forks, development projects, clones and unmanaged installations, and when no actual package identity is available. Native ownership can show Loading while undecided; inferred upstream Pacman/AUR targets are not owning packages. Real npm manifest identities remain visible for installed Node applications. Upstream-version comparisons are unaffected by package-row visibility. Scoped no-newer-input observations and their checked-input count are green; only the following full-build-unknown caveat is yellow, without changing the selectable/copied text. Interpreted applications retain their own version/build evidence without borrowing their runtime's identity.
  Generic process fields are ordered Process name, PID, Running executable image original path, Executable Version (ELF build ID), Command line, Working directory, Parent PID, Process start (ticks since boot). Installation fields follow as Name, Type, Capabilities, Comment, Description, Application languages, Build system, Uncompiled changes, Latest version on disk (ELF build ID), Package, available Pacman/AUR and npm versions, Root package, Launch chain, Desktop file and Icon path. Optional fields are omitted when inapplicable, source-specific labels are retained, and loading does not change their relative order. Window-specific details still precede process fields; application-specific tmux details follow generic process identity. Script names remain in their script headings rather than duplicated in process fields. Source checkout is no longer displayed as a separate path row; its underlying metadata still supports source-language discovery and build checks.
  Process metadata appears before the expensive recorded build-input check completes. Only Uncompiled changes shows `Checking recorded build inputs…`; a separate bounded worker updates that field without blocking other process expansions or overwriting package results. Expanding a previously checked header retains its current-build highlighting while detail rows load. Closing, refreshing, changing the executable or retargeting a launch alias rejects obsolete replies. Queue exhaustion or a failed check produces an explicit Unknown rather than an endless loading state; no new persistent cache is added.
  Settings -> Show Info -> Scroll Sensitivity controls wheel scrolling in all info windows, independently of either launcher panel. It applies immediately, is saved between launches, and supports 0.1x to 10x speed (default 2.5x). Scrollbar dragging and text selection are unchanged.
  Process header text (including PIDs, executable paths and deleted-binary markers) is read-only and selectable for copying. Use the arrow beside a header to expand or collapse its details; selecting text does not toggle the section.
  A recovered shell/Python script name appears separately after Process name. Descriptor-backed scripts such as `/proc/self/fd/3` retain their memfd script name without rewriting the raw command line or replacing the interpreter's process identity.
  Window Info is a top-down process tree with independent dropdowns, branch connectors, PIDs, executable paths and window-owner/active-process markers. The window-owner dropdown starts expanded and contains titles, geometry, minimization, activation history, desktop entry and window application versions. Other dropdowns contain only their own PID, executable, working directory, command, desktop entry and lazily loaded versions. Tmux session/pane fields appear only under the corresponding tmux client/server, not under Pi or a shell. Pi's installed application metadata stays separate from Node's runtime metadata. Collapsing details leaves the hierarchy visible. Text selection spans the visible tree, including headers; copy selected text with the keyboard or the right-click **Copy** and **Copy HTML** actions.
  The tree includes the owning process's descendants, so a Pi launcher shows Pi and its SSH tunnel as siblings rather than incorrectly presenting SSH as Pi's parent. Ancestors are included without unrelated application branches. Detached tmux panes retain their actual server ancestry; no false parent-child connection to the terminal client is drawn. Shared terminal/browser processes can cover more than one window. The snapshot is collected once per Show Info opening (or a change of target process) by a named background worker, not during startup, search, scrolling or each frame. Collection is capped at 32,768 scanned entries, 512 displayed processes, bounded ancestry/descendant depth, 16 KiB per command line and a 500 ms work budget; exited/reused processes and partial captures are reported rather than guessed. Threads are omitted. Reopen Show Info for a fresh snapshot; spinner/title changes do not refresh it or reset dropdowns/text selection. No process is started, signalled or modified to collect the tree; optional version probes use only the existing noninteractive allowlist.
- Show Info installation classification:
  Application tiles show the installed application's on-disk version, Progs installation classification, installed package, available package version, ELF build ID and supported local-build status. Launch chains retain resolved symlink/wrapper paths without describing a browser as its custom launch script. Resolution follows at most 24 file/link steps and reads at most 64 KiB per script. Only literal absolute final `exec` targets and the verified Chromium `CHROME_WRAPPER`/`HERE` template are followed; dynamic, conditional, missing, cyclic or unsupported targets are explicitly unresolved, never evaluated. Scripts are never run, and interpreted applications retain their script identity rather than inheriting their interpreter's version. Every captured launch file/link is rechecked after native metadata probing. An exact Pacman package owner can supply Progs classification and description when its inner executable is not indexed as a launch command; unrelated names and conflicting descriptions are not guessed. A named background worker publishes local metadata first, then optional bounded Pacman/AUR and npm queries, using at most three queued updates. Closing the popup cancels remaining work and rejects late results. Drawing uses captured file-existence state, not repeated filesystem checks, and no application-tile probes run during startup, searching or scrolling. The pane fits its natural content height at 1200 logical pixels wide, subject to screen limits, while retaining continuous text selection.
  Capabilities (including Progs' primary-role summary) and description are also read from the saved Progs inventory. They appear in application-tile info and in the relevant process-tree sections, with Node applications kept separate from their runtime. Missing or conflicting records show `Unavailable`, rather than borrowing another application's description or treating absent capabilities as a data-only package. These are informational fields, not additional search fields; lookups run only on background workers when Show Info is used.
  Installation rows display only Progs' current classification initials: Type uses bare codes such as `FRK`, `DEV`, `CLO`, npm's `NET` or Bash's `PDT`, with the existing Progs colours. npm, uv and Cargo tools retain their explicit/dependency/unknown installation roles rather than using obsolete generic source codes; rustup remains `RUT`. Older local aliases are normalized to current names (`FKM` to `FRK`, for example), while legacy generic toolchain codes become unknown-role tool codes rather than claiming an installation reason. Package retains the owning package's name/version followed by a bracketed package code such as `[PDP]`; expanded classification and management names are omitted. They read `$XDG_CACHE_HOME/progs/scan-v1.json` (or `~/.cache/progs/scan-v1.json`) in the existing background info worker, never run Progs or trigger an inventory scan. Exact executable/alias targets take priority; a local source wrapper can identify a compiled binary only when its name and Git checkout both match. Conflicting, absent, unsupported or oversized inventory data reports UNC instead of guessing from the process name. The parser is capped at 16 MiB, 8,192 programs, 32,768 binaries and 65,536 reverse dependency edges; parsed data is reused until the inventory file changes. These classifications describe the last saved Progs inventory, not proof of the running binary's version. Refresh Progs, then close and reopen Show Info for updated classifications. Actual Pacman package versions are retained in the installation row, independently of executable versions and ELF IDs. Node application entry points and their Node runtime are classified separately.
  Root package shows one original requested target followed by the number of all other installed packages that depend on the inspected package, directly or indirectly. The root is the first target, in command order, from the package's earliest recorded installation transaction whose dependency chain includes that package. Matching uses the current saved Progs dependency graph, not a captured historical graph, so the result is explicitly marked inferred. The displayed version and badge are the root's current Progs metadata; later changes to explicit/dependency flags do not choose a different root. Counts exclude the inspected package and the displayed root, include intermediate dependencies and packages beyond explicit roots, and deduplicate cycles and shared paths. Missing graph evidence produces an explicitly qualified lower bound; absent history, unsupported requests or no matching original target show Unknown rather than selecting an alphabetical/current root. `/var/log/pacman.log` is read without execution or subprocesses, bounded to 16 MiB, 64 KiB per line, 1,024 command arguments, 65,536 historical package/target records and a 200 ms parsing budget. Only completed transactions establish requests; later reinstalls do not replace the first recorded installation. Repository-qualified sync targets, ordinary installer options and version-checked local/AUR package archive targets are supported; skipped `--needed` targets, ambiguous quoting, stdin targets and unsupported options do not invent attribution. The entire available regular log is inspected, not just a recent tail; missing or rotated-away older history cannot be reconstructed. Parsed history is cached until file identity changes, and up to 128 root/count results are cached per inventory and history revision. Reads and traversal run only in the existing on-demand background metadata worker, never in startup, searching, scrolling or frame rendering; no watcher, polling loop, new database or package operation is added.
- Available package versions:
  `Latest package (in last Pacman sync)` and `Latest version (AUR)` are separate from the running version and latest binary on disk. Installed executable ownership identifies the package first; Progs can identify an exact package or a local source checkout's name. Unmanaged scripts and npm/uv/Cargo command names are not assumed to identify OS packages. Node application entry points and the Node runtime are checked independently. A dedicated background worker reads `pacman -Si` in configured repository priority order, then tries `paru -Si --aur` when no exact repository package is found. It never refreshes databases, installs, builds or upgrades packages, prompts for privileges, or runs discovered applications. Pacman values reflect the local sync databases and use the compact format `repository/package version (synced 2 hours ago)`, switching to whole days at 24 hours and showing `less than 1 hour ago` for recent syncs. The age uses the complete timestamp and timezone offset of the latest recorded synchronization request in `/var/log/pacman.log`; the record does not prove that every repository downloaded successfully. Only the last 1 MiB of the regular log file is read, off the GUI thread, and the parsed timestamp is cached until the file changes. Ages are recalculated when the info rows are drawn, including cached results; rendering reads only captured timestamps and does not launch a query, read the log, or add a timer or polling. Missing, unreadable, invalid or future sync timestamps show `sync time unknown`; database download/publication timestamps are not substituted. Paru may contact the AUR using only the package name. AUR values use `aur/package version (checked 2 minutes ago)`, with a monotonic timestamp captured only after a successful lookup and preserved through cache reads and worker publication. They show `checked just now` during the first minute, then minutes, hours and days. AUR never inherits a Pacman sync age. Labels follow the structured query source, not the repository name; pending and failed lookups use neutral `Latest version (Pacman/AUR)` labels rather than inventing a sync or check time. Each tool has a three-second timeout and a 64 KiB accepted response bound. Up to 128 results are cached for five minutes (failures for 15 seconds), with default Pacman sync-directory changes invalidating cache keys. The per-popup package queue is capped at 16 requests and sleeps when empty. Ordinary binary metadata is published first and is not blocked by slow AUR queries. Closing or retargeting an info window clears queued work and rejects stale in-flight results; no lookup runs during startup, search, scrolling or unopened process sections.
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
When no desktop entry is associated, Desktop file is `None` and Icon path, Name
and Comment are `-`. Pending discovery stays `Loading...`; a known expected
desktop-file path still reports `(missing)` when the file does not exist.
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
titles, process headers and metadata. Copying selected text preserves the original
Unicode text, including the raw window title.
Selection can span the entire visible process tree, including headings and
expanded Application/Runtime sections. Right-click selected text for **Copy** or
**Copy HTML**, which preserves colours and strikethrough with a plain-text fallback.
Process headings use the arrow pointer without disabling selection; the collapse
arrows use the hand pointer, and metadata retains the text-selection cursor.
Ordinary application/process metadata stays captured until the information window
is closed and reopened or its process identity changes, without periodic full
reloads. Pi's lightweight startup-receipt updates remain separate.
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
process name, PID, Parent PID, process start, working directory, command line,
executable path, running version/build ID, installed
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
- Temporary border overlay support for highlighting a target window.

## Requirements

- Linux
- KDE Plasma on Wayland
- `kdotool` available in `PATH`
- `libpulse` (including development headers/pkg-config metadata for building), and a PulseAudio-compatible server such as `pipewire-pulse` for playback visualisation. `pactl` is used only for user-requested volume/mute changes.

Install Rust dependencies and build with Cargo. `kdotool` is the main external runtime dependency used for window activation, raising, and closing.

## Build

```bash
scripts/build-debuggable-release
```

This verifies all release profiles, archives exact binaries/symbols/sources and
publishes the observable pair without restarting existing processes. For a
development-only compilation:

```bash
cargo build --release
```

Development compilation does not substitute for release-matrix verification. See
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

### Launcher icon

`src/assets/applicationlauncher.svg` is the launch-grid logo shared by native
windows and desktop integration. KDE Wayland resolves the main window's stable
`applicationlauncher` app ID through `applicationlauncher.desktop`; setting native
window icon pixels alone is not supported by this Wayland backend. Symlink
`src/assets/applicationlauncher.desktop` into `~/.local/share/applications/` and
the SVG into `~/.local/share/icons/hicolor/scalable/apps/`, retaining their
filenames, then run `kbuildsycoca6`. The desktop entry uses the existing
`applicationlauncher` command, which must be on the desktop session's PATH.

All launcher-owned native windows use the same branded viewport builder, including
Show Info, Settings, History, terminal maintenance and the border overlay. The
unique Show Info app IDs remain unchanged to preserve exact-window focus.
On KDE Wayland, import `src/assets/applicationlauncher.kwinrule` in System
Settings → Window Management → Window Rules after installing the desktop entry
and icon. This narrowly scoped rule maps only the launcher's exact app-ID family
to that desktop entry. It changes desktop/icon attribution, not the window class,
focus, geometry or other applications. Append it after existing rules so earlier
user overrides keep their priority, then reconfigure KWin; application restarts
are not required to apply the rule to already matching windows.

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
  Row height, icon size, padding, text spacing, line height, title size, path size, and whether the subtitle path is shown.
  `Show Launcher Window` includes the launcher's main window in the window list and count.
  It is off by default, applies immediately, and is saved across restarts. Settings and
  Show Info windows remain excluded; session history and recovery are unchanged.
  The daemon publishes the main launcher through a separate live-only collection:
  it participates in incremental and full window-feed updates but never in saved
  windows, restore inference, closed history, or mass-disappearance recovery counts.
- General:
  `Disable text select cursor (I-beam)`

Displayed calendar dates use `DD-MM-YYYY`, including saved-session capture dates
and Show Info activation timestamps. Times, UTC markers and relative ages remain
unchanged, as do stored timestamps and user-supplied names or titles. Settings use
compact 4-point grid-row gaps without shrinking the controls.

## Tmux Process Information

Show Info keeps tmux server properties separate from the selected terminal's
client and active pane. The server shows its listener, running/installed build
identity, host, sizing monitor and session/client counts. Independent dropdowns
describe every session (including detached sessions) and every attached client.
Server properties are merged directly into its main process fields, without a
second "Tmux server" dropdown or repeated version/outdated rows. The distinct
running and installed SHA256 hashes sit beside their respective ELF build IDs;
the existing version comparison colours and inline Update link are unchanged.
Capture errors remain visible in the same fields, and metadata from a different
server PID/start-time/listener is not merged.
The client process shows only the record matching its own PID and process start
time, never a different attachment or the server's default active pane.
Its connection fields are merged into the main process details, without a separate
connection dropdown or repeated session/pane/PID fields. The four tmux/network
socket fields replace legacy Socket, address/port and inode aliases; older records
are normalized without confusing Unix IPC sockets with network endpoints. Frontend
PID, versions and build evidence are omitted when that exact PID/start-time pair
already has its own process-tree entry. Distinct remote frontends, running versus
installed evidence, unknown fields and guest routes remain available.

Detailed data comes from the installed `tmux-mosh --socket PATH clients --json`
collector in the separate `../tmux-mosh` repository. It includes local/SSH/Mosh
transport, frontend versions/fingerprints, attachment and reachability state,
keyboard activity versus network packet age, TTY, dimensions, sizing policy and
controlling client, verified Unix socket pairs, network endpoints and guest/VM
route metadata where available. Shared listeners are not connected endpoints.
Latest installed references remain separate from running build evidence.

Collection starts only when a tmux process section is expanded, runs in the
background once per Info capture, shares concurrent requests for the same server
and does not expire on a rendering timer. Session/client subgroups format their
details only when expanded. Reopen Info for a new capture. Missing helpers,
unsupported servers, changed identities and failed queries are shown explicitly;
unknown network state is not treated as disconnected. This path never starts a
server or sizing monitor, changes flags, attaches/detaches clients or runs update
or cleanup commands.

## Terminal Maintenance

The **Update application...** context-menu entry and standalone **Update Application**
popup have been removed. Settings no longer includes **Terminal Maintenance** or
**Stop idle Codex sessions...**. Their backend operations and safety checks remain
available. Show Info now places a text-height **Update** link after a verified
outdated `xfce4-terminal` window-owner header. Choosing **This window** confirms
that exact window and starts its update after a fresh eligibility check, without
another launcher confirmation. **All outdated windows** opens the existing preview
and confirmation workflow. Current or unknown terminal builds and unrelated processes have no
terminal-update link. Cancelling the chooser leaves Show Info open without starting any work;
up-to-date windows are never selected for restart.
These operations use already built/installed binaries; they do not build or upgrade packages.
The window-list context menu starts with **Show info**, **Open window**,
**Open new window**, **Update window** and **Clone window**, in that order,
and uses **Close window** for closing the selected window.
**Update window** opens the same **Update xfce4-terminal?** confirmation dialog
as Show Info, with **This window**, **All outdated windows** and **Cancel**.
Opening the dialog starts no update. The target is the exact window owner, not
its foreground program, tmux client or tmux server. This is currently enabled
only for `xfce4-terminal`; other window owners have a disabled entry. After
confirmation, progress and errors for this window appear inline below the
launcher's search bar; the all-windows choice opens the reviewed bulk workflow.
Launcher navigation and search refocusing are suspended while the modal is open.
The same fresh eligibility and outdated-build checks apply, so current windows
are not restarted.
Both terminal-update dialogs explain the in-place geometry preservation: tmux
tabs reattach to their existing sessions without restarting the server or its
applications, while supported non-tmux applications are restarted in their working
directories. Single-window validation runs automatically after the scope choice,
without opening the Update Terminal Windows maintenance window. Progress and any
blocking error appear inline in the originating Show Info window; an unsupported
terminal never falls back to the bulk review UI or to a forced restart. Older
terminal builds can use daemon-owned replacement without a manual restart. Busy or pending
updates cannot be retargeted or queue another update. Bulk updates still require
reviewing the preview and confirming. Updated terminals accept the launcher's
explicit consent without opening a second confirmation; their native context menu
still shows the captured non-tmux restart plan. Native identity, tab, startup and
placement checks cannot be bypassed by confirmation.

### Lifecycle Ownership

`applicationlauncherd` owns restart policy and transaction coordination through
the versioned `/Lifecycle` D-Bus interface. The GUI only selects scope, obtains
consent and displays results. Native terminal cooperation supplies accurate tab
metadata, holds and controlled handover where supported. Without its updater/hold
API, the daemon stages a new terminal through the normal CLI and restores placement
and closes the exact original window through KWin. Both paths use the same
application/session adapters and restart policy, not separate Codex detectors.
For compatibility updates, original tmux clients and their dedicated idle shell
ancestry are retired only after replacement attachments verify. The tmux server
and pane applications keep running; unrelated background shell jobs block the
update instead of being terminated by a window-close request.

The compatibility preview uses read-only `ListTerminals` metadata when present.
Without it, the daemon requires exactly one actual KWin window for that PID and
one verifiable controlling-PTY child session. Multiple tabs without metadata,
ambiguous shared-process windows, changed sessions and unsupported applications
are refused before interruption, rather than guessing from a title. Previews
record exact process/build/session identities; confirmation cannot silently select
a different conversation. Running-current terminal builds are not restarted.

Confirmed operations run in independent transient user services using the same
daemon executable. They survive GUI closure and daemon replacement. Private,
fsynced plans, stage journals, worker identities and results allow a new daemon
to report the outcome; interrupted work is never blindly replayed. One shared
cross-process lock excludes overlapping window/program/server restarts and
session restores. Automatic snapshots wait while replacements are staged.

A terminal-only update pins each native application's running executable,
including deleted/replaced files, rather than implicitly upgrading it. A
content-addressed recovery copy and literal recovery arguments are retained in
`$XDG_STATE_HOME/applicationlauncher/lifecycle/builds/`; per-window plans and
reports live in `lifecycle/<operation-id>/`. Backups remain until manually removed.
The native image limit is 1 GiB per captured binary. These are restart recipes,
not snapshots of loaded libraries, process memory, unsaved shell state or scrollback.
Individual Fish/Codex updates deliberately select the latest installed build.


For slow updates, an agent can directly enable opt-in scalar stage timing through
the running daemon's same-user `LifecycleTiming1.ActivateUpdateTimings(seconds)`
D-Bus API. No user command, tracing flag or restart is needed. Leases expire
within 300 seconds. Private bounded JSONL files under
`lifecycle/timings/` distinguish confirmation wait, recovery hashing/cache/copy,
terminal discovery and window handover, with monotonic wall and CPU durations.
See `docs/DEBUGGABILITY.md` for controls, retention, privacy and verified
compile-time-minimal artifacts. Detailed timing is off until activated; normal
launches retain the bounded observable baseline.

Restarted Codex runs under an interactive Fish parent. Normal exit, failed exit
and Ctrl+C return to Fish instead of closing the terminal tab. Its exact
conversation, literal arguments (including argv[0]) and working directory remain
unchanged. Window-only updates pin an existing Fish ancestor's running image;
without one they prepare a verified installed Fish before interrupting anything.
Both Fish and Codex get durable recovery images and a return-to-shell command.
This is shared daemon-side policy for native and compatibility window updates,
not application detection in the terminal helper. It applies to new restarts;
existing Codex processes launched by earlier updates are not modified in place.

With a native hold, the old window stays open until replacement application identities
verify. Without one, the daemon fully prepares a minimized, correctly placed
replacement before stopping applications; the old terminal may then auto-close as
its child exits. The already-placed replacement is shown while its applications
are verified. Tmux applications keep running and reattach to their exact existing
server/session. Failures retain recovery records and any usable windows rather
than force-killing, guessing another conversation or replaying a restart blindly.
A non-tmux Pi window remains unsupported because
its complete loaded runtime/extensions build cannot yet be preserved; Pi inside
tmux stays running. The old terminal version is not itself a reason to refuse a
whole-window replacement. This does not introduce a persistent PTY
server or transfer a running non-tmux process to another window.

The main CopyQ window-owner process has an **Update** link regardless of build
status. Its modal runs `systemctl --user restart app-com.github.hluk.copyq@autostart.service`
only after **Restart CopyQ** is clicked. This restarts the installed application;
it does not build or install anything. Cancellation has no effect. The bounded
background restart reports success or errors, refuses a changed process identity,
and cannot queue duplicate restarts across information windows.

Verified outdated **Fish** and **Codex** process headers have their own **Update**
links. The confirmation restarts only that exact PID/start-time identity using
its latest installed binary, in the same terminal tab or tmux pane. The terminal,
tmux server and other panes stay running. The preview checks only that program,
not every session on the server. Codex resumes its exact conversation; Fish must
be idle, with no foreground application or background jobs. Missing terminal
handoff support, changed process identities and ambiguous conversations block
the restart rather than targeting another process. Explicit consent is required;
closing the dialog does not start work or cancel an already confirmed operation.
Reopen Show Info after completion to capture the restarted process. These links
do not build, install or download anything.

Program/server restart APIs require a fresh preview, an eligible action and explicit
confirmation. Codex resumes its exact observed main session ID with supported launch
and permission options. Open child-agent logs are excluded only when their parent
metadata forms a verified chain back to that main conversation. Conflicting roots,
unknown lineage, unreadable metadata or unsupported options are refused, never
replaced with `--last`. Pi's Radiance launcher is supported separately: a sealed
launcher source, same-user PID/start-bound activity marker and unique matching
project session header identify its exact session file, including verified history
migration identities. Restarts replay only supported launcher options, reuse the
backend without installing anything, and never select `--session last`. Only the
launcher's verified loopback SSH tunnel is treated as an owned helper; active
tools and unrelated shell jobs still block a restart. After confirmation Pi is
stopped gracefully and its launcher/tunnel must exit before its pane is recreated.
The restored Pi must match the captured session, installed launcher/entry point and
Node image, not merely have the same process title.
Native tab replacement requires the new xfce4-terminal
`PrepareTabRestart`, `CommitTabRestart`, `CancelTabRestart` and `GetTabRestartStatus`
methods. Update older terminal windows first. The terminal retains its existing
window and tab UUIDs and placement while its child is replaced; unrelated terminal
windows and tmux panes are not restarted. Shell state in the replaced tab/pane is
not preserved. Fish with foreground/background jobs cannot be restarted alone.

Whole-server replacement captures all sessions, windows, pane layouts, directories,
supported program recipes and terminal-client bindings. Every local client must map to
a restart-capable xfce4-terminal tab. A failed client handoff check names its
session and client PID; update that terminal window first, then refresh the
server preview. Grouped sessions, linked windows, unsupported
programs, unrelated shell jobs and more than 128 panes are refused before shutdown.
Prepared tabs remain open as the old server exits. The new server recreates the
sessions and layouts, clients reattach in their original tabs, and startup gates
release the programs only after the client/session bindings are checked. The
final program binary, working directory and exact Codex/Pi session are verified.
Scrollback, shell-local variables and runtime-only tmux options are not preserved.
The original server's global colour preferences (`NO_COLOR`, `FORCE_COLOR`,
`CLICOLOR`, `CLICOLOR_FORCE` and `COLORTERM`) are captured separately. Replacement
startup uses those values, including their absence, rather than inheriting colour
overrides from the updater's command environment. Other environment values are
not copied into this colour snapshot, and intentional no-colour preferences remain
respected.

Before interrupting any application, the updater rehearses the full replacement
on a short private socket using the installed tmux binary and startup configuration.
All application gates stay closed, and the private server is stopped afterward.
A failed rehearsal leaves the original server and its applications running.
Process and terminal identities are checked again after this startup check.
If the real replacement exits unexpectedly during its first startup, it can be
retried at most three times, and only while its listener is provably unused and
no application gate has opened. An existing listener is never adopted or killed.
Once started, subsequent commands are pinned to that server with auto-start
disabled. A lost shutdown acknowledgement does not prevent restoration when the
old server's exact process identity has exited. Uncommitted native-tab holds are
released even after a post-shutdown failure, rather than leaving input disabled
until the terminal's timeout expires.

Attached tmux clients with verified same-user `mosh-server` ancestry are listed
separately in the preview and do not require a local terminal handoff. Confirming
the server update authorizes disconnecting these clients when the old server
shuts down. Their sessions and supported programs are still recreated, but remote
clients are not reattached automatically; reconnect manually afterward. No mosh
process is directly killed and preview never disconnects a client. The captured
client PID/start/build, parent chain and complete attached-client list are checked
again before shutdown. Missing, changed or ambiguous ancestry, unknown remote
transports and unmapped local clients still block the update.

A verified outdated **tmux-server** header in Show Info has its own inline **Update**
link. Its modal offers only **Update server and restart all sessions**, not a
terminal-window or single-pane update. The read-only preview lists every session,
including detached sessions, and its pane applications/directories and replacement
binary. Confirmation requires accepting interruption explicitly. The server PID,
start ticks and socket must match the captured header before confirmation can be
enabled; an old daemon without structured scope metadata cannot enable it. The
saved plan is revalidated again before execution, and existing terminal windows
and tabs are retained. Codex, verified Pi launchers, idle fish/bash/zsh, htop and
nvtop have restart recipes. Unsupported applications, unverified/unmapped clients
and unsafe shell jobs block the entire update before sessions are stopped.

Daemon-owned, fsynced recovery plans are kept at
`~/.local/state/applicationlauncher/application-updates/<operation>/restart-plan.json`
in private directories until manually deleted. Operation reports include the path.
Server restarts also retain a private, fsynced `restart-events.jsonl` alongside
the plan, recording startup attempts, the replacement process identity, rebuilt
pane/gate mappings, client reattachment, application release and the failing stage.
Commands are identified by verb; captured command lines and environment values
are not copied into this progress journal.
These are restart recipes, not a promise to recover unsaved program memory, and
are separate from F9 session snapshots. Changed identities and expired previews
are refused, jobs are serialized with restores and terminal maintenance, and
confirmed work continues if the GUI closes. Failures after shutdown are reported
as failures with the retained backup, never as successful updates or automatic
force-kills. No scanning or restart worker runs during ordinary window listing.

The retained idle-Codex helper sends one `SIGINT` to each verified local idle Codex
process, after checking terminal titles for spinners/attention and rechecking process
identity and foreground ownership. It does not signal terminals, shells or process
groups, force-kill or automatically retry. Unsupported, busy or ambiguous sessions
are skipped. SIGINT is not a literal TUI Ctrl+C keypress; the final title-read/signal
boundary cannot be atomic with Codex starting a new turn. Nothing invokes this helper
from the settings page.

The terminal-maintenance client uses the native mechanics path when the running
terminal advertises both update status and safe window holds. It requests
`UpdateInPlaceWithConsent(window_uuid, true)` when available, otherwise the older
`UpdateInPlace` request. Without those capabilities it uses the daemon-owned
compatibility path described above, not a manual-restart requirement. Both
transports use the same lifecycle coordinator and application adapters; the GUI
does not implement restart policy. The GUI can be closed, and confirmed workers
survive a daemon restart with their private recovery records intact.
Window mapping uses the terminal PID and lifetime UUID; identical captions in
one shared process are refused rather than guessed. There is no maintenance scan
during ordinary listing. Existing sessions, snapshots and named backups remain.

Isolated adapter and popup checks:

```bash
cargo test --lib terminal_upgrade
cargo test --bin applicationlauncher app::terminal_maintenance
UV_CACHE_DIR=/data/.cache/uv uv run --no-project --python /usr/bin/python3 python3 -m unittest discover -s tests -p test_application_updates.py -v
UV_CACHE_DIR=/data/.cache/uv uv run --no-project --python /usr/bin/python3 python3 -m unittest discover -s tests -p test_lifecycle_windows.py -v
UV_CACHE_DIR=/data/.cache/uv uv run --no-project --python /usr/bin/python3 python3 tests/application_update_native.py --wayland ~/repos/xfce4-terminal/build/terminal/xfce4-terminal
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
