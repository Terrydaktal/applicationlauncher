# applicationlauncher

`applicationlauncher` is a Rust GUI launcher and persistent window-session tracker for KDE Plasma on Wayland. The GUI combines searchable open windows and installed applications. The companion `applicationlauncherd` process records window activation and closure history, maintains crash-recovery and named snapshots, and can restore missing windows without closing unrelated work.

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
│   ├── audio.rs
│   ├── models.rs
│   ├── search.rs
│   └── main.rs
├── kwin/applicationlauncher-window-feed/
├── Cargo.toml
├── Cargo.lock
└── README.md
```

- `src/main.rs`: GUI CLI parsing, single-instance startup, and native window creation.
- `src/app/`: Launcher state, feeds, commands, search-row rendering, settings, popups, and the `eframe` update loop.
- `src/launch/`: Desktop-entry parsing and application/window launch actions.
- `src/audio.rs`: Bounded audio activity sampling and waveform levels.
- `src/diagnostics.rs`: Single-instance control socket and low-overhead live-process diagnostics.
- `src/search.rs`: Fuzzy ranking, transient-title normalization, sorting, and highlighting.
- `src/models.rs`: Shared window, application, feed, and audio data types.
- `src/tracker/`: Daemon client, private SQLite persistence, restore policy, service installation, and D-Bus service.
- `src/bin/applicationlauncherd.rs`: Persistent background tracker entry point.
- `src/windows/`: KWin snapshot consumption, process metadata, terminal integration, and icon resolution.
- `kwin/applicationlauncher-window-feed/`: Transactional KWin script that sends compositor window events to the daemon.
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
  State is debounced to disk. An unclean previous boot produces a restore prompt; a same-boot daemon restart does not. The prior recovery snapshot is preserved until restored or dismissed.
- Restoration:
  Existing matching windows are reused and repositioned, only missing windows are launched, and unrelated windows are never closed. Terminal replay is restricted to shell/CWD, `codex resume --last`, `agy -c`, `htop`, and `nvtop`.
- Recent window reopening:
  The newest recently closed window can be reopened globally with `Ctrl+Shift+T`. The KWin shortcut is ignored while Chrome, Chromium, or Firefox is active, preserving browser tab-reopen behavior. A successful reopen removes that entry from the history list.

- Window loading:
  Uses the KWin event feed for incremental updates, with bounded reconciliation through `kdotool`, then resolves metadata such as title, class, PID, icon, executable path, and terminal child processes.
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

## Features

- Dual-panel layout with open windows and an application panel shown together.
- Keyboard navigation across both panels, including cross-panel selection that follows physical row alignment.
- Independent scrolling behavior for the two panels.
- Immediate icon tooltips in application icon mode.
- Pinning and reordering of applications.
- Middle-click on a window entry to launch another instance of the underlying application.
- Right-click on a window entry to open, clone, show metadata, close the application, or inspect its execution chain.
- Optional close-on-blur behavior.
- Temporary border overlay support for highlighting a target window.

## Requirements

- Linux
- KDE Plasma on Wayland
- `kdotool` available in `PATH`

Install Rust dependencies and build with Cargo. `kdotool` is the main external runtime dependency used for window activation, raising, and closing.

## Build

```bash
cargo build --release
```

## Run

```bash
cargo run --release
```

Or run the compiled binary directly:

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
  Private SQLite WAL database containing current windows, closed-window history, recovery state, and named snapshots. History is retained until manually cleared.
- `$HOME/.config/systemd/user/applicationlauncherd.service`
  Auto-installed tracker service with restart-on-failure behavior.
- `$HOME/.local/bin/applicationlauncherd`
  Symbolic link to the daemon binary beside the launcher binary.
- `$XDG_STATE_HOME/applicationlauncher/panic-latest.log` and `hang-latest.log`
  Private crash and live-hang diagnostics. Reports are mode `0600`.

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

    --diagnose
        Capture diagnostics from the already-running launcher without restarting it.

OPERATION
    When launched, the application retrieves a list of all open windows using
    kdotool and installed desktop applications from the local system. It renders
    a frameless GUI window containing a search input, a main window list, and an
    application side panel. As you type, both lists are filtered using a fuzzy
    matcher.

    Keyboard Navigation:
        - Up/Down Arrows: Move selected window.
        - Enter: Activate selected window.
        - Escape: Close launcher.
        - F5: Refresh list.
        - F10: Open launcher settings.

EXAMPLES
    applicationlauncher
        Launch the application launcher.

    applicationlauncher --diagnose
        Attach the diagnostic helper to the running instance and write a bounded
        process/thread report without replacing the running binary.

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

EXIT STATUS
    0   Success.
    1   Failure (e.g., kdotool not found or D-Bus communication failed).

AUTHORS
    Terrydaktal <9lewis9@gmail.com>
```

## Session Restore Limits

Browser windows are restored as application windows in the first release; exact tabs and URLs require browser-native session restore or a future browser extension. File-manager paths and terminal working directories are restored when reliable metadata is available. Failed or ambiguous items are reported rather than replaying unsafe commands.
