use eframe::egui;
use fuzzy_rank::metadata::{
    MetadataCandidate, MetadataQuery, SearchField, dedup_push_search_field,
};
use fuzzy_rank::ranking::{SearchRank, compare_search_results};
use serde::Deserialize;
use std::backtrace::Backtrace;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};
use zbus::interface;

const WINDOW_REMOVAL_CONFIRMATION_POLLS: usize = 2;
const KWIN_WINDOW_FEED_SERVICE: &str = "com.terrydaktal.ApplicationLauncher";
const KWIN_WINDOW_FEED_PATH: &str = "/WindowFeed";
const KWIN_WINDOW_FEED_SCRIPT_ID: &str = "applicationlauncher-window-feed";
const KWIN_WINDOW_FEED_METADATA: &str =
    include_str!("../kwin/applicationlauncher-window-feed/metadata.json");
const KWIN_WINDOW_FEED_MAIN_JS: &str =
    include_str!("../kwin/applicationlauncher-window-feed/contents/code/main.js");
const ATSPI_LOCATION_PROBE: &str = include_str!("atspi_location_probe.py");
const AUDIO_SINK_POLL_MS: u128 = 200;
const AUDIO_ACTIVITY_GRACE_MS: u128 = 350;
const PIPEWIRE_ACTIVE_US_THRESHOLD: f32 = 10.0;
const PIPEWIRE_ACTIVE_TOTAL_US_THRESHOLD: f32 = 20.0;
const AUDIO_IDLE_REPAINT_MS: u64 = 200;
const AUDIO_ACTIVE_REPAINT_MS: u64 = 80;
const WINDOW_FEED_EVENTS_PER_FRAME: usize = 512;
const WINDOW_SNAPSHOTS_PER_FRAME: usize = 4;
const AUDIO_UPDATES_PER_FRAME: usize = 32;
const UI_EVENTS_PER_FRAME: usize = 8;

#[derive(Clone, Debug)]
struct ProcessChainEntry {
    pid: i32,
    name: String,
    exe_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
struct PactlVolumeChannel {
    value_percent: String,
}

#[derive(Clone, Debug, Deserialize)]
struct PactlSinkInput {
    index: u32,
    #[serde(default)]
    corked: bool,
    #[serde(default)]
    mute: bool,
    #[serde(default)]
    volume: HashMap<String, PactlVolumeChannel>,
    #[serde(default)]
    properties: HashMap<String, String>,
}

#[derive(Clone, Debug)]
struct WindowInfo {
    id: String,
    title: String,
    class: String,
    desktop_file_name: Option<String>,
    minimized: Option<bool>,
    icon_path: Option<PathBuf>,
    active_process: Option<String>,
    exe_path: Option<PathBuf>,
    cwd_path: Option<PathBuf>,
    command_line: Option<String>,
    command_summary: Option<String>,
    geometry: Option<(i32, i32, i32, i32)>,
    process_chain: Vec<ProcessChainEntry>,
    pid: Option<i32>,
}

#[derive(Clone, Debug)]
struct AppInfo {
    name: String,
    exec: String,
    icon_path: Option<PathBuf>,
    comment: Option<String>,
    desktop_file_path: PathBuf,
    is_settings_module: bool,
}

#[derive(Clone, Debug)]
struct RankedAppMatch {
    app: AppInfo,
    rank: SearchRank,
    title_is_typo: bool,
    visible_match_priority: u8,
    is_pinned: bool,
    candidate_key: String,
    candidate_score: f64,
}

#[derive(Clone, Debug)]
struct RankedWindowMatch {
    window: WindowInfo,
    rank: SearchRank,
    title_is_typo: bool,
    visible_match_priority: u8,
    candidate_key: String,
    candidate_score: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LauncherMode {
    Windows,
    Apps,
}

enum LoadResult {
    AppsSuccess(Vec<AppInfo>),
    WindowsSuccess(Vec<WindowInfo>),
    Error(String),
}

enum UiEvent {
    FocusLauncher,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KWinWindowPayload {
    id: String,
    title: String,
    class: String,
    #[serde(default)]
    pid: i32,
    #[serde(default)]
    desktop_file_name: String,
    #[serde(default)]
    x: i32,
    #[serde(default)]
    y: i32,
    #[serde(default)]
    width: i32,
    #[serde(default)]
    height: i32,
    #[serde(default)]
    minimized: bool,
}

#[derive(Clone, Debug)]
enum WindowFeedEvent {
    Upsert(KWinWindowPayload),
    Remove(String),
}

#[derive(Clone, Debug)]
struct AudioCacheUpdate {
    sink_inputs: Vec<PactlSinkInput>,
    active_media_app_keys: HashSet<String>,
    observed_pipewire_node_ids: HashSet<u32>,
    active_pipewire_node_ids: HashSet<u32>,
    pipewire_activity_cache_valid: bool,
}

struct SnapshotWindowDetails {
    desktop_file_name: Option<String>,
    geometry: Option<(i32, i32, i32, i32)>,
    minimized: Option<bool>,
}

struct KWinWindowFeed {
    tx: Sender<WindowFeedEvent>,
    repaint_ctx: egui::Context,
}

#[interface(name = "com.terrydaktal.ApplicationLauncher.WindowFeed", spawn = false)]
impl KWinWindowFeed {
    #[zbus(name = "UpsertWindow")]
    fn upsert_window(&self, payload: &str) {
        if let Ok(window) = serde_json::from_str::<KWinWindowPayload>(payload) {
            let _ = self.tx.send(WindowFeedEvent::Upsert(window));
            self.repaint_ctx.request_repaint();
        }
    }

    #[zbus(name = "RemoveWindow")]
    fn remove_window(&self, id: &str) {
        let _ = self.tx.send(WindowFeedEvent::Remove(id.to_string()));
        self.repaint_ctx.request_repaint();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivePane {
    Windows,
    Apps,
}

struct App {
    mode: LauncherMode,
    windows: Vec<WindowInfo>,
    apps: Vec<AppInfo>,
    pinned_apps: Vec<PathBuf>,
    search_query: String,
    selected_index: usize,
    side_panel_selected_index: usize,
    active_pane: ActivePane,
    rendered_app_grid_columns: usize,
    rendered_side_panel_grid_columns: usize,
    rendered_window_row_centers: Vec<f32>,
    rendered_side_panel_item_centers: Vec<f32>,
    scroll_to_first_window_on_focus: bool,
    kdotool_path: Option<PathBuf>,
    error_message: Option<String>,
    start_time: Instant,
    search_focus_until: Option<Instant>,
    close_on_blur: bool,
    force_theme: Option<String>,
    loading: bool,
    receiver: Option<std::sync::mpsc::Receiver<LoadResult>>,
    background_apps_receiver: Option<Receiver<Vec<AppInfo>>>,
    background_window_enrichment_receiver: Option<Receiver<Vec<WindowInfo>>>,
    ui_event_rx: std::sync::mpsc::Receiver<UiEvent>,
    kwin_window_feed_setup_rx: Option<Receiver<Result<(), String>>>,
    width: f32,
    height: f32,
    icon_only: bool,
    show_settings_menu: bool,
    show_system_settings_modules: bool,
    win_icon_size: f32,
    win_padding: f32,
    win_row_height: f32,
    win_text_spacing: f32,
    win_line_height: f32,
    win_show_path: bool,
    win_title_size: f32,
    win_path_size: f32,
    app_icon_size: f32,
    app_icon_tile_size: f32,
    app_icon_show_name: bool,
    app_icon_name_size: f32,
    disable_ibeam: bool,
    process_chain_popup: Option<WindowInfo>,
    window_sender: Sender<Vec<WindowInfo>>,
    window_receiver: Receiver<Vec<WindowInfo>>,
    window_feed_receiver: Receiver<WindowFeedEvent>,
    audio_cache_receiver: Receiver<AudioCacheUpdate>,
    rapid_polling: std::sync::Arc<std::sync::atomic::AtomicBool>,
    last_selected_window_id: Option<String>,
    missing_window_counts: HashMap<String, usize>,
    use_kwin_window_feed: bool,
    window_polling_started: bool,
    cached_sink_inputs: Vec<PactlSinkInput>,
    active_media_app_keys: HashSet<String>,
    observed_pipewire_node_ids: HashSet<u32>,
    active_pipewire_node_ids: HashSet<u32>,
    pipewire_activity_cache_valid: bool,
    app_scroll_sensitivity: f32,
    win_scroll_sensitivity: f32,
    last_stale_prune: Option<Instant>,
    filtered_search_cache: Option<FilteredSearchCache>,
    apps_generation: u64,
    windows_generation: u64,
    pinned_apps_generation: u64,
}

#[derive(Clone, Debug)]
struct FilteredSearchResults {
    apps: Vec<(AppInfo, bool)>,
    windows: Vec<WindowInfo>,
    app_display_titles: Vec<String>,
    window_display_titles: Vec<String>,
    app_highlight_segments: Vec<Vec<(usize, usize, bool)>>,
    window_highlight_segments: Vec<Vec<(usize, usize, bool)>>,
    app_title_is_typos: Vec<bool>,
    window_title_is_typos: Vec<bool>,
}

#[derive(Clone, Debug)]
struct FilteredSearchCache {
    key: FilteredSearchCacheKey,
    results: FilteredSearchResults,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FilteredSearchCacheKey {
    mode: LauncherMode,
    query: String,
    show_system_settings_modules: bool,
    pinned_apps_generation: u64,
    apps_generation: u64,
    windows_generation: u64,
}

#[derive(Clone, Copy)]
struct LauncherSettings {
    show_system_settings_modules: bool,
    app_icon_mode: bool,
    win_icon_size: f32,
    win_padding: f32,
    win_row_height: f32,
    win_text_spacing: f32,
    win_line_height: f32,
    win_show_path: bool,
    win_title_size: f32,
    win_path_size: f32,
    app_icon_size: f32,
    app_icon_tile_size: f32,
    app_icon_show_name: bool,
    app_icon_name_size: f32,
    disable_ibeam: bool,
    app_scroll_sensitivity: f32,
    win_scroll_sensitivity: f32,
}

impl Default for LauncherSettings {
    fn default() -> Self {
        Self {
            show_system_settings_modules: true,
            app_icon_mode: false,
            win_icon_size: 32.0,
            win_padding: 6.0,
            win_row_height: 52.0,
            win_text_spacing: 2.0,
            win_line_height: 14.0,
            win_show_path: true,
            win_title_size: 13.0,
            win_path_size: 10.5,
            app_icon_size: 32.0,
            app_icon_tile_size: 68.0,
            app_icon_show_name: true,
            app_icon_name_size: 10.5,
            disable_ibeam: false,
            app_scroll_sensitivity: 1.0,
            win_scroll_sensitivity: 1.0,
        }
    }
}

fn filtered_search_cache_key(
    mode: LauncherMode,
    query: &str,
    show_system_settings_modules: bool,
    pinned_apps_generation: u64,
    apps_generation: u64,
    windows_generation: u64,
) -> FilteredSearchCacheKey {
    FilteredSearchCacheKey {
        mode,
        query: query.to_string(),
        show_system_settings_modules,
        pinned_apps_generation,
        apps_generation,
        windows_generation,
    }
}

fn print_help() {
    println!(
        r#"NAME
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

    applicationlauncher --no-close-on-blur
        Launch the application launcher without closing on focus loss.

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
    Terrydaktal <9lewis9@gmail.com>"#
    );
}

fn get_kdotool_path() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        let p = PathBuf::from(format!("{}/.cargo/bin/kdotool", home));
        if p.exists() {
            return p;
        }
    }
    // Fallback to searching in system PATH
    PathBuf::from("kdotool")
}

fn load_window_size() -> (f32, f32) {
    if let Ok(home) = std::env::var("HOME") {
        let path = PathBuf::from(format!(
            "{}/.config/applicationlauncher/window_size.txt",
            home
        ));
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(path) {
                let lines: Vec<&str> = content.lines().collect();
                if lines.len() >= 2 {
                    if let (Ok(w), Ok(h)) = (
                        lines[0].trim().parse::<f32>(),
                        lines[1].trim().parse::<f32>(),
                    ) {
                        let w = w.clamp(300.0, 1920.0);
                        let h = h.clamp(200.0, 1080.0);
                        return (w, h);
                    }
                }
            }
        }
    }
    (980.0, 560.0) // Default size
}

fn load_launcher_settings() -> LauncherSettings {
    let mut settings = LauncherSettings::default();

    if let Ok(home) = std::env::var("HOME") {
        let path = PathBuf::from(format!("{}/.config/applicationlauncher/settings.txt", home));
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(path) {
                for line in content.lines() {
                    let mut parts = line.splitn(2, '=');
                    let key = parts.next().unwrap_or("").trim();
                    let value = parts.next().unwrap_or("").trim();

                    match key {
                        "show_system_settings_modules" => {
                            settings.show_system_settings_modules = value
                                .parse::<bool>()
                                .unwrap_or(settings.show_system_settings_modules);
                        }
                        "app_icon_mode" => {
                            settings.app_icon_mode =
                                value.parse::<bool>().unwrap_or(settings.app_icon_mode);
                        }
                        "win_icon_size" => {
                            settings.win_icon_size = value
                                .parse::<f32>()
                                .map(|v| v.clamp(16.0, 64.0))
                                .unwrap_or(settings.win_icon_size);
                        }
                        "win_padding" => {
                            settings.win_padding = value
                                .parse::<f32>()
                                .map(|v| v.clamp(0.0, 24.0))
                                .unwrap_or(settings.win_padding);
                        }
                        "win_row_height" => {
                            settings.win_row_height = value
                                .parse::<f32>()
                                .map(|v| v.clamp(30.0, 100.0))
                                .unwrap_or(settings.win_row_height);
                        }
                        "win_text_spacing" => {
                            settings.win_text_spacing = value
                                .parse::<f32>()
                                .map(|v| v.clamp(0.0, 12.0))
                                .unwrap_or(settings.win_text_spacing);
                        }
                        "win_line_height" => {
                            settings.win_line_height = value
                                .parse::<f32>()
                                .map(|v| v.clamp(8.0, 30.0))
                                .unwrap_or(settings.win_line_height);
                        }
                        "win_show_path" => {
                            settings.win_show_path =
                                value.parse::<bool>().unwrap_or(settings.win_show_path);
                        }
                        "win_title_size" => {
                            settings.win_title_size = value
                                .parse::<f32>()
                                .map(|v| v.clamp(8.0, 24.0))
                                .unwrap_or(settings.win_title_size);
                        }
                        "win_path_size" => {
                            settings.win_path_size = value
                                .parse::<f32>()
                                .map(|v| v.clamp(8.0, 20.0))
                                .unwrap_or(settings.win_path_size);
                        }
                        "app_icon_size" => {
                            settings.app_icon_size = value
                                .parse::<f32>()
                                .map(|v| v.clamp(16.0, 64.0))
                                .unwrap_or(settings.app_icon_size);
                        }
                        "app_icon_tile_size" => {
                            settings.app_icon_tile_size = value
                                .parse::<f32>()
                                .map(|v| v.clamp(48.0, 128.0))
                                .unwrap_or(settings.app_icon_tile_size);
                        }
                        "app_icon_show_name" => {
                            settings.app_icon_show_name =
                                value.parse::<bool>().unwrap_or(settings.app_icon_show_name);
                        }
                        "app_icon_name_size" => {
                            settings.app_icon_name_size = value
                                .parse::<f32>()
                                .map(|v| v.clamp(8.0, 20.0))
                                .unwrap_or(settings.app_icon_name_size);
                        }
                        "disable_ibeam" => {
                            settings.disable_ibeam =
                                value.parse::<bool>().unwrap_or(settings.disable_ibeam);
                        }
                        "app_scroll_sensitivity" => {
                            settings.app_scroll_sensitivity = value
                                .parse::<f32>()
                                .map(|v| v.clamp(0.1, 10.0))
                                .unwrap_or(settings.app_scroll_sensitivity);
                        }
                        "win_scroll_sensitivity" => {
                            settings.win_scroll_sensitivity = value
                                .parse::<f32>()
                                .map(|v| v.clamp(0.1, 10.0))
                                .unwrap_or(settings.win_scroll_sensitivity);
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    settings
}

fn save_launcher_settings(settings: LauncherSettings) {
    if let Ok(home) = std::env::var("HOME") {
        let dir = PathBuf::from(format!("{}/.config/applicationlauncher", home));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("settings.txt");
        let content = format!(
            "show_system_settings_modules={}\napp_icon_mode={}\nwin_icon_size={:.1}\nwin_padding={:.1}\nwin_row_height={:.1}\nwin_text_spacing={:.1}\nwin_line_height={:.1}\nwin_show_path={}\nwin_title_size={:.1}\nwin_path_size={:.1}\napp_icon_size={:.1}\napp_icon_tile_size={:.1}\napp_icon_show_name={}\napp_icon_name_size={:.1}\ndisable_ibeam={}\napp_scroll_sensitivity={:.2}\nwin_scroll_sensitivity={:.2}\n",
            settings.show_system_settings_modules,
            settings.app_icon_mode,
            settings.win_icon_size,
            settings.win_padding,
            settings.win_row_height,
            settings.win_text_spacing,
            settings.win_line_height,
            settings.win_show_path,
            settings.win_title_size,
            settings.win_path_size,
            settings.app_icon_size,
            settings.app_icon_tile_size,
            settings.app_icon_show_name,
            settings.app_icon_name_size,
            settings.disable_ibeam,
            settings.app_scroll_sensitivity,
            settings.win_scroll_sensitivity
        );
        let _ = std::fs::write(path, content);
    }
}

fn parse_icon_from_desktop(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("Icon=") {
            let val = line.strip_prefix("Icon=")?;
            return Some(val.trim().to_string());
        }
    }
    None
}

fn lookup_theme_icon_exact(theme: &str, name: &str) -> Option<PathBuf> {
    let themes_to_check = if theme == "breeze-dark" {
        vec!["breeze-dark", "breeze", "hicolor"]
    } else if theme == "breeze" {
        vec!["breeze", "breeze-dark", "hicolor"]
    } else {
        vec![theme, "breeze-dark", "breeze", "hicolor"]
    };

    for t in themes_to_check {
        if let Some(path) = freedesktop_icons::lookup(name)
            .with_theme(t)
            .with_size(48)
            .find()
        {
            return Some(path);
        }
    }

    let pixmap = PathBuf::from(format!("/usr/share/pixmaps/{}.png", name));
    pixmap.exists().then_some(pixmap)
}

fn find_icon(theme: &str, class: &str) -> Option<PathBuf> {
    if class.is_empty() {
        return None;
    }

    let lower = class.to_lowercase();
    let mut names = vec![lower.clone(), class.to_string()];

    // Handle reverse-DNS formats (e.g., org.xfce.mousepad -> mousepad)
    if lower.contains('.') {
        if let Some(last) = lower.split('.').last() {
            names.push(last.to_string());
        }
    }

    // Try finding the .desktop file to see if it has a hardcoded icon path or an override name
    let mut app_dirs = vec![PathBuf::from("/usr/share/applications")];
    if let Ok(home) = std::env::var("HOME") {
        app_dirs.push(PathBuf::from(format!("{}/.local/share/applications", home)));
    }

    let mut overrides = Vec::new();
    for dir in &app_dirs {
        for name in &names {
            let desktop_path = dir.join(format!("{}.desktop", name));
            if desktop_path.exists() {
                if let Some(icon_val) = parse_icon_from_desktop(&desktop_path) {
                    let p = PathBuf::from(&icon_val);
                    if p.is_absolute() && p.exists() {
                        return Some(p);
                    }
                    if !names.contains(&icon_val) && !overrides.contains(&icon_val) {
                        overrides.push(icon_val);
                    }
                }
            }
        }
    }

    // Insert overrides at the front of the names vector (highest specificity)
    for ovr in overrides.into_iter().rev() {
        names.insert(0, ovr);
    }

    // Keyword fallbacks for generic application categories
    if lower.contains("terminal") {
        names.push("utilities-terminal".to_string());
        names.push("terminal".to_string());
    }
    if lower.contains("mousepad") || lower.contains("editor") || lower.contains("text") {
        names.push("accessories-text-editor".to_string());
        names.push("mousepad".to_string());
    }
    if lower.contains("file-manager")
        || lower.contains("pcmanfm")
        || lower.contains("thunar")
        || lower.contains("dolphin")
    {
        names.push("system-file-manager".to_string());
        names.push("folder-open".to_string());
    }
    if lower.contains("web") || lower.contains("browser") || lower.contains("firefox") {
        names.push("web-browser".to_string());
    }
    if lower.contains("tor browser")
        || lower.contains("tor-browser")
        || lower.contains("torbrowser")
    {
        names.push("tor-browser".to_string());
        names.push("tor-browser-alpha".to_string());
        names.push("torbrowser".to_string());
        names.push("firefox".to_string());
        names.push("web-browser".to_string());
    }
    if lower.contains("copyq") {
        names.push("copyq".to_string());
        names.push("edit-paste".to_string());
    }

    // Try finding in specified theme and standard fallbacks
    let themes_to_check = if theme == "breeze-dark" {
        vec!["breeze-dark", "breeze", "hicolor"]
    } else if theme == "breeze" {
        vec!["breeze", "breeze-dark", "hicolor"]
    } else {
        vec![theme, "breeze-dark", "breeze", "hicolor"]
    };

    for t in themes_to_check {
        for name in &names {
            if let Some(path) = freedesktop_icons::lookup(name)
                .with_theme(t)
                .with_size(48)
                .find()
            {
                return Some(path);
            }
        }
    }

    // Look in legacy /usr/share/pixmaps as a final fallback
    for name in &names {
        let pixmap = PathBuf::from(format!("/usr/share/pixmaps/{}.png", name));
        if pixmap.exists() {
            return Some(pixmap);
        }
    }
    None
}

fn app_search_rank(query: &MetadataQuery, app: &AppInfo) -> Option<SearchRank> {
    let cleaned_exec = clean_exec_cmd(&app.exec);
    let exec_basename = command_basename(&app.exec);
    let desktop_stem = app
        .desktop_file_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.to_string());

    let mut owned_values = Vec::new();
    owned_values.push((0, normalize_metadata_search_value(&app.name)));
    if let Some(value) = exec_basename {
        owned_values.push((1, normalize_metadata_search_value(&value)));
    }
    if let Some(value) = desktop_stem {
        owned_values.push((2, normalize_metadata_search_value(&value)));
    }
    if let Some(value) = app.comment.clone() {
        owned_values.push((3, normalize_metadata_search_value(&value)));
    }
    owned_values.push((4, normalize_metadata_search_value(&cleaned_exec)));

    let mut fields = Vec::new();
    for (priority, value) in &owned_values {
        dedup_push_search_field(&mut fields, *priority, Some(value.as_str()));
    }

    query.search_rank(MetadataCandidate {
        key: "",
        fields: &fields,
        score: 0.0,
    })
}

fn window_search_rank(query: &MetadataQuery, win: &WindowInfo) -> Option<SearchRank> {
    let app_key = window_application_key(win);
    let exe_basename = win
        .exe_path
        .as_ref()
        .and_then(|path| path.file_name().and_then(|name| name.to_str()))
        .map(|name| name.to_string());
    let cwd_display = win.cwd_path.as_ref().map(|path| display_path(path));

    let mut owned_values = Vec::new();
    owned_values.push((0, normalize_metadata_search_value(&win.title)));
    owned_values.push((1, normalize_metadata_search_value(&app_key)));
    if !win.class.eq_ignore_ascii_case(&app_key) {
        owned_values.push((2, normalize_metadata_search_value(&win.class)));
    }
    if let Some(value) = win.active_process.clone() {
        owned_values.push((3, normalize_metadata_search_value(&value)));
    }
    if let Some(value) = win.command_summary.clone() {
        owned_values.push((4, normalize_metadata_search_value(&value)));
    }
    if let Some(value) = win.command_line.clone() {
        owned_values.push((5, normalize_metadata_search_value(&value)));
    }
    if let Some(value) = exe_basename {
        owned_values.push((6, normalize_metadata_search_value(&value)));
    }
    if let Some(value) = cwd_display {
        owned_values.push((7, normalize_metadata_search_value(&value)));
    }

    let mut fields = Vec::new();
    for (priority, value) in &owned_values {
        dedup_push_search_field(&mut fields, *priority, Some(value.as_str()));
    }

    query.search_rank(MetadataCandidate {
        key: "",
        fields: &fields,
        score: 0.0,
    })
}

fn sort_ranked_matches_with_visible<T, FVisible, FKey, FScore, FRank>(
    items: &mut [T],
    visible_priority_fn: FVisible,
    key_fn: FKey,
    score_fn: FScore,
    rank_fn: FRank,
) where
    FVisible: Fn(&T) -> u8,
    FKey: Fn(&T) -> &str,
    FScore: Fn(&T) -> f64,
    FRank: Fn(&T) -> &SearchRank,
{
    items.sort_unstable_by(|left, right| {
        visible_priority_fn(left)
            .cmp(&visible_priority_fn(right))
            .then_with(|| {
                compare_search_results(
                    rank_fn(left),
                    score_fn(left),
                    key_fn(left),
                    rank_fn(right),
                    score_fn(right),
                    key_fn(right),
                )
            })
    });
}

fn pinned_app_position(pinned_apps: &[PathBuf], app: &AppInfo) -> usize {
    pinned_apps
        .iter()
        .position(|path| path == &app.desktop_file_path)
        .unwrap_or(usize::MAX)
}

fn clean_exec_cmd(exec: &str) -> String {
    let mut cleaned = exec.to_string();
    for placeholder in &[
        "%f", "%F", "%u", "%U", "%d", "%D", "%n", "%N", "%i", "%c", "%k", "%v",
    ] {
        cleaned = cleaned.replace(placeholder, "");
    }
    cleaned.trim().to_string()
}

fn launch_app(exec: &str) {
    let cmd_str = clean_exec_cmd(exec);
    std::thread::spawn(move || {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&cmd_str);

        // Clean Python environment variables to prevent version mismatch crashes in launched apps
        cmd.env_remove("PYTHONPATH");
        cmd.env_remove("PYTHONHOME");

        if let Ok(venv) = std::env::var("VIRTUAL_ENV") {
            cmd.env_remove("VIRTUAL_ENV");
            if let Ok(path_val) = std::env::var("PATH") {
                let venv_bin = std::path::PathBuf::from(venv).join("bin");
                let new_paths: Vec<_> = std::env::split_paths(&path_val)
                    .filter(|p| p != &venv_bin)
                    .collect();
                if let Ok(joined) = std::env::join_paths(new_paths) {
                    cmd.env("PATH", joined);
                }
            }
        }

        let _ = cmd.spawn();
    });
}

fn launch_terminal_cd(target: &str) {
    let target = target.trim();
    if target.is_empty() {
        return;
    }

    launch_fish_terminal(Some(target.to_string()), None, None);
}

fn launch_terminal_command(command: &str) {
    let command = command.trim();
    if command.is_empty() {
        return;
    }

    let command = command.to_string();
    std::thread::spawn(move || {
        let mut cmd = Command::new("xfce4-terminal");
        cmd.arg("--command")
            .arg(r#"fish -ic 'eval "$APPLICATIONLAUNCHER_TERMINAL_COMMAND"; exec fish'"#)
            .env("APPLICATIONLAUNCHER_TERMINAL_COMMAND", command);
        scrub_command_env(&mut cmd);
        let _ = cmd.spawn();
    });
}

fn launch_fish_terminal(
    cd_target: Option<String>,
    command_after_cd: Option<&'static str>,
    terminal_title: Option<String>,
) {
    std::thread::spawn(move || {
        let title_command = if terminal_title.is_some() {
            r#"printf '\e]0;%s\a' "$APPLICATIONLAUNCHER_TERMINAL_TITLE"; "#
        } else {
            ""
        };
        let fish_command = match command_after_cd {
            Some(command) => format!(
                r#"{title_command}if test -n "$APPLICATIONLAUNCHER_CD_TARGET"; cd "$APPLICATIONLAUNCHER_CD_TARGET"; end; {command}; exec fish"#
            ),
            None => {
                format!(
                    r#"{title_command}if test -n "$APPLICATIONLAUNCHER_CD_TARGET"; cd "$APPLICATIONLAUNCHER_CD_TARGET"; end; exec fish"#
                )
            }
        };

        let mut cmd = Command::new("xfce4-terminal");
        if let Some(title) = terminal_title {
            cmd.arg("--title")
                .arg(&title)
                .env("APPLICATIONLAUNCHER_TERMINAL_TITLE", title);
        } else {
            cmd.env("APPLICATIONLAUNCHER_TERMINAL_TITLE", "");
        }
        cmd.arg("--command")
            .arg(format!("fish -ic '{}'", fish_command));

        if let Some(target) = cd_target {
            cmd.env("APPLICATIONLAUNCHER_CD_TARGET", target);
        } else {
            cmd.env("APPLICATIONLAUNCHER_CD_TARGET", "");
        }

        cmd.env_remove("PYTHONPATH");
        cmd.env_remove("PYTHONHOME");
        cmd.env_remove("VIRTUAL_ENV");
        cmd.env_remove("UV_ACTIVE");

        let _ = cmd.spawn();
    });
}

fn launch_terminal_window() {
    std::thread::spawn(move || {
        let mut cmd = Command::new("xfce4-terminal");
        cmd.env_remove("PYTHONPATH");
        cmd.env_remove("PYTHONHOME");
        cmd.env_remove("VIRTUAL_ENV");
        cmd.env_remove("UV_ACTIVE");
        let _ = cmd.spawn();
    });
}

fn scrub_command_env(command: &mut Command) {
    command.env_remove("PYTHONPATH");
    command.env_remove("PYTHONHOME");
    command.env_remove("VIRTUAL_ENV");
    command.env_remove("UV_ACTIVE");
}

fn clone_terminal_command_for_window(win: &WindowInfo) -> Option<&'static str> {
    let mut values = vec![win.title.as_str(), win.class.as_str()];
    if let Some(process) = win.active_process.as_deref() {
        values.push(process);
    }
    for entry in &win.process_chain {
        values.push(&entry.name);
    }

    let matches = |needle: &str| {
        values
            .iter()
            .any(|value| normalize_app_match_key(value).contains(needle))
    };

    if matches("codex") {
        Some("codex resume --last")
    } else if matches("agy") {
        Some("agy -c")
    } else if matches("htop") {
        Some("htop")
    } else {
        None
    }
}

fn source_terminal_title_for_clone(win: &WindowInfo) -> String {
    let Some(proc_name) = win.active_process.as_deref() else {
        return win.title.clone();
    };
    let proc_key = normalize_app_match_key(proc_name);
    if proc_key.is_empty() {
        return win.title.clone();
    }

    for sep in [" - ", " — ", " – ", " : ", " | "] {
        let parts: Vec<&str> = win.title.split(sep).collect();
        if parts.len() >= 3 && normalize_app_match_key(parts[1]) == proc_key {
            let mut rebuilt = Vec::with_capacity(parts.len() - 1);
            rebuilt.push(parts[0].trim());
            rebuilt.extend(parts.iter().skip(2).map(|part| part.trim()));
            return rebuilt.join(sep);
        }
    }

    win.title.clone()
}

fn is_chrome_like_window(win: &WindowInfo) -> bool {
    let mut values = vec![win.class.as_str()];
    if let Some(process) = win.active_process.as_deref() {
        values.push(process);
    }
    if let Some(path) = &win.exe_path {
        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            values.push(name);
        }
    }

    values.iter().any(|value| {
        matches!(
            normalize_app_match_key(value).as_str(),
            "googlechrome" | "chrome" | "chromium" | "chromiumbrowser"
        )
    })
}

fn is_pcmanfm_window(win: &WindowInfo) -> bool {
    let mut values = vec![win.class.as_str(), win.title.as_str()];
    if let Some(process) = win.active_process.as_deref() {
        values.push(process);
    }
    if let Some(path) = &win.exe_path {
        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            values.push(name);
        }
    }

    values
        .iter()
        .any(|value| normalize_app_match_key(value).contains("pcmanfm"))
}

fn is_pcmanfm_class(class_lower: &str) -> bool {
    normalize_app_match_key(class_lower).contains("pcmanfm")
}

fn is_dolphin_window(win: &WindowInfo) -> bool {
    let mut values = vec![win.class.as_str(), win.title.as_str()];
    if let Some(process) = win.active_process.as_deref() {
        values.push(process);
    }
    if let Some(path) = &win.exe_path {
        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            values.push(name);
        }
    }

