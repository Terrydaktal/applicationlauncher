# Application Launcher (Wayland / KDE Plasma)

A fast, visually stunning GUI application launcher built in Rust using `egui` and `eframe`. It queries open window objects using `kdotool`, allows searching them via a fuzzy-matching interface, and activates/raises the selected window.

## Project Structure

```
.
├── src
│   └── main.rs         # Core application logic, GUI layout, icon lookup, and window management
├── Cargo.lock          # Dependency lock file
├── Cargo.toml          # Cargo package configuration and dependencies
└── README.md           # Project documentation (this file)
```

### Components

- **[src/main.rs](file:///home/lewis/Dev/applicationlauncher/src/main.rs)**:
  - **D-Bus Querying**: Queries all window metadata (ID, title, class, PID) in a single, chained `kdotool` process execution. This reduces the number of process spawns from $N \times 3$ (where $N$ is the number of open windows) down to exactly 1, reducing launcher startup lag by 20x.
  - **Terminal Active Process Discovery**: Traverses the `/proc` filesystem tree on Linux to identify child process trees of terminal emulators (like `xfce4-terminal`, `kitty`, `konsole`, `alacritty`), automatically finding the leaf active program (e.g., `nvim`, `fish`, `lsyncd`, `python`). It displays this running application name in the subtitle (e.g., `xfce4-terminal (running: lsyncd)`) and uses it to resolve custom application icons (e.g., showing a Neovim icon instead of a generic terminal icon).
  - **Icon Resolution & Caching**: Implements XDG Freedesktop Icon Theme specifications via the `freedesktop-icons` crate, traversing custom theme (`breeze`, `breeze-dark`) and `hicolor` inheritances. Additionally, it parses application `.desktop` files (both system-wide and user-local) to extract hardcoded absolute icon paths (common in custom or user-compiled applications like CopyQ) and specific icon overrides. It caches lookup results to eliminate redundant filesystem traversals.
  - **Fuzzy Matcher**: Uses `fuzzy-matcher`'s `SkimMatcherV2` to perform real-time, fuzzy filtering on window titles and class names.
  - **Asynchronous Startup & Threaded Loading**: Offloads all process execution and filesystem queries to background threads. This opens the main GUI window instantly (0ms perceived latency), allowing the user to focus and type into the search box immediately while the window list loading spinner completes in the background.
  - **GUI Rendering**: Employs `egui` (version `0.33`) to draw a frameless, borderless, semi-transparent acrylic window with rounded corners and a premium vertical accent highlight bar.
  - **Icon Only Grid Mode**: Supports an icon-only grid display mode (`-i` / `--icon-only`) for the application launcher, displaying applications in a visual grid that adjusts dynamically to window resizing.
  - **Temporary Window Border Overlay**: Spawns a borderless fullscreen overlay (`--draw-border`) that draws a temporary fading red outline around the target window for 250ms with KWin background dimming to highlight its location on the screen.
  - **Keyboard Navigation**: Captures system-wide keystrokes within the viewport:
    - `Up/Down Arrows` (and Left/Right in grid mode): Navigate through filtered results.
    - `Enter`: Activate and raise the selected window.
    - `Escape`: Close the launcher.
    - `F5`: Force-refresh the open window list.
    - `Ctrl+P`: Toggle pin/unpin status for applications.
  - **Focus Loss Behavior**: Auto-closes the launcher immediately when the window loses focus (can be disabled via `--no-close-on-blur`).

## Requirements

- **Linux** (Tested on KDE Plasma 6 Wayland / CachyOS)
- **kdotool**: A window control utility for KDE Plasma Wayland.
  - Install it via cargo if it is not already installed:
    ```bash
    cargo install kdotool
    ```

## Build and Execution

To compile and execute the application:

1. **Build the binary**:
   ```bash
   cargo build --release
   ```
2. **Run the launcher**:
   ```bash
   cargo run --release
   ```

## Command Line Interface (CLI) Manual

Refer to the help manual page below (also available by running with `-h` or `--help`):

```
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

    -a, --apps
        Open the launcher in application mode to show and launch installed
        desktop applications rather than active window objects.

    -i, --icon-only
        Show only application icons in a grid format without names (only
        applicable in application launcher mode).

    --close-on-blur
        Close the launcher window automatically when it loses focus.

    --theme <THEME>
        Force a specific icon theme (default: automatically detected).

    --draw-border <x> <y> <w> <h> <id>
        Internal command used to spawn a temporary fading border overlay
        around a window to highlight its location on the screen.

OPERATION
    When launched, the application retrieves a list of all open windows using
    kdotool. It renders a frameless GUI window containing a search input.
    As you type, the list is filtered using a fuzzy matcher.
    
    Keyboard Navigation:
        - Up/Down Arrows: Move selection.
        - Enter: Activate selected window (or launch selected application).
        - Escape: Close launcher.
        - F5: Refresh list.
        - Ctrl+P: Toggle pin/unpin status for the selected application.

EXAMPLES
    applicationlauncher
        Launch the application launcher.

    applicationlauncher --close-on-blur
        Launch the application launcher with auto-close on focus loss enabled.

FILES
    $HOME/.config/applicationlauncher/config.toml
        Optional configuration file (reserved for future use).

    $HOME/.config/applicationlauncher/window_size.txt
        Stores the persisted width and height of the launcher window.

    $HOME/.config/applicationlauncher/pinned_apps.txt
        Stores absolute paths of pinned desktop applications.

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
