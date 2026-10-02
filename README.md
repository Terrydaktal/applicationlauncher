# applicationlauncher

`applicationlauncher` is a Rust GUI launcher and persistent window-session tracker for KDE Plasma on Wayland. The GUI combines searchable open windows and installed applications. The companion `applicationlauncherd` process records window activation and closure history, maintains crash-recovery, five-minute, hourly and named snapshots, and can restore missing windows without closing unrelated work.

## Project Structure

```text
.
├── src/
│   ├── bin/applicationlauncherd.rs
│   ├── tracker/
│   ├── windows/
│   ├── app/
│   │   ├── commands.rs
│   │   ├── feed.rs
│   │   ├── helpers.rs
│   │   ├── popups.rs
│   │   ├── settings.rs
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
- `src/windows/terminal/tmux.rs`: Background per-client active-pane monitoring and latest-state window metadata overlays, independent of tmux's desktop-title forwarding setting.
- `kwin/applicationlauncher-window-feed/`: Transactional KWin script that sends compositor window events to the daemon.
- `scripts/applicationlauncher`: Fast launch wrapper that activates current release binaries and reports newer source without compiling it.
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
- Provides context actions on windows, including closing the window and showing the execution chain popup.
- Re-focuses the existing launcher instance instead of opening a second one.
- Persists window size, pinned applications, and launcher settings under `$HOME/.config/applicationlauncher/`.

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
- Live tmux windows:
  A read-only background monitor maps each local tmux client to its active pane. Window entries use that pane's foreground program, working directory and actual title, preserving braille spinners and attention markers even when `set-titles` is off. Codex entries match the native tmux desktop title: `tmux: diet - codex: ~/tasks/diet - Check ChatGPT link access - Terminal`, with activity immediately before the directory when present. Both legacy `task | project` titles and newer Codex title selections are normalized using the pane's real directory rather than its abbreviated project label; task separators become ` - `. The session label, directory and task remain searchable. The window PID/executable still identify the terminal emulator; Show Info separately lists the tmux client, server socket, session and pane. Queries are batched per server at at most ten updates/second, publish only the latest state, briefly tolerate unavailable servers, and do not rerank searches for spinner-only changes. Idle shells are checked for entry into tmux every two seconds; no remote SSH pane inference or terminal input injection is used.
- Audio indicators:
  Rings respond to actual playback samples, not volume-slider values, MPRIS status or a synthetic animation. Each PulseAudio sink input is monitored individually on its output's monitor source, with no default-source or mixed-output fallback. The worker analyses 16 kHz stereo into eight frequency bands (60 Hz to 6.5 kHz) and peaks in 20 ms blocks, publishing at most 25 updates/second. Silence, corking, mute and stale samples clear the indicator. Audio is consumed in memory and discarded, never saved or sent to the GUI; the GUI receives only bounded, quantised levels and displays the latest result rather than replaying an update backlog. Monitoring is capped at 32 streams with requested 100 ms buffers and 256 metadata records, and reconnects with bounded retry delays after server loss. No analysis, audio-server I/O or process spawning occurs in the drawing path.

  Playback-stream change notifications refresh metadata without recreating healthy monitors. PipeWire emits these notifications when a monitor is attached, so treating every change as a disconnection would create a feedback loop and overload the shared audio services. Removal/replacement still invalidates immediately; refreshed identity, device, mute and cork state determine whether a monitor actually needs replacing.

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
    3. **Window Title**: Alphabetically after transient braille and attention markers are ignored.
  * **When a search query is typed:**
    1. **Fuzzy Match Score**: Best metadata match across title, app name, class, executable, desktop entry, and path-like context.
    2. **Application Key**: Alphabetically (case-insensitive) by application class/key.
    3. **Window Title**: Alphabetically (case-insensitive) by window title.
- UI:
  Draws a frameless launcher window, a separate settings popup window, and a separate execution-chain popup window.
- Single-instance behavior:
  Uses a Unix socket lock so a second launch request focuses the already-running instance.

### Optional Firefox Audio Bridge

Install once, then start Firefox normally at your next convenient browser restart:

```sh
sudo python3 scripts/install-firefox-audio-bridge.py
```

This is privileged browser-side JavaScript, not a WebExtension or a remote-debugging server. Only install trusted bridge code. The installer enables unrestricted **AutoConfig**, not unrestricted web pages or a disabled content sandbox. It preserves an existing dictionary AutoConfig and adds its own loader to that same file. Re-run after a Firefox package upgrade or configuration deployment removes the installed files. No existing browser windows are closed by installation.

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
- Right-click on a window entry to open, clone, show metadata, close the application, or inspect its execution chain.
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

The wrapper never invokes Cargo during launcher activation. It compares any running launcher and daemon processes with the already-built `target/release` executables and restarts both components only when those executable files have been replaced. Source, Cargo metadata, embedded KWin files, and the local `../fuzzy-rank` dependency are checked separately; newer source produces a warning in the launcher while the existing release continues running. Run `cargo build --release --bins` explicitly to build those changes.

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