    values
        .iter()
        .any(|value| normalize_app_match_key(value).contains("dolphin"))
}

fn extract_url_from_text(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|token| token.starts_with("http://") || token.starts_with("https://"))
        .map(|token| {
            token
                .trim_matches(|c: char| matches!(c, '"' | '\'' | ')' | ']' | '}' | ',' | ';'))
                .to_string()
        })
        .filter(|url| !url.is_empty())
}

fn clone_chrome_window(win: &WindowInfo) -> bool {
    let Some(url) = extract_url_from_text(&win.title) else {
        return false;
    };

    let mut command = if let Some(exe) = &win.exe_path {
        Command::new(exe)
    } else {
        Command::new("google-chrome")
    };
    command.arg("--new-window").arg(url);
    command.env_remove("PYTHONPATH");
    command.env_remove("PYTHONHOME");
    command.env_remove("VIRTUAL_ENV");
    command.env_remove("UV_ACTIVE");
    command.spawn().is_ok()
}

fn expand_display_path_candidate(value: &str) -> Option<PathBuf> {
    let trimmed = value
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '\'' | ')' | ']' | '}' | ',' | ';'));
    if trimmed.is_empty() {
        return None;
    }

    if let Some(rest) = trimmed.strip_prefix("~/") {
        let home = std::env::var("HOME").ok()?;
        return Some(PathBuf::from(home).join(rest));
    }

    if trimmed == "~" {
        return std::env::var("HOME").ok().map(PathBuf::from);
    }

    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        return Some(path);
    }

    if trimmed.contains('/') {
        let home = std::env::var("HOME").ok()?;
        return Some(PathBuf::from(home).join(trimmed));
    }

    None
}

fn normalize_file_manager_target(value: &str) -> Option<String> {
    let trimmed = value
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '\'' | ')' | ']' | '}' | ',' | ';'));
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.contains(['\n', '\r', '\t']) || trimmed.contains("No such file or directory") {
        return None;
    }
    if trimmed.contains("://") {
        return Some(trimmed.to_string());
    }
    let path = expand_display_path_candidate(trimmed)?;
    if path.is_dir() {
        Some(path.to_string_lossy().to_string())
    } else {
        None
    }
}

fn accessible_location_for_window(win: &WindowInfo) -> Option<String> {
    let mut command = Command::new("python3");
    command.arg("-c").arg(ATSPI_LOCATION_PROBE);
    if let Some(pid) = win.pid {
        command.arg("--pid").arg(pid.to_string());
    }
    command.arg("--title").arg(&win.title);
    command.arg("--class").arg(&win.class);
    scrub_command_env(&mut command);
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout);
    let line = value.lines().map(str::trim).find(|line| !line.is_empty())?;
    normalize_file_manager_target(line)
}

fn pcmanfm_path_from_title(title: &str) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    candidates.push(title.trim());

    for separator in [" — ", " – ", " - ", " : ", " | "] {
        candidates.extend(title.split(separator).map(str::trim));
    }

    candidates
        .into_iter()
        .find_map(expand_display_path_candidate)
        .filter(|path| path.is_dir())
}

fn pcmanfm_location_hint_from_title(title: &str) -> Option<String> {
    for part in title
        .split(['—', '–'])
        .flat_map(|part| part.split(" - "))
        .map(str::trim)
    {
        if part.is_empty()
            || part.contains(['\n', '\r', '\t'])
            || part.contains("No such file or directory")
            || normalize_app_match_key(part).contains("pcmanfm")
        {
            continue;
        }
        if expand_display_path_candidate(part).is_some() {
            continue;
        }
        if part.contains('/')
            || part.starts_with('.')
            || part.starts_with('(')
            || part.contains("://")
        {
            continue;
        }
        return Some(part.to_string());
    }

    None
}

fn clone_pcmanfm_with_fish_cd(target_hint: String) {
    std::thread::spawn(move || {
        let fallback_target = if normalize_app_match_key(&target_hint) == "trash" {
            "trash:///".to_string()
        } else {
            target_hint.clone()
        };
        let mut cmd = Command::new("fish");
        cmd.arg("-ic")
            .arg(
                r#"if cd "$APPLICATIONLAUNCHER_PCMANFM_TARGET"; pcmanfm --new-win "$PWD"; else; pcmanfm --new-win "$APPLICATIONLAUNCHER_PCMANFM_FALLBACK"; end"#,
            )
            .env("APPLICATIONLAUNCHER_PCMANFM_TARGET", target_hint)
            .env("APPLICATIONLAUNCHER_PCMANFM_FALLBACK", fallback_target);
        scrub_command_env(&mut cmd);
        let _ = cmd.spawn();
    });
}

fn launch_pcmanfm_target(exe_path: Option<PathBuf>, target: &str) -> bool {
    let mut command = if let Some(exe) = exe_path {
        Command::new(exe)
    } else {
        Command::new("pcmanfm")
    };
    command.arg("--new-win").arg(target);
    scrub_command_env(&mut command);
    command.spawn().is_ok()
}

fn clone_pcmanfm_window(win: &WindowInfo) -> bool {
    let win = win.clone();
    std::thread::spawn(move || {
        if let Some(target) = accessible_location_for_window(&win) {
            let _ = launch_pcmanfm_target(win.exe_path.clone(), &target);
            return;
        }

        if let Some(target) = pcmanfm_path_from_title(&win.title) {
            let _ = launch_pcmanfm_target(win.exe_path.clone(), &target.to_string_lossy());
            return;
        }

        if let Some(target_hint) = pcmanfm_location_hint_from_title(&win.title) {
            clone_pcmanfm_with_fish_cd(target_hint);
            return;
        }

        let fallback = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
        let _ = launch_pcmanfm_target(win.exe_path.clone(), &fallback);
    });
    true
}

fn launch_dolphin_target(exe_path: Option<PathBuf>, target: Option<&str>) -> bool {
    let mut command = if let Some(exe) = exe_path {
        Command::new(exe)
    } else {
        Command::new("dolphin")
    };
    command.arg("--new-window");
    if let Some(target) = target {
        command.arg(target);
    }
    scrub_command_env(&mut command);
    command.spawn().is_ok()
}

fn is_dolphin_app(app: &AppInfo) -> bool {
    let mut values = vec![app.name.as_str(), app.exec.as_str()];
    if let Some(stem) = app
        .desktop_file_path
        .file_stem()
        .and_then(|stem| stem.to_str())
    {
        values.push(stem);
    }

    values
        .iter()
        .any(|value| normalize_app_match_key(value).contains("dolphin"))
}

fn push_unique_metadata_part(
    parts: &mut Vec<String>,
    seen: &mut HashSet<String>,
    value: Option<String>,
) {
    let Some(value) = value.map(|value| value.trim().to_string()) else {
        return;
    };
    if value.is_empty() {
        return;
    }
    let key = normalize_metadata_search_value(&value);
    if key.is_empty() || !seen.insert(key) {
        return;
    }
    parts.push(value);
}

fn app_search_metadata_suffix(app: &AppInfo) -> String {
    let cleaned_exec = clean_exec_cmd(&app.exec);
    let exec_basename = command_basename(&app.exec);
    let desktop_stem = app
        .desktop_file_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.to_string());
    let mut parts = Vec::new();
    let mut seen = HashSet::new();
    seen.insert(normalize_metadata_search_value(&app.name));
    push_unique_metadata_part(&mut parts, &mut seen, exec_basename);
    push_unique_metadata_part(&mut parts, &mut seen, desktop_stem);
    push_unique_metadata_part(&mut parts, &mut seen, app.comment.clone());
    push_unique_metadata_part(&mut parts, &mut seen, Some(cleaned_exec));
    parts.join(" | ")
}

fn window_search_metadata_suffix(win: &WindowInfo) -> String {
    let app_key = window_application_key(win);
    let exe_basename = win
        .exe_path
        .as_ref()
        .and_then(|path| path.file_name().and_then(|name| name.to_str()))
        .map(|name| name.to_string());
    let cwd_display = win.cwd_path.as_ref().map(|path| display_path(path));

    let mut parts = Vec::new();
    let mut seen = HashSet::new();
    seen.insert(normalize_metadata_search_value(&win.title));
    push_unique_metadata_part(&mut parts, &mut seen, Some(app_key.clone()));
    if !win.class.eq_ignore_ascii_case(&app_key) {
        push_unique_metadata_part(&mut parts, &mut seen, Some(win.class.clone()));
    }
    push_unique_metadata_part(&mut parts, &mut seen, win.active_process.clone());
    push_unique_metadata_part(&mut parts, &mut seen, win.command_summary.clone());
    push_unique_metadata_part(&mut parts, &mut seen, win.command_line.clone());
    push_unique_metadata_part(&mut parts, &mut seen, exe_basename);
    push_unique_metadata_part(&mut parts, &mut seen, cwd_display);
    parts.join(" | ")
}

fn full_search_visible_app_title(app: &AppInfo) -> String {
    let suffix = app_search_metadata_suffix(app);
    if suffix.is_empty() {
        app.name.clone()
    } else {
        format!("{} | {}", app.name, suffix)
    }
}

fn full_search_visible_window_title(win: &WindowInfo) -> String {
    let suffix = window_search_metadata_suffix(win);
    if suffix.is_empty() {
        win.title.clone()
    } else {
        format!("{} | {}", win.title, suffix)
    }
}

fn search_visible_app_title(app: &AppInfo, query: &str) -> String {
    if query.trim().is_empty() {
        return app.name.clone();
    }
    let full_text = full_search_visible_app_title(app);
    let typo_match = visible_title_has_typo_match(&full_text, query);
    focus_text_around_match(&full_text, query, typo_match, 110)
}

fn search_visible_window_title(win: &WindowInfo, query: &str) -> String {
    if query.trim().is_empty() {
        return win.title.clone();
    }
    let full_text = full_search_visible_window_title(win);
    let typo_match = visible_title_has_typo_match(&full_text, query);
    focus_text_around_match(&full_text, query, typo_match, 120)
}

fn launch_dolphin_app() -> bool {
    let target = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
    launch_dolphin_target(None, Some(&target))
}

fn clone_dolphin_window(win: &WindowInfo) -> bool {
    let win = win.clone();
    std::thread::spawn(move || {
        if let Some(target) = accessible_location_for_window(&win) {
            let _ = launch_dolphin_target(win.exe_path.clone(), Some(&target));
            return;
        }

        if let Some(target) = pcmanfm_path_from_title(&win.title) {
            let target = target.to_string_lossy().to_string();
            let _ = launch_dolphin_target(win.exe_path.clone(), Some(&target));
            return;
        }

        let fallback = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
        let _ = launch_dolphin_target(win.exe_path.clone(), Some(&fallback));
    });
    true
}

fn launch_desktop_entry(desktop_file_path: &Path) -> bool {
    let Some(desktop_id) = desktop_file_path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };

    let desktop_id = desktop_id.to_string();
    std::thread::spawn(move || {
        let mut cmd = Command::new("gtk-launch");
        cmd.arg(&desktop_id);
        cmd.env_remove("PYTHONPATH");
        cmd.env_remove("VIRTUAL_ENV");
        cmd.env_remove("UV_ACTIVE");
        let _ = cmd.spawn();
    });
    true
}

fn normalize_app_match_key(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

fn push_normalized_key_variants(keys: &mut HashSet<String>, value: &str) {
    let normalized = normalize_app_match_key(value);
    if !normalized.is_empty() {
        keys.insert(normalized);
    }

    for token in value
        .split(|c: char| !c.is_ascii_alphanumeric())
        .map(normalize_app_match_key)
        .filter(|token| !token.is_empty())
    {
        keys.insert(token);
    }
}

fn window_application_key(win: &WindowInfo) -> String {
    let class = win.class.trim().to_lowercase();
    if !class.is_empty() {
        if let Some(last_segment) = class.rsplit('.').next() {
            if !last_segment.is_empty() {
                return last_segment.to_string();
            }
        }
        return class;
    }

    if let Some(path) = &win.exe_path {
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            return name.to_lowercase();
        }
    }

    if let Some(proc_name) = &win.active_process {
        return proc_name.to_lowercase();
    }

    String::new()
}

fn duplicate_window_title_key(win: &WindowInfo) -> Option<String> {
    let mut title = win.title.trim();
    if title.is_empty() {
        return None;
    }

    for separator in [" — ", " – "] {
        if let Some((left, right)) = title.rsplit_once(separator) {
            let suffix_key = normalize_app_match_key(right);
            let app_key = normalize_app_match_key(&window_application_key(win));
            let class_key = normalize_app_match_key(&win.class);
            if !suffix_key.is_empty()
                && (suffix_key == app_key
                    || suffix_key == class_key
                    || class_key.ends_with(&suffix_key))
            {
                title = left.trim();
                break;
            }
        }
    }

    (!title.is_empty()).then(|| title.to_string())
}

fn duplicate_window_group_key(win: &WindowInfo) -> Option<(String, String)> {
    let title = duplicate_window_title_key(win)?;
    let app_key = normalize_app_match_key(&window_application_key(win));
    (!app_key.is_empty()).then_some((app_key, title))
}

fn is_braille_spinner_char(ch: char) -> bool {
    ('\u{2800}'..='\u{28ff}').contains(&ch)
}

fn normalize_window_sort_title(title: &str) -> String {
    let without_spinners: String = title
        .chars()
        .filter(|ch| !is_braille_spinner_char(*ch))
        .collect();

    if without_spinners.contains(" - ") {
        return without_spinners
            .split(" - ")
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" - ")
            .to_lowercase();
    }

    without_spinners
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn window_sort_title_key(win: &WindowInfo) -> String {
    normalize_window_sort_title(
        &duplicate_window_title_key(win).unwrap_or_else(|| win.title.trim().to_string()),
    )
}

fn command_basename(exec: &str) -> Option<String> {
    let cleaned = clean_exec_cmd(exec);
    let command = cleaned.split_whitespace().next()?;
    let name = Path::new(command).file_name()?.to_str()?;
    Some(name.to_string())
}

fn is_terminal_app_name(value: &str) -> bool {
    let lower = value.to_lowercase();
    lower.contains("terminal")
        || lower.contains("konsole")
        || lower.contains("kitty")
        || lower.contains("alacritty")
        || lower.contains("wezterm")
}

fn is_terminal_icon_name(value: &str) -> bool {
    let lower = value.to_lowercase();
    lower == "terminal"
        || lower == "utilities-terminal"
        || lower.ends_with("-terminal")
        || lower.contains("terminal-symbolic")
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }

    let keep = max_chars.saturating_sub(3);
    let mut truncated: String = text.chars().take(keep).collect();
    truncated.push_str("...");
    truncated
}

fn focus_text_around_match(text: &str, query: &str, typo_match: bool, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }

    let ranges = if typo_match {
        typo_title_match_ranges(text, query)
    } else {
        title_match_ranges(text, query)
    };
    let Some((match_start_byte, match_end_byte)) = ranges.first().copied() else {
        return truncate_chars(text, max_chars);
    };

    let match_start_char = text[..match_start_byte].chars().count();
    let match_end_char = text[..match_end_byte].chars().count();
    let match_len = match_end_char.saturating_sub(match_start_char).max(1);
    let available_context = max_chars.saturating_sub(match_len);
    let left_context = available_context.min(24);

    let mut start_char = match_start_char.saturating_sub(left_context);
    let mut end_char = (start_char + max_chars).min(char_count);
    if end_char.saturating_sub(start_char) < max_chars {
        start_char = end_char.saturating_sub(max_chars);
    }
    if match_end_char > end_char {
        end_char = match_end_char.min(char_count);
        start_char = end_char.saturating_sub(max_chars);
    }

    let mut result = String::new();
    if start_char > 0 {
        result.push_str("...");
    }
    result.extend(
        text.chars()
            .skip(start_char)
            .take(end_char.saturating_sub(start_char)),
    );
    if end_char < char_count {
        result.push_str("...");
    }
    result
}

fn effective_list_row_height(
    configured_height: f32,
    icon_height: f32,
    vertical_padding: f32,
    line_height: f32,
    text_spacing: f32,
    show_path: bool,
) -> f32 {
    let text_height = if show_path {
        line_height + text_spacing + line_height * 0.8
    } else {
        line_height
    };

    configured_height
        .max(icon_height + vertical_padding * 2.0)
        .max(text_height + vertical_padding * 2.0)
}

fn display_path(path: &Path) -> String {
    if let Ok(home) = std::env::var("HOME") {
        let home_path = Path::new(&home);
        if let Ok(stripped) = path.strip_prefix(home_path) {
            if stripped.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", stripped.to_string_lossy());
        }
    }
    path.to_string_lossy().to_string()
}

fn normalize_metadata_search_value(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    for ch in value.chars() {
        let mapped = match ch {
            '—' | '–' | '−' => '-',
            '•' | '·' | '●' | '▪' | '◦' | '‣' => ' ',
            c if c.is_ascii() => c,
            _ => ' ',
        };
        normalized.push(mapped);
    }

    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn read_proc_cmdline(pid: i32) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("/proc/{}/cmdline", pid)).ok()?;
    let args = raw
        .split(|byte| *byte == 0)
        .filter_map(|part| {
            if part.is_empty() {
                return None;
            }
            std::str::from_utf8(part)
                .ok()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_string())
        })
        .collect::<Vec<_>>();
    (!args.is_empty()).then_some(args)
}

fn compact_command_part(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let candidate = trimmed.trim_end_matches('/');
    Path::new(candidate)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(trimmed)
        .to_string()
}

fn summarize_command_line(args: &[String]) -> Option<String> {
    let first = args.first()?;
    let mut summary = vec![compact_command_part(first)];

    for arg in args.iter().skip(1) {
        if summary.len() >= 3 {
            break;
        }
        if arg.trim().is_empty() || arg.starts_with('-') {
            continue;
        }
        let compact = compact_command_part(arg);
        if compact.is_empty() || summary.iter().any(|part| part == &compact) {
            continue;
        }
        summary.push(compact);
    }

    (!summary.is_empty()).then_some(summary.join(" "))
}

fn launcher_state_dir() -> PathBuf {
    if let Ok(state_home) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(state_home).join("applicationlauncher");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".local/state/applicationlauncher");
    }
    std::env::temp_dir().join("applicationlauncher")
}

fn install_panic_hook() {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let mut message = String::new();
        message.push_str(&format!("panic: {panic_info}\n"));
        if let Some(location) = panic_info.location() {
            message.push_str(&format!(
                "location: {}:{}:{}\n",
                location.file(),
                location.line(),
                location.column()
            ));
        }
        message.push_str(&format!("backtrace:\n{}\n", Backtrace::force_capture()));

        let state_dir = launcher_state_dir();
        if std::fs::create_dir_all(&state_dir).is_ok() {
            let panic_log = state_dir.join("panic.log");
            let mut panic_entry = String::new();
            panic_entry.push_str("\n==== applicationlauncher panic ====\n");
            panic_entry.push_str(&format!("{:?}\n", std::time::SystemTime::now()));
            panic_entry.push_str(&message);
            let _ = std::fs::write(&panic_log, panic_entry.as_bytes());
            let latest_log = state_dir.join("panic-latest.log");
            let _ = std::fs::write(latest_log, message.as_bytes());
        }

        eprintln!("{message}");
        previous_hook(panic_info);
    }));
}

fn paint_wayland_fallback_icon(painter: &egui::Painter, rect: egui::Rect) {
    let radius = (rect.width().min(rect.height()) * 0.18).clamp(4.0, 9.0);
    painter.rect_filled(
        rect,
        egui::CornerRadius::same(radius as u8),
        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 12),
    );
    painter.rect_stroke(
        rect.shrink(0.5),
        egui::CornerRadius::same(radius as u8),
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 28),
        ),
        egui::StrokeKind::Inside,
    );

    let c = rect.center();
    let scale = rect.width().min(rect.height()) / 48.0;
    let stroke = egui::Stroke::new(
        (3.0 * scale).max(1.5),
        egui::Color32::from_rgba_unmultiplied(230, 245, 255, 210),
    );
    let accent = egui::Color32::from_rgb(61, 174, 233);

    let points = [
        egui::pos2(c.x - 14.0 * scale, c.y - 9.0 * scale),
        egui::pos2(c.x - 7.0 * scale, c.y + 12.0 * scale),
        egui::pos2(c.x, c.y - 2.0 * scale),
        egui::pos2(c.x + 7.0 * scale, c.y + 12.0 * scale),
        egui::pos2(c.x + 14.0 * scale, c.y - 9.0 * scale),
    ];
    painter.line(points.to_vec(), stroke);
    painter.circle_filled(points[0], 3.2 * scale, accent);
    painter.circle_filled(points[2], 3.2 * scale, accent);
    painter.circle_filled(points[4], 3.2 * scale, accent);
}

fn paint_icon_in_rect(
    ui: &mut egui::Ui,
    icon_path: Option<&PathBuf>,
    rect: egui::Rect,
    icon_size: egui::Vec2,
) {
    if let Some(path) = icon_path {
        let uri = format!("file://{}", path.to_string_lossy());
        let mut child_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        child_ui.add(egui::Image::new(uri).max_size(icon_size));
    } else {
        paint_wayland_fallback_icon(ui.painter(), rect);
    }
}

fn sink_input_is_actively_rendering(
    sink: &PactlSinkInput,
    active_media_app_keys: &HashSet<String>,
    observed_pipewire_node_ids: &HashSet<u32>,
    active_pipewire_node_ids: &HashSet<u32>,
    pipewire_activity_cache_valid: bool,
) -> bool {
    if sink_input_is_browser_like(sink) {
        return sink_input_media_keys(sink)
            .iter()
            .any(|key| active_media_app_keys.contains(key));
    }

    // `pw-top` snapshots can miss short-lived activity, so only treat PipeWire
    // as authoritative when this node actually appeared in the sampled output.
    if !pipewire_activity_cache_valid {
        return true;
    }

    let Some(id) = sink
        .properties
        .get("object.id")
        .and_then(|id| id.parse::<u32>().ok())
    else {
        return true;
    };

    if !observed_pipewire_node_ids.contains(&id) {
        return true;
    }

    active_pipewire_node_ids.contains(&id)
}

fn sink_input_level(
    sink: &PactlSinkInput,
    active_media_app_keys: &HashSet<String>,
    observed_pipewire_node_ids: &HashSet<u32>,
    active_pipewire_node_ids: &HashSet<u32>,
    pipewire_activity_cache_valid: bool,
) -> f32 {
    if sink.mute || sink.corked {
        return 0.0;
    }

    if sink
        .properties
        .get("media.category")
        .is_some_and(|category| !category.eq_ignore_ascii_case("Playback"))
    {
        return 0.0;
    }

    if sink.properties.get("media.class").is_some_and(|class| {
        let class = class.to_ascii_lowercase();
        !class.contains("output") && !class.contains("playback")
    }) {
        return 0.0;
    }

    if !sink_input_is_actively_rendering(
        sink,
        active_media_app_keys,
        observed_pipewire_node_ids,
        active_pipewire_node_ids,
        pipewire_activity_cache_valid,
    ) {
        return 0.0;
    }

    let mut total = 0.0;
    let mut count = 0.0;
    for channel in sink.volume.values() {
        if let Ok(percent) = channel.value_percent.trim_end_matches('%').parse::<f32>() {
            total += percent;
            count += 1.0;
        }
    }

    if count == 0.0 {
        return 0.0;
    }

    let level = total / count / 100.0;
    if level < 0.01 {
        0.0
    } else {
        level.clamp(0.0, 1.5)
    }
}

fn active_audio_level_for_sinks(
    sinks: &[PactlSinkInput],
    active_media_app_keys: &HashSet<String>,
    observed_pipewire_node_ids: &HashSet<u32>,
    active_pipewire_node_ids: &HashSet<u32>,
    pipewire_activity_cache_valid: bool,
) -> Option<f32> {
    let mut max_level = 0.0f32;
    for sink in sinks {
        max_level = max_level.max(sink_input_level(
            sink,
            active_media_app_keys,
            observed_pipewire_node_ids,
            active_pipewire_node_ids,
            pipewire_activity_cache_valid,
        ));
    }
    (max_level > 0.0).then_some(max_level)
}

fn app_audio_level(
    app: &AppInfo,
    sink_inputs: &[PactlSinkInput],
    active_media_app_keys: &HashSet<String>,
    observed_pipewire_node_ids: &HashSet<u32>,
    active_pipewire_node_ids: &HashSet<u32>,
    pipewire_activity_cache_valid: bool,
) -> Option<f32> {
    let stem = app
        .desktop_file_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(normalize_app_match_key);
    let exec_name = command_basename(&app.exec).map(|name| normalize_app_match_key(&name));
    let app_name = normalize_app_match_key(&app.name);

    let mut matches = Vec::new();
    for sink in sink_inputs {
        if sink_input_level(
            sink,
            active_media_app_keys,
            observed_pipewire_node_ids,
            active_pipewire_node_ids,
            pipewire_activity_cache_valid,
        ) <= 0.0
        {
            continue;
        }

        let candidates = [
            sink.properties.get("application.id"),
            sink.properties.get("application.name"),
            sink.properties.get("application.icon_name"),
            sink.properties.get("application.process.binary"),
        ];

        let matched = candidates.iter().flatten().any(|value| {
            let normalized = normalize_app_match_key(value);
            !normalized.is_empty()
                && (normalized == app_name
                    || stem.as_ref().is_some_and(|stem| normalized == *stem)
                    || exec_name
                        .as_ref()
                        .is_some_and(|exec_name| normalized == *exec_name))
        });

        if matched {
            matches.push(sink.clone());
        }
    }

    active_audio_level_for_sinks(
        &matches,
        active_media_app_keys,
        observed_pipewire_node_ids,
        active_pipewire_node_ids,
        pipewire_activity_cache_valid,
    )
}

