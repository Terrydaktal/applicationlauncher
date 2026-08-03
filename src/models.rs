use fuzzy_rank::ranking::SearchRank;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessChainEntry {
    pub pid: i32,
    pub name: String,
    pub exe_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PactlVolumeChannel {
    pub value_percent: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PactlSinkInput {
    pub index: u32,
    #[serde(default)]
    pub corked: bool,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub volume: HashMap<String, PactlVolumeChannel>,
    #[serde(default)]
    pub properties: HashMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: String,
    pub title: String,
    pub raw_title: String,
    pub class: String,
    pub desktop_file_name: Option<String>,
    pub minimized: Option<bool>,
    pub demands_attention: bool,
    pub icon_path: Option<PathBuf>,
    pub active_process: Option<String>,
    pub exe_path: Option<PathBuf>,
    pub cwd_path: Option<PathBuf>,
    pub command_line: Option<String>,
    pub command_summary: Option<String>,
    pub geometry: Option<(i32, i32, i32, i32)>,
    pub process_chain: Vec<ProcessChainEntry>,
    pub pid: Option<i32>,
    pub last_activated_at_ms: Option<i64>,
    pub activation_sequence: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WindowIconCacheKey {
    pub class: String,
    pub desktop_file_name: Option<String>,
    pub active_process: Option<String>,
    pub executable: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalDbusRecord {
    pub window_uuid: String,
    pub tab_uuid: String,
    pub active: bool,
    pub window_title: String,
    pub working_directory: String,
    pub child_pid: u32,
    pub foreground_pid: u32,
    pub foreground_pgid: u32,
    pub pty: String,
}

#[derive(Clone, Debug, Default)]
pub struct TerminalWindowIdentity {
    pub normalized_title: String,
    pub cwd: Option<PathBuf>,
    pub process_pids: HashSet<u32>,
    pub process_groups: HashSet<u32>,
    pub ptys: HashSet<String>,
}

#[derive(Clone, Debug)]
pub struct AppInfo {
    pub name: String,
    pub exec: String,
    pub icon_path: Option<PathBuf>,
    pub comment: Option<String>,
    pub desktop_file_path: PathBuf,
    pub is_settings_module: bool,
}

#[derive(Clone, Debug)]
pub struct RankedAppMatch {
    pub app: AppInfo,
    pub rank: SearchRank,
    pub title_is_typo: bool,
    pub visible_match_priority: u8,
    pub is_pinned: bool,
    pub display_title: String,
    pub highlight_segments: Vec<(usize, usize, bool)>,
    pub search_values: Vec<(u8, String)>,
    pub candidate_key: String,
    pub candidate_score: f64,
}

#[derive(Clone, Debug)]
pub struct RankedWindowMatch {
    pub window: WindowInfo,
    pub rank: SearchRank,
    pub title_is_typo: bool,
    pub visible_match_priority: u8,
    pub display_title: String,
    pub highlight_segments: Vec<(usize, usize, bool)>,
    pub search_values: Vec<(u8, String)>,
    pub candidate_key: String,
    pub candidate_score: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LauncherMode {
    Windows,
    Apps,
}

pub enum LoadResult {
    AppsSuccess(Vec<AppInfo>),
    WindowsSuccess(Vec<WindowInfo>),
    Error(String),
}

pub enum UiEvent {
    FocusLauncher,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct KWinWindowPayload {
    pub id: String,
    pub title: String,
    pub class: String,
    #[serde(default)]
    pub pid: i32,
    #[serde(default)]
    pub desktop_file_name: String,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default)]
    pub width: i32,
    #[serde(default)]
    pub height: i32,
    #[serde(default)]
    pub minimized: bool,
    #[serde(default)]
    pub demands_attention: bool,
    #[serde(default)]
    pub last_activated_at_ms: Option<i64>,
    #[serde(default)]
    pub activation_sequence: i64,
}

#[derive(Clone, Debug)]
pub enum WindowFeedEvent {
    Reset,
    Snapshot(Vec<KWinWindowPayload>),
    Upsert(KWinWindowPayload),
    Remove(String),
    RearmAttentionAutomation,
}

#[derive(Clone, Debug)]
pub struct AudioCacheUpdate {
    pub sink_inputs: Vec<PactlSinkInput>,
    pub active_media_app_keys: HashSet<String>,
    pub observed_pipewire_node_ids: HashSet<u32>,
    pub active_pipewire_node_ids: HashSet<u32>,
    pub pipewire_activity_cache_valid: bool,
}

#[derive(Clone, Debug, Default)]
pub struct WindowAudioCache {
    pub sink_matches: HashMap<String, Vec<PactlSinkInput>>,
    pub level_buckets: HashMap<String, u8>,
}

pub struct SnapshotWindowDetails {
    pub desktop_file_name: Option<String>,
    pub geometry: Option<(i32, i32, i32, i32)>,
    pub minimized: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivePane {
    Windows,
    Apps,
}

#[derive(Clone, Debug)]
pub struct FilteredSearchResults {
    pub apps: Arc<Vec<(AppInfo, bool)>>,
    pub windows: Arc<Vec<WindowInfo>>,
    pub app_display_titles: Arc<Vec<String>>,
    pub window_display_titles: Arc<Vec<String>>,
    pub app_highlight_segments: Arc<Vec<Vec<(usize, usize, bool)>>>,
    pub app_name_highlight_segments: Arc<Vec<Vec<(usize, usize, bool)>>>,
    pub window_highlight_segments: Arc<Vec<Vec<(usize, usize, bool)>>>,
    pub app_title_is_typos: Arc<Vec<bool>>,
    pub window_title_is_typos: Arc<Vec<bool>>,
}

#[derive(Clone, Debug)]
pub struct FilteredSearchCache {
    pub key: FilteredSearchCacheKey,
    pub results: FilteredSearchResults,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilteredSearchCacheKey {
    pub mode: LauncherMode,
    pub query: String,
    pub show_system_settings_modules: bool,
    pub pinned_apps_generation: u64,
    pub apps_generation: u64,
    pub windows_generation: u64,
}

#[derive(Clone, Copy)]
pub struct LauncherSettings {
    pub show_system_settings_modules: bool,
    pub app_icon_mode: bool,
    pub win_icon_size: f32,
    pub win_top_padding: f32,
    pub win_bottom_padding: f32,
    pub win_left_padding: f32,
    pub win_right_padding: f32,
    pub win_row_height: f32,
    pub win_text_spacing: f32,
    pub win_line_height: f32,
    pub win_show_path: bool,
    pub win_show_last_activation: bool,
    pub show_run_in_terminal: bool,
    pub show_cd_in_terminal: bool,
    pub auto_send_enter_on_attention: bool,
    pub win_title_size: f32,
    pub win_path_size: f32,
    pub app_icon_size: f32,
    pub app_icon_tile_size: f32,
    pub app_top_padding: f32,
    pub app_bottom_padding: f32,
    pub app_left_padding: f32,
    pub app_right_padding: f32,
    pub app_icon_show_name: bool,
    pub app_icon_name_size: f32,
    pub disable_ibeam: bool,
    pub app_scroll_sensitivity: f32,
    pub win_scroll_sensitivity: f32,
    pub ui_scale: f32,
}

impl Default for LauncherSettings {
    fn default() -> Self {
        Self {
            show_system_settings_modules: true,
            app_icon_mode: false,
            win_icon_size: 32.0,
            win_top_padding: 6.0,
            win_bottom_padding: 6.0,
            win_left_padding: 12.0,
            win_right_padding: 12.0,
            win_row_height: 52.0,
            win_text_spacing: 2.0,
            win_line_height: 14.0,
            win_show_path: true,
            win_show_last_activation: false,
            show_run_in_terminal: true,
            show_cd_in_terminal: true,
            auto_send_enter_on_attention: false,
            win_title_size: 13.0,
            win_path_size: 10.5,
            app_icon_size: 32.0,
            app_icon_tile_size: 68.0,
            app_top_padding: 6.0,
            app_bottom_padding: 6.0,
            app_left_padding: 12.0,
            app_right_padding: 12.0,
            app_icon_show_name: true,
            app_icon_name_size: 10.5,
            disable_ibeam: false,
            app_scroll_sensitivity: 1.0,
            win_scroll_sensitivity: 1.0,
            ui_scale: 1.0,
        }
    }
}

#[derive(Clone, Debug)]
pub enum PopupEvent {
    CloseSettings,
    CloseWindowInfo,
    CloseAppInfo,
    CloseHistory,
}

#[derive(Clone)]
pub struct SettingsWindowState {
    pub settings: LauncherSettings,
    pub pending_ui_scale: f32,
    pub scale_anchor: f32,
    pub revision: u64,
}

#[derive(Clone)]
pub struct InfoPopupRow {
    pub label: String,
    pub value: String,
    pub searched: bool,
}

#[derive(Clone)]
pub struct InfoPopupData {
    pub title: String,
    pub heading: String,
    pub subtitle: String,
    pub rows: Vec<InfoPopupRow>,
    pub execution_chain: Vec<(String, String)>,
}
