use std::path::{Path, PathBuf};

use crate::is_terminal_app_name;
use crate::models::WindowIconCacheKey;

pub(crate) fn is_terminal_class(class_lower: &str) -> bool {
    class_lower.contains("terminal")
        || class_lower == "konsole"
        || class_lower == "kitty"
        || class_lower == "alacritty"
        || class_lower == "wezterm"
        || class_lower == "foot"
}

pub(crate) fn window_icon_cache_key(
    class: &str,
    desktop_file_name: Option<&str>,
    active_process: Option<&str>,
    executable: Option<&Path>,
) -> WindowIconCacheKey {
    WindowIconCacheKey {
        class: class.trim().to_lowercase(),
        desktop_file_name: desktop_file_name.map(|value| value.trim().to_lowercase()),
        active_process: active_process.map(|value| value.trim().to_lowercase()),
        executable: executable.map(Path::to_path_buf),
    }
}

pub(crate) fn resolve_window_icon(
    theme: &str,
    class: &str,
    desktop_file_name: Option<&str>,
    active_process: Option<&str>,
    executable: Option<&Path>,
) -> Option<PathBuf> {
    let terminal_window = is_terminal_class(&class.to_lowercase());
    let tor_browser_window = is_tor_browser_identity(class);
    let mut candidates = Vec::new();
    let mut push_candidate = |candidate: Option<&str>| {
        let Some(candidate) = candidate.map(str::trim).filter(|value| !value.is_empty()) else {
            return;
        };
        if !terminal_window && is_terminal_app_name(candidate) {
            return;
        }
        if !candidates
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(candidate))
        {
            candidates.push(candidate.to_string());
        }
    };

    if terminal_window {
        push_candidate(active_process);
    }
    push_candidate(desktop_file_name);
    if let Some(desktop_stem) = desktop_file_name.and_then(|value| value.strip_suffix(".desktop")) {
        push_candidate(Some(desktop_stem));
    }
    push_candidate(Some(class));
    if !tor_browser_window {
        push_candidate(
            executable
                .and_then(Path::file_name)
                .and_then(|name| name.to_str()),
        );
    }

    candidates
        .into_iter()
        .find_map(|candidate| find_icon(theme, &candidate))
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

pub(crate) fn lookup_theme_icon_exact(theme: &str, name: &str) -> Option<PathBuf> {
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

pub(crate) fn is_tor_browser_identity(value: &str) -> bool {
    let lower = value.to_lowercase();
    lower.contains("tor browser") || lower.contains("tor-browser") || lower.contains("torbrowser")
}

pub(crate) fn find_icon(theme: &str, class: &str) -> Option<PathBuf> {
    if class.is_empty() {
        return None;
    }

    let lower = class.to_lowercase();
    let mut names = vec![lower.clone(), class.to_string()];
    let is_tor_browser = is_tor_browser_identity(&lower);
    if is_tor_browser {
        names.insert(0, "org.torproject.torbrowser-launcher".to_string());
        names.push("tor-browser".to_string());
        names.push("tor-browser-alpha".to_string());
        names.push("torbrowser".to_string());
    }

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
    if !is_tor_browser
        && (lower.contains("web") || lower.contains("browser") || lower.contains("firefox"))
    {
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