fn fetch_sink_inputs() -> Vec<PactlSinkInput> {
    let output = Command::new("pactl")
        .args(["--format=json", "list", "sink-inputs"])
        .output();
    match output {
        Ok(out) if out.status.success() => {
            serde_json::from_slice::<Vec<PactlSinkInput>>(&out.stdout).unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

fn sink_input_media_keys(sink: &PactlSinkInput) -> HashSet<String> {
    [
        "application.id",
        "application.name",
        "application.icon_name",
        "application.process.binary",
        "node.name",
    ]
    .iter()
    .filter_map(|key| sink.properties.get(*key))
    .map(|value| normalize_app_match_key(value))
    .filter(|value| !value.is_empty())
    .collect()
}

fn sink_input_is_browser_like(sink: &PactlSinkInput) -> bool {
    sink_input_media_keys(sink).iter().any(|key| {
        matches!(
            key.as_str(),
            "firefox"
                | "librewolf"
                | "floorp"
                | "zen"
                | "googlechrome"
                | "chrome"
                | "chromium"
                | "brave"
                | "bravebrowser"
                | "microsoftedge"
                | "edge"
                | "vivaldi"
        )
    })
}

fn mpris_service_names() -> Vec<String> {
    let output = Command::new("busctl")
        .args(["--user", "list", "--no-legend"])
        .output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .filter(|name| name.starts_with("org.mpris.MediaPlayer2."))
            .map(ToOwned::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn busctl_string_property(service: &str, interface: &str, property: &str) -> Option<String> {
    let output = Command::new("busctl")
        .args([
            "--user",
            "get-property",
            service,
            "/org/mpris/MediaPlayer2",
            interface,
            property,
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_quote = stdout.find('"')?;
    let rest = &stdout[first_quote + 1..];
    let second_quote = rest.find('"')?;
    Some(rest[..second_quote].to_owned())
}

fn fetch_active_media_app_keys() -> HashSet<String> {
    let mut keys = HashSet::new();
    for service in mpris_service_names() {
        let is_playing =
            busctl_string_property(&service, "org.mpris.MediaPlayer2.Player", "PlaybackStatus")
                .is_some_and(|status| status.eq_ignore_ascii_case("Playing"));
        if !is_playing {
            continue;
        }

        if let Some(identity) =
            busctl_string_property(&service, "org.mpris.MediaPlayer2", "Identity")
        {
            push_normalized_key_variants(&mut keys, &identity);
        }

        if let Some(service_suffix) = service.strip_prefix("org.mpris.MediaPlayer2.") {
            push_normalized_key_variants(&mut keys, service_suffix);
            if let Some(base_name) = service_suffix.split(".instance_").next() {
                push_normalized_key_variants(&mut keys, base_name);
            }
        }
    }
    keys
}

fn fetch_pipewire_activity() -> (HashSet<u32>, HashSet<u32>, bool) {
    let output = Command::new("pw-top").args(["-b", "-n", "1"]).output();
    match output {
        Ok(out) if out.status.success() => {
            let mut observed_ids = HashSet::new();
            let mut active_ids = HashSet::new();
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty()
                    || trimmed.starts_with("PipeWire")
                    || trimmed.starts_with("ID ")
                {
                    continue;
                }

                let cols: Vec<&str> = trimmed.split_whitespace().collect();
                if cols.len() < 6 {
                    continue;
                }

                let Some(state) = cols.first().and_then(|value| value.chars().next()) else {
                    continue;
                };
                if !matches!(state, 'R' | 'S' | 'I' | 'C' | 'X') {
                    continue;
                }

                let Some(id) = cols.get(1).and_then(|value| value.parse::<u32>().ok()) else {
                    continue;
                };
                observed_ids.insert(id);

                let wait_us = cols
                    .get(4)
                    .and_then(|value| value.strip_suffix("us"))
                    .and_then(|value| value.parse::<f32>().ok())
                    .unwrap_or(0.0);
                let busy_us = cols
                    .get(5)
                    .and_then(|value| value.strip_suffix("us"))
                    .and_then(|value| value.parse::<f32>().ok())
                    .unwrap_or(0.0);
                let wait_active = wait_us >= PIPEWIRE_ACTIVE_US_THRESHOLD;
                let busy_active = busy_us >= PIPEWIRE_ACTIVE_US_THRESHOLD;
                let total_active = (wait_us + busy_us) >= PIPEWIRE_ACTIVE_TOTAL_US_THRESHOLD;
                let is_active = (wait_active || busy_active) && total_active;

                if is_active {
                    active_ids.insert(id);
                }
            }

            (observed_ids, active_ids, true)
        }
        _ => (HashSet::new(), HashSet::new(), false),
    }
}

fn paint_audio_activity_ring(
    painter: &egui::Painter,
    rect: egui::Rect,
    level: f32,
    time_seconds: f32,
) {
    let strength = level.clamp(0.12, 1.2);
    let center = rect.center();
    let base_radius = rect.width().max(rect.height()) * 0.57;
    let max_bar = (rect.width().max(rect.height()) * 0.18).clamp(4.0, 14.0);
    let bars = 24;

    for i in 0..bars {
        let t = i as f32 / bars as f32;
        let angle = t * std::f32::consts::TAU;
        let wave_a = ((time_seconds * 7.5 + t * 13.0).sin() * 0.5 + 0.5).powf(1.4);
        let wave_b = ((time_seconds * 11.0 - t * 19.0).sin() * 0.5 + 0.5) * 0.45;
        let bar_level = (0.25 + wave_a * 0.75 + wave_b).clamp(0.0, 1.0) * strength;
        let inner = base_radius + 1.0;
        let outer = inner + max_bar * bar_level;
        let dir = egui::vec2(angle.cos(), angle.sin());
        let alpha = (70.0 + 135.0 * bar_level).clamp(45.0, 210.0) as u8;
        let color = if i % 3 == 0 {
            egui::Color32::from_rgba_unmultiplied(126, 226, 255, alpha)
        } else {
            egui::Color32::from_rgba_unmultiplied(61, 174, 233, alpha)
        };

        painter.line_segment(
            [center + dir * inner, center + dir * outer],
            egui::Stroke::new((1.2 + 1.7 * bar_level).clamp(1.2, 3.0), color),
        );
    }
}

fn process_exists(pid: i32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

fn replace_terminal_suffix_path(original_suffix: &str, cwd: &str) -> String {
    let trimmed = original_suffix.trim();
    if trimmed.is_empty() {
        return cwd.to_string();
    }

    match trimmed.rfind(char::is_whitespace) {
        Some(split_at) => format!("{} {}", trimmed[..split_at].trim_end(), cwd),
        None => cwd.to_string(),
    }
}

fn is_terminal_title_marker(value: &str) -> bool {
    let key = normalize_app_match_key(value);
    key == "terminal"
        || key == "xfce4terminal"
        || key == "konsole"
        || key == "kitty"
        || key == "alacritty"
        || key == "wezterm"
        || key == "foot"
        || key.ends_with("terminal")
}

fn terminal_title_segments(dynamic_title: &str, proc_name: &str, cwd: Option<&str>) -> Vec<String> {
    let dynamic_title = dynamic_title.trim();
    let mut segments = Vec::new();
    let mut after_process = Vec::new();

    match cwd {
        Some(cwd) if !cwd.trim().is_empty() => {
            let cwd = cwd.trim();
            if dynamic_title.is_empty() {
                after_process.push(cwd.to_string());
            } else {
                let cwd_context = replace_terminal_suffix_path(dynamic_title, cwd);
                if dynamic_title == cwd || dynamic_title == cwd_context {
                    segments.push(dynamic_title.to_string());
                } else if dynamic_title.chars().any(char::is_whitespace) {
                    segments.push(cwd_context);
                } else {
                    segments.push(dynamic_title.to_string());
                    after_process.push(cwd_context);
                }
            }
        }
        _ if !dynamic_title.is_empty() => segments.push(dynamic_title.to_string()),
        _ => {}
    }

    segments.push(proc_name.trim().to_string());
    segments.extend(after_process);
    segments.push("Terminal".to_string());
    segments
}

fn terminal_display_title(raw_title: &str, proc_name: &str, cwd: Option<&str>) -> String {
    let separators = [" - ", " — ", " – ", " : ", " | "];

    for sep in separators {
        let parts: Vec<&str> = raw_title.split(sep).map(str::trim).collect();
        if parts.len() < 2 {
            continue;
        }

        if parts
            .first()
            .is_some_and(|part| is_terminal_title_marker(part))
        {
            let suffix = parts[1..].join(sep);
            return terminal_title_segments(&suffix, proc_name, cwd).join(sep);
        }

        if parts
            .last()
            .is_some_and(|part| is_terminal_title_marker(part))
        {
            let suffix = parts[..parts.len() - 1].join(sep);
            return terminal_title_segments(&suffix, proc_name, cwd).join(sep);
        }
    }

    terminal_title_segments(raw_title, proc_name, cwd).join(" - ")
}

fn normalize_terminal_title_marker_position(raw_title: &str) -> String {
    let separators = [" - ", " — ", " – ", " : ", " | "];

    for sep in separators {
        let parts: Vec<&str> = raw_title.split(sep).map(str::trim).collect();
        if parts.len() < 2 {
            continue;
        }

        let first_is_marker = parts
            .first()
            .is_some_and(|part| is_terminal_title_marker(part));
        let last_is_marker = parts
            .last()
            .is_some_and(|part| is_terminal_title_marker(part));

        if first_is_marker && last_is_marker && parts.len() >= 3 {
            return parts[1..].join(sep);
        }

        if first_is_marker {
            let body = parts[1..]
                .iter()
                .copied()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>();
            if !body.is_empty() {
                let mut rebuilt = body;
                rebuilt.push("Terminal");
                return rebuilt.join(sep);
            }
        }
    }

    raw_title.trim().to_string()
}

fn best_app_match_score(window_keys: &[String], app: &AppInfo) -> Option<(usize, usize, usize)> {
    let mut best_score: Option<(usize, usize, usize)> = None;

    let mut app_keys = Vec::new();
    app_keys.push(normalize_app_match_key(&app.name));

    if let Some(stem) = app
        .desktop_file_path
        .file_stem()
        .and_then(|stem| stem.to_str())
    {
        app_keys.push(normalize_app_match_key(stem));
    }

    if let Some(exec_name) = command_basename(&app.exec) {
        app_keys.push(normalize_app_match_key(&exec_name));
    }

    app_keys.retain(|key| !key.is_empty());

    for window_key in window_keys {
        for app_key in &app_keys {
            let score = if window_key == app_key {
                Some((0, app_key.len().abs_diff(window_key.len()), app.name.len()))
            } else if app_key.starts_with(window_key) || window_key.starts_with(app_key) {
                Some((1, app_key.len().abs_diff(window_key.len()), app.name.len()))
            } else if app_key.contains(window_key) || window_key.contains(app_key) {
                Some((2, app_key.len().abs_diff(window_key.len()), app.name.len()))
            } else {
                None
            };

            if let Some(score) = score {
                if best_score.is_none_or(|current| score < current) {
                    best_score = Some(score);
                }
            }
        }
    }

    best_score
}

fn truncate_tile_label(text: &str, tile_size: f32) -> String {
    let max_chars = ((tile_size / 7.0).floor() as usize).clamp(6, 22);
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }

    let keep = max_chars.saturating_sub(3);
    let mut truncated: String = text.chars().take(keep).collect();
    truncated.push_str("...");
    truncated
}

fn title_match_ranges(text: &str, query: &str) -> Vec<(usize, usize)> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }

    let mut normalized = String::new();
    let mut mapping = Vec::new();

    for (start, ch) in text.char_indices() {
        let end = start + ch.len_utf8();
        let lower = ch.to_lowercase().collect::<String>();
        let lower_start = normalized.len();
        normalized.push_str(&lower);
        let lower_end = normalized.len();
        mapping.push((lower_start, lower_end, start, end));
    }

    let query_terms = normalize_metadata_search_value(query)
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if query_terms.is_empty() {
        return Vec::new();
    }
    let mut ranges = Vec::new();

    for query_term in query_terms {
        for (match_start, _) in normalized.match_indices(&query_term) {
            let match_end = match_start + query_term.len();
            let mut original_start = None;
            let mut original_end = None;

            for (lower_start, lower_end, start, end) in &mapping {
                if *lower_end <= match_start || *lower_start >= match_end {
                    continue;
                }
                original_start.get_or_insert(*start);
                original_end = Some(*end);
            }

            if let (Some(start), Some(end)) = (original_start, original_end) {
                ranges.push((start, end));
            }
        }
    }

    ranges.sort_by_key(|(start, end)| (*start, *end));
    let mut merged = Vec::new();
    for (start, end) in ranges {
        if let Some((_, previous_end)) = merged.last_mut() {
            if start <= *previous_end {
                *previous_end = (*previous_end).max(end);
                continue;
            }
        }
        merged.push((start, end));
    }

    merged
}

fn normalized_query_terms(query: &str) -> Vec<String> {
    normalize_metadata_search_value(query)
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

fn alnum_tokens_with_ranges(text: &str) -> Vec<(usize, usize, String)> {
    let mut tokens = Vec::new();
    let mut token_start = None;

    for (idx, ch) in text.char_indices() {
        if ch.is_ascii_alphanumeric() {
            token_start.get_or_insert(idx);
            continue;
        }
        if let Some(start) = token_start.take() {
            let end = idx;
            let normalized = normalize_metadata_search_value(&text[start..end]).to_lowercase();
            if !normalized.is_empty() {
                tokens.push((start, end, normalized));
            }
        }
    }

    if let Some(start) = token_start {
        let end = text.len();
        let normalized = normalize_metadata_search_value(&text[start..end]).to_lowercase();
        if !normalized.is_empty() {
            tokens.push((start, end, normalized));
        }
    }

    tokens
}

fn typo_title_match_ranges(text: &str, query: &str) -> Vec<(usize, usize)> {
    let Some(rank) = visible_title_match_provenance(text, query) else {
        return Vec::new();
    };
    let Some((_, _, winning_token)) = alnum_tokens_with_ranges(text)
        .into_iter()
        .nth(rank.provenance().token_index)
    else {
        return Vec::new();
    };

    alnum_tokens_with_ranges(text)
        .into_iter()
        .filter_map(|(start, end, token)| (token == winning_token).then_some((start, end)))
        .collect()
}

fn title_highlight_segments(text: &str, query: &str) -> Vec<(usize, usize, bool)> {
    let query_terms = normalized_query_terms(query);
    let mut red_ranges = Vec::new();
    let mut matched_terms = HashSet::new();
    for term in &query_terms {
        let ranges = title_match_ranges(text, term);
        if !ranges.is_empty() {
            matched_terms.insert(term.clone());
            red_ranges.extend(ranges);
        }
    }
    red_ranges.sort_by_key(|(start, end)| (*start, *end));
    let mut merged_red_ranges = Vec::new();
    for (start, end) in red_ranges {
        if let Some((_, previous_end)) = merged_red_ranges.last_mut() {
            if start <= *previous_end {
                *previous_end = (*previous_end).max(end);
                continue;
            }
        }
        merged_red_ranges.push((start, end));
    }

    let mut yellow_ranges = Vec::new();
    for term in &query_terms {
        if matched_terms.contains(term) {
            continue;
        }
        if let Some((start, end)) = typo_title_match_ranges(text, term).into_iter().next() {
            let overlaps_red = merged_red_ranges
                .iter()
                .any(|(red_start, red_end)| start < *red_end && end > *red_start);
            if !overlaps_red {
                yellow_ranges.push((start, end));
            }
        }
    }
    yellow_ranges.sort_by_key(|(start, end)| (*start, *end));
    let mut merged_yellow_ranges = Vec::new();
    for (start, end) in yellow_ranges {
        if let Some((_, previous_end)) = merged_yellow_ranges.last_mut() {
            if start <= *previous_end {
                *previous_end = (*previous_end).max(end);
                continue;
            }
        }
        merged_yellow_ranges.push((start, end));
    }

    let mut segments = Vec::new();
    for (start, end) in merged_red_ranges {
        segments.push((start, end, true));
    }
    for (start, end) in merged_yellow_ranges {
        segments.push((start, end, false));
    }
    segments.sort_by_key(|(start, end, is_red)| (*start, *end, !*is_red));

    segments
}

fn highlighted_title_job_from_segments(
    text: &str,
    font_size: f32,
    segments: &[(usize, usize, bool)],
) -> egui::text::LayoutJob {
    let default_format = egui::TextFormat {
        font_id: egui::FontId::proportional(font_size),
        color: egui::Color32::WHITE,
        ..Default::default()
    };
    let highlight_format = egui::TextFormat {
        font_id: egui::FontId::proportional(font_size),
        color: egui::Color32::from_rgb(235, 90, 90),
        ..Default::default()
    };
    let typo_highlight_format = egui::TextFormat {
        font_id: egui::FontId::proportional(font_size),
        color: egui::Color32::from_rgb(235, 196, 72),
        ..Default::default()
    };

    let mut job = egui::text::LayoutJob::default();

    if segments.is_empty() {
        job.append(text, 0.0, default_format);
        return job;
    }

    let mut cursor = 0usize;
    for &(start, end, is_red) in segments {
        if cursor < start {
            job.append(&text[cursor..start], 0.0, default_format.clone());
        }
        job.append(
            &text[start..end],
            0.0,
            if is_red {
                highlight_format.clone()
            } else {
                typo_highlight_format.clone()
            },
        );
        cursor = end;
    }
    if cursor < text.len() {
        job.append(&text[cursor..], 0.0, default_format);
    }

    job
}

fn highlighted_title_job(
    text: &str,
    query: &str,
    font_size: f32,
    _typo_match: bool,
) -> egui::text::LayoutJob {
    let segments = title_highlight_segments(text, query);
    highlighted_title_job_from_segments(text, font_size, &segments)
}

fn rank_matches_visible_title_via_typo(rank: &SearchRank) -> bool {
    rank.provenance().field_priority == 0
}

fn pick_better_rank(left: SearchRank, right: SearchRank) -> SearchRank {
    if left <= right { left } else { right }
}

fn visible_title_has_typo_match(title: &str, query: &str) -> bool {
    if query.trim().is_empty() || !title_match_ranges(title, query).is_empty() {
        return false;
    }
    visible_title_match_provenance(title, query).is_some()
}

fn visible_match_priority(title: &str, query: &str) -> u8 {
    if query.trim().is_empty() {
        0
    } else if !title_match_ranges(title, query).is_empty()
        || visible_title_has_typo_match(title, query)
    {
        0
    } else {
        1
    }
}

fn visible_title_match_provenance(text: &str, query: &str) -> Option<SearchRank> {
    let typo_query = MetadataQuery::new(query)?.with_typo_fallback(true);
    let normalized = normalize_metadata_search_value(text);
    if normalized.is_empty() {
        return None;
    }
    let fields = [SearchField {
        priority: 0,
        value: normalized.as_str(),
    }];
    let candidate = MetadataCandidate {
        key: "",
        fields: &fields,
        score: 0.0,
    };
    let rank = typo_query.search_rank(candidate)?;
    rank_matches_visible_title_via_typo(&rank).then_some(rank)
}

fn paint_centered_title_job(
    ui: &egui::Ui,
    rect: egui::Rect,
    query: &str,
    text: &str,
    font_size: f32,
    typo_match: bool,
    fallback_color: egui::Color32,
) {
    let galley = ui.ctx().fonts_mut(|fonts| {
        fonts.layout_job(highlighted_title_job(text, query, font_size, typo_match))
    });
    let position = egui::pos2(
        rect.center().x - galley.size().x / 2.0,
        rect.center().y - galley.size().y / 2.0,
    );
    ui.painter().galley(position, galley, fallback_color);
}

fn grid_move_down(index: usize, len: usize, columns: usize) -> usize {
    if len == 0 {
        return 0;
    }

    let columns = columns.max(1);
    let next = index.saturating_add(columns);
    if next < len { next } else { index % columns }
}

fn grid_move_up(index: usize, len: usize, columns: usize) -> usize {
    if len == 0 {
        return 0;
    }

    let columns = columns.max(1);
    if index >= columns {
        return index - columns;
    }

    let column = index % columns;
    let mut last_in_column = column.min(len - 1);
    while last_in_column + columns < len {
        last_in_column += columns;
    }
    last_in_column
}

fn nearest_center_index(centers: &[f32], target_y: f32) -> Option<usize> {
    centers
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            let da = (*a - target_y).abs();
            let db = (*b - target_y).abs();
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(index, _)| index)
}

fn show_immediate_icon_tooltip(response: &egui::Response, text: &str) {
    if !response.hovered() {
        return;
    }

    let _ = egui::Tooltip::always_open(
        response.ctx.clone(),
        response.layer_id,
        response.id.with("icon_tooltip"),
        response.rect,
    )
    .gap(8.0)
    .show(|ui| {
        ui.label(text);
    });
}

fn parse_desktop_file(path: &Path, theme: &str) -> Option<AppInfo> {
    let content = std::fs::read_to_string(path).ok()?;

    let mut in_desktop_entry = false;
    let mut name = None;
    let mut exec = None;
    let mut icon = None;
    let mut comment = None;
    let mut no_display = false;
    let mut is_application = false;
    let mut is_settings_module = false;
    let mut exec_command = None;
    let mut x_kde_alias_for = None;

    if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
        let name_lower = file_name.to_lowercase();
        if name_lower.starts_with("kcm_") {
            is_settings_module = true;
        }
    }

    // Use current locale language code if available
    let lang = std::env::var("LANG")
        .ok()
        .and_then(|l| l.split('.').next().map(|s| s.to_string()))
        .and_then(|l| l.split('_').next().map(|s| s.to_string()));

    let name_key = lang.as_ref().map(|l| format!("Name[{}]", l));
    let comment_key = lang.as_ref().map(|l| format!("Comment[{}]", l));

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            if line == "[Desktop Entry]" {
                in_desktop_entry = true;
            } else {
                in_desktop_entry = false;
            }
            continue;
        }

        if !in_desktop_entry {
            continue;
        }

        if let Some(pos) = line.find('=') {
            let key = line[..pos].trim();
            let val = line[pos + 1..].trim();

            if key == "Name" && name.is_none() {
                name = Some(val.to_string());
            } else if let Some(ref nk) = name_key {
                if key == nk {
                    name = Some(val.to_string());
                }
            }

            if key == "Comment" && comment.is_none() {
                comment = Some(val.to_string());
            } else if let Some(ref ck) = comment_key {
                if key == ck {
                    comment = Some(val.to_string());
                }
            }

            if key == "Exec" {
                exec = Some(val.to_string());
                exec_command = Some(val.to_string());
            }
            if key == "Icon" {
                icon = Some(val.to_string());
            }
            if key == "NoDisplay" && val.to_lowercase() == "true" {
                no_display = true;
            }
            if key == "Type" && val == "Application" {
                is_application = true;
            }
            if key == "Categories" && val.split(';').any(|c| c == "SettingsPanel") {
                is_settings_module = true;
            }
            if key == "X-KDE-AliasFor" {
                x_kde_alias_for = Some(val.to_string());
            }
        }
    }

    if x_kde_alias_for.as_deref() == Some("systemsettings") && no_display {
        is_settings_module = true;
    }

    if let Some(exec_cmd) = exec_command.as_deref() {
        let exec_lower = exec_cmd.to_lowercase();
        if exec_lower.starts_with("kcmshell")
            || exec_lower.contains(" kcm_")
            || exec_lower.starts_with("systemsettings kcm_")
        {
            is_settings_module = true;
        }
    }

    if (no_display && !is_settings_module) || !is_application {
        return None;
    }

    let name = name?;
    let exec = exec?;

    let is_terminal_app = is_terminal_app_name(&name)
        || is_terminal_app_name(&exec)
        || command_basename(&exec)
            .as_deref()
            .is_some_and(is_terminal_app_name);

    let icon_path = icon.and_then(|i| {
        let p = PathBuf::from(&i);
        if p.is_absolute() && p.exists() {
            return Some(p);
        }
        if !is_terminal_app && is_terminal_icon_name(&i) {
            return None;
        }
        lookup_theme_icon_exact(theme, &i)
    });

    Some(AppInfo {
        name,
        exec,
        icon_path,
        comment,
        desktop_file_path: path.to_path_buf(),
        is_settings_module,
    })
}

fn get_installed_apps(theme: &str) -> Vec<AppInfo> {
    let mut apps = Vec::new();
    let mut app_dirs = vec![PathBuf::from("/usr/share/applications")];
    if let Ok(home) = std::env::var("HOME") {
        app_dirs.push(PathBuf::from(format!("{}/.local/share/applications", home)));
    }
    let flatpak_dir = PathBuf::from("/var/lib/flatpak/exports/share/applications");
    if flatpak_dir.exists() {
        app_dirs.push(flatpak_dir);
    }
    let user_flatpak_dir = if let Ok(home) = std::env::var("HOME") {
        Some(PathBuf::from(format!(
            "{}/.local/share/flatpak/exports/share/applications",
            home
        )))
    } else {
        None
    };
    if let Some(dir) = user_flatpak_dir {
        if dir.exists() {
            app_dirs.push(dir);
        }
    }

    let mut seen_entries = std::collections::HashSet::new();

    for dir in app_dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().map_or(false, |ext| ext == "desktop") {
                    if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                        if seen_entries.contains(file_name) {
                            continue;
                        }
                        seen_entries.insert(file_name.to_string());
                    }

                    if let Some(app) = parse_desktop_file(&path, theme) {
                        apps.push(app);
                    }
                }
            }
        }
    }

    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}

fn desktop_entry_search_dirs() -> Vec<PathBuf> {
    let mut app_dirs = vec![PathBuf::from("/usr/share/applications")];
    if let Ok(home) = std::env::var("HOME") {
        app_dirs.push(PathBuf::from(format!("{}/.local/share/applications", home)));
        let user_flatpak_dir = PathBuf::from(format!(
            "{}/.local/share/flatpak/exports/share/applications",
            home
        ));
        if user_flatpak_dir.exists() {
            app_dirs.push(user_flatpak_dir);
        }
    }
    let flatpak_dir = PathBuf::from("/var/lib/flatpak/exports/share/applications");
    if flatpak_dir.exists() {
        app_dirs.push(flatpak_dir);
    }
    app_dirs
}

fn resolve_desktop_file_path(desktop_file_name: &str) -> Option<PathBuf> {
    let trimmed = desktop_file_name.trim();
    if trimmed.is_empty() {
        return None;
    }

    let candidate = PathBuf::from(trimmed);
    if candidate.is_absolute() && candidate.exists() {
        return Some(candidate);
    }

    let base_name = if trimmed.ends_with(".desktop") {
        trimmed.to_string()
    } else {
        format!("{trimmed}.desktop")
    };

    for dir in desktop_entry_search_dirs() {
        let path = dir.join(&base_name);
        if path.exists() {
            return Some(path);
        }
    }

    None
}
fn parse_proc_stat(stat_content: &str) -> Option<(i32, String, i32)> {
    let last_paren = stat_content.rfind(')')?;
    let (left, right) = stat_content.split_at(last_paren);

    let pid_part = left.split_whitespace().next()?;
    let pid: i32 = pid_part.parse().ok()?;

    let name_start = left.find('(')? + 1;
    let name = left[name_start..].to_string();

    let tokens: Vec<&str> = right[1..].split_whitespace().collect();
    if tokens.len() < 2 {
        return None;
    }
    let ppid: i32 = tokens[1].parse().ok()?;

    Some((pid, name, ppid))
}

