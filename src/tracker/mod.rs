mod client;
mod database;
mod install;
mod model;
mod restore;
mod service;

pub use client::TrackerClient;
pub use install::{ensure_tracker_installed, tracker_binary_path};
pub use model::*;
pub use restore::{restore_entries, restore_snapshot};
pub use service::run_tracker_daemon;

pub const SERVICE_NAME: &str = "com.terrydaktal.ApplicationLauncher";
pub const FEED_PATH: &str = "/WindowFeed";
pub const TRACKER_PATH: &str = "/Tracker";
pub const TRACKER_INTERFACE: &str = "com.terrydaktal.ApplicationLauncher.Tracker1";

pub fn state_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .map(|home| home.join(".local/state"))
        })
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("applicationlauncher")
}