fn get_process_tree() -> (
    HashMap<i32, Vec<i32>>,
    HashMap<i32, String>,
    HashMap<i32, i32>,
) {
    let mut ppid_to_children = HashMap::new();
    let mut pid_to_name = HashMap::new();
    let mut pid_to_ppid = HashMap::new();

    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries {
            if let Ok(entry) = entry {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if name.chars().all(|c| c.is_ascii_digit()) {
                            let stat_path = path.join("stat");
                            if let Ok(content) = std::fs::read_to_string(stat_path) {
                                if let Some((pid, proc_name, ppid)) = parse_proc_stat(&content) {
                                    pid_to_name.insert(pid, proc_name);
                                    pid_to_ppid.insert(pid, ppid);
                                    ppid_to_children
                                        .entry(ppid)
                                        .or_insert_with(Vec::new)
                                        .push(pid);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    (ppid_to_children, pid_to_name, pid_to_ppid)
}

fn is_shell(name: &str) -> bool {
    let n = name.to_lowercase();
    n == "bash"
        || n == "fish"
        || n == "zsh"
        || n == "sh"
        || n == "dash"
        || n == "tcsh"
        || n == "ksh"
}

fn find_terminal_leaf(
    terminal_pid: i32,
    ppid_to_children: &HashMap<i32, Vec<i32>>,
    pid_to_name: &HashMap<i32, String>,
) -> Option<(i32, String)> {
    let mut current_pid = terminal_pid;

    // First, try to locate a shell among the direct children of the terminal emulator.
    // If one is found, we start our search for commands run inside the shell from there,
    // which prevents being distracted by background helper processes spawned directly by the terminal.
    if let Some(children) = ppid_to_children.get(&terminal_pid) {
        for &child in children {
            if let Some(name) = pid_to_name.get(&child) {
                if is_shell(name) {
                    current_pid = child;
                    break;
                }
            }
        }
    }

    loop {
        if let Some(children) = ppid_to_children.get(&current_pid) {
            let mut valid_children = Vec::new();
            for &child in children {
                if let Some(name) = pid_to_name.get(&child) {
                    valid_children.push((child, name.clone()));
                }
            }

            if valid_children.is_empty() {
                break;
            }

            valid_children.sort_by(|a, b| {
                let a_is_shell = is_shell(&a.1);
                let b_is_shell = is_shell(&b.1);
                match (a_is_shell, b_is_shell) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => a.0.cmp(&b.0),
                }
            });

            if let Some(&(best_child, _)) = valid_children.last() {
                current_pid = best_child;
            } else {
                break;
            }
        } else {
            break;
        }
    }

    if current_pid == terminal_pid {
        None
    } else {
        pid_to_name
            .get(&current_pid)
            .map(|name| (current_pid, name.clone()))
    }
}

fn build_process_chain(
    start_pid: i32,
    pid_to_name: &HashMap<i32, String>,
    pid_to_ppid: &HashMap<i32, i32>,
) -> Vec<ProcessChainEntry> {
    let mut chain = Vec::new();
    let mut current_pid = Some(start_pid);

    while let Some(pid) = current_pid {
        let name = pid_to_name
            .get(&pid)
            .cloned()
            .unwrap_or_else(|| pid.to_string());
        let exe_path = std::fs::read_link(format!("/proc/{}/exe", pid)).ok();
        chain.push(ProcessChainEntry {
            pid,
            name,
            exe_path,
        });

        current_pid = pid_to_ppid
            .get(&pid)
            .copied()
            .filter(|ppid| *ppid > 0 && *ppid != pid);
    }

    chain
}

fn is_terminal_class(class_lower: &str) -> bool {
    class_lower.contains("terminal")
        || class_lower == "konsole"
        || class_lower == "kitty"
        || class_lower == "alacritty"
        || class_lower == "wezterm"
        || class_lower == "foot"
}

fn build_window_info(
    id: String,
    title: String,
    class: String,
    desktop_file_name: Option<String>,
    pid: Option<i32>,
    geometry: Option<(i32, i32, i32, i32)>,
    minimized: Option<bool>,
    theme: &str,
    icon_cache: &mut HashMap<String, Option<PathBuf>>,
    ppid_to_children: &HashMap<i32, Vec<i32>>,
    pid_to_name: &HashMap<i32, String>,
    pid_to_ppid: &HashMap<i32, i32>,
) -> Option<WindowInfo> {
    let class_lower = class.to_lowercase();
    let my_pid = std::process::id() as i32;

    if class_lower.contains("plasmashell")
        || class_lower == "kwin_wayland"
        || class_lower.is_empty()
        || class_lower == "applicationlauncher"
        || title == "Open Application Windows"
        || pid == Some(my_pid)
    {
        return None;
    }

    if let Some(pid) = pid {
        if !process_exists(pid) {
            return None;
        }
    }

    let display_title = if title.is_empty() {
        class.clone()
    } else {
        title
    };

    let mut active_process = None;
    let mut exe_path = None;
    let mut cwd_path = None;
    let mut command_line = None;
    let mut command_summary = None;
    let mut process_chain = Vec::new();
    if let Some(pid) = pid {
        let mut target_pid = pid;
        if is_terminal_class(&class_lower) {
            if let Some((leaf_pid, leaf_name)) =
                find_terminal_leaf(pid, ppid_to_children, pid_to_name)
            {
                active_process = Some(leaf_name);
                target_pid = leaf_pid;
            }
        }

        if let Ok(path) = std::fs::read_link(format!("/proc/{}/exe", pid)) {
            exe_path = Some(path);
        }

        if let Ok(path) = std::fs::read_link(format!("/proc/{}/cwd", target_pid)) {
            cwd_path = Some(path);
        }

        if let Some(args) = read_proc_cmdline(target_pid) {
            command_summary = summarize_command_line(&args);
            command_line = Some(args.join(" "));
        }

        process_chain = build_process_chain(target_pid, pid_to_name, pid_to_ppid);
    }

    let mut final_title = display_title;
    if let Some(ref proc_name) = active_process {
        if is_terminal_class(&class_lower) {
            let terminal_suffix = cwd_path.as_ref().map(|path| display_path(path));
            final_title =
                terminal_display_title(&final_title, proc_name, terminal_suffix.as_deref());
        } else {
            let separators = [" - ", " — ", " – ", " : ", " | "];
            let mut split_found = false;
            for sep in separators {
                if let Some(pos) = final_title.find(sep) {
                    let (left, right) = final_title.split_at(pos);
                    let original_suffix = &right[sep.len()..];
                    final_title = format!(
                        "{}{}{}{}{}",
                        left.trim(),
                        sep,
                        proc_name,
                        sep,
                        original_suffix.trim()
                    );
                    split_found = true;
                    break;
                }
            }
            if !split_found {
                final_title = format!("{} - {}", final_title, proc_name);
            }
        }
    } else if is_terminal_class(&class_lower) {
        final_title = normalize_terminal_title_marker_position(&final_title);
    }
    if is_pcmanfm_class(&class_lower) {
        let title_key = normalize_app_match_key(&final_title);
        if !title_key.ends_with("pcmanfm") {
            final_title = format!("{} — PCManFM", final_title.trim());
        }
    }

    let icon_key = active_process.as_ref().unwrap_or(&class).clone();
    let icon_path = icon_cache
        .entry(icon_key.clone())
        .or_insert_with(|| {
            let mut path = None;
            if let Some(ref proc_name) = active_process {
                path = find_icon(theme, proc_name);
            }
            if path.is_none() {
                path = find_icon(theme, &class);
            }
            if path.is_none() {
                if let Some(name) = exe_path
                    .as_ref()
                    .and_then(|path| path.file_name())
                    .and_then(|name| name.to_str())
                {
                    path = find_icon(theme, name);
                }
            }
            path
        })
        .clone();

    Some(WindowInfo {
        id,
        title: final_title,
        class,
        desktop_file_name,
        minimized,
        icon_path,
        active_process,
        exe_path,
        cwd_path,
        command_line,
        command_summary,
        geometry,
        process_chain,
        pid,
    })
}

fn kwin_window_script_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        PathBuf::from(home)
            .join(".local/share/kwin/scripts")
            .join(KWIN_WINDOW_FEED_SCRIPT_ID),
    )
}

fn install_kwin_window_feed_script() -> Result<(), String> {
    let Some(script_dir) = kwin_window_script_dir() else {
        return Err("HOME is not set; cannot install KWin window feed script.".to_string());
    };

    let code_dir = script_dir.join("contents/code");
    std::fs::create_dir_all(&code_dir)
        .map_err(|err| format!("Failed to create KWin script directory: {err}"))?;
    std::fs::write(script_dir.join("metadata.json"), KWIN_WINDOW_FEED_METADATA)
        .map_err(|err| format!("Failed to write KWin script metadata: {err}"))?;
    std::fs::write(code_dir.join("main.js"), KWIN_WINDOW_FEED_MAIN_JS)
        .map_err(|err| format!("Failed to write KWin script source: {err}"))?;
    Ok(())
}

fn enable_kwin_window_feed_script() -> Result<(), String> {
    let status = Command::new("kwriteconfig6")
        .args([
            "--file",
            "kwinrc",
            "--group",
            "Plugins",
            "--key",
            &format!("{}Enabled", KWIN_WINDOW_FEED_SCRIPT_ID),
            "true",
        ])
        .status()
        .map_err(|err| format!("Failed to enable KWin window feed script: {err}"))?;

    if !status.success() {
        return Err("kwriteconfig6 failed while enabling the KWin window feed script.".to_string());
    }

    Ok(())
}

fn reload_kwin_config() -> Result<(), String> {
    let status = Command::new("qdbus6")
        .args(["org.kde.KWin", "/KWin", "reconfigure"])
        .status()
        .map_err(|err| format!("Failed to reload KWin configuration: {err}"))?;

    if !status.success() {
        return Err("qdbus6 returned a failure while reloading KWin.".to_string());
    }

    Ok(())
}

fn start_kwin_window_feed_service(
    tx: Sender<WindowFeedEvent>,
    repaint_ctx: egui::Context,
) -> Result<(), String> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let ready_tx_success = ready_tx.clone();
        let result = pollster::block_on(async move {
            let connection = zbus::connection::Builder::session()
                .map_err(|err| err.to_string())?
                .name(KWIN_WINDOW_FEED_SERVICE)
                .map_err(|err| err.to_string())?
                .serve_at(KWIN_WINDOW_FEED_PATH, KWinWindowFeed { tx, repaint_ctx })
                .map_err(|err| err.to_string())?
                .build()
                .await
                .map_err(|err| err.to_string())?;

            let _ = ready_tx_success.send(Ok(()));
            let _connection = connection;
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            Ok::<(), String>(())
        });

        if let Err(err) = result {
            let _ = ready_tx.send(Err(err));
        }
    });

    ready_rx
        .recv()
        .map_err(|err| format!("Failed to start KWin window feed service: {err}"))?
}

fn setup_kwin_window_feed(
    tx: Sender<WindowFeedEvent>,
    repaint_ctx: egui::Context,
) -> Result<(), String> {
    start_kwin_window_feed_service(tx, repaint_ctx)?;
    install_kwin_window_feed_script()?;
    enable_kwin_window_feed_script()?;
    reload_kwin_config()?;
    Ok(())
}

fn window_info_from_kwin_payload(
    payload: KWinWindowPayload,
    theme: &str,
    ppid_to_children: &HashMap<i32, Vec<i32>>,
    pid_to_name: &HashMap<i32, String>,
    pid_to_ppid: &HashMap<i32, i32>,
) -> Option<WindowInfo> {
    let mut icon_cache = HashMap::new();
    let desktop_file_name_value = payload.desktop_file_name.trim().to_string();
    let class = if payload.class.trim().is_empty() {
        desktop_file_name_value.clone()
    } else {
        payload.class
    };
    let pid = (payload.pid > 0).then_some(payload.pid);
    let desktop_file_name =
        (!desktop_file_name_value.is_empty()).then_some(desktop_file_name_value);
    let geometry = (payload.width > 0 && payload.height > 0).then_some((
        payload.x,
        payload.y,
        payload.width,
        payload.height,
    ));
    let minimized = Some(payload.minimized);
    build_window_info(
        payload.id,
        payload.title,
        class,
        desktop_file_name,
        pid,
        geometry,
        minimized,
        theme,
        &mut icon_cache,
        ppid_to_children,
        pid_to_name,
        pid_to_ppid,
    )
}

fn coalesce_window_feed_events(events: Vec<WindowFeedEvent>) -> Vec<WindowFeedEvent> {
    let mut latest_by_id: HashMap<String, WindowFeedEvent> = HashMap::new();
    let mut order = Vec::new();

    for event in events {
        let id = match &event {
            WindowFeedEvent::Upsert(payload) => payload.id.clone(),
            WindowFeedEvent::Remove(id) => id.clone(),
        };
        if !latest_by_id.contains_key(&id) {
            order.push(id.clone());
        }
        latest_by_id.insert(id, event);
    }

    order
        .into_iter()
        .filter_map(|id| latest_by_id.remove(&id))
        .collect()
}

fn get_open_windows_with_snapshot_mode(
    kdotool_path: &Path,
    theme: &str,
    include_snapshot_details: bool,
) -> Option<Vec<WindowInfo>> {
    // 1. Fetch all window IDs using kdotool search
    let output = match Command::new(kdotool_path)
        .arg("search")
        .arg("--title")
        .arg("")
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Failed to execute kdotool search: {:?}", e);
            return None;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return None;
    }

    let mut ids = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if !line.is_empty() {
            ids.push(line.to_string());
        }
    }

    if ids.is_empty() {
        return None;
    }

    // 2. Query all window metadata in a single chained kdotool invocation!
    // This reduces process spawning from N*3 down to exactly 1, eliminating startup lag.
    let mut cmd = Command::new(kdotool_path);
    for id in &ids {
        cmd.arg("getwindowid")
            .arg(id)
            .arg("getwindowname")
            .arg(id)
            .arg("getwindowclassname")
            .arg(id)
            .arg("getwindowpid")
            .arg(id);
    }

    let meta_output = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!(
                "Failed to execute chained kdotool metadata command: {:?}",
                e
            );
            return None;
        }
    };

    if !meta_output.status.success() {
        return None;
    }

    let meta_stdout = String::from_utf8_lossy(&meta_output.stdout);
    let lines: Vec<&str> = meta_stdout.lines().collect();

    // 3. Scan /proc once to build process tree before querying PIDs
    let (ppid_to_children, pid_to_name, pid_to_ppid) = get_process_tree();

    let mut windows = Vec::new();
    let theme_str = theme.to_string();
    let mut icon_cache = HashMap::new();

    // Parse blocks of metadata. Since invalid windows get skipped, we search for UUID patterns
    // to identify the start of each valid window's metadata block.
    let mut window_blocks = Vec::new();
    let mut current_block = Vec::new();

    for line in lines {
        let trimmed = line.trim();
        if trimmed.starts_with('{') && trimmed.ends_with('}') {
            if !current_block.is_empty() {
                window_blocks.push(current_block);
            }
            current_block = vec![trimmed.to_string()];
        } else {
            current_block.push(line.to_string());
        }
    }
    if !current_block.is_empty() {
        window_blocks.push(current_block);
    }

    for block in window_blocks {
        if block.is_empty() {
            continue;
        }
        let id = block[0].clone();

        let mut title = String::new();
        let mut class = String::new();
        let mut pid = None;

        if block.len() >= 2 {
            let last_line = block.last().unwrap().trim();
            if let Ok(p) = last_line.parse::<i32>() {
                pid = Some(p);
                if block.len() >= 3 {
                    class = block[block.len() - 2].trim().to_string();
                    if block.len() > 3 {
                        title = block[1..block.len() - 2].join(" ").trim().to_string();
                    }
                }
            } else {
                class = block.get(2).cloned().unwrap_or_default();
                title = block.get(1).cloned().unwrap_or_default();
            }
        }

        let snapshot_details = if include_snapshot_details {
            get_snapshot_window_details(&id)
        } else {
            SnapshotWindowDetails {
                desktop_file_name: None,
                geometry: None,
                minimized: None,
            }
        };

        if let Some(window) = build_window_info(
            id,
            title,
            class,
            snapshot_details.desktop_file_name,
            pid,
            snapshot_details.geometry,
            snapshot_details.minimized,
            &theme_str,
            &mut icon_cache,
            &ppid_to_children,
            &pid_to_name,
            &pid_to_ppid,
        ) {
            windows.push(window);
        }
    }

    Some(windows)
}

fn get_open_windows(kdotool_path: &Path, theme: &str) -> Option<Vec<WindowInfo>> {
    get_open_windows_with_snapshot_mode(kdotool_path, theme, true)
}

fn get_open_windows_fast(kdotool_path: &Path, theme: &str) -> Option<Vec<WindowInfo>> {
    get_open_windows_with_snapshot_mode(kdotool_path, theme, false)
}

fn load_pinned_apps() -> Vec<PathBuf> {
    let mut pinned = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        let path = PathBuf::from(format!(
            "{}/.config/applicationlauncher/pinned_apps.txt",
            home
        ));
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(path) {
                for line in content.lines() {
                    let line = line.trim();
                    if !line.is_empty() {
                        let p = PathBuf::from(line);
                        if !pinned.contains(&p) {
                            pinned.push(p);
                        }
                    }
                }
            }
        }
    }
    pinned
}

fn get_window_geometry(kpath: &Path, id: &str) -> Option<(f32, f32, f32, f32)> {
    let output = Command::new(kpath)
        .args(["getwindowgeometry", id])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut x = None;
    let mut y = None;
    let mut width = None;
    let mut height = None;

    for line in stdout.lines() {
        let line = line.trim();
        if line.starts_with("Position:") {
            let pos_part = line.strip_prefix("Position:")?.trim();
            let coords: Vec<&str> = pos_part.split(',').collect();
            if coords.len() >= 2 {
                x = coords[0].parse::<f32>().ok();
                y = coords[1].parse::<f32>().ok();
            }
        } else if line.starts_with("Geometry:") {
            let geom_part = line.strip_prefix("Geometry:")?.trim();
            let dims: Vec<&str> = geom_part.split('x').collect();
            if dims.len() >= 2 {
                width = dims[0].parse::<f32>().ok();
                height = dims[1].parse::<f32>().ok();
            }
        }
    }

    Some((x?, y?, width?, height?))
}

fn get_snapshot_window_details(id: &str) -> SnapshotWindowDetails {
    let output = Command::new("qdbus6")
        .args(["org.kde.KWin", "/KWin", "org.kde.KWin.getWindowInfo", id])
        .output();

    let Ok(output) = output else {
        return SnapshotWindowDetails {
            desktop_file_name: None,
            geometry: None,
            minimized: None,
        };
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut desktop_file_name = None;
    let mut minimized = None;
    let mut x = None;
    let mut y = None;
    let mut width = None;
    let mut height = None;

    for line in stdout.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("desktopFile:") {
            let value = value.trim();
            if !value.is_empty() {
                desktop_file_name = Some(value.to_string());
            }
        } else if let Some(value) = line.strip_prefix("minimized:") {
            minimized = match value.trim() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            };
        } else if let Some(value) = line.strip_prefix("x:") {
            x = value.trim().parse::<f64>().ok().map(|v| v.round() as i32);
        } else if let Some(value) = line.strip_prefix("y:") {
            y = value.trim().parse::<f64>().ok().map(|v| v.round() as i32);
        } else if let Some(value) = line.strip_prefix("width:") {
            width = value.trim().parse::<f64>().ok().map(|v| v.round() as i32);
        } else if let Some(value) = line.strip_prefix("height:") {
            height = value.trim().parse::<f64>().ok().map(|v| v.round() as i32);
        }
    }

    SnapshotWindowDetails {
        desktop_file_name,
        geometry: match (x, y, width, height) {
            (Some(x), Some(y), Some(width), Some(height)) if width > 0 && height > 0 => {
                Some((x, y, width, height))
            }
            _ => None,
        },
        minimized,
    }
}

impl App {
    fn new(
        cc: &eframe::CreationContext<'_>,
        close_on_blur: bool,
        force_theme: Option<String>,
        mode: LauncherMode,
        icon_only: bool,
        ui_event_rx: std::sync::mpsc::Receiver<UiEvent>,
    ) -> Self {
        // Install loaders to enable SVG and PNG image support
        egui_extras::install_image_loaders(&cc.egui_ctx);

        setup_system_fonts(&cc.egui_ctx);

        // Styling the theme for custom dark acrylic style
        let mut visuals = egui::Visuals::dark();
        visuals.window_corner_radius = egui::CornerRadius::same(12);
        visuals.widgets.active.corner_radius = egui::CornerRadius::same(8);
        visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(8);
        visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(8);

        visuals.widgets.inactive.weak_bg_fill =
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 6);
        visuals.widgets.hovered.weak_bg_fill =
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 16);
        visuals.widgets.active.weak_bg_fill =
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30);
        visuals.override_text_color = Some(egui::Color32::WHITE);

        cc.egui_ctx.set_visuals(visuals);

        let kdotool_path = get_kdotool_path();
        let (width, height) = load_window_size();
        let pinned_apps = load_pinned_apps();
        let settings = load_launcher_settings();

        let (window_tx, window_rx) = std::sync::mpsc::channel();
        let (window_feed_tx, window_feed_rx) = std::sync::mpsc::channel();
        let (audio_cache_tx, audio_cache_rx) = std::sync::mpsc::channel();
        let (kwin_window_feed_setup_tx, kwin_window_feed_setup_rx) = std::sync::mpsc::channel();
        let rapid_polling = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let audio_repaint_ctx = cc.egui_ctx.clone();
        let kwin_window_feed_repaint_ctx = cc.egui_ctx.clone();

        std::thread::spawn(move || {
            let result = setup_kwin_window_feed(window_feed_tx, kwin_window_feed_repaint_ctx);
            let _ = kwin_window_feed_setup_tx.send(result);
        });

        let now = Instant::now();
        let mut app = Self {
            mode,
            windows: Vec::new(),
            apps: Vec::new(),
            pinned_apps,
            search_query: String::new(),
            selected_index: 0,
            side_panel_selected_index: 0,
            active_pane: if mode == LauncherMode::Windows {
                ActivePane::Windows
            } else {
                ActivePane::Apps
            },
            rendered_app_grid_columns: 1,
            rendered_side_panel_grid_columns: 1,
            rendered_window_row_centers: Vec::new(),
            rendered_side_panel_item_centers: Vec::new(),
            scroll_to_first_window_on_focus: false,
            kdotool_path: Some(kdotool_path),
            error_message: None,
            start_time: now,
            search_focus_until: Some(now + Duration::from_millis(1200)),
            close_on_blur,
            force_theme,
            loading: false,
            receiver: None,
            background_apps_receiver: None,
            background_window_enrichment_receiver: None,
            ui_event_rx,
            kwin_window_feed_setup_rx: Some(kwin_window_feed_setup_rx),
            width,
            height,
            icon_only: icon_only || settings.app_icon_mode,
            show_settings_menu: false,
            show_system_settings_modules: settings.show_system_settings_modules,
            win_icon_size: settings.win_icon_size,
            win_padding: settings.win_padding,
            win_row_height: settings.win_row_height,
            win_text_spacing: settings.win_text_spacing,
            win_line_height: settings.win_line_height,
            win_show_path: settings.win_show_path,
            win_title_size: settings.win_title_size,
            win_path_size: settings.win_path_size,
            app_icon_size: settings.app_icon_size,
            app_icon_tile_size: settings.app_icon_tile_size,
            app_icon_show_name: settings.app_icon_show_name,
            app_icon_name_size: settings.app_icon_name_size,
            disable_ibeam: settings.disable_ibeam,
            process_chain_popup: None,
            window_sender: window_tx.clone(),
            window_receiver: window_rx,
            window_feed_receiver: window_feed_rx,
            audio_cache_receiver: audio_cache_rx,
            rapid_polling: std::sync::Arc::clone(&rapid_polling),
            last_selected_window_id: None,
            missing_window_counts: HashMap::new(),
            use_kwin_window_feed: false,
            window_polling_started: false,
            cached_sink_inputs: Vec::new(),
            active_media_app_keys: HashSet::new(),
            observed_pipewire_node_ids: HashSet::new(),
            active_pipewire_node_ids: HashSet::new(),
            pipewire_activity_cache_valid: false,
            app_scroll_sensitivity: settings.app_scroll_sensitivity,
            win_scroll_sensitivity: settings.win_scroll_sensitivity,
            last_stale_prune: None,
            filtered_search_cache: None,
            apps_generation: 0,
            windows_generation: 0,
            pinned_apps_generation: 0,
        };

        std::thread::spawn(move || {
            let mut recent_active_pipewire_nodes: HashMap<u32, std::time::Instant> = HashMap::new();
            loop {
                let sink_inputs = fetch_sink_inputs();
                let active_media_app_keys = fetch_active_media_app_keys();
                let (
                    observed_pipewire_node_ids,
                    active_pipewire_node_ids,
                    pipewire_activity_cache_valid,
                ) = fetch_pipewire_activity();
                let now = std::time::Instant::now();

                if pipewire_activity_cache_valid {
                    for id in active_pipewire_node_ids {
                        recent_active_pipewire_nodes.insert(id, now);
                    }
                    recent_active_pipewire_nodes.retain(|_, last_seen| {
                        now.duration_since(*last_seen).as_millis() <= AUDIO_ACTIVITY_GRACE_MS
                    });
                } else {
                    recent_active_pipewire_nodes.clear();
                }

                let effective_active_pipewire_node_ids = recent_active_pipewire_nodes
                    .keys()
                    .copied()
                    .collect::<HashSet<u32>>();

                if audio_cache_tx
                    .send(AudioCacheUpdate {
                        sink_inputs,
                        active_media_app_keys,
                        observed_pipewire_node_ids,
                        active_pipewire_node_ids: effective_active_pipewire_node_ids,
                        pipewire_activity_cache_valid,
                    })
                    .is_err()
                {
                    break;
                }
                audio_repaint_ctx.request_repaint();
                std::thread::sleep(std::time::Duration::from_millis(AUDIO_SINK_POLL_MS as u64));
            }
        });

        match app.mode {
            LauncherMode::Apps => app.refresh_apps(),
            LauncherMode::Windows => {
                app.refresh_windows();
                app.start_background_app_load();
                app.start_background_window_enrichment();
            }
        }

        app
    }

    fn save_window_size(&self) {
        if let Ok(home) = std::env::var("HOME") {
            let dir = PathBuf::from(format!("{}/.config/applicationlauncher", home));
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join("window_size.txt");
            let content = format!("{}\n{}", self.width, self.height);
            let _ = std::fs::write(path, content);
        }
    }

    fn save_settings(&self) {
        save_launcher_settings(LauncherSettings {
            show_system_settings_modules: self.show_system_settings_modules,
            app_icon_mode: self.icon_only,
            win_icon_size: self.win_icon_size,
            win_padding: self.win_padding,
            win_row_height: self.win_row_height,
            win_text_spacing: self.win_text_spacing,
            win_line_height: self.win_line_height,
            win_show_path: self.win_show_path,
            win_title_size: self.win_title_size,
            win_path_size: self.win_path_size,
            app_icon_size: self.app_icon_size,
            app_icon_tile_size: self.app_icon_tile_size,
            app_icon_show_name: self.app_icon_show_name,
            app_icon_name_size: self.app_icon_name_size,
            disable_ibeam: self.disable_ibeam,
            app_scroll_sensitivity: self.app_scroll_sensitivity,
            win_scroll_sensitivity: self.win_scroll_sensitivity,
        });
    }

    fn save_pinned_apps(&mut self) {
        if let Ok(home) = std::env::var("HOME") {
            let dir = PathBuf::from(format!("{}/.config/applicationlauncher", home));
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join("pinned_apps.txt");
            let mut content = String::new();
            for p in &self.pinned_apps {
                content.push_str(&p.to_string_lossy());
                content.push('\n');
            }
            let _ = std::fs::write(path, content);
        }
        self.pinned_apps_generation = self.pinned_apps_generation.wrapping_add(1);
    }

    fn render_settings_panel(&mut self, ui: &mut egui::Ui) -> bool {
        let mut close_requested = false;

        ui.add_space(8.0);
        ui.vertical_centered(|ui| {
            ui.heading(
                egui::RichText::new("Launcher Settings")
                    .color(egui::Color32::WHITE)
                    .strong(),
            );
        });
        ui.add_space(8.0);

        ui.add(egui::Separator::default());
        ui.add_space(12.0);

        ui.horizontal(|ui| {
            let mut disable_ibeam = self.disable_ibeam;
            let checkbox_response = ui.checkbox(
                &mut disable_ibeam,
                egui::RichText::new("Disable text select cursor (I-beam)")
                    .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 220))
                    .size(13.0),
            );
            if checkbox_response.changed() {
                self.disable_ibeam = disable_ibeam;
                self.save_settings();
            }
        });

        ui.add_space(14.0);
        ui.add(egui::Separator::default());
        ui.add_space(10.0);

        ui.label(
            egui::RichText::new("Application panel")
                .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 220))
                .strong()
                .size(13.0),
        );
        ui.add_space(6.0);

        egui::Grid::new("app_panel_settings_grid")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new("Show System Modules:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut show_val = self.show_system_settings_modules;
                if ui.checkbox(&mut show_val, "").changed() {
                    self.show_system_settings_modules = show_val;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Icon Grid Mode:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut icon_mode = self.icon_only;
                if ui.checkbox(&mut icon_mode, "").changed() {
                    self.icon_only = icon_mode;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Icon Size:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut app_icon_size = self.app_icon_size;
                if ui
                    .add(egui::Slider::new(&mut app_icon_size, 16.0..=64.0).show_value(true))
                    .changed()
                {
                    self.app_icon_size = app_icon_size;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Tile Size:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut app_tile_size = self.app_icon_tile_size;
                if ui
                    .add(egui::Slider::new(&mut app_tile_size, 48.0..=128.0).show_value(true))
                    .changed()
                {
                    self.app_icon_tile_size = app_tile_size;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Show Names:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut app_icon_show_name = self.app_icon_show_name;
                if ui.checkbox(&mut app_icon_show_name, "").changed() {
                    self.app_icon_show_name = app_icon_show_name;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Name Size:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut app_icon_name_size = self.app_icon_name_size;
                if ui
                    .add(egui::Slider::new(&mut app_icon_name_size, 8.0..=20.0).show_value(true))
                    .changed()
                {
                    self.app_icon_name_size = app_icon_name_size;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Scroll Sensitivity:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut app_scroll_sens = self.app_scroll_sensitivity;
                if ui
                    .add(egui::Slider::new(&mut app_scroll_sens, 0.1..=5.0).show_value(true))
                    .changed()
                {
                    self.app_scroll_sensitivity = app_scroll_sens;
                    self.save_settings();
                }
                ui.end_row();
            });

        ui.add_space(14.0);
        ui.add(egui::Separator::default());
        ui.add_space(10.0);

        ui.label(
            egui::RichText::new("Open window view")
                .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 220))
                .strong()
                .size(13.0),
        );
        ui.add_space(6.0);

        egui::Grid::new("win_settings_grid")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new("Icon Size:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut icon_size = self.win_icon_size;
                if ui
                    .add(egui::Slider::new(&mut icon_size, 16.0..=64.0).show_value(true))
                    .changed()
                {
                    self.win_icon_size = icon_size;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Padding:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut padding = self.win_padding;
                if ui
                    .add(egui::Slider::new(&mut padding, 0.0..=24.0).show_value(true))
                    .changed()
                {
                    self.win_padding = padding;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Row Height:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut row_height = self.win_row_height;
                if ui
                    .add(egui::Slider::new(&mut row_height, 30.0..=100.0).show_value(true))
                    .changed()
                {
                    self.win_row_height = row_height;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Text Spacing:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut text_spacing = self.win_text_spacing;
                if ui
                    .add(egui::Slider::new(&mut text_spacing, 0.0..=12.0).show_value(true))
                    .changed()
                {
                    self.win_text_spacing = text_spacing;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Line Height:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut line_height = self.win_line_height;
                if ui
                    .add(egui::Slider::new(&mut line_height, 8.0..=30.0).show_value(true))
                    .changed()
                {
                    self.win_line_height = line_height;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Show Path:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut show_path = self.win_show_path;
                if ui.checkbox(&mut show_path, "").changed() {
                    self.win_show_path = show_path;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Title Size:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut title_size = self.win_title_size;
                if ui
                    .add(egui::Slider::new(&mut title_size, 8.0..=24.0).show_value(true))
                    .changed()
                {
                    self.win_title_size = title_size;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Path Size:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut path_size = self.win_path_size;
                if ui
                    .add(egui::Slider::new(&mut path_size, 8.0..=20.0).show_value(true))
                    .changed()
                {
                    self.win_path_size = path_size;
                    self.save_settings();
                }
                ui.end_row();

                ui.label(
                    egui::RichText::new("Scroll Sensitivity:")
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
                let mut win_scroll_sens = self.win_scroll_sensitivity;
                if ui
                    .add(egui::Slider::new(&mut win_scroll_sens, 0.1..=5.0).show_value(true))
                    .changed()
                {
                    self.win_scroll_sensitivity = win_scroll_sens;
                    self.save_settings();
                }
                ui.end_row();
            });

        ui.add_space(16.0);
        ui.vertical_centered(|ui| {
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("Close Settings (F10)")
                            .color(egui::Color32::WHITE)
                            .size(13.0),
                    )
                    .fill(egui::Color32::from_rgba_unmultiplied(61, 174, 233, 200)),
                )
                .clicked()
            {
                close_requested = true;
            }
        });
        ui.add_space(8.0);

        close_requested
    }

    fn show_settings_popup(&mut self, ctx: &egui::Context) {
        let viewport_id = egui::ViewportId::from_hash_of("launcher_settings_popup");
        let builder = egui::ViewportBuilder::default()
            .with_title("Launcher Settings")
            .with_inner_size([380.0, 760.0])
            .with_min_inner_size([340.0, 500.0])
            .with_resizable(true);

        let mut should_close = false;
        ctx.show_viewport_immediate(viewport_id, builder, |ctx, _class| {
            if ctx.input(|i| i.viewport().close_requested()) {
                should_close = true;
                return;
            }

            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                should_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }

            egui::CentralPanel::default()
                .frame(
                    egui::Frame::window(&ctx.style())
                        .fill(egui::Color32::from_rgba_unmultiplied(20, 20, 20, 240))
                        .stroke(egui::Stroke::new(
                            1.0,
                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20),
                        ))
                        .corner_radius(egui::CornerRadius::same(12)),
                )
                .show(ctx, |ui| {
                    if self.render_settings_panel(ui) {
                        should_close = true;
                    }
                });
        });

        if should_close {
            self.show_settings_menu = false;
        }
    }

    fn show_window_info_popup(&mut self, ctx: &egui::Context) {
        let Some(window_info) = self.process_chain_popup.clone() else {
            return;
        };

        let viewport_id = egui::ViewportId::from_hash_of("launcher_process_chain_popup");
        let builder = egui::ViewportBuilder::default()
            .with_title(format!("Window Info: {}", window_info.title))
            .with_inner_size([760.0, 680.0])
            .with_min_inner_size([520.0, 360.0])
            .with_resizable(true);

        let mut should_close = false;
        ctx.show_viewport_immediate(viewport_id, builder, |ctx, _class| {
            if ctx.input(|i| i.viewport().close_requested()) {
                should_close = true;
                return;
            }

            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                should_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }

            egui::CentralPanel::default()
                .frame(
                    egui::Frame::window(&ctx.style())
                        .fill(egui::Color32::from_rgba_unmultiplied(20, 20, 20, 240))
                        .stroke(egui::Stroke::new(
                            1.0,
                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20),
                        ))
                        .corner_radius(egui::CornerRadius::same(12)),
                )
                .show(ctx, |ui| {
                    let searchable_label_color = egui::Color32::from_rgb(214, 184, 86);
                    let searchable_value_color = egui::Color32::from_rgb(255, 236, 170);
                    let neutral_label_color =
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 170);
                    let neutral_value_color = egui::Color32::WHITE;
                    let app_key = window_application_key(&window_info);
                    let exe_basename = window_info
                        .exe_path
                        .as_ref()
                        .and_then(|path| path.file_name().and_then(|name| name.to_str()))
                        .map(|name| name.to_string())
                        .unwrap_or_else(|| "Unavailable".to_string());
                    let desktop_file_path = self
                        .desktop_file_path_for_window(&window_info)
                        .map(|path| path.to_string_lossy().to_string())
                        .unwrap_or_else(|| "Unavailable".to_string());
                    let cwd_search_value = window_info
                        .cwd_path
                        .as_ref()
                        .map(|path| display_path(path))
                        .unwrap_or_else(|| "Unavailable".to_string());
                    let class_is_searched = !window_info.class.eq_ignore_ascii_case(&app_key);

                    let info_row =
                        |ui: &mut egui::Ui, label: &str, value: String, searched: bool| {
                            let label_color = if searched {
                                searchable_label_color
                            } else {
                                neutral_label_color
                            };
                            let value_color = if searched {
                                searchable_value_color
                            } else {
                                neutral_value_color
                            };
                            ui.label(egui::RichText::new(label).color(label_color).strong());
                            ui.label(egui::RichText::new(value).color(value_color).monospace());
                            ui.end_row();
                        };

                    ui.heading(
                        egui::RichText::new(&window_info.title)
                            .color(egui::Color32::WHITE)
                            .strong(),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(
                            "Window metadata, process details, and execution chain",
                        )
                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 170)),
                    );
                    ui.add_space(10.0);

                    egui::Grid::new("window_info_grid")
                        .num_columns(2)
                        .spacing([14.0, 8.0])
                        .striped(true)
                        .show(ui, |ui| {
                            info_row(ui, "Title", window_info.title.clone(), true);
                            info_row(ui, "Application key", app_key.clone(), true);
                            info_row(ui, "Window ID", window_info.id.clone(), false);
                            info_row(ui, "Class", window_info.class.clone(), class_is_searched);
                            info_row(ui, "Desktop file", desktop_file_path, false);
                            info_row(
                                ui,
                                "PID",
                                window_info
                                    .pid
                                    .map(|pid| pid.to_string())
                                    .unwrap_or_else(|| "Unavailable".to_string()),
                                false,
                            );
                            info_row(
                                ui,
                                "Active process",
                                window_info
                                    .active_process
                                    .clone()
                                    .unwrap_or_else(|| "Unavailable".to_string()),
                                true,
                            );
                            info_row(ui, "Executable basename", exe_basename, true);
                            info_row(
                                ui,
                                "Executable path",
                                window_info
                                    .exe_path
                                    .as_ref()
                                    .map(|path| path.to_string_lossy().to_string())
                                    .unwrap_or_else(|| "Unavailable".to_string()),
                                false,
                            );
                            info_row(ui, "Working directory", cwd_search_value, true);
                            info_row(
                                ui,
                                "Command summary",
                                window_info
                                    .command_summary
                                    .clone()
                                    .unwrap_or_else(|| "Unavailable".to_string()),
                                true,
                            );
                            info_row(
                                ui,
                                "Command line",
                                window_info
                                    .command_line
                                    .clone()
                                    .unwrap_or_else(|| "Unavailable".to_string()),
                                true,
                            );
                            info_row(
                                ui,
                                "Geometry",
                                window_info
                                    .geometry
                                    .map(|(x, y, width, height)| {
                                        format!(
                                            "x={}, y={}, width={}, height={}",
                                            x, y, width, height
                                        )
                                    })
                                    .unwrap_or_else(|| "Unavailable".to_string()),
                                false,
                            );
                            info_row(
                                ui,
                                "Minimized",
                                window_info
                                    .minimized
                                    .map(|value| value.to_string())
                                    .unwrap_or_else(|| "Unavailable".to_string()),
                                false,
                            );
                        });

                    ui.add_space(14.0);
                    ui.separator();
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new("Execution chain")
                            .color(egui::Color32::WHITE)
                            .strong(),
                    );
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for entry in &window_info.process_chain {
                            ui.group(|ui| {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} (pid {})",
                                        entry.name, entry.pid
                                    ))
                                    .color(egui::Color32::WHITE)
                                    .strong(),
                                );
                                let path_text = entry
                                    .exe_path
                                    .as_ref()
                                    .map(|path| path.to_string_lossy().to_string())
                                    .unwrap_or_else(|| "Executable path unavailable".to_string());
                                ui.label(egui::RichText::new(path_text).color(
                                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 160),
                                ));
                            });
                            ui.add_space(6.0);
                        }
                    });
                });
        });

        if should_close {
            self.process_chain_popup = None;
        }
    }

    fn apply_window_snapshot(&mut self, new_windows: Vec<WindowInfo>) {
        if self.windows.is_empty() {
            self.windows = new_windows;
            self.missing_window_counts.clear();
            self.windows_generation = self.windows_generation.wrapping_add(1);
            return;
        }

        let mut new_by_id: HashMap<String, WindowInfo> = new_windows
            .iter()
            .cloned()
            .map(|window| (window.id.clone(), window))
            .collect();
        let old_ids: HashSet<String> = self
            .windows
            .iter()
            .map(|window| window.id.clone())
            .collect();
        let mut merged = Vec::new();

        for old_window in &self.windows {
            if let Some(new_window) = new_by_id.remove(&old_window.id) {
                self.missing_window_counts.remove(&old_window.id);
                merged.push(new_window);
                continue;
            }

            let missing_count = self
                .missing_window_counts
                .entry(old_window.id.clone())
                .or_insert(0);
            *missing_count += 1;

            if *missing_count < WINDOW_REMOVAL_CONFIRMATION_POLLS {
                merged.push(old_window.clone());
            }
        }

        for new_window in new_windows {
            if !old_ids.contains(&new_window.id) {
                self.missing_window_counts.remove(&new_window.id);
                merged.push(new_window);
            }
        }

        let retained_ids: HashSet<String> = merged.iter().map(|window| window.id.clone()).collect();
        self.missing_window_counts
            .retain(|window_id, _| retained_ids.contains(window_id));
        self.windows = merged;
        self.windows_generation = self.windows_generation.wrapping_add(1);
    }

    fn apply_window_feed_events(&mut self, events: Vec<WindowFeedEvent>) {
        if events.is_empty() {
            return;
        }

        let events = coalesce_window_feed_events(events);
        let theme = self
            .force_theme
            .as_deref()
            .unwrap_or("breeze-dark")
            .to_string();
        let (ppid_to_children, pid_to_name, pid_to_ppid) = get_process_tree();
        let mut changed = false;

        for event in events {
            match event {
                WindowFeedEvent::Upsert(payload) => {
                    if let Some(window) = window_info_from_kwin_payload(
                        payload,
                        &theme,
                        &ppid_to_children,
                        &pid_to_name,
                        &pid_to_ppid,
                    ) {
                        self.missing_window_counts.remove(&window.id);
                        if let Some(existing) =
                            self.windows.iter_mut().find(|item| item.id == window.id)
                        {
                            *existing = window;
                        } else {
                            self.windows.push(window);
                        }
                        changed = true;
                    }
                }
                WindowFeedEvent::Remove(id) => {
                    self.missing_window_counts.remove(&id);
                    let previous_len = self.windows.len();
                    self.windows.retain(|window| window.id != id);
                    changed |= self.windows.len() != previous_len;
                }
            }
        }

        if changed {
            self.windows_generation = self.windows_generation.wrapping_add(1);
        }
    }

    fn prune_stale_windows(&mut self) {
        let now = Instant::now();
        if self
            .last_stale_prune
            .is_some_and(|last| now.duration_since(last) < std::time::Duration::from_secs(1))
        {
            return;
        }
        self.last_stale_prune = Some(now);

        let stale_ids: HashSet<String> = self
            .windows
            .iter()
            .filter(|window| window.pid.is_some_and(|pid| !process_exists(pid)))
            .map(|window| window.id.clone())
            .collect();

        if stale_ids.is_empty() {
            return;
        }

        self.windows
            .retain(|window| !stale_ids.contains(&window.id));
        self.missing_window_counts
            .retain(|window_id, _| !stale_ids.contains(window_id));
        self.windows_generation = self.windows_generation.wrapping_add(1);

        if self
            .last_selected_window_id
            .as_ref()
            .is_some_and(|window_id| stale_ids.contains(window_id))
        {
            self.last_selected_window_id = None;
        }
    }

    fn refresh_windows(&mut self) {
        if let Some(ref kpath) = self.kdotool_path {
            let kpath = kpath.clone();
            let theme = self
                .force_theme
                .as_deref()
                .unwrap_or("breeze-dark")
                .to_string();
            let (tx, rx) = std::sync::mpsc::channel();
            self.loading = true;
            self.receiver = Some(rx);

            std::thread::spawn(
                move || match Command::new(&kpath).arg("--version").output() {
                    Ok(_) => {
                        let windows = get_open_windows_fast(&kpath, &theme).unwrap_or_default();
                        let _ = tx.send(LoadResult::WindowsSuccess(windows));
                    }
                    Err(_) => {
                        let _ = tx.send(LoadResult::Error(format!(
                            "kdotool utility not found.\n\nPlease install it using cargo:\n\ncargo install kdotool"
                        )));
                    }
                },
            );
        }
    }

    fn start_background_window_enrichment(&mut self) {
        let Some(kpath) = self.kdotool_path.clone() else {
            self.background_window_enrichment_receiver = None;
            return;
        };
        let theme = self
            .force_theme
            .as_deref()
            .unwrap_or("breeze-dark")
            .to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        self.background_window_enrichment_receiver = Some(rx);

        std::thread::spawn(move || {
            let windows = get_open_windows(&kpath, &theme).unwrap_or_default();
            let _ = tx.send(windows);
        });
    }

    fn start_background_app_load(&mut self) {
        let theme = self
            .force_theme
            .as_deref()
            .unwrap_or("breeze-dark")
            .to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        self.background_apps_receiver = Some(rx);

        std::thread::spawn(move || {
            let apps = get_installed_apps(&theme);
            let _ = tx.send(apps);
        });
    }

    fn refresh_apps(&mut self) {
        let theme = self
            .force_theme
            .as_deref()
            .unwrap_or("breeze-dark")
            .to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        self.loading = true;
        self.receiver = Some(rx);

        std::thread::spawn(move || {
            let apps = get_installed_apps(&theme);
            let _ = tx.send(LoadResult::AppsSuccess(apps));
        });
    }

    fn start_window_polling_thread(&mut self, ctx: &egui::Context) {
        if self.window_polling_started {
            return;
        }
        self.window_polling_started = true;

        let Some(kpath) = self.kdotool_path.clone() else {
            return;
        };
        let theme = self
            .force_theme
            .as_deref()
            .unwrap_or("breeze-dark")
            .to_string();
        let rapid_polling_thread = std::sync::Arc::clone(&self.rapid_polling);
        let window_tx = self.window_sender.clone();
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            let mut rapid_poll_count = 0;
            loop {
                if rapid_polling_thread.load(std::sync::atomic::Ordering::SeqCst) {
                    rapid_polling_thread.store(false, std::sync::atomic::Ordering::SeqCst);
                    rapid_poll_count = 15; // 15 * 300ms = 4.5 seconds of rapid polling
                }

                let sleep_dur = if rapid_poll_count > 0 {
                    rapid_poll_count -= 1;
                    std::time::Duration::from_millis(300)
                } else {
                    std::time::Duration::from_millis(1000)
                };

                std::thread::sleep(sleep_dur);

                if let Some(windows) = get_open_windows(&kpath, &theme) {
                    if window_tx.send(windows).is_ok() {
                        ctx.request_repaint();
                    } else {
                        break;
                    }
                }
            }
        });
    }

    fn launch_app_and_exit(&self, app: &AppInfo, ctx: &egui::Context) {
        self.rapid_polling
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if is_dolphin_app(app) {
            launch_dolphin_app();
        } else if !launch_desktop_entry(&app.desktop_file_path) {
            launch_app(&app.exec);
        }
        ctx.request_repaint();
    }

    fn find_app_for_window<'a>(&'a self, win: &WindowInfo) -> Option<&'a AppInfo> {
        let mut window_keys = Vec::new();

        let class = win.class.trim();
        if !class.is_empty() {
            window_keys.push(normalize_app_match_key(class));
            if let Some(last_segment) = class.rsplit('.').next() {
                window_keys.push(normalize_app_match_key(last_segment));
            }
        }

        if let Some(path) = &win.exe_path {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                window_keys.push(normalize_app_match_key(name));
            }
        }

        if let Some(proc_name) = &win.active_process {
            window_keys.push(normalize_app_match_key(proc_name));
        }

        window_keys.retain(|key| !key.is_empty());
        if window_keys.is_empty() {
            return None;
        }

        self.apps
            .iter()
            .filter_map(|app| best_app_match_score(&window_keys, app).map(|score| (app, score)))
            .min_by_key(|(app, score)| (*score, app.is_settings_module))
            .map(|(app, _)| app)
    }

    fn desktop_file_path_for_window(&self, win: &WindowInfo) -> Option<PathBuf> {
        if let Some(path) = win
            .desktop_file_name
            .as_deref()
            .and_then(resolve_desktop_file_path)
        {
            return Some(path);
        }

        self.find_app_for_window(win)
            .map(|app| app.desktop_file_path.clone())
    }

    fn launch_window_app_and_exit(&self, win: &WindowInfo, ctx: &egui::Context) {
        self.rapid_polling
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if is_terminal_class(&win.class.to_lowercase()) {
            launch_terminal_window();
            ctx.request_repaint();
            return;
        }

        if let Some(exe_path) = &win.exe_path {
            let exe = exe_path.clone();
            std::thread::spawn(move || {
                let mut cmd = Command::new(exe);
                cmd.env_remove("PYTHONPATH");
                cmd.env_remove("PYTHONHOME");
                cmd.env_remove("VIRTUAL_ENV");
                cmd.env_remove("UV_ACTIVE");
                let _ = cmd.spawn();
            });
            ctx.request_repaint();
            return;
        }

        if let Some(proc_name) = &win.active_process {
            launch_app(proc_name);
            ctx.request_repaint();
            return;
        }

        if let Some(app) = self.find_app_for_window(win) {
            self.launch_app_and_exit(app, ctx);
        }
    }

    fn clone_window_and_exit(&self, win: &WindowInfo, ctx: &egui::Context) {
        self.rapid_polling
            .store(true, std::sync::atomic::Ordering::SeqCst);

        if is_terminal_class(&win.class.to_lowercase()) {
            let cwd = win
                .cwd_path
                .as_ref()
                .map(|path| path.to_string_lossy().to_string());
            launch_fish_terminal(
                cwd,
                clone_terminal_command_for_window(win),
                Some(source_terminal_title_for_clone(win)),
            );
            ctx.request_repaint();
            return;
        }

        if is_pcmanfm_window(win) && clone_pcmanfm_window(win) {
            ctx.request_repaint();
            return;
        }

        if is_dolphin_window(win) && clone_dolphin_window(win) {
            ctx.request_repaint();
            return;
        }

        if is_chrome_like_window(win) && clone_chrome_window(win) {
            ctx.request_repaint();
            return;
        }

        self.launch_window_app_and_exit(win, ctx);
    }

    fn activate_and_exit(&self, id: String, ctx: &egui::Context) {
        if let Some(ref kpath) = self.kdotool_path {
            let kpath = kpath.clone();
            let id_clone = id.clone();
            std::thread::spawn(move || {
                // Query geometry first
                let geom = get_window_geometry(&kpath, &id_clone);

                // Activate the window and raise it to make sure it comes to the top
                let _ = Command::new(&kpath)
                    .args(["windowactivate", &id_clone, "windowraise", &id_clone])
                    .status();

                // If we got geometry, spawn the border overlay process
                if let Some((x, y, w, h)) = geom {
                    if let Ok(current_exe) = std::env::current_exe() {
                        let _ = Command::new(current_exe)
                            .args([
                                "--draw-border",
                                &x.to_string(),
                                &y.to_string(),
                                &w.to_string(),
                                &h.to_string(),
                                &id_clone,
                            ])
                            .spawn();
                    }
                }
            });
        }
        ctx.request_repaint();
    }

    fn close_window_and_exit(&self, id: String, ctx: &egui::Context) {
        self.rapid_polling
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(ref kpath) = self.kdotool_path {
            let kpath = kpath.clone();
            std::thread::spawn(move || {
                let _ = Command::new(&kpath).args(["windowclose", &id]).status();
            });
        }
        ctx.request_repaint();
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Track window size changes in memory
        let current_size = ctx.viewport_rect().size();
        if (current_size.x - self.width).abs() > 1.0 || (current_size.y - self.height).abs() > 1.0 {
            self.width = current_size.x;
            self.height = current_size.y;
        }

        let mut handled_focus_launcher = false;
        let mut ui_event_count = 0;
        for _ in 0..UI_EVENTS_PER_FRAME {
            let Ok(event) = self.ui_event_rx.try_recv() else {
                break;
            };
            ui_event_count += 1;
            match event {
                UiEvent::FocusLauncher => {
                    handled_focus_launcher = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                        egui::WindowLevel::AlwaysOnTop,
                    ));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                        egui::UserAttentionType::Informational,
                    ));
                    request_launcher_foreground();
                    self.search_focus_until = Some(Instant::now() + Duration::from_millis(1200));
                    self.search_query.clear();
                    self.selected_index = 0;
                    self.side_panel_selected_index = 0;
                    self.last_selected_window_id = None;
                    self.scroll_to_first_window_on_focus = self.mode == LauncherMode::Windows;
                    self.active_pane = if self.mode == LauncherMode::Windows {
                        ActivePane::Windows
                    } else {
                        ActivePane::Apps
                    };
                }
            }
        }
        if ui_event_count == UI_EVENTS_PER_FRAME || handled_focus_launcher {
            ctx.request_repaint();
        }

        if let Some(result) = self
            .kwin_window_feed_setup_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.kwin_window_feed_setup_rx = None;
            match result {
                Ok(()) => {
                    self.use_kwin_window_feed = true;
                    ctx.request_repaint();
                }
                Err(err) => {
                    eprintln!("Falling back to kdotool window polling: {err}");
                    self.start_window_polling_thread(ctx);
                }
            }
        }

        if !handled_focus_launcher && !self.loading && self.use_kwin_window_feed {
            let mut pending_events = Vec::with_capacity(WINDOW_FEED_EVENTS_PER_FRAME);
            for _ in 0..WINDOW_FEED_EVENTS_PER_FRAME {
                match self.window_feed_receiver.try_recv() {
                    Ok(event) => pending_events.push(event),
                    Err(_) => break,
                }
            }
            let hit_window_feed_budget = pending_events.len() == WINDOW_FEED_EVENTS_PER_FRAME;
            if hit_window_feed_budget {
                ctx.request_repaint();
            }
            self.apply_window_feed_events(pending_events);
        } else if !self.use_kwin_window_feed {
            // Check background receiver for periodic window updates
            let mut latest_windows = None;
            let mut window_snapshot_count = 0;
            for _ in 0..WINDOW_SNAPSHOTS_PER_FRAME {
                match self.window_receiver.try_recv() {
                    Ok(new_windows) => {
                        latest_windows = Some(new_windows);
                        window_snapshot_count += 1;
                    }
                    Err(_) => break,
                }
            }
            if let Some(new_windows) = latest_windows {
                if !self.loading {
                    self.apply_window_snapshot(new_windows);
                }
            }
            if window_snapshot_count == WINDOW_SNAPSHOTS_PER_FRAME {
                ctx.request_repaint();
            }
        }

        if !handled_focus_launcher {
            match self
                .background_window_enrichment_receiver
                .as_ref()
                .map(|rx| rx.try_recv())
            {
                Some(Ok(windows)) => {
                    self.apply_window_snapshot(windows);
                    self.background_window_enrichment_receiver = None;
                    ctx.request_repaint();
                }
                Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                    self.background_window_enrichment_receiver = None;
                }
                _ => {}
            }
        }

        if !handled_focus_launcher {
            match self
                .background_apps_receiver
                .as_ref()
                .map(|rx| rx.try_recv())
            {
                Some(Ok(apps)) => {
                    self.apps = apps;
                    self.apps_generation = self.apps_generation.wrapping_add(1);
                    self.background_apps_receiver = None;
                    ctx.request_repaint();
                }
                Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                    self.background_apps_receiver = None;
                }
                _ => {}
            }
        }

        // Check background receiver for window query results
        if !handled_focus_launcher && self.loading {
            ctx.request_repaint(); // Keep repainting until loaded to check channel promptly
            if let Some(ref rx) = self.receiver {
                if let Ok(result) = rx.try_recv() {
                    self.loading = false;
                    match result {
                        LoadResult::AppsSuccess(apps) => {
                            self.apps = apps;
                            self.apps_generation = self.apps_generation.wrapping_add(1);
                            self.selected_index = 0;
                            self.side_panel_selected_index = 0;
                            self.active_pane = ActivePane::Apps;
                        }
                        LoadResult::WindowsSuccess(windows) => {
                            self.windows = windows;
                            self.missing_window_counts.clear();
                            self.windows_generation = self.windows_generation.wrapping_add(1);
                            self.selected_index = 0;
                            self.side_panel_selected_index = 0;
                            self.active_pane = ActivePane::Windows;
                        }
                        LoadResult::Error(err) => {
                            self.error_message = Some(err);
                            self.kdotool_path = None;
                        }
                    }
                }
            }
        }

        if !handled_focus_launcher {
            let mut latest_audio_update = None;
            let mut audio_update_count = 0;
            for _ in 0..AUDIO_UPDATES_PER_FRAME {
                match self.audio_cache_receiver.try_recv() {
                    Ok(update) => {
                        latest_audio_update = Some(update);
                        audio_update_count += 1;
                    }
                    Err(_) => break,
                }
            }
            if let Some(update) = latest_audio_update {
                self.cached_sink_inputs = update.sink_inputs;
                self.active_media_app_keys = update.active_media_app_keys;
                self.observed_pipewire_node_ids = update.observed_pipewire_node_ids;
                self.active_pipewire_node_ids = update.active_pipewire_node_ids;
                self.pipewire_activity_cache_valid = update.pipewire_activity_cache_valid;
            }
            if audio_update_count == AUDIO_UPDATES_PER_FRAME {
                ctx.request_repaint();
            }
        }

        let has_active_audio = self.cached_sink_inputs.iter().any(|sink| {
            sink_input_level(
                sink,
                &self.active_media_app_keys,
                &self.observed_pipewire_node_ids,
                &self.active_pipewire_node_ids,
                self.pipewire_activity_cache_valid,
            ) > 0.0
        });
        let audio_repaint_ms = if has_active_audio {
            AUDIO_ACTIVE_REPAINT_MS
        } else {
            AUDIO_IDLE_REPAINT_MS
        };
        ctx.request_repaint_after(std::time::Duration::from_millis(audio_repaint_ms));

        if !handled_focus_launcher {
            self.prune_stale_windows();
        }

        // Focus loss auto-close
        if self.close_on_blur
            && self.start_time.elapsed().as_millis() > 500
            && !ctx.input(|i| i.focused)
            && !self.show_settings_menu
            && self.process_chain_popup.is_none()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        // Main transparent window frame
        let panel_frame = egui::Frame {
            fill: egui::Color32::TRANSPARENT,
            ..Default::default()
        };

        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ctx, |ui| {
                // Translucent acrylic-like container
                let container_frame = egui::Frame {
                    fill: egui::Color32::from_rgba_unmultiplied(22, 23, 27, 240), // Dark glass fill
                    corner_radius: egui::CornerRadius::same(12),
                    stroke: egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18),
                    ),
                    inner_margin: egui::Margin::same(14),
                    ..Default::default()
                };

                container_frame.show(ui, |ui| {
                    if let Some(ref err) = self.error_message {
                        ui.vertical_centered(|ui| {
                            ui.add_space(20.0);
                            ui.label(
                                egui::RichText::new("⚠️ Error")
                                    .color(egui::Color32::from_rgb(218, 68, 83))
                                    .strong()
                                    .size(24.0),
                            );
                            ui.add_space(10.0);
                            ui.label(egui::RichText::new(err).size(14.0));
                            ui.add_space(20.0);
                            if ui.button("Exit").clicked() {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        });
                        return;
                    }

                    // 1. Search Bar Container
                    let search_bar_frame = egui::Frame {
                        fill: egui::Color32::from_rgba_unmultiplied(255, 255, 255, 10),
                        corner_radius: egui::CornerRadius::same(8),
                        inner_margin: egui::Margin::symmetric(12, 8),
                        stroke: egui::Stroke::new(
                            1.0,
                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 15),
                        ),
                        ..Default::default()
                    };

                    let mut text_edit_response = None;
                    let mut search_query_changed = false;

                    search_bar_frame.show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("🔍")
                                    .size(18.0)
                                    .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 160)),
                            );
                            ui.add_space(4.0);
                            let hint_text = match self.mode {
                                LauncherMode::Apps => "Search applications...",
                                LauncherMode::Windows => "Search open windows...",
                            };
                            let search_width = (ui.available_width() - 42.0).max(120.0);
                            let text_edit = egui::TextEdit::singleline(&mut self.search_query)
                                .hint_text(hint_text)
                                .desired_width(search_width)
                                .frame(false)
                                .font(egui::FontId::proportional(16.0));

                            let response = ui.add(text_edit);
                            search_query_changed = response.changed();
                            text_edit_response = Some(response);
                            ui.add_space(8.0);
                            if ui
                                .button(
                                    egui::RichText::new("⚙")
                                        .size(16.0)
                                        .color(egui::Color32::from_rgba_unmultiplied(
                                            255, 255, 255, 190,
                                        )),
                                )
                                .on_hover_text("Settings")
                                .clicked()
                            {
                                self.show_settings_menu = !self.show_settings_menu;
                            }
                        });
                    });

                    // KWin may raise the window a few frames after we request it.
                    // Keep retrying briefly so shortcut activation lands in search.
                    if let Some(ref resp) = text_edit_response {
                        if self
                            .search_focus_until
                            .is_some_and(|deadline| Instant::now() <= deadline)
                        {
                            resp.request_focus();
                        } else {
                            self.search_focus_until = None;
                        }
                    }

                    ui.add_space(10.0);

	                    // 2. Filtering list
	                    let mut filtered_apps: Vec<(AppInfo, bool)> = Vec::new();
	                    let mut filtered_windows: Vec<WindowInfo> = Vec::new();
                        let mut filtered_app_display_titles: Vec<String> = Vec::new();
                        let mut filtered_window_display_titles: Vec<String> = Vec::new();
                        let mut filtered_app_highlight_segments: Vec<Vec<(usize, usize, bool)>> =
                            Vec::new();
                        let mut filtered_window_highlight_segments: Vec<Vec<(usize, usize, bool)>> =
                            Vec::new();
                        let search_query = self.search_query.trim().to_string();
                        let has_search_query = !search_query.is_empty();
                        let mut filtered_app_title_is_typos: Vec<bool> = Vec::new();
                        let mut filtered_window_title_is_typos: Vec<bool> = Vec::new();
                        let filter_cache_key = has_search_query.then(|| {
                            filtered_search_cache_key(
                                self.mode,
                                &search_query,
                                self.show_system_settings_modules,
                                self.pinned_apps_generation,
                                self.apps_generation,
                                self.windows_generation,
                            )
                        });

                        if let Some(cache) = filter_cache_key.as_ref().and_then(|cache_key| {
                            self.filtered_search_cache
                                .as_ref()
                                .filter(|cache| cache.key == *cache_key)
                        }) {
                            filtered_apps = cache.results.apps.clone();
                            filtered_windows = cache.results.windows.clone();
                            filtered_app_display_titles = cache.results.app_display_titles.clone();
                            filtered_window_display_titles =
                                cache.results.window_display_titles.clone();
                            filtered_app_highlight_segments =
                                cache.results.app_highlight_segments.clone();
                            filtered_window_highlight_segments =
                                cache.results.window_highlight_segments.clone();
                            filtered_app_title_is_typos = cache.results.app_title_is_typos.clone();
                            filtered_window_title_is_typos =
                                cache.results.window_title_is_typos.clone();
                        } else {
		                    match self.mode {
	                        LauncherMode::Apps => {
		                            if !has_search_query {
	                                filtered_apps = self.apps
                                    .iter()
                                    .filter(|app| self.show_system_settings_modules || !app.is_settings_module)
                                    .map(|app| {
                                        let is_pinned = self.pinned_apps.contains(&app.desktop_file_path);
                                        (app.clone(), is_pinned)
                                    })
                                    .collect();
		                                filtered_apps.sort_by(|a, b| {
                                    a.0.is_settings_module
                                        .cmp(&b.0.is_settings_module)
                                        .then_with(|| match (a.1, b.1) {
                                            (true, false) => std::cmp::Ordering::Less,
                                            (false, true) => std::cmp::Ordering::Greater,
                                            (true, true) => {
                                                pinned_app_position(&self.pinned_apps, &a.0)
                                                    .cmp(&pinned_app_position(&self.pinned_apps, &b.0))
                                            }
                                            (false, false) => a.0.name.to_lowercase().cmp(&b.0.name.to_lowercase()),
                                        })
				                                });
			                            } else if let (Some(base_query), Some(typo_query)) = (
                                    MetadataQuery::new(&search_query),
                                    MetadataQuery::new(&search_query).map(|q| q.with_typo_fallback(true)),
                                ) {
		                                let mut ranked_apps: Vec<RankedAppMatch> = self.apps
		                                    .iter()
                                    .filter(|app| self.show_system_settings_modules || !app.is_settings_module)
                                    .filter_map(|app| {
                                        let is_pinned = self.pinned_apps.contains(&app.desktop_file_path);
                                        let base_rank = app_search_rank(&base_query, app);
                                        let typo_rank = app_search_rank(&typo_query, app);
                                        let rank = match (base_rank, typo_rank) {
                                            (Some(base_rank), Some(typo_rank)) => {
                                                pick_better_rank(base_rank, typo_rank)
                                            }
                                            (Some(base_rank), None) => base_rank,
                                            (None, Some(typo_rank)) => typo_rank,
                                            (None, None) => return None,
                                        };
                                        let title_is_typo = visible_title_has_typo_match(
                                            &full_search_visible_app_title(app),
                                            &search_query,
                                        );
                                        let visible_match_priority = visible_match_priority(
                                            &full_search_visible_app_title(app),
                                            &search_query,
                                        );
                                        let pin_position = pinned_app_position(&self.pinned_apps, app);
                                        let candidate_score = if is_pinned {
                                            2_000_000.0 - pin_position as f64
                                        } else if !app.is_settings_module {
                                            1_000_000.0
                                        } else {
                                            0.0
                                        };
                                        Some(RankedAppMatch {
                                            app: app.clone(),
                                            rank,
                                            title_is_typo,
                                            visible_match_priority,
                                            is_pinned,
                                            candidate_key: format!(
                                                "{}\u{0}{}",
                                                app.name.to_lowercase(),
                                                app.desktop_file_path.to_string_lossy()
                                            ),
                                            candidate_score,
	                                        })
		                                    })
		                                    .collect();
		                                sort_ranked_matches_with_visible(
		                                    &mut ranked_apps,
                                            |item| item.visible_match_priority,
	                                    |item| &item.candidate_key,
		                                    |item| item.candidate_score,
		                                    |item| &item.rank,
	                                );
                                        filtered_app_title_is_typos = ranked_apps
                                            .iter()
                                            .map(|item| item.title_is_typo)
                                            .collect();
                                        filtered_app_display_titles = ranked_apps
                                            .iter()
                                            .map(|item| {
                                                search_visible_app_title(
                                                    &item.app,
                                                    &search_query,
                                                )
                                            })
                                            .collect();
                                        filtered_app_highlight_segments =
                                            filtered_app_display_titles
                                                .iter()
                                                .map(|title| {
                                                    title_highlight_segments(
                                                        title,
                                                        &search_query,
                                                    )
                                                })
                                                .collect();
			                                filtered_apps = ranked_apps
		                                    .into_iter()
		                                    .map(|item| (item.app, item.is_pinned))
	                                    .collect();
	                            } else {
                                    filtered_apps.clear();
                                }
	                        }
	                        LauncherMode::Windows => {
		                            if !has_search_query {
	                                filtered_windows = self.windows.clone();
		                                filtered_windows.sort_by(|a, b| {
                                    window_application_key(a)
                                        .cmp(&window_application_key(b))
                                        .then_with(|| {
                                            window_sort_title_key(a).cmp(&window_sort_title_key(b))
                                        })
                                        .then_with(|| a.id.cmp(&b.id))
				                                });
			                            } else if let (Some(base_query), Some(typo_query)) = (
                                    MetadataQuery::new(&search_query),
                                    MetadataQuery::new(&search_query).map(|q| q.with_typo_fallback(true)),
                                ) {
		                                let mut ranked_windows: Vec<RankedWindowMatch> = self.windows
                                    .iter()
                                    .filter_map(|win| {
                                        let base_rank = window_search_rank(&base_query, win);
                                        let typo_rank = window_search_rank(&typo_query, win);
                                        let rank = match (base_rank, typo_rank) {
                                            (Some(base_rank), Some(typo_rank)) => {
                                                pick_better_rank(base_rank, typo_rank)
                                            }
                                            (Some(base_rank), None) => base_rank,
                                            (None, Some(typo_rank)) => typo_rank,
                                            (None, None) => return None,
                                        };
                                        let title_is_typo = visible_title_has_typo_match(
                                            &full_search_visible_window_title(win),
                                            &search_query,
                                        );
                                        let visible_match_priority = visible_match_priority(
                                            &full_search_visible_window_title(win),
                                            &search_query,
                                        );
                                        Some(RankedWindowMatch {
                                            window: win.clone(),
                                            rank,
                                            title_is_typo,
                                            visible_match_priority,
                                            candidate_key: format!(
                                                "{}\u{0}{}\u{0}{}",
                                                window_application_key(win),
                                                window_sort_title_key(win),
                                                win.id
                                            ),
                                            candidate_score: 0.0,
                                        })
                                    })
                                    .collect();
		                                sort_ranked_matches_with_visible(
		                                    &mut ranked_windows,
                                            |item| item.visible_match_priority,
	                                    |item| &item.candidate_key,
		                                    |item| item.candidate_score,
		                                    |item| &item.rank,
	                                );
                                        filtered_window_title_is_typos = ranked_windows
                                            .iter()
                                            .map(|item| item.title_is_typo)
                                            .collect();
                                        filtered_window_display_titles = ranked_windows
                                            .iter()
                                            .map(|item| {
                                                search_visible_window_title(
                                                    &item.window,
                                                    &search_query,
                                                )
                                            })
                                            .collect();
                                        filtered_window_highlight_segments =
                                            filtered_window_display_titles
                                                .iter()
                                                .map(|title| {
                                                    title_highlight_segments(
                                                        title,
                                                        &search_query,
                                                    )
                                                })
                                                .collect();
			                                filtered_windows =
		                                    ranked_windows.into_iter().map(|item| item.window).collect();
	                            } else {
                                    filtered_windows.clear();
                                }
	                        }
	                    }

		                    if self.mode == LauncherMode::Windows {
		                        if !has_search_query {
		                            filtered_apps = self.apps
	                                .iter()
	                                .filter(|app| self.show_system_settings_modules || !app.is_settings_module)
	                                .map(|app| {
	                                    let is_pinned = self.pinned_apps.contains(&app.desktop_file_path);
	                                    (app.clone(), is_pinned)
	                                })
	                                .collect();
			                            filtered_apps.sort_by(|a, b| {
		                                a.0.is_settings_module
		                                    .cmp(&b.0.is_settings_module)
		                                    .then_with(|| match (a.1, b.1) {
	                                        (true, false) => std::cmp::Ordering::Less,
	                                        (false, true) => std::cmp::Ordering::Greater,
	                                        (true, true) => {
                                            pinned_app_position(&self.pinned_apps, &a.0)
                                                .cmp(&pinned_app_position(&self.pinned_apps, &b.0))
			                    }
		                                        (false, false) => a.0.name.to_lowercase().cmp(&b.0.name.to_lowercase()),
		                                    })
				                            });
				                        } else if let (Some(base_query), Some(typo_query)) = (
                                MetadataQuery::new(&search_query),
                                MetadataQuery::new(&search_query).map(|q| q.with_typo_fallback(true)),
                            ) {
		                            let mut ranked_apps: Vec<RankedAppMatch> = self.apps
		                                .iter()
		                                .filter(|app| self.show_system_settings_modules || !app.is_settings_module)
		                                .filter_map(|app| {
		                                    let is_pinned = self.pinned_apps.contains(&app.desktop_file_path);
		                                    let base_rank = app_search_rank(&base_query, app);
		                                    let typo_rank = app_search_rank(&typo_query, app);
		                                    let rank = match (base_rank, typo_rank) {
		                                        (Some(base_rank), Some(typo_rank)) => {
		                                            pick_better_rank(base_rank, typo_rank)
		                                        }
		                                        (Some(base_rank), None) => base_rank,
		                                        (None, Some(typo_rank)) => typo_rank,
		                                        (None, None) => return None,
		                                    };
		                                    let title_is_typo = visible_title_has_typo_match(
		                                        &full_search_visible_app_title(app),
		                                        &search_query,
		                                    );
		                                    let visible_match_priority = visible_match_priority(
		                                        &full_search_visible_app_title(app),
		                                        &search_query,
		                                    );
		                                    let pin_position = pinned_app_position(&self.pinned_apps, app);
		                                    let candidate_score = if is_pinned {
		                                        2_000_000.0 - pin_position as f64
	                                    } else if !app.is_settings_module {
	                                        1_000_000.0
	                                    } else {
	                                        0.0
	                                    };
		                                    Some(RankedAppMatch {
		                                        app: app.clone(),
		                                        rank,
		                                        title_is_typo,
		                                        visible_match_priority,
		                                        is_pinned,
		                                        candidate_key: format!(
		                                            "{}\u{0}{}",
	                                            app.name.to_lowercase(),
	                                            app.desktop_file_path.to_string_lossy()
	                                        ),
	                                        candidate_score,
	                                    })
		                                })
		                                .collect();
			                            sort_ranked_matches_with_visible(
			                                &mut ranked_apps,
                                            |item| item.visible_match_priority,
		                                |item| &item.candidate_key,
			                                |item| item.candidate_score,
			                                |item| &item.rank,
		                            );
	                                    filtered_app_title_is_typos = ranked_apps
	                                        .iter()
	                                        .map(|item| item.title_is_typo)
	                                        .collect();
                                        filtered_app_display_titles = ranked_apps
                                            .iter()
                                            .map(|item| {
                                                search_visible_app_title(
                                                    &item.app,
                                                    &search_query,
                                                )
                                            })
                                            .collect();
                                        filtered_app_highlight_segments =
                                            filtered_app_display_titles
                                                .iter()
                                                .map(|title| {
                                                    title_highlight_segments(
                                                        title,
                                                        &search_query,
                                                    )
                                                })
                                                .collect();
			                            filtered_apps = ranked_apps
		                                .into_iter()
		                                .map(|item| (item.app, item.is_pinned))
	                                .collect();
		                        } else {
	                                filtered_apps.clear();
	                            }
		                    }

                            if let Some(cache_key) = filter_cache_key {
                                self.filtered_search_cache = Some(FilteredSearchCache {
                                    key: cache_key,
                                    results: FilteredSearchResults {
                                        apps: filtered_apps.clone(),
                                        windows: filtered_windows.clone(),
                                        app_display_titles: filtered_app_display_titles.clone(),
                                        window_display_titles: filtered_window_display_titles
                                            .clone(),
                                        app_highlight_segments: filtered_app_highlight_segments
                                            .clone(),
                                        window_highlight_segments:
                                            filtered_window_highlight_segments.clone(),
                                        app_title_is_typos: filtered_app_title_is_typos.clone(),
                                        window_title_is_typos: filtered_window_title_is_typos
                                            .clone(),
                                    },
                                });
                            } else {
                                self.filtered_search_cache = None;
                            }
                        }

		                    if self.mode == LauncherMode::Windows {
                            if search_query_changed {
                                self.selected_index = 0;
                                self.last_selected_window_id = None;
                                self.active_pane = ActivePane::Windows;
                            } else if let Some(ref last_id) = self.last_selected_window_id {
	                            if let Some(pos) = filtered_windows.iter().position(|w| &w.id == last_id) {
	                                self.selected_index = pos;
	                            }
	                        }
                    }

                    let show_terminal_actions =
                        self.mode == LauncherMode::Windows && has_search_query;
                    let terminal_run_result_index = filtered_windows.len();
                    let terminal_cd_result_index = filtered_windows.len() + 1;

                    let total_items = match self.mode {
                        LauncherMode::Apps => filtered_apps.len(),
                        LauncherMode::Windows => {
                            filtered_windows.len() + if show_terminal_actions { 2 } else { 0 }
                        }
                    };

                    // Safety bounds check for list changes (run early to prevent index out of bounds)
                    if self.selected_index >= total_items {
                        self.selected_index = 0;
                    }
                    if self.side_panel_selected_index >= filtered_apps.len() {
                        self.side_panel_selected_index = 0;
                    }

                    let duplicate_window_titles: HashMap<(String, String), usize> = if self.mode
                        == LauncherMode::Windows
                    {
                        let mut counts = HashMap::new();
                        for window in &filtered_windows {
                            if let Some(key) = duplicate_window_group_key(window) {
                                *counts.entry(key).or_insert(0) += 1;
                            }
                        }
                        counts
                    } else {
                        HashMap::new()
                    };

                    let render_side_panel = self.mode == LauncherMode::Windows;
                    let mut scroll_to_selected = false;
                    let mut scroll_to_side_selected = false;
                    if self.scroll_to_first_window_on_focus {
                        self.selected_index = 0;
                        self.active_pane = ActivePane::Windows;
                        scroll_to_selected = true;
                        self.scroll_to_first_window_on_focus = false;
                    }
                    let columns = if self.icon_only && self.mode == LauncherMode::Apps {
                        self.rendered_app_grid_columns.max(1)
                    } else {
                        1
                    };
                    let side_panel_columns = if render_side_panel && self.icon_only {
                        self.rendered_side_panel_grid_columns.max(1)
                    } else {
                        1
                    };

                    if ctx.input(|i| i.key_pressed(egui::Key::F10)) {
                        self.show_settings_menu = !self.show_settings_menu;
                    }

                    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                        if self.show_settings_menu {
                            self.show_settings_menu = false;
                        } else {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }

                    if !self.show_settings_menu {
                        // Keyboard navigation inputs
                        if render_side_panel && self.icon_only {
	                            if self.active_pane == ActivePane::Windows {
	                                if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight))
	                                    && !filtered_apps.is_empty()
		                                {
			                                    let target_y = self
			                                        .rendered_window_row_centers
			                                        .get(self.selected_index)
			                                        .copied()
			                                        .unwrap_or_else(|| {
                                                    let row_height = effective_list_row_height(
                                                        self.win_row_height,
                                                        self.win_icon_size,
                                                        self.win_padding,
                                                        self.win_line_height,
                                                        self.win_text_spacing,
                                                        self.win_show_path,
                                                    );
                                                    self.selected_index as f32 * row_height
                                                });
		                                    self.side_panel_selected_index = nearest_center_index(
		                                        &self.rendered_side_panel_item_centers,
		                                        target_y,
		                                    )
		                                    .unwrap_or(0)
		                                    .min(filtered_apps.len() - 1);
		                                    self.active_pane = ActivePane::Apps;
		                                    scroll_to_side_selected = true;
		                                }
                                if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown))
                                    && total_items > 0
                                {
                                    self.selected_index = (self.selected_index + 1) % total_items;
                                    scroll_to_selected = true;
                                }
                                if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp))
                                    && total_items > 0
                                {
                                    self.selected_index = if self.selected_index == 0 {
                                        total_items - 1
                                    } else {
                                        self.selected_index - 1
                                    };
                                    scroll_to_selected = true;
                                }
		                            } else if !filtered_apps.is_empty() {
		                                if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
		                                    if self.side_panel_selected_index % side_panel_columns == 0 {
		                                        let target_y = self
		                                            .rendered_side_panel_item_centers
		                                            .get(self.side_panel_selected_index)
		                                            .copied()
		                                            .unwrap_or(self.side_panel_selected_index as f32);
		                                        if total_items > 0 {
		                                            self.selected_index = nearest_center_index(
		                                                &self.rendered_window_row_centers,
		                                                target_y,
		                                            )
		                                            .unwrap_or(0)
		                                            .min(total_items - 1);
		                                        }
		                                        self.active_pane = ActivePane::Windows;
		                                        scroll_to_selected = true;
		                                    } else {
	                                        self.side_panel_selected_index -= 1;
	                                        scroll_to_side_selected = true;
	                                    }
	                                }
                                if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
                                    self.side_panel_selected_index =
                                        (self.side_panel_selected_index + 1) % filtered_apps.len();
                                    scroll_to_side_selected = true;
                                }
	                                if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
	                                    self.side_panel_selected_index =
	                                        grid_move_down(
	                                            self.side_panel_selected_index,
	                                            filtered_apps.len(),
	                                            side_panel_columns,
	                                        );
	                                    scroll_to_side_selected = true;
	                                }
	                                if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
	                                    self.side_panel_selected_index =
	                                        grid_move_up(
	                                            self.side_panel_selected_index,
	                                            filtered_apps.len(),
	                                            side_panel_columns,
	                                        );
	                                    scroll_to_side_selected = true;
	                                }
                            }
                        } else if self.icon_only && self.mode == LauncherMode::Apps {
                            if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) && total_items > 0 {
                                self.selected_index = (self.selected_index + 1) % total_items;
                                scroll_to_selected = true;
                            }
                            if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) && total_items > 0 {
                                self.selected_index = if self.selected_index == 0 {
                                    total_items - 1
                                } else {
                                    self.selected_index - 1
                                };
                                scroll_to_selected = true;
                            }
	                            if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) && total_items > 0 {
	                                self.selected_index =
	                                    grid_move_down(self.selected_index, total_items, columns);
	                                scroll_to_selected = true;
	                            }
	                            if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)) && total_items > 0 {
	                                self.selected_index =
	                                    grid_move_up(self.selected_index, total_items, columns);
	                                scroll_to_selected = true;
	                            }
                        } else {
                            if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) && total_items > 0 {
                                self.selected_index = (self.selected_index + 1) % total_items;
                                scroll_to_selected = true;
                            }
                            if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)) && total_items > 0 {
                                self.selected_index = if self.selected_index == 0 {
                                    total_items - 1
                                } else {
                                    self.selected_index - 1
                                };
                                scroll_to_selected = true;
                            }
                        }

                        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) && total_items > 0 {
                            match self.mode {
                                LauncherMode::Apps => {
                                    let app = &filtered_apps[self.selected_index].0;
                                    self.launch_app_and_exit(app, ctx);
                                }
                                LauncherMode::Windows => {
                                    if render_side_panel && self.icon_only && self.active_pane == ActivePane::Apps {
                                        if let Some(app) =
                                            filtered_apps.get(self.side_panel_selected_index).map(|item| &item.0)
                                        {
                                            self.launch_app_and_exit(app, ctx);
                                        }
                                    } else if show_terminal_actions
                                        && self.selected_index == terminal_run_result_index
                                    {
                                        launch_terminal_command(&search_query);
                                        ctx.request_repaint();
                                    } else if show_terminal_actions
                                        && self.selected_index == terminal_cd_result_index
                                    {
                                        launch_terminal_cd(&search_query);
                                        ctx.request_repaint();
                                    } else {
                                        let win = &filtered_windows[self.selected_index];
                                        self.activate_and_exit(win.id.clone(), ctx);
                                    }
                                }
                            }
                        }
                        if ctx.input(|i| i.key_pressed(egui::Key::F5)) {
                            match self.mode {
                                LauncherMode::Apps => self.refresh_apps(),
                                LauncherMode::Windows => {
                                    self.refresh_windows();
                                    self.start_background_app_load();
                                    self.start_background_window_enrichment();
                                }
                            }
                        }
                        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::P)) && total_items > 0 {
                            if let LauncherMode::Apps = self.mode {
                                let app = &filtered_apps[self.selected_index].0;
                                let path = app.desktop_file_path.clone();
                                if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                    self.pinned_apps.remove(pos);
                                } else {
                                    self.pinned_apps.push(path);
                                }
                                self.save_pinned_apps();
                            }
                        }
                        if let LauncherMode::Apps = self.mode {
                            if total_items > 0 {
                                let app = &filtered_apps[self.selected_index].0;
                                let path = app.desktop_file_path.clone();
                                if self.pinned_apps.contains(&path) {
                                    if self.icon_only {
                                        if ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::ArrowLeft)) {
                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                if pos > 0 {
                                                    self.pinned_apps.swap(pos, pos - 1);
                                                    self.selected_index -= 1;
                                                    self.save_pinned_apps();
                                                    scroll_to_selected = true;
                                                }
                                            }
                                        }
                                        if ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::ArrowRight)) {
                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                if pos + 1 < self.pinned_apps.len() {
                                                    self.pinned_apps.swap(pos, pos + 1);
                                                    self.selected_index += 1;
                                                    self.save_pinned_apps();
                                                    scroll_to_selected = true;
                                                }
                                            }
                                        }
                                        if ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::ArrowUp)) {
                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                if pos >= columns {
                                                    self.pinned_apps.swap(pos, pos - columns);
                                                    self.selected_index -= columns;
                                                    self.save_pinned_apps();
                                                    scroll_to_selected = true;
                                                }
                                            }
                                        }
                                        if ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::ArrowDown)) {
                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                if pos + columns < self.pinned_apps.len() {
                                                    self.pinned_apps.swap(pos, pos + columns);
                                                    self.selected_index += columns;
                                                    self.save_pinned_apps();
                                                    scroll_to_selected = true;
                                                }
                                            }
                                        }
                                    } else {
                                        if ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::ArrowUp)) {
                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                if pos > 0 {
                                                    self.pinned_apps.swap(pos, pos - 1);
                                                    self.selected_index -= 1;
                                                    self.save_pinned_apps();
                                                    scroll_to_selected = true;
                                                }
                                            }
                                        }
                                        if ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::ArrowDown)) {
                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                if pos + 1 < self.pinned_apps.len() {
                                                    self.pinned_apps.swap(pos, pos + 1);
                                                    self.selected_index += 1;
                                                    self.save_pinned_apps();
                                                    scroll_to_selected = true;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

	                    // 3. Render Items ScrollArea
		                    let list_height = ui.available_height().max(100.0);
	                    let window_icon_size = egui::vec2(self.win_icon_size, self.win_icon_size);
	                    let app_icon_size = egui::vec2(self.app_icon_size, self.app_icon_size);
                        let window_row_height = effective_list_row_height(
                            self.win_row_height,
                            window_icon_size.y,
                            self.win_padding,
                            self.win_line_height,
                            self.win_text_spacing,
                            self.win_show_path,
                        );
                        let app_row_height = effective_list_row_height(
                            self.win_row_height,
                            app_icon_size.y,
                            self.win_padding,
                            self.win_line_height,
                            self.win_text_spacing,
                            self.win_show_path,
                        );
	                    let row_height = match self.mode {
                            LauncherMode::Apps => app_row_height,
                            LauncherMode::Windows => window_row_height,
                        };

                    if render_side_panel {
                        let previous_spacing = ui.spacing().item_spacing;
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.columns(2, |panes| {
                            let ui = &mut panes[0];

	                    if total_items == 0 {
	                        if self.mode == LauncherMode::Windows {
	                            self.rendered_window_row_centers.clear();
	                        }
	                        ui.allocate_ui(egui::vec2(ui.available_width(), list_height), |ui| {
                            ui.vertical_centered(|ui| {
                                ui.add_space(80.0);
                                if self.loading {
                                    ui.add(egui::Spinner::new().size(24.0));
                                    ui.add_space(10.0);
                                    ui.label(
                                        egui::RichText::new(match self.mode {
                                            LauncherMode::Apps => "Loading installed applications...",
                                            LauncherMode::Windows => "Loading open windows...",
                                        })
                                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 120))
                                        .size(14.0),
                                    );
                                } else {
                                    ui.label(
                                        egui::RichText::new(match self.mode {
                                            LauncherMode::Apps => "No matching installed applications found",
                                            LauncherMode::Windows => "No matching open windows found",
                                        })
                                        .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 120))
                                        .size(15.0),
                                    );
                                }
                            });
                        });
                    } else if self.icon_only && self.mode == LauncherMode::Apps {
                        let sensitivity = self.app_scroll_sensitivity;
                        egui::ScrollArea::vertical()
                            .wheel_scroll_multiplier(egui::vec2(1.0, sensitivity))
                            .id_salt("apps_main_icon_grid_scroll")
                            .max_height(list_height)
                            .show(ui, |ui| {
	                                ui.spacing_mut().item_spacing = egui::vec2(12.0, 12.0);
	                                let mut rendered_columns = 0usize;
	                                let mut first_row_y = None;
	                                ui.horizontal_wrapped(|ui| {
		                                    for index in 0..total_items {
		                                        let is_selected = index == self.selected_index;
			                                        let app = &filtered_apps[index].0;
			                                        let tile_size = self.app_icon_tile_size;
                                                let audio_level =
                                                    app_audio_level(
                                                        app,
                                                        &self.cached_sink_inputs,
                                                        &self.active_media_app_keys,
                                                        &self.observed_pipewire_node_ids,
                                                        &self.active_pipewire_node_ids,
                                                        self.pipewire_activity_cache_valid,
                                                    );

                                        let (rect, response) = ui.allocate_exact_size(
                                            egui::vec2(tile_size, tile_size),
	                                            egui::Sense::click(),
	                                        );

	                                        let center_y = rect.center().y;
	                                        match first_row_y {
	                                            None => {
	                                                first_row_y = Some(center_y);
	                                                rendered_columns = 1;
	                                            }
	                                            Some(row_y) if (center_y - row_y).abs() < 1.0 => {
	                                                rendered_columns += 1;
	                                            }
	                                            Some(_) => {}
	                                        }

		                                        show_immediate_icon_tooltip(&response, &app.name);

                                        response.clone().context_menu(|ui| {
                                            let path = app.desktop_file_path.clone();
                                            let is_pinned = self.pinned_apps.contains(&path);
                                            let label = if is_pinned { "📌 Unpin application" } else { "📌 Pin application" };
                                            if ui.button(label).clicked() {
                                                if is_pinned {
                                                    if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                        self.pinned_apps.remove(pos);
                                                    }
                                                } else {
                                                    self.pinned_apps.push(path.clone());
                                                }
                                                self.save_pinned_apps();
                                                ui.close();
                                            }

                                            if is_pinned {
                                                if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                    if pos >= columns {
                                                        if ui.button("⬆ Move up").clicked() {
                                                            self.pinned_apps.swap(pos, pos - columns);
                                                            self.save_pinned_apps();
                                                            ui.close();
                                                        }
                                                    }
                                                    if pos + columns < self.pinned_apps.len() {
                                                        if ui.button("⬇ Move down").clicked() {
                                                            self.pinned_apps.swap(pos, pos + columns);
                                                            self.save_pinned_apps();
                                                            ui.close();
                                                        }
                                                    }
                                                    if pos > 0 {
                                                        if ui.button("⬅ Move left").clicked() {
                                                            self.pinned_apps.swap(pos, pos - 1);
                                                            self.save_pinned_apps();
                                                            ui.close();
                                                        }
                                                    }
                                                    if pos + 1 < self.pinned_apps.len() {
                                                        if ui.button("➡ Move right").clicked() {
                                                            self.pinned_apps.swap(pos, pos + 1);
                                                            self.save_pinned_apps();
                                                            ui.close();
                                                        }
                                                    }
                                                }
                                            }
                                        });

                                        if is_selected && scroll_to_selected {
                                            response.scroll_to_me(None);
                                        }

                                        if response.clicked() || response.middle_clicked() {
                                            self.launch_app_and_exit(app, ctx);
                                        }

	                                        let bg_color = if is_selected {
	                                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18)
	                                        } else if response.hovered() {
	                                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 10)
	                                        } else {
	                                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 4)
	                                        };

                                        ui.painter().rect_filled(
                                            rect,
                                            egui::CornerRadius::same(12),
                                            bg_color,
                                        );

                                        if is_selected {
                                            ui.painter().rect_stroke(
                                                rect,
                                                egui::CornerRadius::same(12),
                                                egui::Stroke::new(1.5, egui::Color32::from_rgb(61, 174, 233)),
                                                egui::StrokeKind::Inside,
                                            );
                                        } else {
                                            ui.painter().rect_stroke(
                                                rect,
                                                egui::CornerRadius::same(12),
                                                egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 10)),
                                                egui::StrokeKind::Inside,
                                            );
                                        }

                                        let inner_rect = rect.shrink2(egui::vec2(6.0, 6.0));
                                        let label_height = if self.app_icon_show_name {
                                            (self.app_icon_name_size + 10.0).max(16.0)
                                        } else {
                                            0.0
                                        };
                                        let icon_center_y = inner_rect.min.y
                                            + (inner_rect.height() - label_height) / 2.0;
	                                        let icon_rect = egui::Rect::from_center_size(
	                                            egui::pos2(rect.center().x, icon_center_y),
	                                            app_icon_size,
	                                        );
                                                if let Some(level) = audio_level {
                                                    paint_audio_activity_ring(
                                                        ui.painter(),
                                                        icon_rect,
                                                        level,
                                                        ctx.input(|i| i.time) as f32,
                                                    );
                                                }
	                                        let label_rect = egui::Rect::from_min_max(
                                            egui::pos2(inner_rect.min.x, inner_rect.max.y - label_height),
                                            inner_rect.max,
                                        );

                                        paint_icon_in_rect(
                                            ui,
                                            app.icon_path.as_ref(),
                                            icon_rect,
                                            app_icon_size,
                                        );

                                        if self.pinned_apps.contains(&app.desktop_file_path) {
                                            let badge_pos = egui::pos2(rect.max.x - 12.0, rect.min.y + 12.0);
                                            ui.painter().text(
                                                badge_pos,
                                                egui::Align2::CENTER_CENTER,
                                                "📌",
                                                egui::FontId::proportional(11.0),
                                                egui::Color32::WHITE,
                                            );
                                        }

                                        if self.app_icon_show_name {
                                            let label = truncate_tile_label(&app.name, tile_size);
                                            let title_is_typo = filtered_app_title_is_typos
                                                .get(index)
                                                .copied()
                                                .unwrap_or(false);
                                            paint_centered_title_job(
                                                ui,
                                                label_rect,
                                                &search_query,
                                                &label,
                                                self.app_icon_name_size,
                                                title_is_typo,
                                                egui::Color32::from_rgba_unmultiplied(
                                                    255,
                                                    255,
                                                    255,
                                                    210,
                                                ),
                                            );
                                        }
	                                    }
	                                });
	                                self.rendered_app_grid_columns = rendered_columns.max(1);
	                            });
                    } else {
                        let mut rendered_window_row_centers = Vec::new();
                        let sensitivity = match self.mode {
                            LauncherMode::Apps => self.app_scroll_sensitivity,
                            LauncherMode::Windows => self.win_scroll_sensitivity,
                        };
                        egui::ScrollArea::vertical()
                            .wheel_scroll_multiplier(egui::vec2(1.0, sensitivity))
                            .id_salt(match self.mode {
                                LauncherMode::Apps => "apps_main_list_scroll",
                                LauncherMode::Windows => "windows_main_list_scroll",
	                            })
	                            .max_height(list_height)
	                            .show(ui, |ui| {
                                    let previous_item_spacing = ui.spacing().item_spacing;
                                    ui.spacing_mut().item_spacing.y = 0.0;
				                                for index in 0..total_items {
                                            let terminal_action_label =
                                                if self.mode == LauncherMode::Windows
                                                    && show_terminal_actions
                                                    && index == terminal_run_result_index
                                                {
                                                    Some("run in Terminal")
                                                } else if self.mode == LauncherMode::Windows
                                                    && show_terminal_actions
                                                    && index == terminal_cd_result_index
                                                {
                                                    Some("cd in Terminal")
                                                } else {
                                                    None
                                                };
			                                    let is_selected = index == self.selected_index
			                                        && (self.mode == LauncherMode::Apps
			                                            || self.active_pane == ActivePane::Windows);
                                            let has_duplicate_window_title = self.mode
                                                == LauncherMode::Windows
                                                && filtered_windows
                                                    .get(index)
                                                    .and_then(duplicate_window_group_key)
                                                    .and_then(|key| {
                                                        duplicate_window_titles
                                                            .get(&key)
                                                            .copied()
                                                    })
                                                    .is_some_and(|count| count > 1);

		                                    let (rect, response) = ui.allocate_exact_size(
	                                        egui::vec2(ui.available_width(), row_height),
	                                        egui::Sense::click(),
	                                    );
	                                    let row_visual_rect = rect.intersect(ui.clip_rect());
	                                    if self.mode == LauncherMode::Windows {
	                                        rendered_window_row_centers.push(rect.center().y);
	                                    }

                                     if is_selected && scroll_to_selected {
                                         response.scroll_to_me(None);
                                     }

		                                    let bg_color = if is_selected && has_duplicate_window_title {
		                                        egui::Color32::from_rgba_unmultiplied(255, 214, 92, 48)
		                                    } else if is_selected {
		                                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18)
		                                    } else if has_duplicate_window_title && response.hovered() {
		                                        egui::Color32::from_rgba_unmultiplied(255, 214, 92, 42)
		                                    } else if has_duplicate_window_title {
		                                        egui::Color32::from_rgba_unmultiplied(255, 214, 92, 30)
		                                    } else if response.hovered() {
		                                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 9)
		                                    } else {
		                                        egui::Color32::TRANSPARENT
		                                    };

                                    ui.painter().rect_filled(
                                        row_visual_rect,
                                        egui::CornerRadius::same(8),
                                        bg_color,
                                    );
                                    if is_selected {
                                        ui.painter().rect_stroke(
                                            row_visual_rect.shrink(0.5),
                                            egui::CornerRadius::same(8),
                                            egui::Stroke::new(
                                                1.0,
                                                egui::Color32::from_rgba_unmultiplied(
                                                    61, 174, 233, 140,
                                                ),
                                            ),
                                            egui::StrokeKind::Inside,
                                        );
                                    }

                                    // Premium left accent highlight bar
                                    if is_selected {
                                        let accent_rect = egui::Rect::from_min_size(
                                            egui::pos2(
                                                row_visual_rect.min.x + 2.0,
                                                row_visual_rect.min.y
                                                    + (row_visual_rect.height() - 28.0) / 2.0,
                                            ),
                                            egui::vec2(3.0, 28.0),
                                        );
                                        ui.painter().rect_filled(
                                            accent_rect,
                                            egui::CornerRadius::same(2),
                                            egui::Color32::from_rgb(61, 174, 233), // KDE blue theme accent
                                        );
                                    }

                                    // Content placement
                                    let content_rect =
                                        rect.shrink2(egui::vec2(12.0, self.win_padding));
                                    let mut child_ui = ui.new_child(
                                        egui::UiBuilder::new()
                                            .max_rect(content_rect)
                                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                                    );

                                    match self.mode {
	                                        LauncherMode::Apps => {
	                                            let app = &filtered_apps[index].0;
                                                let audio_level =
                                                    app_audio_level(
                                                        app,
                                                        &self.cached_sink_inputs,
                                                        &self.active_media_app_keys,
                                                        &self.observed_pipewire_node_ids,
                                                        &self.active_pipewire_node_ids,
                                                        self.pipewire_activity_cache_valid,
                                                    );

		                                            // Icon render
                                                let (icon_rect, _) = child_ui.allocate_exact_size(
                                                    app_icon_size,
                                                    egui::Sense::hover(),
                                                );
                                                if let Some(level) = audio_level {
                                                    paint_audio_activity_ring(
                                                        child_ui.painter(),
                                                        icon_rect,
                                                        level,
                                                        ctx.input(|i| i.time) as f32,
                                                    );
                                                }
                                                paint_icon_in_rect(
                                                    &mut child_ui,
                                                    app.icon_path.as_ref(),
                                                    icon_rect,
                                                    app_icon_size,
                                                );

                                            child_ui.add_space(10.0);

			                                            let display_title = filtered_app_display_titles
			                                                .get(index)
			                                                .cloned()
			                                                .unwrap_or_else(|| app.name.clone());
			                                            let show_search_metadata =
			                                                !search_query.trim().is_empty();
			                                            let mut label_clicked = false;
                                                let _title_is_typo = filtered_app_title_is_typos
                                                    .get(index)
                                                    .copied()
                                                    .unwrap_or(false);
	                                            if self.win_show_path {
                                                let text_min_x = content_rect.min.x
                                                    + app_icon_size.x
                                                    + 10.0;
                                                let text_rect = egui::Rect::from_min_max(
                                                    egui::pos2(text_min_x, content_rect.min.y),
                                                    content_rect.max,
                                                );
                                                let mut text_ui = ui.new_child(
                                                    egui::UiBuilder::new()
                                                        .max_rect(text_rect)
                                                        .layout(egui::Layout::top_down(
                                                            egui::Align::Min,
                                                        )),
                                                );
		                                                text_ui.spacing_mut().item_spacing.y = 0.0;
		                                                let text_block_height = if show_search_metadata {
		                                                    self.win_line_height
		                                                } else {
		                                                    self.win_line_height
		                                                        + self.win_line_height * 0.8
		                                                        + self.win_text_spacing
		                                                };
		                                                text_ui.add_space(
		                                                    ((content_rect.height() - text_block_height) / 2.0)
		                                                        .max(0.0),
		                                                );

	                                                let title_response = text_ui.add(
		                                                    egui::Label::new(
                                                                highlighted_title_job_from_segments(
                                                                    &display_title,
                                                                    self.win_title_size,
                                                                    filtered_app_highlight_segments
                                                                        .get(index)
                                                                        .map(|segments| segments.as_slice())
                                                                        .unwrap_or(&[]),
                                                                ),
                                                            )
	                                                    .sense(egui::Sense::click())
	                                                    .truncate(),
	                                                );
                                                if title_response.clicked() {
                                                    label_clicked = true;
                                                }
                                                if self.disable_ibeam && title_response.hovered() {
                                                    text_ui
                                                        .ctx()
                                                        .set_cursor_icon(egui::CursorIcon::Default);
                                                }

                                                    if !show_search_metadata {
	                                                    text_ui.add_space(self.win_text_spacing);

	                                                    let is_link =
	                                                        std::fs::symlink_metadata(&app.desktop_file_path)
	                                                            .map(|m| m.file_type().is_symlink())
	                                                            .unwrap_or(false);
	                                                    let mut subtext =
	                                                        app.desktop_file_path.to_string_lossy().to_string();
	                                                    if is_link {
	                                                        subtext.push('@');
	                                                    }
	                                                    let path_response = text_ui.add(
	                                                        egui::Label::new(
	                                                            egui::RichText::new(subtext)
	                                                                .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 130))
	                                                                .size(self.win_path_size)
	                                                                .line_height(Some(self.win_line_height * 0.8)),
	                                                        )
	                                                        .sense(egui::Sense::click())
	                                                        .truncate(),
	                                                    );
	                                                    if path_response.clicked() {
	                                                        label_clicked = true;
	                                                    }
	                                                    if self.disable_ibeam && path_response.hovered() {
	                                                        text_ui
	                                                            .ctx()
	                                                            .set_cursor_icon(egui::CursorIcon::Default);
	                                                    }
                                                    }
                                                if self.pinned_apps.contains(&app.desktop_file_path) {
                                                    text_ui.add_space(4.0);
                                                    text_ui.label(
                                                        egui::RichText::new("📌")
                                                            .size(11.0)
                                                            .color(egui::Color32::from_rgb(61, 174, 233)),
                                                    );
                                                }
                                            } else {
	                                                let title_response = child_ui.add(
		                                                    egui::Label::new(
                                                                highlighted_title_job_from_segments(
                                                                    &display_title,
                                                                    self.win_title_size,
                                                                    filtered_app_highlight_segments
                                                                        .get(index)
                                                                        .map(|segments| segments.as_slice())
                                                                        .unwrap_or(&[]),
                                                                ),
                                                            )
	                                                    .sense(egui::Sense::click())
	                                                    .truncate(),
	                                                );
                                                if title_response.clicked() {
                                                    label_clicked = true;
                                                }
                                                if self.disable_ibeam && title_response.hovered() {
                                                    child_ui
                                                        .ctx()
                                                        .set_cursor_icon(egui::CursorIcon::Default);
                                                }
                                                if self.pinned_apps.contains(&app.desktop_file_path) {
                                                    child_ui.add_space(4.0);
                                                    child_ui.label(
                                                        egui::RichText::new("📌")
                                                            .size(11.0)
                                                            .color(egui::Color32::from_rgb(61, 174, 233)),
                                                    );
                                                }
                                            }

                                            if label_clicked {
                                                self.launch_app_and_exit(app, ctx);
                                            }
	                                        }
		                                        LauncherMode::Windows => {
                                                if let Some(terminal_action_label) = terminal_action_label {
                                                    let (icon_rect, _) = child_ui.allocate_exact_size(
                                                        window_icon_size,
                                                        egui::Sense::hover(),
                                                    );
                                                    child_ui.painter().rect_filled(
                                                        icon_rect.shrink(2.0),
                                                        egui::CornerRadius::same(5),
                                                        egui::Color32::from_rgba_unmultiplied(
                                                            61, 174, 233, 45,
                                                        ),
                                                    );
                                                    child_ui.painter().rect_stroke(
                                                        icon_rect.shrink(2.0),
                                                        egui::CornerRadius::same(5),
                                                        egui::Stroke::new(
                                                            1.0,
                                                            egui::Color32::from_rgba_unmultiplied(
                                                                61, 174, 233, 120,
                                                            ),
                                                        ),
                                                        egui::StrokeKind::Inside,
                                                    );
                                                    child_ui.painter().text(
                                                        icon_rect.center(),
                                                        egui::Align2::CENTER_CENTER,
                                                        ">_",
                                                        egui::FontId::monospace(15.0),
                                                        egui::Color32::from_rgba_unmultiplied(
                                                            255, 255, 255, 220,
                                                        ),
                                                    );
                                                    child_ui.add_space(10.0);

                                                    if self.win_show_path {
                                                        child_ui.vertical(|ui| {
                                                            ui.spacing_mut().item_spacing.y = 0.0;
                                                            ui.label(
                                                                egui::RichText::new(terminal_action_label)
                                                                    .color(egui::Color32::WHITE)
                                                                    .strong()
                                                                    .size(self.win_title_size)
                                                                    .line_height(Some(self.win_line_height)),
                                                            );
                                                            ui.add_space(self.win_text_spacing);
                                                            ui.label(
                                                                egui::RichText::new(&search_query)
                                                                    .color(egui::Color32::from_rgba_unmultiplied(
                                                                        255, 255, 255, 130,
                                                                    ))
                                                                    .size(self.win_path_size)
                                                                    .line_height(Some(
                                                                        self.win_line_height * 0.8,
                                                                    )),
                                                            );
                                                        });
                                                    } else {
                                                        child_ui.label(
                                                            egui::RichText::new(terminal_action_label)
                                                                .color(egui::Color32::WHITE)
                                                                .strong()
                                                                .size(self.win_title_size)
                                                                .line_height(Some(self.win_line_height)),
                                                        );
                                                    }
                                                } else {
		                                            let win = &filtered_windows[index];
	                                                let audio_level =
	                                                    active_audio_level_for_sinks(
	                                                        &find_sink_inputs_for_window(
                                                            win,
                                                            &self.cached_sink_inputs,
                                                        ),
                                                        &self.active_media_app_keys,
                                                        &self.observed_pipewire_node_ids,
                                                        &self.active_pipewire_node_ids,
                                                        self.pipewire_activity_cache_valid,
                                                    );

		                                            // Icon render
                                                let (icon_rect, _) = child_ui.allocate_exact_size(
                                                    window_icon_size,
                                                    egui::Sense::hover(),
                                                );
                                                if let Some(level) = audio_level {
                                                    paint_audio_activity_ring(
                                                        child_ui.painter(),
                                                        icon_rect,
                                                        level,
                                                        ctx.input(|i| i.time) as f32,
                                                    );
                                                }
                                                paint_icon_in_rect(
                                                    &mut child_ui,
                                                    win.icon_path.as_ref(),
                                                    icon_rect,
                                                    window_icon_size,
                                                );

                                            child_ui.add_space(10.0);

		                                            let display_title = filtered_window_display_titles
		                                                .get(index)
		                                                .cloned()
		                                                .unwrap_or_else(|| truncate_chars(&win.title, 65));
		                                            let show_search_metadata =
		                                                !search_query.trim().is_empty();
                                                let _title_is_typo = filtered_window_title_is_typos
                                                    .get(index)
                                                    .copied()
                                                    .unwrap_or(false);

                                            if self.win_show_path {
                                                child_ui.vertical(|ui| {
                                                    ui.spacing_mut().item_spacing.y = 0.0;

	                                                    let title_response = ui.add(
	                                                        egui::Label::new(
                                                                    highlighted_title_job_from_segments(
                                                                        &display_title,
                                                                        self.win_title_size,
                                                                        filtered_window_highlight_segments
                                                                            .get(index)
                                                                            .map(|segments| segments.as_slice())
                                                                            .unwrap_or(&[]),
                                                                    ),
                                                                )
	                                                        .sense(egui::Sense::hover())
	                                                        .truncate(),
	                                                    );
                                                    if self.disable_ibeam
                                                        && title_response.hovered()
                                                    {
                                                        ui.ctx().set_cursor_icon(
                                                            egui::CursorIcon::Default,
                                                        );
                                                    }

                                                    if !show_search_metadata {
	                                                        ui.add_space(self.win_text_spacing);

	                                                        let subtext = if let Some(ref path) = win.cwd_path
	                                                        {
	                                                            let path_display = display_path(path);
	                                                            if let Some(ref command_summary) =
	                                                                win.command_summary
	                                                            {
	                                                                if !normalize_app_match_key(command_summary)
	                                                                    .eq(&normalize_app_match_key(&path_display))
	                                                                {
	                                                                    format!(
	                                                                        "{} | {}",
	                                                                        path_display, command_summary
	                                                                    )
	                                                                } else {
	                                                                    path_display
	                                                                }
	                                                            } else {
	                                                                path_display
	                                                            }
	                                                        } else if let Some(ref command_summary) =
	                                                            win.command_summary
	                                                        {
	                                                            command_summary.clone()
	                                                        } else if let Some(ref path) = win.exe_path {
	                                                            let is_link = std::fs::symlink_metadata(path)
	                                                                .map(|m| m.file_type().is_symlink())
	                                                                .unwrap_or(false);
	                                                            let mut path_str = display_path(path);
	                                                            if is_link {
	                                                                path_str.push('@');
	                                                            }
	                                                            path_str
	                                                        } else if let Some(ref proc_name) =
	                                                            win.active_process
	                                                        {
	                                                            format!(
	                                                                "{} (running: {})",
	                                                                win.class, proc_name
	                                                            )
	                                                        } else {
	                                                            win.class.clone()
	                                                        };
	                                                        let path_response = ui.add(
	                                                            egui::Label::new(
	                                                                egui::RichText::new(subtext)
	                                                                    .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 130))
	                                                                    .size(self.win_path_size)
	                                                                    .line_height(Some(self.win_line_height * 0.8)),
	                                                            )
	                                                            .sense(egui::Sense::hover())
	                                                            .truncate(),
	                                                        );
	                                                        if self.disable_ibeam && path_response.hovered()
	                                                        {
	                                                            ui.ctx().set_cursor_icon(
	                                                                egui::CursorIcon::Default,
	                                                            );
	                                                        }
                                                    }
                                                });
                                            } else {
	                                                let title_response = child_ui.add(
	                                                    egui::Label::new(
                                                                highlighted_title_job_from_segments(
                                                                    &display_title,
                                                                    self.win_title_size,
                                                                    filtered_window_highlight_segments
                                                                        .get(index)
                                                                        .map(|segments| segments.as_slice())
                                                                        .unwrap_or(&[]),
                                                                ),
                                                            )
	                                                    .sense(egui::Sense::hover())
	                                                    .truncate(),
	                                                );
	                                                if self.disable_ibeam && title_response.hovered() {
	                                                    child_ui
	                                                        .ctx()
	                                                        .set_cursor_icon(egui::CursorIcon::Default);
	                                                }
	                                            }
                                                }
	                                        }
	                                    }

                                    let overlay_response = ui.interact(
                                        rect,
                                        ui.id().with(("main_row_overlay", index)),
                                        egui::Sense::click(),
                                    );

                                    if terminal_action_label.is_none() {
                                        overlay_response.clone().context_menu(|ui| {
                                            match self.mode {
                                                LauncherMode::Apps => {
                                                    let app = &filtered_apps[index].0;
                                                    let path = app.desktop_file_path.clone();
                                                    let is_pinned = self.pinned_apps.contains(&path);
                                                    let label = if is_pinned { "📌 Unpin application" } else { "📌 Pin application" };
                                                    if ui.button(label).clicked() {
                                                        if is_pinned {
                                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                                self.pinned_apps.remove(pos);
                                                            }
                                                        } else {
                                                            self.pinned_apps.push(path.clone());
                                                        }
                                                        self.save_pinned_apps();
                                                        ui.close();
                                                    }

                                                    if is_pinned {
                                                        if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                            if pos > 0 {
                                                                if ui.button("⬆ Move up").clicked() {
                                                                    self.pinned_apps.swap(pos, pos - 1);
                                                                    self.save_pinned_apps();
                                                                    ui.close();
                                                                }
                                                            }
                                                            if pos + 1 < self.pinned_apps.len() {
                                                                if ui.button("⬇ Move down").clicked() {
                                                                    self.pinned_apps.swap(pos, pos + 1);
                                                                    self.save_pinned_apps();
                                                                    ui.close();
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                                LauncherMode::Windows => {
                                                    let win = &filtered_windows[index];
                                                    if ui.button("Clone window").clicked() {
                                                        self.active_pane = ActivePane::Windows;
                                                        self.selected_index = index;
                                                        self.clone_window_and_exit(win, ctx);
                                                        ui.close();
                                                    }
                                                    if ui.button("Open new window").clicked() {
                                                        self.active_pane = ActivePane::Windows;
                                                        self.selected_index = index;
                                                        self.launch_window_app_and_exit(win, ctx);
                                                        ui.close();
                                                    }
                                                    if ui.button("Open window").clicked() {
                                                        self.active_pane = ActivePane::Windows;
                                                        self.selected_index = index;
                                                        self.activate_and_exit(win.id.clone(), ctx);
                                                        ui.close();
                                                    }
                                                    if ui.button("Show info").clicked() {
                                                        self.process_chain_popup = Some(win.clone());
                                                        ui.close();
                                                    }
                                                    ui.separator();
                                                    if ui.button("Close application").clicked() {
                                                        self.close_window_and_exit(win.id.clone(), ctx);
                                                        ui.close();
                                                    }

                                                    // Volume Control
                                                    let matching_sinks =
                                                        dedup_sink_inputs_for_controls(
                                                            &find_sink_inputs_for_window(
                                                                win,
                                                                &self.cached_sink_inputs,
                                                            ),
                                                        );
                                                    if !matching_sinks.is_empty() {
                                                        ui.separator();
                                                        ui.label("🔊 Volume Control");
                                                        for sink in &matching_sinks {
                                                            let sink_index = sink.index;
                                                            let sink_process_id = sink
                                                                .properties
                                                                .get("application.process.id")
                                                                .cloned();
                                                            let current_vol =
                                                                sink_display_volume_percent(sink)
                                                                    as f32;
                                                            let mut current_mute = sink.mute;

                                                            ui.horizontal(|ui| {
                                                                // Mute button
                                                                let mute_label = if current_mute { "🔇" } else { "🔊" };
                                                                if ui.button(mute_label).clicked() {
                                                                    current_mute = !current_mute;
                                                                    for cached_sink in
                                                                        self.cached_sink_inputs.iter_mut()
                                                                    {
                                                                        let same_group = cached_sink.index == sink_index
                                                                            || sink_process_id.as_ref().is_some_and(|pid| {
                                                                                cached_sink
                                                                                    .properties
                                                                                    .get("application.process.id")
                                                                                    == Some(pid)
                                                                            });
                                                                        if same_group {
                                                                            set_sink_input_mute(
                                                                                cached_sink.index,
                                                                                current_mute,
                                                                            );
                                                                            cached_sink.mute =
                                                                                current_mute;
                                                                        }
                                                                    }
                                                                }

                                                                // Volume slider
                                                                let mut vol_val = current_vol as u32;
                                                                if ui.add(egui::Slider::new(&mut vol_val, 0..=100).show_value(true)).changed() {
                                                                    for cached_sink in
                                                                        self.cached_sink_inputs.iter_mut()
                                                                    {
                                                                        let same_group = cached_sink.index == sink_index
                                                                            || sink_process_id.as_ref().is_some_and(|pid| {
                                                                                cached_sink
                                                                                    .properties
                                                                                    .get("application.process.id")
                                                                                    == Some(pid)
                                                                            });
                                                                        if same_group {
                                                                            set_sink_input_volume(
                                                                                cached_sink.index,
                                                                                vol_val,
                                                                            );
                                                                            if let Some(chan) =
                                                                                cached_sink
                                                                                    .volume
                                                                                    .values_mut()
                                                                                    .next()
                                                                            {
                                                                                chan.value_percent =
                                                                                    format!(
                                                                                        "{}%",
                                                                                        vol_val
                                                                                    );
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                            });
                                                        }
                                                    }
                                                }
                                            }
                                        });
                                    }

	                                    if overlay_response.hovered() {
	                                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
	                                    }

		                                    if overlay_response.clicked() {
		                                        match self.mode {
		                                            LauncherMode::Apps => {
		                                                self.selected_index = index;
		                                                let app = &filtered_apps[index].0;
		                                                self.launch_app_and_exit(app, ctx);
		                                            }
                                                    LauncherMode::Windows => {
		                                                self.active_pane = ActivePane::Windows;
		                                                self.selected_index = index;
                                                    if show_terminal_actions
                                                        && index == terminal_run_result_index
                                                    {
                                                        launch_terminal_command(&search_query);
                                                        ctx.request_repaint();
                                                    } else if show_terminal_actions
                                                        && index == terminal_cd_result_index
                                                    {
                                                        launch_terminal_cd(&search_query);
                                                        ctx.request_repaint();
                                                    } else {
		                                                let win = &filtered_windows[index];
		                                                self.activate_and_exit(win.id.clone(), ctx);
                                                    }
		                                            }
	                                        }
		                                    }

			                                    if overlay_response.middle_clicked() {
			                                        if let LauncherMode::Apps = self.mode {
                                                    self.selected_index = index;
                                                    let app = &filtered_apps[index].0;
                                                    self.launch_app_and_exit(app, ctx);
                                                } else if let LauncherMode::Windows = self.mode {
                                                    if terminal_action_label.is_some() {
                                                        continue;
                                                    }
			                                            self.active_pane = ActivePane::Windows;
			                                            self.selected_index = index;
			                                            let win = &filtered_windows[index];
		                                            self.launch_window_app_and_exit(win, ctx);
		                                        }
			                                    }
		                                }
                                    ui.spacing_mut().item_spacing = previous_item_spacing;
			                            });
                        if self.mode == LauncherMode::Windows {
                            self.rendered_window_row_centers = rendered_window_row_centers;
                        } else {
                            self.rendered_window_row_centers.clear();
                        }
	                    }

	                            let ui = &mut panes[1];
	                            let edge_x = ui.min_rect().min.x;
	                            let edge_y = ui.min_rect().min.y;
	                            ui.painter().line_segment(
	                                [
	                                    egui::pos2(edge_x, edge_y),
	                                    egui::pos2(edge_x, edge_y + list_height),
	                                ],
	                                egui::Stroke::new(
	                                    1.0,
	                                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18),
	                                ),
	                            );
                                    ui.vertical(|ui| {
                                        let sensitivity = self.app_scroll_sensitivity;
                                        egui::ScrollArea::vertical()
                                            .wheel_scroll_multiplier(egui::vec2(1.0, sensitivity))
                                            .id_salt("apps_side_panel_scroll")
                                            .max_height(list_height)
                                            .show(ui, |ui| {
		                                        if filtered_apps.is_empty() {
		                                            self.rendered_side_panel_item_centers.clear();
		                                            self.rendered_side_panel_grid_columns = 1;
		                                            ui.add_space(20.0);
	                                            ui.label(
	                                                egui::RichText::new(if self.loading {
	                                                    "Loading applications..."
	                                                } else {
	                                                    "No matching applications found"
	                                                })
	                                                .color(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 120))
	                                                .size(13.0),
	                                            );
		                                                } else if self.icon_only {
		                                                    ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
		                                                    let mut rendered_columns = 0usize;
		                                                    let mut first_row_y = None;
		                                                    let mut rendered_item_centers = Vec::new();
		                                                    ui.horizontal_wrapped(|ui| {
				                                                        for (index, item) in filtered_apps.iter().enumerate() {
			                                                            let app = &item.0;
			                                                            let tile_size = self.app_icon_tile_size;
                                                                    let audio_level =
                                                                        app_audio_level(
                                                                            app,
                                                                            &self.cached_sink_inputs,
                                                                            &self.active_media_app_keys,
                                                                            &self.observed_pipewire_node_ids,
                                                                            &self.active_pipewire_node_ids,
                                                                            self.pipewire_activity_cache_valid,
                                                                        );
		                                                            let is_selected = self.active_pane == ActivePane::Apps
	                                                                && index == self.side_panel_selected_index;
                                                            let (rect, response) = ui.allocate_exact_size(
                                                                egui::vec2(tile_size, tile_size),
	                                                                egui::Sense::click(),
	                                                            );
		                                                            let center_y = rect.center().y;
		                                                            rendered_item_centers.push(center_y);
		                                                            match first_row_y {
	                                                                None => {
	                                                                    first_row_y = Some(center_y);
	                                                                    rendered_columns = 1;
	                                                                }
	                                                                Some(row_y)
	                                                                    if (center_y - row_y).abs() < 1.0 =>
	                                                                {
	                                                                    rendered_columns += 1;
	                                                                }
	                                                                Some(_) => {}
	                                                            }
	                                                            show_immediate_icon_tooltip(&response, &app.name);
                                                            if is_selected && scroll_to_side_selected {
                                                                response.scroll_to_me(None);
                                                            }
		                                                    response.clone().context_menu(|ui| {
		                                                        let path = app.desktop_file_path.clone();
		                                                        let is_pinned = self.pinned_apps.contains(&path);
	                                                        let label = if is_pinned { "📌 Unpin application" } else { "📌 Pin application" };
	                                                        if ui.button(label).clicked() {
	                                                            if is_pinned {
	                                                                if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
	                                                                    self.pinned_apps.remove(pos);
	                                                                }
	                                                            } else {
	                                                                self.pinned_apps.push(path.clone());
	                                                            }
	                                                            self.save_pinned_apps();
		                                                            ui.close();
		                                                        }
                                                                if is_pinned {
                                                                    if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                                        if pos > 0 {
                                                                            if ui.button("⬆ Move up").clicked() {
                                                                                self.pinned_apps.swap(pos, pos - 1);
                                                                                self.save_pinned_apps();
                                                                                ui.close();
                                                                            }
                                                                        }
                                                                        if pos + 1 < self.pinned_apps.len() {
                                                                            if ui.button("⬇ Move down").clicked() {
                                                                                self.pinned_apps.swap(pos, pos + 1);
                                                                                self.save_pinned_apps();
                                                                                ui.close();
                                                                            }
                                                                        }
                                                                    }
                                                                }
		                                                    });
			                                                    if response.clicked() || response.middle_clicked() {
		                                                                self.active_pane = ActivePane::Apps;
		                                                                self.side_panel_selected_index = index;
			                                                        self.launch_app_and_exit(app, ctx);
			                                                    }
		                                                    ui.painter().rect_filled(
		                                                        rect,
		                                                        egui::CornerRadius::same(10),
			                                                        if is_selected {
			                                                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 18)
			                                                        } else if response.hovered() {
			                                                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 10)
			                                                        } else {
			                                                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 5)
			                                                        },
		                                                    );
                                                            if is_selected {
                                                                ui.painter().rect_stroke(
                                                                    rect,
                                                                    egui::CornerRadius::same(10),
                                                                    egui::Stroke::new(
                                                                        1.5,
                                                                        egui::Color32::from_rgb(61, 174, 233),
                                                                    ),
                                                                    egui::StrokeKind::Inside,
                                                                );
                                                            }
                                                            let inner_rect = rect.shrink2(egui::vec2(6.0, 6.0));
                                                            let label_height = if self.app_icon_show_name {
                                                                (self.app_icon_name_size + 10.0).max(16.0)
                                                            } else {
                                                                0.0
                                                            };
                                                            let icon_center_y = inner_rect.min.y
                                                                + (inner_rect.height() - label_height)
                                                                    / 2.0;
	                                                            let icon_rect = egui::Rect::from_center_size(
	                                                                egui::pos2(rect.center().x, icon_center_y),
	                                                                app_icon_size,
	                                                            );
                                                                    if let Some(level) = audio_level {
                                                                        paint_audio_activity_ring(
                                                                            ui.painter(),
                                                                            icon_rect,
                                                                            level,
                                                                            ctx.input(|i| i.time)
                                                                                as f32,
                                                                        );
                                                                    }
	                                                            let label_rect = egui::Rect::from_min_max(
                                                                egui::pos2(
                                                                    inner_rect.min.x,
                                                                    inner_rect.max.y - label_height,
                                                                ),
                                                                inner_rect.max,
                                                            );
                                                            paint_icon_in_rect(
                                                                ui,
                                                                app.icon_path.as_ref(),
                                                                icon_rect,
                                                                app_icon_size,
                                                            );
                                                            if self.pinned_apps.contains(&app.desktop_file_path) {
		                                                        ui.painter().text(
		                                                            egui::pos2(rect.max.x - 10.0, rect.min.y + 10.0),
	                                                            egui::Align2::CENTER_CENTER,
	                                                            "📌",
	                                                            egui::FontId::proportional(10.0),
		                                                            egui::Color32::WHITE,
		                                                        );
		                                                    }

                                                            if self.app_icon_show_name {
                                                                let label =
                                                                    truncate_tile_label(&app.name, tile_size);
                                                            let title_is_typo =
                                                                filtered_app_title_is_typos
                                                                    .get(index)
                                                                    .copied()
                                                                        .unwrap_or(false);
                                                                paint_centered_title_job(
                                                                    ui,
                                                                    label_rect,
                                                                    &search_query,
                                                                    &label,
                                                                    self.app_icon_name_size,
                                                                    title_is_typo,
                                                                    egui::Color32::from_rgba_unmultiplied(
                                                                        255, 255, 255, 210,
                                                                    ),
                                                                );
                                                            }
		                                                    let _ = index;
		                                                        }
	                                                    });
		                                                    self.rendered_side_panel_grid_columns =
		                                                        rendered_columns.max(1);
			                                                    self.rendered_side_panel_item_centers =
			                                                        rendered_item_centers;
	                                                } else {
	                                                    self.rendered_side_panel_item_centers.clear();
	                                                    self.rendered_side_panel_grid_columns = 1;
			                                                    for (index, item) in filtered_apps.iter().enumerate() {
	                                                        let app = &item.0;
                                                            let audio_level =
                                                                app_audio_level(
                                                                    app,
                                                                    &self.cached_sink_inputs,
                                                                    &self.active_media_app_keys,
                                                                    &self.observed_pipewire_node_ids,
                                                                    &self.active_pipewire_node_ids,
                                                                    self.pipewire_activity_cache_valid,
                                                                );
	                                                        let (rect, response) = ui.allocate_exact_size(
                                                            egui::vec2(ui.available_width(), app_row_height),
                                                            egui::Sense::click(),
                                                        );
	                                                response.clone().context_menu(|ui| {
	                                                    let path = app.desktop_file_path.clone();
	                                                    let is_pinned = self.pinned_apps.contains(&path);
	                                                    let label = if is_pinned { "📌 Unpin application" } else { "📌 Pin application" };
	                                                    if ui.button(label).clicked() {
	                                                        if is_pinned {
	                                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
	                                                                self.pinned_apps.remove(pos);
	                                                            }
	                                                        } else {
	                                                            self.pinned_apps.push(path.clone());
	                                                        }
	                                                        self.save_pinned_apps();
	                                                        ui.close();
	                                                    }
                                                        if is_pinned {
                                                            if let Some(pos) = self.pinned_apps.iter().position(|x| x == &path) {
                                                                if pos > 0 {
                                                                    if ui.button("⬆ Move up").clicked() {
                                                                        self.pinned_apps.swap(pos, pos - 1);
                                                                        self.save_pinned_apps();
                                                                        ui.close();
                                                                    }
                                                                }
                                                                if pos + 1 < self.pinned_apps.len() {
                                                                    if ui.button("⬇ Move down").clicked() {
                                                                        self.pinned_apps.swap(pos, pos + 1);
                                                                        self.save_pinned_apps();
                                                                        ui.close();
                                                                    }
                                                                }
                                                            }
                                                        }
	                                                });
		                                                if response.clicked() || response.middle_clicked() {
		                                                    self.launch_app_and_exit(app, ctx);
		                                                }
                                                        if response.hovered() {
                                                            ui.painter().rect_filled(
                                                                rect,
	                                                        egui::CornerRadius::same(8),
                                                                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 12),
                                                            );
                                                        }
                                                        let content_rect =
                                                            rect.shrink2(egui::vec2(12.0, self.win_padding));
                                                        let mut child_ui = ui.new_child(
                                                            egui::UiBuilder::new()
                                                                .max_rect(content_rect)
                                                                .layout(egui::Layout::left_to_right(egui::Align::Center)),
                                                        );
                                                            let (icon_rect, _) = child_ui.allocate_exact_size(
                                                                app_icon_size,
                                                                egui::Sense::hover(),
                                                            );
                                                            if let Some(level) = audio_level {
                                                                paint_audio_activity_ring(
                                                                    child_ui.painter(),
                                                                    icon_rect,
                                                                    level,
                                                                    ctx.input(|i| i.time) as f32,
                                                                );
                                                            }
                                                            paint_icon_in_rect(
                                                                &mut child_ui,
                                                                app.icon_path.as_ref(),
                                                                icon_rect,
                                                                app_icon_size,
                                                            );
                                                        child_ui.add_space(10.0);

		                                                        let display_title = filtered_app_display_titles
		                                                            .get(index)
		                                                            .cloned()
		                                                            .unwrap_or_else(|| app.name.clone());
		                                                        let show_search_metadata =
		                                                            !search_query.trim().is_empty();
		                                                        let mut label_clicked = false;
	                                                        if self.win_show_path {
                                                            let text_min_x =
                                                                content_rect.min.x + app_icon_size.x + 10.0;
                                                            let text_rect = egui::Rect::from_min_max(
                                                                egui::pos2(text_min_x, content_rect.min.y),
                                                                content_rect.max,
                                                            );
                                                            let mut text_ui = ui.new_child(
                                                                egui::UiBuilder::new()
                                                                    .max_rect(text_rect)
                                                                    .layout(egui::Layout::top_down(
                                                                        egui::Align::Min,
                                                                    )),
                                                            );
	                                                            text_ui.spacing_mut().item_spacing.y = 0.0;
	                                                            let text_block_height = if show_search_metadata {
	                                                                self.win_line_height
	                                                            } else {
	                                                                self.win_line_height
	                                                                    + self.win_line_height * 0.8
	                                                                    + self.win_text_spacing
	                                                            };
	                                                            text_ui.add_space(
	                                                                ((content_rect.height() - text_block_height) / 2.0)
	                                                                    .max(0.0),
	                                                            );

	                                                            let title_response = text_ui.add(
	                                                                egui::Label::new(
                                                                            highlighted_title_job_from_segments(
                                                                                &display_title,
                                                                                self.win_title_size,
                                                                                filtered_app_highlight_segments
                                                                                    .get(index)
                                                                                    .map(|segments| segments.as_slice())
                                                                                    .unwrap_or(&[]),
                                                                            ),
                                                                        )
	                                                                .sense(egui::Sense::click())
	                                                                .truncate(),
	                                                            );
                                                            if title_response.clicked() {
                                                                label_clicked = true;
                                                            }
                                                            if self.disable_ibeam && title_response.hovered() {
                                                                text_ui
                                                                    .ctx()
                                                                    .set_cursor_icon(egui::CursorIcon::Default);
                                                            }

                                                            if !show_search_metadata {
	                                                            text_ui.add_space(self.win_text_spacing);

	                                                            let is_link = std::fs::symlink_metadata(
	                                                                &app.desktop_file_path,
	                                                            )
	                                                            .map(|m| m.file_type().is_symlink())
	                                                            .unwrap_or(false);
	                                                            let mut subtext = app
	                                                                .desktop_file_path
	                                                                .to_string_lossy()
	                                                                .to_string();
	                                                            if is_link {
	                                                                subtext.push('@');
	                                                            }
	                                                            let path_response = text_ui.add(
	                                                                egui::Label::new(
	                                                                    egui::RichText::new(subtext)
	                                                                        .color(egui::Color32::from_rgba_unmultiplied(
	                                                                            255, 255, 255, 130,
	                                                                        ))
	                                                                        .size(self.win_path_size)
	                                                                        .line_height(Some(
	                                                                            self.win_line_height * 0.8,
	                                                                        )),
	                                                                )
	                                                                .sense(egui::Sense::click())
	                                                                .truncate(),
	                                                            );
	                                                            if path_response.clicked() {
	                                                                label_clicked = true;
	                                                            }
	                                                            if self.disable_ibeam && path_response.hovered() {
	                                                                text_ui
	                                                                    .ctx()
	                                                                    .set_cursor_icon(egui::CursorIcon::Default);
	                                                            }
                                                            }
                                                        } else {
	                                                            let title_response = child_ui.add(
	                                                                egui::Label::new(
                                                                            highlighted_title_job_from_segments(
                                                                                &display_title,
                                                                                self.win_title_size,
                                                                                filtered_app_highlight_segments
                                                                                    .get(index)
                                                                                    .map(|segments| segments.as_slice())
                                                                                    .unwrap_or(&[]),
                                                                            ),
                                                                        )
	                                                                .sense(egui::Sense::click())
	                                                                .truncate(),
	                                                            );
                                                            if title_response.clicked() {
                                                                label_clicked = true;
                                                            }
                                                            if self.disable_ibeam && title_response.hovered() {
                                                                child_ui
                                                                    .ctx()
                                                                    .set_cursor_icon(egui::CursorIcon::Default);
                                                            }
                                                        }
                                                        if label_clicked {
                                                            self.launch_app_and_exit(app, ctx);
                                                        }
                                                    }
                                                }
                                            });
	                            });
	                        });
	                        ui.spacing_mut().item_spacing = previous_spacing;
	                    }

                    // Custom drag-resize handle at the bottom-right corner
                    let resize_handle_size = egui::vec2(16.0, 16.0);
                    let resize_rect = egui::Rect::from_min_size(
                        ui.max_rect().max - resize_handle_size,
                        resize_handle_size,
                    );

                    let resize_response = ui.allocate_rect(resize_rect, egui::Sense::drag());

                    // Draw visual resize handle (diagonal grip lines)
                    let color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 100);
                    let br = resize_rect.max - egui::vec2(2.0, 2.0);
                    for i in 0..3 {
                        let offset = i as f32 * 4.0;
                        ui.painter().line_segment(
                            [
                                br - egui::vec2(offset + 4.0, 0.0),
                                br - egui::vec2(0.0, offset + 4.0),
                            ],
                            egui::Stroke::new(1.0, color),
                        );
                    }

                    if resize_response.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeSouthEast);
                    }

                    if resize_response.drag_started() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::BeginResize(
                            egui::ResizeDirection::SouthEast,
                        ));
                    }

                    if self.show_settings_menu {
                        self.show_settings_popup(ctx);
                    }
                    if self.process_chain_popup.is_some() {
                        self.show_window_info_popup(ctx);
                    }

                    if let Some(ref resp) = text_edit_response {
                        if ctx.input(|i| i.focused) {
                            resp.request_focus();
                        }
                    }

                    if self.mode == LauncherMode::Windows && !filtered_windows.is_empty() {
                        self.last_selected_window_id = filtered_windows.get(self.selected_index).map(|w| w.id.clone());
                    } else {
                        self.last_selected_window_id = None;
                    }
                });
            });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_window_size();
    }
}

struct SingleInstanceLock {
    path: PathBuf,
}

impl Drop for SingleInstanceLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn get_socket_path(mode: LauncherMode) -> PathBuf {
    let filename = match mode {
        LauncherMode::Apps => "applicationlauncher-apps.sock",
        LauncherMode::Windows => "applicationlauncher-windows.sock",
    };
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime_dir).join(filename)
    } else {
        std::env::temp_dir().join(filename)
    }
}

fn focus_existing_launcher_window() {
    let kpath = get_kdotool_path();
    let mut ids = Vec::new();

    for args in [
        ["search", "--class", "applicationlauncher"].as_slice(),
        ["search", "--title", "Open Application Windows"].as_slice(),
    ] {
        if let Ok(output) = Command::new(&kpath).args(args).output() {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for id in stdout.lines().map(str::trim).filter(|id| !id.is_empty()) {
                    if !ids.iter().any(|existing: &String| existing == id) {
                        ids.push(id.to_string());
                    }
                }
            }
        }
    }

    for id in ids {
        let _ = Command::new(&kpath)
            .args(["windowstate", "--remove", "MINIMIZED", &id])
            .status();
        std::thread::sleep(std::time::Duration::from_millis(60));
        let _ = Command::new(&kpath).args(["windowactivate", &id]).status();
        let _ = Command::new(&kpath).args(["windowraise", &id]).status();
    }
}

fn request_launcher_foreground() {
    std::thread::spawn(focus_existing_launcher_window);
}

struct BorderOverlay {
    start_time: Instant,
    duration: std::time::Duration,
    local_x: f32,
    local_y: f32,
    tw: f32,
    th: f32,
}

impl eframe::App for BorderOverlay {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.start_time.elapsed() >= self.duration {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        ctx.request_repaint();

        let panel_frame = egui::Frame {
            fill: egui::Color32::TRANSPARENT,
            ..Default::default()
        };

        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ctx, |ui| {
                let elapsed = self.start_time.elapsed().as_secs_f32();
                let total_secs = self.duration.as_secs_f32();
                let progress = (elapsed / total_secs).clamp(0.0, 1.0);
                let alpha = ((1.0 - progress) * 255.0) as u8;

                let rect = egui::Rect::from_min_size(
                    egui::pos2(self.local_x, self.local_y),
                    egui::vec2(self.tw, self.th),
                );

                ui.painter().rect_stroke(
                    rect,
                    egui::CornerRadius::same(6),
                    egui::Stroke::new(
                        3.0,
                        egui::Color32::from_rgba_unmultiplied(235, 40, 40, alpha),
                    ),
                    egui::StrokeKind::Inside,
                );
            });
    }
}

struct MonitorInfo {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    scale: f32,
}

fn get_monitors() -> Vec<MonitorInfo> {
    let mut monitors = Vec::new();
    let output = match Command::new("kscreen-doctor").arg("-j").output() {
        Ok(o) => o,
        Err(_) => return monitors,
    };
    let stdout = String::from_utf8_lossy(&output.stdout);

    let blocks: Vec<&str> = stdout.split("\"name\":").collect();
    if blocks.len() <= 1 {
        return monitors;
    }

    for block in &blocks[1..] {
        let mut x = None;
        let mut y = None;
        let mut width = None;
        let mut height = None;
        let mut scale = Some(1.0);

        if let Some(pos_idx) = block.find("\"pos\":") {
            let pos_str = &block[pos_idx..];
            if let Some(brace_open) = pos_str.find('{') {
                if let Some(brace_close) = pos_str.find('}') {
                    let pos_content = &pos_str[brace_open + 1..brace_close];
                    for line in pos_content.lines() {
                        let line = line.trim();
                        if line.starts_with("\"x\":") {
                            x = line
                                .strip_prefix("\"x\":")
                                .map(|s| s.trim_matches(|c| c == ',' || c == ' ' || c == '\n'))
                                .and_then(|s| s.parse::<f32>().ok());
                        } else if line.starts_with("\"y\":") {
                            y = line
                                .strip_prefix("\"y\":")
                                .map(|s| s.trim_matches(|c| c == ',' || c == ' ' || c == '\n'))
                                .and_then(|s| s.parse::<f32>().ok());
                        }
                    }
                }
            }
        }

        if let Some(size_idx) = block.find("\"size\":") {
            let size_str = &block[size_idx..];
            if let Some(brace_open) = size_str.find('{') {
                if let Some(brace_close) = size_str.find('}') {
                    let size_content = &size_str[brace_open + 1..brace_close];
                    for line in size_content.lines() {
                        let line = line.trim();
                        if line.starts_with("\"width\":") {
                            width = line
                                .strip_prefix("\"width\":")
                                .map(|s| s.trim_matches(|c| c == ',' || c == ' ' || c == '\n'))
                                .and_then(|s| s.parse::<f32>().ok());
                        } else if line.starts_with("\"height\":") {
                            height = line
                                .strip_prefix("\"height\":")
                                .map(|s| s.trim_matches(|c| c == ',' || c == ' ' || c == '\n'))
                                .and_then(|s| s.parse::<f32>().ok());
                        }
                    }
                }
            }
        }

        for line in block.lines() {
            let line = line.trim();
            if line.starts_with("\"scale\":") {
                if let Some(s_val) = line
                    .strip_prefix("\"scale\":")
                    .map(|s| s.trim_matches(|c| c == ',' || c == ' ' || c == '\n'))
                    .and_then(|s| s.parse::<f32>().ok())
                {
                    scale = Some(s_val);
                }
            }
        }

        if let (Some(x), Some(y), Some(width), Some(height), Some(scale)) =
            (x, y, width, height, scale)
        {
            monitors.push(MonitorInfo {
                x,
                y,
                width,
                height,
                scale,
            });
        }
    }
    monitors
}

fn main() -> eframe::Result {
    install_panic_hook();
    let args: Vec<String> = std::env::args().collect();

    if args.len() >= 7 && args[1] == "--draw-border" {
        let tx: f32 = args[2].parse().unwrap_or(0.0);
        let ty: f32 = args[3].parse().unwrap_or(0.0);
        let tw: f32 = args[4].parse().unwrap_or(100.0);
        let th: f32 = args[5].parse().unwrap_or(100.0);

        // Find which monitor contains the target window center
        let target_center_x = tx + tw / 2.0;
        let target_center_y = ty + th / 2.0;

        let mut mx = 0.0;
        let mut my = 0.0;

        for monitor in get_monitors() {
            let logical_w = monitor.width / monitor.scale;
            let logical_h = monitor.height / monitor.scale;
            if target_center_x >= monitor.x
                && target_center_x <= monitor.x + logical_w
                && target_center_y >= monitor.y
                && target_center_y <= monitor.y + logical_h
            {
                mx = monitor.x;
                my = monitor.y;
                break;
            }
        }

        let local_x = tx - mx;
        let local_y = ty - my;

        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("Border Overlay")
                .with_decorations(false)
                .with_transparent(true)
                .with_always_on_top()
                .with_fullscreen(true)
                .with_mouse_passthrough(true),
            ..Default::default()
        };

        let _ = eframe::run_native(
            "Border Overlay",
            options,
            Box::new(move |_cc| {
                Ok(Box::new(BorderOverlay {
                    start_time: Instant::now(),
                    duration: std::time::Duration::from_millis(250),
                    local_x,
                    local_y,
                    tw,
                    th,
                }))
            }),
        );
        return Ok(());
    }

    let mode = LauncherMode::Windows;

    // Single instance check using Unix domain socket
    let socket_path = get_socket_path(mode);
    if socket_path.exists() {
        if std::os::unix::net::UnixStream::connect(&socket_path).is_ok() {
            focus_existing_launcher_window();
            return Ok(());
        }
        let _ = std::fs::remove_file(&socket_path);
    }

    let listener = match std::os::unix::net::UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(_) => return Ok(()),
    };

    let (ui_event_tx, ui_event_rx) = std::sync::mpsc::channel();

    let _lock = SingleInstanceLock { path: socket_path };

    let mut close_on_blur = false;
    let mut force_theme = None;
    let icon_only = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            "--close-on-blur" => {
                close_on_blur = true;
                i += 1;
            }
            "--theme" => {
                if i + 1 < args.len() {
                    force_theme = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!("Error: --theme requires a value");
                    std::process::exit(1);
                }
            }
            _ => {
                eprintln!("Error: Unknown argument: {}", args[i]);
                print_help();
                std::process::exit(1);
            }
        }
    }

    let (width, height) = load_window_size();

    let title = "Open Application Windows";

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_inner_size([width, height])
            .with_resizable(true),
        ..Default::default()
    };

    eframe::run_native(
        title,
        options,
        Box::new(move |cc| {
            let repaint_ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        Ok(_) => {
                            let _ = ui_event_tx.send(UiEvent::FocusLauncher);
                            repaint_ctx.request_repaint();
                        }
                        Err(_) => break,
                    }
                }
            });

            Ok(Box::new(App::new(
                cc,
                close_on_blur,
                force_theme,
                mode,
                icon_only,
                ui_event_rx,
            )))
        }),
    )
}

fn set_sink_input_volume(index: u32, volume_percent: u32) {
    let _ = Command::new("pactl")
        .args(&[
            "set-sink-input-volume",
            &index.to_string(),
            &format!("{}%", volume_percent),
        ])
        .status();
}

fn set_sink_input_mute(index: u32, mute: bool) {
    let _ = Command::new("pactl")
        .args(&[
            "set-sink-input-mute",
            &index.to_string(),
            if mute { "1" } else { "0" },
        ])
        .status();
}

fn sink_display_volume_percent(sink: &PactlSinkInput) -> u32 {
    sink.volume
        .values()
        .next()
        .and_then(|chan| chan.value_percent.trim_end_matches('%').parse::<u32>().ok())
        .unwrap_or(100)
}

fn dedup_sink_inputs_for_controls(sink_inputs: &[PactlSinkInput]) -> Vec<PactlSinkInput> {
    let mut deduped = Vec::new();
    let mut seen_process_ids = HashSet::new();

    for sink in sink_inputs {
        if let Some(process_id) = sink.properties.get("application.process.id") {
            if seen_process_ids.insert(process_id.clone()) {
                deduped.push(sink.clone());
            }
            continue;
        }
        deduped.push(sink.clone());
    }

    deduped
}

fn find_sink_inputs_for_window(
    window: &WindowInfo,
    sink_inputs: &[PactlSinkInput],
) -> Vec<PactlSinkInput> {
    let mut matches = Vec::new();

    // 1. Try to match by PID
    if let Some(wpid) = window.pid {
        let wpid_str = wpid.to_string();
        for sink in sink_inputs {
            if let Some(pid_val) = sink.properties.get("application.process.id") {
                if pid_val == &wpid_str {
                    matches.push(sink.clone());
                }
            }
        }
    }

    // 2. Try to match by process chain PIDs
    if matches.is_empty() {
        for entry in &window.process_chain {
            let pid_str = entry.pid.to_string();
            for sink in sink_inputs {
                if let Some(pid_val) = sink.properties.get("application.process.id") {
                    if pid_val == &pid_str {
                        matches.push(sink.clone());
                    }
                }
            }
        }
    }

    // 3. Try to match by class or active process name
    if matches.is_empty() {
        let class_lower = window.class.to_lowercase();
        let active_lower = window.active_process.as_ref().map(|s| s.to_lowercase());
        for sink in sink_inputs {
            let app_name = sink
                .properties
                .get("application.name")
                .map(|s| s.to_lowercase());
            let app_binary = sink
                .properties
                .get("application.process.binary")
                .map(|s| s.to_lowercase());

            let name_match = app_name.as_ref().map_or(false, |n| {
                n.contains(&class_lower)
                    || class_lower.contains(n)
                    || active_lower
                        .as_ref()
                        .map_or(false, |act| n.contains(act) || act.contains(n))
            });
            let binary_match = app_binary.as_ref().map_or(false, |b| {
                b.contains(&class_lower)
                    || class_lower.contains(b)
                    || active_lower
                        .as_ref()
                        .map_or(false, |act| b.contains(act) || act.contains(b))
            });

            if name_match || binary_match {
                matches.push(sink.clone());
            }
        }
    }

    matches
}

fn setup_system_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // Fallback paths for symbol fonts supporting Braille
    let paths = [
        "/usr/share/fonts/noto/NotoSansSymbols-Regular.ttf",
        "/usr/share/fonts/noto/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    ];

    let mut loaded_any = false;
    for (i, path) in paths.iter().enumerate() {
        if let Ok(data) = std::fs::read(path) {
            let key = format!("sys_symbol_{}", i);
            fonts.font_data.insert(
                key.clone(),
                std::sync::Arc::new(egui::FontData::from_owned(data)),
            );
            if let Some(vec) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                vec.push(key.clone());
            }
            if let Some(vec) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                vec.push(key);
            }
            loaded_any = true;
        }
    }

    if loaded_any {
        ctx.set_fonts(fonts);
    }
}
